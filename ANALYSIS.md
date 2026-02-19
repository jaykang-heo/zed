# Crash Analysis: UTF-8 char boundary crash in edit prediction parsing

## Crash Summary
- **Sentry Issue:** [ZED-4VS](https://sentry.io/organizations/zed-dev/issues/7243282041/)
- **Error:** `byte index <int> is not a char boundary; it is inside 'о' (bytes <int>..<int>)`
- **Crash Site:** `edit_prediction::zeta1::parse_edits` in `zeta1.rs` line 334 (old version)
- **Event Count:** 538
- **First Seen:** 2026-02-05T00:47:33Z
- **Last Seen:** 2026-02-19T11:11:16Z
- **Affected Version:** 0.223.3+stable

## Root Cause

The crash occurred in the `parse_edits` function when parsing LLM-generated edit predictions. The function extracts content between `<|editable_region_start|>` and `<|editable_region_end|>` markers, attempting to skip newlines adjacent to these markers.

The vulnerable code (pre-fix) was:
```rust
let content_start = start_markers
    .first()
    .map(|e| e.0 + EDITABLE_REGION_START_MARKER.len() + 1) // +1 to skip \n after marker
    .unwrap_or(0);
let content_end = end_markers
    .first()
    .map(|e| e.0.saturating_sub(1)) // -1 to exclude \n before marker
    .unwrap_or(content.strip_suffix("\n").unwrap_or(&content).len());
```

The bug: The code **unconditionally** added `+1` to skip a newline after the start marker and **unconditionally** subtracted 1 to skip a newline before the end marker, without checking:
1. Whether a newline actually exists at those positions
2. Whether those positions are valid UTF-8 character boundaries

When the LLM returned content with multi-byte UTF-8 characters (like Cyrillic text) immediately adjacent to the markers, the `+1` or `-1` offset would land in the middle of a multi-byte character sequence, causing the subsequent string slice operation to panic.

**Example triggering case:**
```
<|editable_region_start|>Отец с двумя детьми...
```
Here, 'О' (Cyrillic capital O) is a 2-byte UTF-8 character. The old code would blindly add 1 to the position after the start marker, landing in the middle of the 'О' character.

## Fix Status

This issue was **already fixed** in commit `65027dd4aa` ("Handle newlines better in parse_edits") on 2026-02-12. The fix properly checks character boundaries before adjusting positions:

```rust
let content_start = start_markers
    .first()
    .map(|e| e.0 + EDITABLE_REGION_START_MARKER.len())
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
    .unwrap_or(0);
```

The fix was cherry-picked to:
- Stable: `306201f291` ("cherry-pick to stable")
- Preview: `71ead4c58f` ("cherry-pick to preview")

## Why Crashes Continue

The crashes are occurring on users running older versions (0.223.3) that don't have the fix. The fix is in 0.224.x and later. As users update to newer versions, the crash rate should decrease.

## Reproduction

The crash can be reproduced with this test case:

```rust
#[gpui::test]
fn test_parse_edits_multibyte_char_after_start_marker(cx: &mut App) {
    let text = "Отец";  // Cyrillic text
    let buffer = cx.new(|cx| Buffer::local(text, cx));
    let snapshot = buffer.read(cx).snapshot();

    // No newline after start marker, multibyte char immediately follows
    let output = "<|editable_region_start|>Отец\n<|editable_region_end|>";
    let editable_range = 0..text.len();

    // This would panic in old code, should work now
    let edits = parse_edits(output, editable_range, &snapshot).unwrap();
    assert!(edits.is_empty());  // No changes
}
```

Run with: `cargo test -p edit_prediction zeta1::tests::test_parse_edits`

## Verification

The existing tests in `zeta1.rs` verify the fix:
- `test_parse_edits_multibyte_char_before_end_marker` - Tests multi-byte char before end marker
- `test_parse_edits_multibyte_char_after_start_marker` - Tests multi-byte char after start marker
- `test_parse_edits_empty_editable_region` - Tests edge case of empty region

All tests pass with the current code.
