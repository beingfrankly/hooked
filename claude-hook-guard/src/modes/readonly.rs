use super::{ModeChecker, Violation};
use crate::walker;
use crate::lists;
use tree_sitter::Tree;

pub struct ReadOnlyChecker;

impl ModeChecker for ReadOnlyChecker {
    fn check(&self, tree: &Tree, source: &[u8]) -> Option<Violation> {
        let root = tree.root_node();

        // --- Structural checks (inherited from strict, except pipelines) ---

        // Multiple top-level statements (semicolons)
        if walker::count_top_level_statements(root) > 1 {
            return Some(Violation::new(
                "BLOCKED: Read-only mode — multiple commands (semicolons/newlines) not allowed. Run a single command."
            ));
        }

        // Forbidden node types (NOT including pipeline)
        for kind in ["command_substitution", "process_substitution", "subshell", "list"] {
            if walker::has_node_type(root, kind) {
                return Some(Violation::new(
                    "BLOCKED: Read-only mode — command substitution, subshells, and chains are not allowed."
                ));
            }
        }

        // Heredocs
        if walker::has_node_type(root, "heredoc_redirect") {
            return Some(Violation::new(
                "BLOCKED: Read-only mode — heredocs are not allowed."
            ));
        }

        // File redirections (allow /dev/null)
        for redirect in walker::collect_nodes_of_type(root, "file_redirect") {
            if !walker::is_redirect_to_dev_null(redirect, source) {
                return Some(Violation::new(
                    "BLOCKED: Read-only mode — file redirections are not allowed (except /dev/null)."
                ));
            }
        }

        // Background
        if walker::detect_background(root, source) {
            return Some(Violation::new(
                "BLOCKED: Read-only mode — background execution is not allowed."
            ));
        }

        // --- Pipeline check ---
        for pipeline in walker::collect_nodes_of_type(root, "pipeline") {
            let segments = walker::get_pipeline_segments(pipeline);
            for (i, segment) in segments.iter().enumerate() {
                if i == 0 {
                    continue; // first segment doesn't need to be in allowlist
                }
                // segment might be a command, redirected_statement, etc.
                // Try to extract the command name from it.
                let cmd_name = if segment.kind() == "command" {
                    walker::extract_command_name(*segment, source)
                } else {
                    // For redirected_statement or other wrappers, look for a command child.
                    let cmds = walker::collect_all_commands(*segment, source);
                    cmds.into_iter().next().map(|c| c.name)
                };

                if let Some(name) = cmd_name {
                    if !lists::ALLOWED_PIPE_TARGETS.contains(&name.as_str()) {
                        return Some(Violation::new(format!(
                            "BLOCKED: Read-only mode — pipe target '{}' is not allowed. Allowed: head, tail, grep, sort, wc, jq, ...",
                            name
                        )));
                    }
                }
            }
        }

        // --- Write command denylist ---
        let commands = walker::collect_all_commands(root, source);
        for cmd in &commands {
            // Check simple write commands.
            if lists::WRITE_COMMANDS.contains(&cmd.name.as_str()) {
                return Some(Violation::new(format!(
                    "BLOCKED: Read-only mode — command '{}' modifies the filesystem. Use read-only alternatives or delegate to a worker agent.",
                    cmd.name
                )));
            }

            // Check two-word write subcommands.
            if let Some(first_arg) = cmd.args.first() {
                let first_arg_str = first_arg.as_str();
                for (parent, sub) in lists::WRITE_SUBCOMMANDS {
                    if cmd.name == *parent && first_arg_str == *sub {
                        return Some(Violation::new(format!(
                            "BLOCKED: Read-only mode — command '{} {}' modifies the filesystem. Use read-only alternatives or delegate to a worker agent.",
                            cmd.name, sub
                        )));
                    }
                }
            }

            // Check in-place edit commands (sed -i, awk -i).
            if lists::INPLACE_EDIT_COMMANDS.contains(&cmd.name.as_str()) {
                let has_inplace = cmd.args.iter().any(|a| a == "-i" || a.starts_with("-i"));
                if has_inplace {
                    return Some(Violation::new(format!(
                        "BLOCKED: Read-only mode — command '{} -i' modifies the filesystem. Use read-only alternatives or delegate to a worker agent.",
                        cmd.name
                    )));
                }
            }
        }

        None
    }
}
