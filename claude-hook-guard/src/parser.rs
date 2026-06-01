use tree_sitter::{Language, Parser, Tree};

pub fn parse_bash(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    let language: Language = tree_sitter_bash::LANGUAGE.into();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}
