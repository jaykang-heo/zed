# Terminal Tool SendInput Implementation Plan

## Status: ✅ CORE IMPLEMENTATION COMPLETE, CLEANUP TODO

## Overview

This document describes the implementation of the `SendInput` action for the terminal tool, which allows the AI model to interact with already-running terminal processes.

## Current State

The core functionality is implemented:
- ✅ `RunCmd` - runs a command and returns output (no longer kills on timeout)
- ✅ `SendInput` - sends input to a running terminal
- ✅ Terminals persist in the `AcpThread::terminals` map after `RunCmd` returns
- ⚠️ **TODO**: Terminal cleanup when thread ends or non-terminal tool is called

## Key Files

- **`zed/crates/agent/src/tools/terminal_tool.rs`** - The terminal tool implementation
- **`zed/crates/agent/src/thread.rs`** - Defines `TerminalHandle` trait and `ThreadEnvironment` trait
- **`zed/crates/agent/src/agent.rs`** - Defines `AcpTerminalHandle` and `AcpThreadEnvironment`
- **`zed/crates/acp_thread/src/acp_thread.rs`** - Contains `AcpThread` with the `terminals` HashMap
- **`zed/crates/acp_thread/src/terminal.rs`** - The `acp_thread::Terminal` wrapper
- **`zed/crates/terminal/src/terminal.rs`** - The low-level terminal with `input()` method

## How It Works

### Terminal Lifecycle

1. **Creation**: `RunCmd` creates a terminal via `environment.create_terminal()`
2. **Persistence**: The terminal is stored in `AcpThread::terminals` HashMap by its ID
3. **Interaction**: `SendInput` looks up terminals by ID via `environment.get_terminal()`
4. **Cleanup**: **TODO** - see below

### RunCmd Behavior (Changed)

When `RunCmd` is called with a timeout:
- If the process exits before timeout: returns the exit status and output
- If timeout is reached: **does NOT kill the process** - returns current output with instructions for the model

The model is told it can:
- Use `SendInput` to interact with the process (e.g., "q" to quit less)
- Make a different tool call or respond with text (which should kill the terminal)

### SendInput Behavior

1. Looks up terminal by ID in `AcpThread::terminals`
2. Sends input (with newline appended) via `terminal.inner().input()`
3. Waits briefly for the process to exit
4. Returns current terminal state

## TODO: Terminal Cleanup

Currently, terminals persist in the `AcpThread::terminals` map until the thread itself is dropped. We need to implement cleanup so that terminals are killed when:

### 1. Thread Ends (StopReason::EndTurn or similar)

When the model's turn ends (it produces a final text response that ends the turn), any running terminals should be killed.

**Implementation approach:**
- In `zed/crates/agent/src/thread.rs`, when handling a `Stop` event, iterate through all terminals and kill them
- Need to add a method to `ThreadEnvironment` like `kill_all_terminals()` or iterate through terminal IDs

### 2. Non-Terminal Tool Call

When the model makes a tool call that is NOT:
- `terminal` with `SendInput` action

Then any running terminals should be killed before the new tool runs.

**Implementation approach:**
- In `zed/crates/agent/src/thread.rs`, in `handle_tool_use_event`:
  - Check if the tool being called is NOT a terminal SendInput
  - If so, kill all running terminals first
  - This requires being able to identify terminal tool calls by their action type

**Key question:** How to identify if a tool call is a terminal SendInput without parsing the input? Options:
1. Parse the input JSON to check the action field
2. Add a method to tools to indicate "keeps terminals alive"
3. Track which terminals are "active" and clean them up based on tool name alone

### Code Locations for Cleanup

1. **`zed/crates/agent/src/thread.rs`**:
   - `handle_completion_event` - when `Stop` event is received
   - `handle_tool_use_event` - before running a non-terminal tool

2. **`zed/crates/agent/src/agent.rs`**:
   - Add `kill_all_terminals()` to `ThreadEnvironment` trait
   - Implement it in `AcpThreadEnvironment` to call `release_terminal` for all terminals

3. **`zed/crates/acp_thread/src/acp_thread.rs`**:
   - Add a method to kill/release all terminals: `release_all_terminals()`

### Example Implementation Sketch

```rust
// In thread.rs - ThreadEnvironment trait
pub trait ThreadEnvironment {
    // ... existing methods ...
    
    fn kill_all_terminals(&self, cx: &mut AsyncApp) -> Result<()>;
}

// In agent.rs - AcpThreadEnvironment
impl ThreadEnvironment for AcpThreadEnvironment {
    fn kill_all_terminals(&self, cx: &mut AsyncApp) -> Result<()> {
        self.acp_thread.update(cx, |thread, cx| {
            thread.release_all_terminals(cx);
        })?;
        Ok(())
    }
}

// In acp_thread.rs - AcpThread
impl AcpThread {
    pub fn release_all_terminals(&mut self, cx: &mut Context<Self>) {
        let terminal_ids: Vec<_> = self.terminals.keys().cloned().collect();
        for id in terminal_ids {
            self.release_terminal(id, cx).ok();
        }
    }
}

// In thread.rs - when handling tool use
fn handle_tool_use_event(&mut self, tool_use: LanguageModelToolUse, ...) {
    // Check if this is a terminal tool with SendInput or Wait
    let is_terminal_interaction = tool_use.name.as_ref() == "terminal" 
        && is_send_input_or_wait(&tool_use.input);
    
    if !is_terminal_interaction {
        // Kill all running terminals before running this tool
        self.environment.kill_all_terminals(cx)?;
    }
    
    // ... rest of existing code ...
}

fn is_send_input_or_wait(input: &serde_json::Value) -> bool {
    input.get("action")
        .and_then(|a| a.as_object())
        .map(|obj| obj.contains_key("SendInput") || obj.contains_key("Wait"))
        .unwrap_or(false)
}
```

## Testing

### Existing Tests
- `test_terminal_tool_send_input` - tests SendInput with fake terminal
- `test_terminal_tool_timeout_kills_handle` - tests timeout behavior
- `test_terminal_tool_less_command_times_out` - tests real `less` command

### Tests Needed for Cleanup
- Test that terminals are killed when thread ends
- Test that terminals are killed when non-terminal tool is called
- Test that terminals are NOT killed when SendInput/Wait is called

## Debug Logging

Debug statements are prefixed with `[INTERACTIVE-TERMINAL-DEBUG]` and log:
- When terminal tool is called (action and timeout)
- Terminal creation and ID assignment
- Timeout events (with process still running vs exited)
- Terminal lookups (found vs not found)
- Input being sent to terminal