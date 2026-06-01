use super::{ModeChecker, Violation};
use crate::walker;
use crate::lists;
use tree_sitter::Tree;

/// Tools that must be delegated to specific subagent types.
/// Maps tool_name → (subagent_type, suggested label, purpose description).
const DELEGATED_TOOLS: &[(&str, &str, &str)] = &[
    ("Glob", "search", "file discovery and content search"),
    ("Grep", "search", "file discovery and content search"),
];

pub struct DelegationChecker {
    /// Empty string means the main (orchestrator) agent.
    pub agent_type: String,
}

impl ModeChecker for DelegationChecker {
    fn check(&self, tree: &Tree, source: &[u8]) -> Option<Violation> {
        let commands = walker::collect_all_commands(tree.root_node(), source);
        for cmd in &commands {
            if let Some(v) = self.check_build_command(cmd) {
                return Some(v);
            }
        }
        None
    }
}

impl DelegationChecker {
    fn check_build_command(&self, cmd: &walker::CommandInfo) -> Option<Violation> {
        // Handle bash/sh/zsh -c "...": parse and check the inner command string.
        if matches!(cmd.name.as_str(), "bash" | "sh" | "zsh") {
            return self.check_shell_exec(cmd);
        }

        // Handle prefix commands: env, sudo, command.
        if matches!(cmd.name.as_str(), "env" | "sudo" | "command") {
            return self.strip_prefix_command(cmd);
        }

        let is_build = self.is_build_command(cmd);
        if !is_build {
            return None;
        }

        let full_cmd = if cmd.args.is_empty() {
            cmd.name.clone()
        } else {
            format!("{} {}", cmd.name, cmd.args.join(" "))
        };

        if self.agent_type.is_empty() {
            Some(Violation::new(format!(
                "BLOCKED: Delegate to the build-runner subagent. Example: Agent(subagent_type='build-runner', prompt='Run: {} in /path', model='haiku')",
                full_cmd
            )))
        } else if self.agent_type == "build-runner" {
            // The build-runner agent is allowed to run build/test commands.
            None
        } else {
            Some(Violation::new(format!(
                "BLOCKED: Only the build-runner agent can run build/test commands. You are '{}'.",
                self.agent_type
            )))
        }
    }

    fn is_build_command(&self, cmd: &walker::CommandInfo) -> bool {
        // Check commands that are blocked regardless of subcommand.
        if lists::BUILD_COMMANDS_ANY.contains(&cmd.name.as_str()) {
            return true;
        }

        // Check commands that are blocked only for specific subcommands.
        for (parent, subcommands) in lists::BUILD_COMMANDS_WITH_SUBCOMMANDS {
            if cmd.name == *parent {
                if let Some(sub) = cmd.args.first() {
                    if subcommands.contains(&sub.as_str()) {
                        return true;
                    }
                }
            }
        }

        false
    }

    // bash/sh/zsh -c "...": parse and recursively check the inner command string.
    fn check_shell_exec(&self, cmd: &walker::CommandInfo) -> Option<Violation> {
        let mut iter = cmd.args.iter();
        let inner_cmd = loop {
            match iter.next() {
                None => return None,
                Some(a) if a == "-c" => {
                    match iter.next() {
                        Some(inner) => break inner.clone(),
                        None => return None,
                    }
                }
                _ => continue,
            }
        };

        let inner = inner_cmd.trim_matches(|c| c == '\'' || c == '"');
        if inner.is_empty() {
            return None;
        }

        let tree = crate::parser::parse_bash(inner)?;
        let inner_commands = walker::collect_all_commands(tree.root_node(), inner.as_bytes());
        for inner_cmd in &inner_commands {
            if let Some(v) = self.check_build_command(inner_cmd) {
                return Some(v);
            }
        }
        None
    }

    // env/sudo/command: strip prefix and any leading flags/env-vars, then re-check.
    fn strip_prefix_command(&self, cmd: &walker::CommandInfo) -> Option<Violation> {
        let mut iter = cmd.args.iter();
        let inner_name = loop {
            match iter.next() {
                None => return None,
                Some(a) if a.starts_with('-') => continue,
                Some(a) if a.contains('=') => continue,
                Some(a) => break a.clone(),
            }
        };
        let inner_args: Vec<String> = iter.cloned().collect();
        let inner = walker::CommandInfo {
            name: inner_name,
            args: inner_args,
        };
        self.check_build_command(&inner)
    }

    /// Check if a non-Bash tool should be delegated based on tool_name alone.
    /// Returns a violation if the main agent is calling a delegated tool.
    pub fn check_tool_name(&self, tool_name: &str) -> Option<Violation> {
        for &(tool, subagent, purpose) in DELEGATED_TOOLS {
            if tool_name == tool {
                if self.agent_type.is_empty() {
                    return Some(Violation::new(format!(
                        "BLOCKED: {} cannot be used directly from the main agent. Spawn an {} subagent (subagent_type='{}') for all {}.",
                        tool_name, subagent, subagent, purpose
                    )));
                }
                // Non-main agents are allowed (the search subagent itself needs Glob/Grep)
                return None;
            }
        }
        None
    }
}
