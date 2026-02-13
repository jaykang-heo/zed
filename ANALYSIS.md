# Crash Analysis: `CFRunLoopIsWaiting` use-after-free in FSEvent watcher

## Status: Fix verified ✅

The fix has been written and tested in `/tmp/notify-fix` (branch `fix/cfrunloop-use-after-free`).
It needs to be pushed to `https://github.com/zed-industries/notify.git` and the rev updated in `Cargo.toml`.

## Crash Summary

- **Error:** `EXC_BREAKPOINT / EXC_ARM_BREAKPOINT` at `__CFCheckCFInfoPACSignature` — a Pointer Authentication Code (PAC) validation failure on ARM64 macOS, indicating a use-after-free on a `CFRunLoopRef`.
- **Crash Site:** `notify::fsevent::FsEventWatcher::stop` (`fsevent.rs:340`) calling `CFRunLoopIsWaiting` on an invalid `CFRunLoopRef`.

## Stack Trace (key frames)

```
CoreFoundation       __CFCheckCFInfoPACSignature
CoreFoundation       CFRunLoopIsWaiting
notify::fsevent::FsEventWatcher::stop              (fsevent.rs:340)
notify::fsevent::FsEventWatcher::watch_inner        (fsevent.rs:311)
notify::fsevent::FsEventWatcher::watch              (fsevent.rs:588)
fs::fs_watcher::GlobalWatcher::add                  (fs_watcher.rs:198)
fs::fs_watcher::FsWatcher::add                      (fs_watcher.rs:92)
fs::fs_watcher::global                              (fs_watcher.rs:279)
fs::RealFs::watch                                   (fs.rs:994)
worktree::LocalWorktree::start_background_scanner   (worktree.rs:1081)
```

## Root Cause

The crash is a **use-after-free** on a `CFRunLoopRef` stored inside `notify::fsevent::FsEventWatcher`.

### How the FSEvent watcher works

The `notify` crate's `FsEventWatcher` (used on macOS) manages an FSEvent stream that runs on a dedicated thread's `CFRunLoop`. The lifecycle is:

1. **`run()`** spawns a thread that:
   - Gets its run loop via `CFRunLoopGetCurrent()` (returns a **non-retained** "Get-rule" reference)
   - Schedules and starts an `FSEventStream` on that run loop
   - Sends the `CFRunLoopRef` back to the caller via a channel
   - Enters `CFRunLoopRun()` (blocks until stopped)
   - On return: cleans up the stream and exits

2. **`stop()`** (called before every `watch`/`unwatch` to reconfigure):
   - Takes `self.runloop` (the stored `CFRunLoopRef` + `JoinHandle`)
   - **Spin-loops** calling `CFRunLoopIsWaiting(runloop)` until it returns true
   - Calls `CFRunLoopStop(runloop)` to unblock the thread
   - Joins the thread

3. **`watch_inner()`** calls `stop()`, appends the new path, then calls `run()`.

### The bug

According to Apple's documentation, `CFRunLoopRun()` returns not only when `CFRunLoopStop()` is called, but also **when all sources and timers are removed from the run loop's default mode**:

> "The current thread's run loop runs in the default mode until the run loop is stopped with `CFRunLoopStop` or all the sources and timers are removed from the default run loop mode."

If the FSEvent stream is invalidated by the system (e.g., the watched volume is unmounted, disk ejected, or system resource pressure causes stream teardown), the stream — which is the only source on the run loop — is removed. This causes `CFRunLoopRun()` to return on its own. The thread then:

- Runs cleanup code (`FSEventStreamStop`, `FSEventStreamInvalidate`, `FSEventStreamRelease`)
- Exits

