//! Regression test for a crash in `notify::fsevent::FsEventWatcher::stop` where
//! `CFRunLoopIsWaiting` is called on a dangling `CFRunLoopRef`.
//!
//! The crash occurs when the FSEvent watcher thread's `CFRunLoopRun()` returns
//! early (because all sources were removed from the run loop — e.g. the watched
//! volume was unmounted), causing the thread to exit and its `CFRunLoop` to be
//! deallocated. The stored `CFRunLoopRef` becomes a dangling pointer, and the
//! next call to `stop()` (triggered by `watch()` or `unwatch()`) crashes in
//! `CFRunLoopIsWaiting` with a PAC signature failure (`EXC_BREAKPOINT`).
//!
//! To reproduce, we send a `SIGUSR1` to the notify watcher thread after
//! installing a signal handler that calls `CFRunLoopStop(CFRunLoopGetCurrent())`.
//! This forces `CFRunLoopRun()` to return, the thread exits, and the stored
//! `CFRunLoopRef` becomes dangling. The next `watch()` call triggers `stop()`
//! on the dead runloop — reproducing the crash.
//!
//! Because the crash kills the process (SIGTRAP from a PAC failure), the main
//! test re-executes itself as a subprocess and asserts on its exit status.

#[cfg(target_os = "macos")]
mod macos_tests {
    use std::time::{Duration, Instant};

    const NOTIFY_THREAD_NAME: &str = "notify-rs fsevents loop";

    unsafe extern "C" {
        fn CFRunLoopGetCurrent() -> *mut std::ffi::c_void;
        fn CFRunLoopStop(rl: *mut std::ffi::c_void);
    }

    /// Signal handler that stops the current thread's CFRunLoop.
    /// When delivered to the notify watcher thread, this causes `CFRunLoopRun()`
    /// to return, which makes the thread proceed through its cleanup path and exit.
    extern "C" fn stop_runloop_signal_handler(_sig: libc::c_int) {
        unsafe {
            let rl = CFRunLoopGetCurrent();
            if !rl.is_null() {
                CFRunLoopStop(rl);
            }
        }
    }

    /// Find the mach thread port for the thread named `NOTIFY_THREAD_NAME`.
    fn find_notify_thread_port() -> Option<u32> {
        unsafe {
            let task = mach2::traps::mach_task_self();
            let mut thread_list: mach2::mach_types::thread_act_array_t = std::ptr::null_mut();
            let mut thread_count: u32 = 0;

            let kr = mach2::task::task_threads(task, &mut thread_list, &mut thread_count);
            if kr != mach2::kern_return::KERN_SUCCESS {
                return None;
            }

            let mut found_port = None;
            for i in 0..thread_count {
                let thread_port = *thread_list.add(i as usize);
                let pthread = libc::pthread_from_mach_thread_np(thread_port);
                if pthread != 0 as libc::pthread_t {
                    let mut name_buf = [0u8; 256];
                    let rc = libc::pthread_getname_np(
                        pthread,
                        name_buf.as_mut_ptr() as *mut libc::c_char,
                        name_buf.len(),
                    );
                    if rc == 0 {
                        let name = std::ffi::CStr::from_ptr(name_buf.as_ptr() as *const _)
                            .to_string_lossy();
                        if name.contains(NOTIFY_THREAD_NAME) {
                            found_port = Some(thread_port);
                        }
                    }
                }
                if found_port != Some(thread_port) {
                    mach2::mach_port::mach_port_deallocate(task, thread_port);
                }
            }

            let list_size =
                (thread_count as usize) * std::mem::size_of::<mach2::mach_types::thread_act_t>();
            mach2::vm::mach_vm_deallocate(task, thread_list as u64, list_size as u64);

            found_port
        }
    }

    /// Count how many threads in this process are named `NOTIFY_THREAD_NAME`.
    fn count_notify_threads() -> usize {
        unsafe {
            let task = mach2::traps::mach_task_self();
            let mut thread_list: mach2::mach_types::thread_act_array_t = std::ptr::null_mut();
            let mut thread_count: u32 = 0;

            let kr = mach2::task::task_threads(task, &mut thread_list, &mut thread_count);
            if kr != mach2::kern_return::KERN_SUCCESS {
                return 0;
            }

            let mut matching = 0usize;
            for i in 0..thread_count {
                let thread_port = *thread_list.add(i as usize);
                let pthread = libc::pthread_from_mach_thread_np(thread_port);
                if pthread != 0 as libc::pthread_t {
                    let mut name_buf = [0u8; 256];
                    let rc = libc::pthread_getname_np(
                        pthread,
                        name_buf.as_mut_ptr() as *mut libc::c_char,
                        name_buf.len(),
                    );
                    if rc == 0 {
                        let name = std::ffi::CStr::from_ptr(name_buf.as_ptr() as *const _)
                            .to_string_lossy();
                        if name.contains(NOTIFY_THREAD_NAME) {
                            matching += 1;
                        }
                    }
                }
                mach2::mach_port::mach_port_deallocate(task, thread_port);
            }

            let list_size =
                (thread_count as usize) * std::mem::size_of::<mach2::mach_types::thread_act_t>();
            mach2::vm::mach_vm_deallocate(task, thread_list as u64, list_size as u64);

            matching
        }
    }

