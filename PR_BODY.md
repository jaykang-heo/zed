# Fix RefCell already borrowed crash during window creation (ZED-1K)

## Crash Summary

**Sentry Issue:** [ZED-1K](https://sentry.io/organizations/zed-dev/issues/6798453910/) (807 events)

A `RefCell already borrowed` panic occurs on macOS when opening a new window. The crash happens specifically when macOS's automatic window tabbing is enabled and `addTabbedWindow:ordered:` is called, which can trigger a synchronous `display_layer` callback during window creation while the `AppCell` is already mutably borrowed.

## Root Cause

The thermal state detection feature (PR #45638) added a call to `cx.update(|cx| cx.thermal_state())` at the beginning of the `on_request_frame` callback. This is normally safe because `on_request_frame` is called asynchronously by the display link.

However, on macOS, when a new window is added as a tab to an existing window via `addTabbedWindow:ordered:`, Core Animation can **synchronously** call `display_layer`, which invokes the `on_request_frame` callback. At this point, the `App` is still mutably borrowed by the outer `open_window` call, causing the `RefCell` to panic.

The call sequence:
1. `App::open_window` → `self.update(|cx| ...)` (acquires mutable borrow of `AppCell`)
2. Inside update: `Window::new` → `MacWindow::open` → `addTabbedWindow:ordered:`
3. macOS synchronously triggers `display_layer` → `on_request_frame` callback
4. Callback calls `cx.update(|cx| cx.thermal_state())` → attempts second mutable borrow → **panic**

## Fix

Added a `try_update` method to `AsyncApp` that uses `try_borrow_mut` instead of `borrow_mut`, returning a `Result` instead of panicking. The `on_request_frame` callback now uses `try_update` for the thermal state check and gracefully returns early if the borrow fails.

This is safe because:
- During window creation, there's nothing meaningful to render yet
- Another frame will be requested once the window creation completes
- Normal frame rendering continues to work with thermal throttling

## Validation

- [x] Code compiles (`cargo check -p gpui`)
- [x] Unit tests added for `try_update` method behavior
- [ ] Full test suite (requires macOS system libraries unavailable in CI environment)
- [ ] Manual testing on macOS with automatic window tabbing enabled

Note: Full tests and clippy require X11/Wayland system libraries that are not available in this CI environment. The fix should be validated on a macOS development machine before merging.

## Potentially Related Issues

### High Confidence
- None found

### Medium Confidence  
- None found

### Low Confidence
- None found

GitHub issue searches for related terms (RefCell borrow, display_layer, window tabbing, thermal state) returned no matching user-reported issues.

## Reviewer Checklist

- [ ] Verify the fix doesn't break thermal throttling in normal scenarios
- [ ] Test on macOS with automatic window tabbing enabled:
  - Open System Settings → Desktop & Dock → Windows → "Prefer tabs when opening documents"
  - Set to "Always" or "In Full Screen Only"
  - Open multiple Zed windows and verify no crash
- [ ] Confirm `try_update` is the appropriate pattern vs other approaches (e.g., deferring the frame)
- [ ] Consider if other callbacks in `Window::new` might have similar issues

---

Release Notes:

- Fixed a crash that could occur when opening new windows on macOS with automatic window tabbing enabled (ZED-1K)
