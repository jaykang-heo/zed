# Potentially Related GitHub Issues

## High Confidence

- [#48712](https://github.com/zed-industries/zed/issues/48712) — zed crashed and auto exited when typing
  - Why: Exact same crash - panic at `zeta1.rs:328` with "byte index is not a char boundary" for Chinese character '讷'
  - Evidence: Stack trace shows `edit_prediction::zeta1::parse_edits` crash, same root cause as ZED-4VS

- [#48960](https://github.com/zed-industries/zed/pull/48960) — Handle newlines better in parse_edits (MERGED)
  - Why: This is the fix PR for this crash
  - Evidence: PR directly addresses the byte boundary issue by adding `is_char_boundary()` checks

- [#48822](https://github.com/zed-industries/zed/pull/48822) — Fix panic in zeta1 prompt parsing (MERGED)
  - Why: Related fix in the same file addressing similar parsing issues
  - Evidence: Closes #48712, addresses panic in zeta1 module

## Medium Confidence

- [#49142](https://github.com/zed-industries/zed/pull/49142) — Add regression tests for ZED-4VS UTF-8 char boundary crash (OPEN)
  - Why: Regression tests specifically for this crash (ZED-4VS)
  - Evidence: PR explicitly mentions ZED-4VS and adds tests for multi-byte UTF-8 near markers

- [#49147](https://github.com/zed-industries/zed/pull/49147) — edit_prediction: Fix crash when anchor buffer_id doesn't match snapshot (OPEN)
  - Why: Another edit_prediction crash fix, may have overlapping scenarios
  - Evidence: Same module but different crash mechanism (anchor mismatch vs byte boundary)

## Low Confidence

- None found

## Reviewer Checklist

- [x] Confirm High confidence issues should be referenced in PR body
- [x] Confirm the main fix (#48960) is already merged
- [ ] Confirm PR #49142 regression tests should be reviewed/merged
- [ ] Reject false positives before merge
