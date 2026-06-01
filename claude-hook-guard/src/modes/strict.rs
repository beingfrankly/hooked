use super::{ModeChecker, Violation};
use crate::walker;
use tree_sitter::Tree;

const BLOCKED_MSG: &str = "BLOCKED: Only simple commands allowed (no pipes, chains, subshells, or redirections). Run a single command with arguments.";

pub struct StrictChecker;

impl ModeChecker for StrictChecker {
    fn check(&self, tree: &Tree, source: &[u8]) -> Option<Violation> {
        let root = tree.root_node();

        // Check for multiple top-level statements (semicolons between commands).
        if walker::count_top_level_statements(root) > 1 {
            return Some(Violation::new(BLOCKED_MSG));
        }

        // Check for forbidden node types.
        let forbidden = [
            ("pipeline", "pipes"),
            ("list", "command chains"),
            ("command_substitution", "command substitution"),
            ("process_substitution", "process substitution"),
            ("subshell", "subshells"),
        ];

        for (kind, _desc) in &forbidden {
            if walker::has_node_type(root, kind) {
                return Some(Violation::new(BLOCKED_MSG));
            }
        }

        // Check heredocs.
        if walker::has_node_type(root, "heredoc_redirect") {
            return Some(Violation::new(BLOCKED_MSG));
        }

        // Check file redirections — allow only /dev/null and fd duplication.
        let redirects = walker::collect_nodes_of_type(root, "file_redirect");
        for redirect in redirects {
            if !walker::is_redirect_to_dev_null(redirect, source) {
                return Some(Violation::new(BLOCKED_MSG));
            }
        }

        // Check background execution (&).
        if walker::detect_background(root, source) {
            return Some(Violation::new(BLOCKED_MSG));
        }

        None // allowed
    }
}
