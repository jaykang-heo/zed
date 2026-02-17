# Potentially Related GitHub Issues

## High Confidence
- None found

## Medium Confidence
- None found

## Low Confidence
- None found

## Notes

GitHub issue searches for the following terms returned no matching issues:
- "RefCell already borrowed"
- "borrow_mut panic"
- "display_layer crash"
- "window tabbing crash"
- "addTabbedWindow"
- "thermal state crash"
- "open_window panic"
- "AppCell borrow"

This crash appears to be a new issue introduced by PR #45638 (thermal state detection) that manifests specifically when:
1. macOS automatic window tabbing is enabled
2. A new window is opened while another compatible window exists
3. The `addTabbedWindow:ordered:` call triggers a synchronous `display_layer` callback

The combination of conditions required to trigger this crash may explain why no user-reported issues match.

## Reviewer Checklist
- [ ] Confirm High confidence issues should be referenced in PR body
- [ ] Confirm any issue should receive closing keywords (`Fixes #...`)
- [ ] Reject false positives before merge
