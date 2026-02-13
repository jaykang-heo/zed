# Crate-Level `.rules` Ownership Plan

This document outlines who should write crate-level `.rules` files for the most complex crates, what each should address, and how to use `REVIEWERS.conl` to identify domain experts.

## Background

The root `.rules` file provides general guidance, but complex crates like `gpui`, `editor`, and `agent` need crate-specific documentation that only domain experts can write. These `.rules` files help AI agents (Factory, Codex, Claude Code, Zed Agent Panel) understand:
- Internal architecture and module layout
- Key patterns and abstractions
- Common gotchas and anti-patterns
- Testing infrastructure specific to the crate

## Using REVIEWERS.conl to Identify Owners

`REVIEWERS.conl` maps domain areas to people with deep expertise. Use this to identify who should write each crate's `.rules`:

| Domain Area | Reviewers | Relevant Crates |
|------------|-----------|-----------------|
| `gpui` | @Anthony-Eid, @cameron1024, @mikayla-maki, @probably-neb | `gpui`, `gpui_macros` |
| `multi_buffer` | @Veykril, @SomeoneToIgnore | `multi_buffer`, `editor` (display_map) |
| `text` | @Veykril | `text`, `rope` |
| `ai` | @benbrandt, @bennetbo, @danilo-leal, @rtfeldman | `agent`, `agent_ui`, `agent_ui_v2`, `language_model` |
| `lsp` | @osiewicz, @smitbarmase, @SomeoneToIgnore, @Veykril | `lsp`, `language` |
| `languages` | @osiewicz, @probably-neb, @smitbarmase, @SomeoneToIgnore, @Veykril | `languages` |
| `crashes` | @Veykril | (cross-cutting expertise) |
| `debugger` | @Anthony-Eid, @kubkon, @osiewicz | `dap`, `debugger_ui` |
| `terminal` | @kubkon, @Veykril | `terminal`, `terminal_view` |
| `git` | @cole-miller, @danilo-leal, @yara-blue, @kubkon, @Anthony-Eid, @cameron1024 | `git`, `git_ui` |
| `vim` | @ConradIrwin, @dinocosta, @probably-neb | `vim` |
| `extension` | @kubkon | `extension`, `extension_host`, `extension_api` |
| `tasks` | @SomeoneToIgnore, @Veykril | `task`, `tasks_ui` |

## Priority Crates for `.rules` Files

### Tier 1 (Highest Priority)

These crates have the highest agent failure rate and complexity:

#### 1. `crates/gpui/.rules`
**Owner:** GPUI maintainers (@Anthony-Eid, @cameron1024, @mikayla-maki)
**Time estimate:** ~1 hour

Should cover:
- Module map: `app.rs`, `window.rs`, `elements/`, `platform/`
- Rendering pipeline: `Render::render()` → element tree → layout → paint
- Entity lifecycle: creation, reading, updating, dropping, weak references
- Key types: `Element`, `IntoElement`, `RenderOnce`, `Styled`, `InteractiveElement`
- Common patterns: `cx.listener()`, `cx.spawn()`, `cx.observe()`, `cx.subscribe()`
- Test infra: `TestAppContext`, `VisualTestContext`, `#[gpui::test]` behavior
- Gotchas: re-entrant entity updates panic, `run_until_parked()` semantics

#### 2. `crates/editor/.rules`
**Owner:** Editor maintainers (@Veykril, @SomeoneToIgnore)
**Time estimate:** ~1 hour

Should cover:
- `Editor` is used for code editing AND all text inputs (important context)
- Display pipeline: `Editor` → `DisplayMap` → `DisplaySnapshot` → render
- `display_map/` breakdown: folds, inlays, block decorations, wrap maps, tab maps
- Selection model: `SelectionsCollection`, cursors, multiple cursors
- `MultiBuffer` wraps `Buffer`(s) — stale offset gotcha
- Test utilities: `EditorTestContext`, `EditorLspTestContext`, marked text format

#### 3. `crates/agent/.rules`
**Owner:** Agent team (@benbrandt, @bennetbo, @rtfeldman)
**Time estimate:** ~1 hour

Should cover:
- Thread store, threads, tools, edit agent, templates
- Tool system: how tools are defined (`tools/` directory), permissions
- Edit agent: `edit_agent/` directory, streaming edit parser
- Relationship between `agent` (logic), `agent_ui` (UI), `agent_ui_v2` (new UI)
- Eval framework in `edit_agent/evals/`

#### 4. `crates/workspace/.rules`
**Owner:** Core team
**Time estimate:** ~45 minutes

Should cover:
- Hierarchy: Workspace → Panes → Items; Docks → Panels
- `Item` trait and `Panel` trait: how to register new types
- Persistence: `persistence/` module
- Modal system: `modal_layer.rs`

#### 5. `crates/project/.rules`
**Owner:** Core team (@SomeoneToIgnore for LSP, others for stores)
**Time estimate:** ~45 minutes

Should cover:
- Store pattern: `BufferStore`, `LspStore`, `GitStore`, `TaskStore` — each wraps local/remote
- File operations through Project
- LSP lifecycle: `lsp_store/`
- Git state: `git_store/`

### Tier 2 (Nice to Have)

| Crate | Suggested Owner | What to Cover |
|-------|-----------------|---------------|
| `language` | @osiewicz, @SomeoneToIgnore | Grammar loading, highlighting, LSP integration |
| `ui` | Design team | Component library, styling patterns |
| `settings` | Core team | Settings derive macro, layering, schema |
| `extension_host` | @kubkon | Extension loading, API surface, sandboxing |

## Template for Crate `.rules` Files

```markdown
# <Crate Name>

## Overview
Brief description of what this crate does and its role in Zed.

## Module Map
- `src/module_a.rs` — description
- `src/module_b/` — description of subdirectory

## Key Types
- `TypeA` — what it represents, when to use it
- `TypeB` — what it represents, when to use it

## Common Patterns
```rust
// Example of the right way to do X
```

## Gotchas
- Don't do X because Y
- Watch out for Z when doing W

## Testing
- How to run tests: `cargo test -p <crate>`
- Test utilities available in this crate
- Example test structure
```

## Process

1. **Week 1:** Reach out to identified owners via Slack or Linear, share this plan
2. **Week 2:** Owners write their crate `.rules` (estimated ~1 hour each)
3. **Ongoing:** When PRs restructure a crate, update that crate's `.rules`

## Success Criteria

- [ ] `crates/gpui/.rules` exists and covers key gotchas
- [ ] `crates/editor/.rules` exists and covers display pipeline + testing
- [ ] `crates/agent/.rules` exists and covers tool system + edit agent
- [ ] `crates/workspace/.rules` exists and covers Item/Panel traits
- [ ] `crates/project/.rules` exists and covers store pattern
- [ ] At least one AI agent session shows improved behavior when working in a crate with `.rules`

## Related

- Background Agent plan (BIZOPS-801)
- Improving `.rules` proposal (`improving-rules-proposal.md`)
- Eric Holk's Sentry crash experiment (proved `.rules` quality is critical for good test generation)
