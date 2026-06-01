use crate::agent_spawn::validate_agent_spawn;
use crate::command_match::{is_banned_wrapper, matches_prefix, normalize_command};
use crate::config::{StructurePolicy, ValidatedConfig};
use crate::input::HookInput;
use crate::parser;
use crate::safety::evaluate_safety;
use crate::structure::check_structure;
use crate::walker;

const GLOBAL_BASH_COMMAND_PREFIXES: &[&[&str]] = &[
    &["bd", "--help"],
    &["bd", "-h"],
    &["bd", "help"],
    &["bd", "prime"],
    &["bd", "ready"],
    &["bd", "show"],
    &["bd", "list"],
    &["bd", "status"],
    &["bd", "stats"],
    &["bd", "search"],
    &["bd", "blocked"],
    &["bd", "graph"],
    &["bd", "count"],
    &["bd", "children"],
    &["bd", "history"],
    &["bd", "types"],
    &["bd", "statuses"],
    &["bd", "context"],
    &["bd", "where"],
    &["bd", "info"],
    &["bd", "recall"],
    &["bd", "state"],
    &["bd", "quickstart"],
    &["bd", "human"],
    &["bd", "version"],
    &["bd", "preflight"],
    &["bd", "lint"],
    &["bd", "stale"],
    &["bd", "defer"],
    &["bd", "diff"],
    &["bd", "memories"],
    &["bd", "dep", "list"],
    &["bd", "dep", "tree"],
    &["bd", "dep", "cycles"],
];
const GLOBAL_BASH_STRUCTURE: StructurePolicy = StructurePolicy {
    single_command: true,
    no_redirection: true,
    allow_pipeline: false,
};

pub struct Engine<'a> {
    pub config: &'a ValidatedConfig,
}

impl<'a> Engine<'a> {
    pub fn evaluate(&self, input: &HookInput) -> Option<String> {
        let raw_agent_type = input.agent_type.as_deref();
        let profile_id = self
            .config
            .resolve_profile_id(raw_agent_type)
            .or(Some("_default"))
            .unwrap();

        if raw_agent_type.is_some() && profile_id == "_default" {
            return Some(format!(
                "BLOCKED: unregistered agent type '{}'.",
                raw_agent_type.unwrap()
            ));
        }

        let profile = self.config.rules.profiles.get(profile_id)?;
        let tool_name = input.tool_name.as_deref()?;
        let global_bash_command_allowed = if tool_name == "Bash" {
            input
                .tool_input
                .as_ref()
                .and_then(|ti| ti.command.as_deref())
                .is_some_and(is_global_bash_command_allowed)
        } else {
            false
        };

        if tool_name.starts_with("mcp__") {
            if !mcp_tool_allowed(profile, tool_name) {
                return Some(format!(
                    "BLOCKED: MCP tool {} not allowed for profile '{}'.",
                    tool_name, profile_id
                ));
            }
        } else if !tool_allowed(profile, tool_name) && !global_bash_command_allowed {
            if profile_id == "main" && matches!(tool_name, "Glob" | "Grep") {
                return Some(format!(
                    "BLOCKED: Delegate {} to the search profile.",
                    tool_name
                ));
            }
            return Some(format!(
                "BLOCKED: Tool {} not allowed for profile '{}'.",
                tool_name, profile_id
            ));
        }

        if is_agent_spawn_tool(tool_name) {
            return validate_agent_spawn(self.config, profile_id, input);
        }

        if tool_name != "Bash" {
            return None;
        }

        let command = input
            .tool_input
            .as_ref()
            .and_then(|ti| ti.command.as_deref())
            .filter(|s| !s.trim().is_empty())?;

        let command = strip_setup_preamble(command);

        let tree = parser::parse_bash(command)
            .ok_or_else(|| "BLOCKED: could not parse bash command.".to_string())
            .ok()?;

        let source = command.as_bytes();
        if let Some(structure) = &profile.commands.structure {
            if let Some(reason) = check_structure(structure, &tree, source) {
                return Some(reason);
            }
        }

        let command_nodes = walker::collect_nodes_of_type(tree.root_node(), "command");
        if command_nodes.is_empty() {
            return Some("BLOCKED: no executable command found.".to_string());
        }

        for node in command_nodes {
            let Some(name) = walker::extract_command_name(node, source) else {
                return Some("BLOCKED: cannot verify dynamically-generated commands.".to_string());
            };
            let args = walker::extract_command_args(node, source);
            let info = walker::CommandInfo { name, args };
            let normalized = normalize_command(&info);

            if is_banned_wrapper(&normalized) {
                return Some("BLOCKED: shell wrappers are not allowed.".to_string());
            }

            let allowed = profile
                .commands
                .categories
                .iter()
                .filter_map(|category_name| self.config.rules.command_categories.get(category_name))
                .flat_map(|category| category.allow.iter())
                .any(|prefix| matches_prefix(&normalized, prefix))
                || (global_bash_command_allowed
                    && command_matches_global_bash_allowlist(&normalized));

            if !allowed {
                let mut reason = format!(
                    "BLOCKED: Command '{}' not allowed for profile '{}'.",
                    normalized.join(" "),
                    profile_id
                );
                if let Some(suggestion) = blocked_command_suggestion(&normalized) {
                    reason.push(' ');
                    reason.push_str(suggestion);
                }
                return Some(reason);
            }

            if let Some(reason) =
                evaluate_safety(self.config, profile_id, &normalized, &normalized[1..])
            {
                return Some(reason);
            }
        }

        None
    }
}

