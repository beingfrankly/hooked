pub mod strict;
pub mod readonly;
pub mod safety;
pub mod native_tools;
pub mod delegation;

use tree_sitter::Tree;

/// Represents a mode-specific violation (blocked command)
#[derive(Debug)]
pub struct Violation {
    pub reason: String,
}

impl Violation {
    pub fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

pub trait ModeChecker {
    fn check(&self, tree: &Tree, source: &[u8]) -> Option<Violation>;
}
