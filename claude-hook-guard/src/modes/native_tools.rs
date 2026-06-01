use super::{ModeChecker, Violation};
use crate::walker;
use crate::lists;
use tree_sitter::{Node, Tree};

pub struct NativeToolsChecker;

impl ModeChecker for NativeToolsChecker {
    fn check(&self, tree: &Tree, source: &[u8]) -> Option<Violation> {
        let root = tree.root_node();

        // Check all command nodes in the entire tree (handles if/while/for/case etc.)
        let command_nodes = walker::collect_nodes_of_type(root, "command");
        for cmd_node in command_nodes {
            // Skip commands that are pipe targets (not the primary command).
            if is_pipe_target(cmd_node) {
                continue;
            }
            if let Some(v) = check_command_node(cmd_node, source) {
                return Some(v);
            }
        }

        // Also check redirected_statement nodes for echo/printf > file pattern.
        let redirected_nodes = walker::collect_nodes_of_type(root, "redirected_statement");
        for redir_node in redirected_nodes {
            if is_pipe_target(redir_node) {
                continue;
            }
            if let Some(v) = check_echo_redirect(redir_node, source) {
                return Some(v);
            }
        }

        None
    }
}

/// Returns true if `node` is a non-first segment within a `pipeline` ancestor.
fn is_pipe_target(node: Node) -> bool {
    let cmd_start = node.start_byte();
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "pipeline" {
            let mut cursor = parent.walk();
            if let Some(first_segment) = parent.named_children(&mut cursor).next() {
                // If the command starts at or after the end of the first segment,
                // it is a pipe target.
                return cmd_start >= first_segment.end_byte();
            }
            return false;
        }
        current = parent;
    }
    false
}

fn check_command_node(cmd_node: Node, source: &[u8]) -> Option<Violation> {
    let name = walker::extract_command_name(cmd_node, source)?;

    // obsidian commands are unconditionally exempt.
    if name == "obsidian" {
        return None;
    }

    // Handle prefix commands: env, sudo, command — unwrap and check the inner command.
    if matches!(name.as_str(), "env" | "sudo" | "command") {
        let args = walker::extract_command_args(cmd_node, source);
        // Find the actual command name after flags and KEY=VALUE pairs.
        let inner_name = args
            .iter()
            .find(|a| !a.starts_with('-') && !a.contains('='))?;
        if inner_name == "obsidian" {
            return None;
        }
        return lookup_native_tool(inner_name);
    }

    // Handle bash/sh/zsh -c "cmd" — parse and check the inner command string.
    if matches!(name.as_str(), "bash" | "sh" | "zsh") {
        let args = walker::extract_command_args(cmd_node, source);
        return check_shell_c_flag(&args);
    }

    lookup_native_tool(&name)
}

fn lookup_native_tool(name: &str) -> Option<Violation> {
    for (cmd, msg) in lists::NATIVE_TOOL_COMMANDS {
        if name == *cmd {
            return Some(Violation::new(*msg));
        }
    }
    None
}

fn check_shell_c_flag(args: &[String]) -> Option<Violation> {
    let mut iter = args.iter();
    let inner_cmd = loop {
        match iter.next() {
            None => return None,
            Some(a) if a == "-c" => match iter.next() {
                Some(inner) => break inner.clone(),
                None => return None,
            },
            _ => continue,
        }
    };

    let inner = inner_cmd.trim_matches(|c| c == '\'' || c == '"');
    if inner.is_empty() {
        return None;
    }

    let tree = crate::parser::parse_bash(inner)?;
    let command_nodes = walker::collect_nodes_of_type(tree.root_node(), "command");
    for cmd_node in command_nodes {
        if is_pipe_target(cmd_node) {
            continue;
        }
        if let Some(v) = check_command_node(cmd_node, inner.as_bytes()) {
            return Some(v);
        }
    }
    None
}

fn check_echo_redirect(redir_node: Node, source: &[u8]) -> Option<Violation> {
    // Find the command body inside the redirected_statement.
    let mut cursor = redir_node.walk();
    let body = redir_node.named_children(&mut cursor).next()?;
    if body.kind() != "command" {
        return None;
    }
    let name = walker::extract_command_name(body, source)?;
    if name != "echo" && name != "printf" {
        return None;
    }
    // Check if any redirect is to a file (not /dev/null).
    let redirects = walker::collect_nodes_of_type(redir_node, "file_redirect");
    for r in redirects {
        if !walker::is_redirect_to_dev_null(r, source) {
            return Some(Violation::new(
                "BLOCKED: Use the Write tool to create or overwrite files.",
            ));
        }
    }
    None
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_bash;

    fn check(source: &str) -> Option<Violation> {
        let tree = parse_bash(source).unwrap();
        NativeToolsChecker.check(&tree, source.as_bytes())
    }

    // Basic blocked commands
    #[test]
    fn find_blocked() {
        assert!(check("find . -name '*.rs'").is_some());
    }

    #[test]
    fn grep_blocked() {
        assert!(check("grep -r foo src/").is_some());
    }

    #[test]
    fn cat_blocked() {
        assert!(check("cat file.txt").is_some());
    }

    #[test]
    fn ls_blocked() {
        assert!(check("ls -la").is_some());
    }

    #[test]
    fn sed_blocked() {
        assert!(check("sed 's/foo/bar/g' file.txt").is_some());
    }

    // Allowed commands
    #[test]
    fn git_allowed() {
        assert!(check("git status").is_none());
    }

    #[test]
    fn echo_allowed() {
        assert!(check("echo hello").is_none());
    }

    // echo/printf redirect blocked
    #[test]
    fn echo_redirect_blocked() {
        assert!(check("echo 'hello' > file.txt").is_some());
    }

    #[test]
    fn echo_redirect_dev_null_allowed() {
        assert!(check("echo hello > /dev/null").is_none());
    }

    // Pipe targets are allowed (only primary command checked)
    #[test]
    fn grep_as_pipe_target_allowed() {
        assert!(check("git log | grep foo").is_none());
    }

    #[test]
    fn cat_as_pipe_target_allowed() {
        assert!(check("some-cmd | cat").is_none());
    }

    // Commands inside control flow are now checked
    #[test]
    fn find_in_if_blocked() {
        assert!(check("if find . -name '*.rs'; then echo done; fi").is_some());
    }

    #[test]
    fn grep_in_while_blocked() {
        assert!(check("while grep -q foo file.txt; do sleep 1; done").is_some());
    }

    #[test]
    fn cat_in_for_blocked() {
        assert!(check("for f in *.txt; do cat \"$f\"; done").is_some());
    }

    // bash -c bypass detection
    #[test]
    fn bash_c_find_blocked() {
        assert!(check("bash -c 'find . -name *.rs'").is_some());
    }

    #[test]
    fn sh_c_grep_blocked() {
        assert!(check("sh -c 'grep foo bar'").is_some());
    }

    // env bypass detection
    #[test]
    fn env_find_blocked() {
        assert!(check("env find . -name '*.rs'").is_some());
    }

    #[test]
    fn env_var_grep_blocked() {
        assert!(check("env FOO=bar grep foo src/").is_some());
    }

    // obsidian exempt
    #[test]
    fn obsidian_allowed() {
        assert!(check("obsidian daily:append content='foo'").is_none());
    }
}