When the thread exits, its `CFRunLoop` is **deallocated** (since `CFRunLoopGetCurrent()` returned a non-retained reference tied to the thread's lifetime). However, `self.runloop` still holds the now-dangling `CFRunLoopRef` pointer.

The next call to `watch()` or `unwatch()` triggers `stop()`, which calls `CFRunLoopIsWaiting(dangling_ptr)`. The ARM64 PAC check on the freed `CFRunLoop` object fails, producing the `EXC_BREAKPOINT` / `__CFCheckCFInfoPACSignature` crash.

### Sequence of events

```
Thread A (watcher thread)              Thread B (caller)
─────────────────────────              ─────────────────
CFRunLoopRun() running...
                                       // ... some time later ...
[system invalidates stream]
CFRunLoopRun() returns (no sources)
FSEventStreamStop(stream)
FSEventStreamInvalidate(stream)
FSEventStreamRelease(stream)
thread exits → CFRunLoop deallocated
                                       GlobalWatcher::add(new_path)
                                         watcher.lock().watch(new_path)
                                           watch_inner()
                                             stop()
                                               self.runloop.take() → Some((DANGLING, handle))
                                               CFRunLoopIsWaiting(DANGLING) → 💥 CRASH
```

### Contributing factors

1. **Non-retained `CFRunLoopRef`**: `CFRunLoopGetCurrent()` follows the "Get Rule" — the caller does not own the reference. A `CFRetain` call would extend the lifetime beyond the thread, converting the crash into a safe no-op (calling `CFRunLoopIsWaiting` / `CFRunLoopStop` on a stopped-but-valid run loop is safe).

2. **No detection of early thread exit**: The `stop()` method assumes the thread is always alive when `self.runloop.is_some()`. It never checks whether the thread has already exited (e.g., via `thread_handle.is_finished()`).

3. **`kFSEventStreamCreateFlagWatchRoot`**: Zed's fork of `notify` adds this flag, which causes additional events when the root path changes (rename, delete, unmount). This may make stream invalidation more likely compared to upstream `notify`.

## Reproduction

The crash is reproduced **deterministically** by the test in `crates/fs/tests/integration/fs_watcher_stop_crash.rs`.

The test simulates the conditions that cause `CFRunLoopRun()` to return early by sending `SIGUSR1` to the notify watcher thread. A process-wide signal handler is installed that calls `CFRunLoopStop(CFRunLoopGetCurrent())` when `SIGUSR1` is delivered. The test then:

1. Creates a `notify::FsEventWatcher` and watches a temporary directory (this spawns the `"notify-rs fsevents loop"` thread and stores its `CFRunLoopRef`).
2. Enumerates mach threads to find the watcher thread by name, then sends it `SIGUSR1` via `pthread_kill`. The signal handler stops the thread's run loop, causing `CFRunLoopRun()` to return. The thread runs its cleanup code and exits, deallocating the `CFRunLoop`.
3. Calls `watcher.watch()` on a new path, which triggers `watch_inner()` → `stop()` → `CFRunLoopIsWaiting(dangling_ptr)` → **SIGTRAP** (PAC failure).

Because the crash kills the process, the test re-executes itself as a subprocess (detected via the `__FS_WATCHER_CRASH_SUBPROCESS` env var) and asserts the subprocess is killed by `SIGTRAP` (signal 5), confirming the `EXC_BREAKPOINT` from the original crash report.

In production, the same effect occurs when macOS invalidates the FSEvent stream (e.g., volume unmount, disk ejection, system resource pressure), removing it as a source from the run loop and causing `CFRunLoopRun()` to return on its own.

To run:

```
cargo test -p fs --features test-support -- fs_watcher_stop_crash
```

Output **before** the fix (bug present — subprocess crashes with SIGTRAP):

```
subprocess crashed with SIGTRAP as expected (use-after-free on CFRunLoopRef confirmed)
test fs_watcher_stop_crash::macos_tests::fs_watcher_stop_after_runloop_killed ... ok
```

Output **after** the fix (subprocess exits cleanly, no crash):

```
subprocess exited cleanly (fix is in place, no crash)
test fs_watcher_stop_crash::macos_tests::fs_watcher_stop_after_runloop_killed ... ok
```

## Fix (verified ✅)

The fix is in the `notify` crate fork (`zed-industries/notify`), file `notify/src/fsevent.rs`.
A tested patch exists at `/tmp/notify-fix` on branch `fix/cfrunloop-use-after-free`.

### What changed

Three small changes to `notify/src/fsevent.rs`:

**1. Declare `CFRetain`** in the existing `extern "C"` block (line ~265):

```
extern "C" {
    fn CFRetain(cf: cf::CFRef) -> cf::CFRef;

    /// Indicates whether the run loop is waiting for an event.
    fn CFRunLoopIsWaiting(runloop: cf::CFRunLoopRef) -> cf::Boolean;
}
```

**2. Retain the run loop in `run()`** (line ~480, inside the spawned thread):

```
let cur_runloop = cf::CFRunLoopGetCurrent();

// Prevent the run loop from being deallocated when this
// thread exits. Balanced by CFRelease in stop().
CFRetain(cur_runloop);

fs::FSEventStreamScheduleWithRunLoop(
```

**3. Guard the spin loop and release in `stop()`** (line ~339):

```
fn stop(&mut self) {
    if !self.is_running() {
        return;
    }

    if let Some((runloop, thread_handle)) = self.runloop.take() {
        unsafe {
            let runloop = runloop as *mut raw::c_void;

            // If the thread has already exited (e.g. CFRunLoopRun returned
            // early because all sources were removed), skip the spin loop.
            // The CFRunLoopRef is still valid (we retained it) but the run
            // loop will never enter the waiting state, so spinning would
            // hang forever.
            if !thread_handle.is_finished() {
                while CFRunLoopIsWaiting(runloop) == 0 {
                    thread::yield_now();
                }

                cf::CFRunLoopStop(runloop);
            }

            // Balance the CFRetain in run().
            cf::CFRelease(runloop);
        }

        // Wait for the thread to shut down.
        thread_handle.join().expect("thread to shut down");
    }
}
```

### Why this works

- **`CFRetain`** keeps the `CFRunLoopRef` valid even after the thread exits, eliminating the use-after-free. `CFRunLoopIsWaiting` and `CFRunLoopStop` are safe to call on a retained-but-stopped run loop.
- **`is_finished()`** prevents an infinite spin loop: a run loop whose thread has already exited will never enter the "waiting" state, so `CFRunLoopIsWaiting` would return 0 forever.
- While there is a theoretical TOCTOU race between `is_finished()` and `CFRunLoopIsWaiting`, the `CFRetain` makes it safe — even if the thread exits between the two calls, the pointer is still valid. In the worst case, `CFRunLoopStop` is called on an already-stopped run loop, which is a documented no-op.
- **`CFRelease`** after joining balances the retain, preventing a leak.

### Deployment steps

1. Push the `fix/cfrunloop-use-after-free` branch from `/tmp/notify-fix` to `https://github.com/zed-industries/notify.git`.
2. Update `Cargo.toml` `[patch.crates-io]` to point to the new rev:
   ```
   notify = { git = "https://github.com/zed-industries/notify.git", rev = "<new-commit-sha>" }
   notify-types = { git = "https://github.com/zed-industries/notify.git", rev = "<new-commit-sha>" }
   ```
3. Run `cargo update -p notify` to update `Cargo.lock`.
4. Verify: `cargo test -p fs --features test-support -- fs_watcher_stop_crash` should print
   `subprocess exited cleanly (fix is in place, no crash)`.