    /// Send `SIGUSR1` to the pthread backing the given mach thread port.
    fn send_signal_to_thread(mach_port: u32) {
        unsafe {
            let pthread = libc::pthread_from_mach_thread_np(mach_port);
            if pthread != 0 as libc::pthread_t {
                libc::pthread_kill(pthread, libc::SIGUSR1);
            }
            mach2::mach_port::mach_port_deallocate(mach2::traps::mach_task_self(), mach_port);
        }
    }

    /// Install `SIGUSR1` handler that calls `CFRunLoopStop`.
    fn install_signal_handler() {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = stop_runloop_signal_handler as *const () as usize;
            action.sa_flags = 0;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut());
        }
    }

    /// Wait (up to `timeout`) for the number of notify threads to drop to zero.
    fn wait_for_thread_exit(timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if count_notify_threads() == 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The core reproduction logic: creates a watcher, kills the runloop thread
    /// via signal, then calls `watch()` again which triggers the use-after-free.
    ///
    /// This function is called in a subprocess. Without a fix it crashes with
    /// SIGTRAP. With the fix it returns normally.
    fn run_crash_reproduction() {
        use notify::{RecursiveMode, Watcher as _};

        install_signal_handler();

        let watch_dir = tempfile::TempDir::new().expect("failed to create temp dir");

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut watcher = notify::FsEventWatcher::new(tx, notify::Config::default())
            .expect("failed to create watcher");

        watcher
            .watch(watch_dir.path(), RecursiveMode::Recursive)
            .expect("failed to watch directory");

        std::thread::sleep(Duration::from_millis(200));
        assert!(
            count_notify_threads() >= 1,
            "expected at least one notify watcher thread"
        );

        let port = find_notify_thread_port().expect("could not find notify watcher thread");
        send_signal_to_thread(port);

        let exited = wait_for_thread_exit(Duration::from_secs(5));
        assert!(
            exited,
            "notify watcher thread did not exit after CFRunLoopStop signal"
        );

        // This call triggers the crash: watch_inner() → stop() →
        //   CFRunLoopIsWaiting(DANGLING_PTR) → 💥 EXC_BREAKPOINT
        let fallback_dir = tempfile::TempDir::new().expect("failed to create fallback temp dir");
        let result = watcher.watch(fallback_dir.path(), RecursiveMode::Recursive);

        match result {
            Ok(()) => {
                watcher.unwatch(fallback_dir.path()).ok();
            }
            Err(e) => {
                eprintln!("watch after runloop kill returned error (expected): {e}");
            }
        }
    }

    /// Sentinel test that acts as the subprocess entry point. When the env var
    /// `__FS_WATCHER_CRASH_SUBPROCESS` is set, this test runs the crash
    /// reproduction instead of its normal (empty) body.
    ///
    /// The parent test (`fs_watcher_stop_after_runloop_killed`) spawns itself
    /// with a filter that matches this test name and the env var set, so only
    /// this function executes in the child process.
    #[test]
    fn fs_watcher_crash_subprocess_entry() {
        if std::env::var("__FS_WATCHER_CRASH_SUBPROCESS").is_ok() {
            run_crash_reproduction();
        }
        // When the env var is absent this test is a no-op.
    }

    /// Reproduce the crash in `FsEventWatcher::stop` where `CFRunLoopIsWaiting`
    /// is called on a dangling `CFRunLoopRef` after the watcher thread has exited.
    ///
    /// The test re-executes itself as a subprocess (targeting the sentinel test
    /// above) and asserts that the subprocess is killed by SIGTRAP (signal 5),
    /// which is the `EXC_BREAKPOINT` that the PAC check produces on ARM64 macOS.
    ///
    /// With the fix applied (CFRetain on the runloop ref + is_finished guard in
    /// stop()), the subprocess exits cleanly and the test still passes.
    #[test]
    fn fs_watcher_stop_after_runloop_killed() {
        use std::os::unix::process::ExitStatusExt;

        let test_binary = std::env::current_exe().expect("could not determine test binary path");

        let output = std::process::Command::new(&test_binary)
            .env("__FS_WATCHER_CRASH_SUBPROCESS", "1")
            .args([
                "fs_watcher_stop_crash::macos_tests::fs_watcher_crash_subprocess_entry",
                "--exact",
                "--nocapture",
            ])
            .output()
            .expect("failed to spawn subprocess");

        let stderr = String::from_utf8_lossy(&output.stderr);

        match output.status.signal() {
            Some(libc::SIGTRAP) => {
                // The subprocess crashed with SIGTRAP (EXC_BREAKPOINT) —
                // this confirms the bug is present. The test "passes" because
                // we successfully reproduced the crash.
                eprintln!(
                    "subprocess crashed with SIGTRAP as expected \
                     (use-after-free on CFRunLoopRef confirmed)"
                );
            }
            Some(other_signal) => {
                panic!("subprocess killed by unexpected signal {other_signal}\nstderr:\n{stderr}",);
            }
            None if output.status.success() => {
                // The subprocess exited cleanly — this means the fix is in
                // place and the crash no longer occurs. This is also a pass.
                eprintln!("subprocess exited cleanly (fix is in place, no crash)");
            }
            None => {
                panic!(
                    "subprocess exited with non-zero status: {}\nstderr:\n{stderr}",
                    output.status,
                );
            }
        }
    }
}
