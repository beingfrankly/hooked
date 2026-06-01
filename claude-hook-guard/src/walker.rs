use tree_sitter::Node;

#[derive(Debug, Clone)]
pub struct CommandInfo {
    pub name: String,
    pub args: Vec<String>,
}

/// Recursively check if any descendant of `node` has the given node kind.
pub fn has_node_type(node: Node, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if has_node_type(child, kind) {
            return true;
        }
    }
    false
}

/// Recursively collect all descendant nodes (including `node` itself) of the given kind.
pub fn collect_nodes_of_type<'a>(node: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    let mut results = Vec::new();
    if node.kind() == kind {
        results.push(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        results.extend(collect_nodes_of_type(child, kind));
    }
    results
}

/// For a `command` node, return the text of the command name word.
/// Returns None if the command name is a variable expansion.
pub fn extract_command_name(cmd_node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = cmd_node.walk();
    for child in cmd_node.children(&mut cursor) {
        if child.kind() == "command_name" {
            // Look inside command_name for the actual word or expansion.
            let mut inner_cursor = child.walk();
            for inner in child.children(&mut inner_cursor) {
                match inner.kind() {
                    // Variable expansions — skip, return None.
                    "simple_expansion" | "expansion" => return None,
                    _ => {
                        let text = inner.utf8_text(source).unwrap_or("").to_string();
                        if !text.is_empty() {
                            return Some(text);
                        }
                    }
                }
            }
            // command_name had no children with text — use its own text.
            let text = child.utf8_text(source).unwrap_or("").to_string();
            if text.is_empty() {
                return None;
            }
            return Some(text);
        }
    }
    None
}

/// Collect all argument children of a `command` node, excluding the name child
/// and any redirect nodes.
pub fn extract_command_args(cmd_node: Node, source: &[u8]) -> Vec<String> {
    let mut args = Vec::new();
    let mut name_seen = false;
    let mut cursor = cmd_node.walk();
    for child in cmd_node.children(&mut cursor) {
        match child.kind() {
            "file_redirect" | "heredoc_redirect" => continue,
            "command_name" => {
                name_seen = true;
            }
            _ => {
                if name_seen {
                    let text = child.utf8_text(source).unwrap_or("").to_string();
                    if !text.is_empty() {
                        args.push(text);
                    }
                }
            }
        }
    }
    args
}

/// Recursively find all `command` nodes in the tree and return their info.
/// Commands whose name cannot be extracted (e.g. variable expansions) are skipped.
pub fn collect_all_commands(node: Node, source: &[u8]) -> Vec<CommandInfo> {
    let mut results = Vec::new();
    if node.kind() == "command" {
        if let Some(name) = extract_command_name(node, source) {
            let args = extract_command_args(node, source);
            results.push(CommandInfo { name, args });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        results.extend(collect_all_commands(child, source));
    }
    results
}

/// For a `pipeline` node, return the named children (skipping anonymous `|` nodes).
pub fn get_pipeline_segments<'a>(pipeline: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = pipeline.walk();
    pipeline.named_children(&mut cursor).collect()
}

/// For a `file_redirect` node, check if the redirect target is `/dev/null` or a
/// safe fd duplication like `&1` / `&2`. Returns true if the redirect is safe.
pub fn is_redirect_to_dev_null(redirect: Node, source: &[u8]) -> bool {
    // fd_redirect (2>&1) nodes are entirely safe.
    if redirect.kind() == "file_redirect" {
        // The redirect target is the last named child of the file_redirect node.
        let mut cursor = redirect.walk();
        if let Some(target) = redirect.named_children(&mut cursor).last() {
            let text = target.utf8_text(source).unwrap_or("");
            // "/dev/null" or fd duplication like "&1", "&2".
            if text == "/dev/null" || text.starts_with('&') {
                return true;
            }
        }
    }
    false
}

/// Count the named top-level statements in the root `program` node,
/// excluding `comment` nodes.
pub fn count_top_level_statements(root: Node) -> usize {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .count()
}

/// Check whether any statement in the tree is backgrounded with `&`.
/// Walks children of the `program` node; for each statement checks whether
/// any anonymous child has text `&`. Also checks the program node itself.
pub fn detect_background(root: Node, source: &[u8]) -> bool {
    fn statement_is_backgrounded(node: Node, source: &[u8]) -> bool {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !child.is_named() {
                let text = child.utf8_text(source).unwrap_or("");
                if text == "&" {
                    return true;
                }
            }
        }
        false
    }

    // Check the program node itself.
    if statement_is_backgrounded(root, source) {
        return true;
    }

    // Check all top-level children.
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if statement_is_backgrounded(child, source) {
            return true;
        }
    }

    false
}