fn tool_allowed(profile: &crate::config::Profile, tool_name: &str) -> bool {
    if profile.tools.iter().any(|tool| tool == tool_name) {
        return true;
    }

    match tool_name {
        "Task" => profile.tools.iter().any(|tool| tool == "Agent"),
        "TodoRead" => profile
            .tools
            .iter()
            .any(|tool| matches!(tool.as_str(), "TaskGet" | "TaskList" | "TaskOutput")),
        "TodoWrite" => profile
            .tools
            .iter()
            .any(|tool| matches!(tool.as_str(), "TaskCreate" | "TaskUpdate")),
        _ => false,
    }
}

fn mcp_tool_allowed(profile: &crate::config::Profile, tool_name: &str) -> bool {
    profile.mcp_tools.iter().any(|allowed| {
        allowed == tool_name
            || allowed
                .strip_suffix('*')
                .is_some_and(|prefix| tool_name.starts_with(prefix))
    })
}

fn is_agent_spawn_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Agent" | "Task")
}

fn is_global_bash_command_allowed(command: &str) -> bool {
    let command = strip_setup_preamble(command);
    let Some(tree) = parser::parse_bash(command) else {
        return false;
    };
    let source = command.as_bytes();

    if check_structure(&GLOBAL_BASH_STRUCTURE, &tree, source).is_some() {
        return false;
    }

    let command_nodes = walker::collect_nodes_of_type(tree.root_node(), "command");
    !command_nodes.is_empty()
        && command_nodes.iter().all(|node| {
            let Some(name) = walker::extract_command_name(*node, source) else {
                return false;
            };
            let args = walker::extract_command_args(*node, source);
            let info = walker::CommandInfo { name, args };
            let normalized = normalize_command(&info);
            command_matches_global_bash_allowlist(&normalized)
        })
}

fn command_matches_global_bash_allowlist(command: &[String]) -> bool {
    GLOBAL_BASH_COMMAND_PREFIXES
        .iter()
        .any(|prefix| matches_str_prefix(command, prefix))
}

fn blocked_command_suggestion(command: &[String]) -> Option<&'static str> {
    let name = command.first()?.as_str();
    match name {
        "grep" | "egrep" | "fgrep" | "rg" => Some("Use the native Grep tool instead."),
        "find" | "fd" => Some("Use the native Glob tool instead."),
        "cat" | "head" | "tail" | "sed" | "awk" => {
            Some("Use the native Read tool for file inspection.")
        }
        "ls" | "stat" | "file" | "readlink" | "greadlink" | "realpath" => Some(
            "Use Read, Glob, and task context for path inspection; for symlinks, resolve the real target with allowed native tools or report the blocker.",
        ),
        "python" | "python3" | "node" | "ruby" | "perl" => Some(
            "Do not use scripting languages as shell substitutes; use native tools or report the blocked command.",
        ),
        "nvim" | "vim" | "vi" | "emacs" => Some(
            "Use Edit/Write for file changes; if runtime/editor verification is needed, report the blocked command so the orchestrator can route or update policy.",
        ),
        "npm" | "pnpm" | "mvn" | "mvnw" | "gradle" | "cargo" | "go" | "jest" | "vitest" => {
            Some("Ask the orchestrator to delegate build/test verification to build-runner.")
        }
        _ => None,
    }
}

fn matches_str_prefix(command: &[String], prefix: &[&str]) -> bool {
    command.len() >= prefix.len()
        && command
            .iter()
            .zip(prefix.iter())
            .all(|(actual, expected)| actual == expected)
}

fn strip_setup_preamble(command: &str) -> &str {
    let trimmed = command.trim();
    for prefix in ["cd ", "source ", "export "] {
        if trimmed.starts_with(prefix) {
            if let Some((_, rest)) = trimmed.split_once("&&") {
                return rest.trim();
            }
        }
    }
    trimmed
}
