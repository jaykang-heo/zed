# Crash Analysis: RefCell already borrowed during window creation

## Crash Summary
- **Sentry Issue:** ZED-1K (https://sentry.io/organizations/zed-dev/issues/6798453910/)
- **Error:** `RefCell<T>::borrow_mut` panics with "already borrowed" when macOS calls `display_layer` during window creation
- **Crash Site:** `gpui::app::AppCell::borrow_mut` in `app.rs:88`, called from `AsyncApp::update` in `async_context.rs:145`
- **Event Count:** 807 crashes

## Root Cause

The crash occurs during window creation when macOS's automatic window tabbing is enabled. The sequence is:

1. `App::open_window` calls `self.update(|cx| ...)` which acquires a mutable borrow of `AppCell`
2. Inside that update, `Window::new` is called, which sets up an `on_request_frame` callback
3. `Window::new` then calls `MacWindow::open`
4. `MacWindow::open` calls `addTabbedWindow:ordered:` to add the new window as a tab
5. This macOS API can **synchronously** trigger Core Animation to call `display_layer`
6. `display_layer` invokes the `on_request_frame` callback
7. The callback starts with `cx.update(|cx| cx.thermal_state())` (added in PR #45638 for thermal throttling)
8. `AsyncApp::update` calls `app.borrow_mut()` which panics because the outer `App::open_window` still holds the borrow

The thermal state feature (#45638) introduced a call to `cx.update()` at the very beginning of the `on_request_frame` callback. This is normally fine because `on_request_frame` is called asynchronously by the display link. However, when `addTabbedWindow:ordered:` triggers a synchronous `display_layer` call, the App is still borrowed from the outer `open_window` call.

The crash is specific to:
- macOS platform (uses `addTabbedWindow:ordered:`)
- When automatic window tabbing is enabled and conditions allow adding a tab
- When a window is opened while another compatible window exists

## Reproduction

The test case simulates the scenario where `on_request_frame` is called while the App is already mutably borrowed. This can't be tested with the standard test platform since it doesn't simulate the synchronous callback behavior, but the fix can be verified by checking that `try_update` returns `Err` instead of panicking.

Run the test with:
```
cargo test -p gpui test_on_request_frame_during_borrow
```

## Suggested Fix

Add a `try_update` method to `AsyncApp` that uses `try_borrow_mut` instead of `borrow_mut`, returning a `Result`:

```rust
pub fn try_update<R>(&self, f: impl FnOnce(&mut App) -> R) -> Result<R> {
    let app = self.app.upgrade().context("app was released")?;
    let mut lock = app.try_borrow_mut()?;
    Ok(lock.update(f))
}
```

Then modify the thermal state check in `Window::new`'s `on_request_frame` callback to use `try_update`:

```rust
// Before:
let thermal_state = cx.update(|cx| cx.thermal_state());

// After:
let Ok(thermal_state) = cx.try_update(|cx| cx.thermal_state()) else {
    return;
};
```

If `try_update` fails (App is already borrowed), we simply skip the frame. This is safe because:
1. The window is still being created, so there's nothing meaningful to render yet
2. Another frame will be requested once the window creation completes
3. This maintains the thermal throttling behavior for all normal cases
