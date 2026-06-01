use crate::config::StructurePolicy;
use crate::walker;
use tree_sitter::Tree;

pub fn check_structure(policy: &StructurePolicy, tree: &Tree, source: &[u8]) -> Option<String> {
    let root = tree.root_node();

    if policy.single_command && walker::count_top_level_statements(root) > 1 {
        return Some("BLOCKED: profile only allows a single command.".to_string());
    }

    if policy.single_command
        && (((!policy.allow_pipeline) && walker::has_node_type(root, "pipeline"))
            || walker::has_node_type(root, "list")
            || walker::has_node_type(root, "process_substitution")
            || walker::has_node_type(root, "command_substitution")
            || walker::has_node_type(root, "subshell")
            || walker::detect_background(root, source))
    {
        return Some("BLOCKED: profile does not allow shell composition.".to_string());
    }

    if policy.no_redirection
        && (walker::has_node_type(root, "file_redirect")
            || walker::has_node_type(root, "heredoc_redirect"))
    {
        return Some("BLOCKED: profile does not allow shell redirection.".to_string());
    }

    None
}
