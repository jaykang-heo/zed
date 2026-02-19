# edit_prediction: Add regression tests for ZED-4VS UTF-8 char boundary crash

## Crash Summary

[Sentry ZED-4VS](https://sentry.io/organizations/zed-dev/issues/7243282041/) - 538 events between 2026-02-05 and 2026-02-19.

**Error:** `byte index <int> is not a char boundary; it is inside 'о' (bytes <int>..<int>)`

The crash occurred in `parse_edits` when the LLM returned edit predictions with multi-byte UTF-8 characters (Cyrillic, Chinese, etc.) immediately adjacent to `<|editable_region_start|>` or `<|editable_region_end|>` markers.

## Root Cause

The old code unconditionally added `+1` to skip a newline after the start marker and subtracted `1` to skip a newline before the end marker, without verifying:
1. Whether a newline actually exists at those positions
2. Whether those positions are valid UTF-8 character boundaries

When multi-byte characters like Cyrillic 'О' (2 bytes) or Chinese '讷' (3 bytes) appeared right after a marker, the `+1` offset landed in the middle of the character, causing a panic on the subsequent string slice.

## Fix

The fix was already merged in [#48960](https://github.com/zed-industries/zed/pull/48960) (2026-02-12). The fix adds proper `is_char_boundary()` checks before adjusting positions:

```rust
// Before (crash-prone):
.map(|e| e.0 + EDITABLE_REGION_START_MARKER.len() + 1)

// After (safe):
.map(|start| {
    if content.len() > start
        && content.is_char_boundary(start)
        && content[start..].starts_with('\n')
    {
        start + 1
    } else {
        start
    }
})
```

This PR adds additional regression tests that exercise the specific crash scenario from ZED-4VS with Cyrillic text.

## Validation

- All 70 tests in `edit_prediction` crate pass
- Clippy passes with no warnings
- New tests specifically cover:
  - Cyrillic text with blank line after start marker
  - Cyrillic text without end marker (fallback path)

Run tests with: `cargo test -p edit_prediction zeta1::tests`

## Potentially Related Issues

### High Confidence
- [#48712](https://github.com/zed-industries/zed/issues/48712) — Same crash with Chinese characters (CLOSED, fixed by #48822)
- [#48960](https://github.com/zed-industries/zed/pull/48960) — The fix PR (MERGED)
- [#48822](https://github.com/zed-industries/zed/pull/48822) — Related zeta1 parsing fix (MERGED)

### Medium Confidence
- [#49142](https://github.com/zed-industries/zed/pull/49142) — Another regression test PR for ZED-4VS (OPEN)

## Reviewer Checklist

- [ ] Verify tests exercise the crash scenario from the Sentry report
- [ ] Confirm fix in #48960 is complete and handles all edge cases
- [ ] Consider merging this with #49142 if overlapping

Release Notes:

- N/A
