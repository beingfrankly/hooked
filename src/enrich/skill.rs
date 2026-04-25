//! Skill-name and skill-type detection from tool-input text.
//!
//! Mirrors Python `_detect_skill`, `_SKILL_RE`, and `_SKILL_TYPE_MAP` from
//! `~/.claude/telemetry/ingest.py` (lines 210-243).
//!
//! ## How Python's logic works
//!
//! 1. The regex `_SKILL_RE` scans the text for a `.claude/skills/` path
//!    fragment, capturing everything after that prefix up to the first
//!    whitespace or quote character.
//! 2. Any trailing quote or whitespace noise is stripped by splitting on
//!    `["\'\s]` and taking element `[0]`.
//! 3. The *stem* of the captured path (filename without extension) becomes
//!    `skill_name`.
//! 4. The path is compared against the three prefix keys in `_SKILL_TYPE_MAP`
//!    to determine `skill_type`.
//!
//! ## Mixed-quote handling
//!
//! The regex character class `[^\s"\']+` stops at the first whitespace, `"`,
//! or `'` character, which means quoted skill paths such as
//! `".claude/skills/agents/foo-skill"` or `'.claude/skills/system/bar'` are
//! handled correctly: the quote that *opened* the path is not part of the
//! matched fragment, and the post-match split on `["\'\s]` strips any closing
//! quote that the regex did capture before the delimiter.  Both single-quoted
//! and double-quoted variants therefore yield the same `skill_name`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

// ---------------------------------------------------------------------------
// Regex — ported from Python `_SKILL_RE`
// ---------------------------------------------------------------------------

/// Matches a `.claude/skills/<path>` fragment inside arbitrary text.
///
/// Python source:
/// ```python
/// _SKILL_RE = re.compile(
///     r'\.claude/skills/(?P<path>[^\s"\']+)',
///     re.IGNORECASE,
/// )
/// ```
///
/// The `(?i)` inline flag is the Rust `regex` crate equivalent of
/// Python's `re.IGNORECASE`.  Named capture groups use the same
/// `(?P<name>...)` syntax in both.
static SKILL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\.claude/skills/(?P<path>[^\s"']+)"#).expect("SKILL_RE is a valid regex")
});

// ---------------------------------------------------------------------------
// Skill-type map — ported from Python `_SKILL_TYPE_MAP`
// ---------------------------------------------------------------------------

/// Maps path-prefix substrings to skill-type labels.
///
/// Python source:
/// ```python
/// _SKILL_TYPE_MAP = {
///     "agents/": "agent_definition",
///     "project/": "project_skill",
///     "system/": "system_skill",
/// }
/// ```
///
/// Iteration order during lookup is unspecified (HashMap); this matches Python
/// dict behaviour in CPython ≥ 3.7 where insertion order is preserved for
/// *iteration* but the lookup in `_detect_skill` uses a `for prefix, stype in
/// _SKILL_TYPE_MAP.items(): if prefix in path: break` pattern, which is
/// effectively "first matching prefix wins".  Because no two prefixes in this
/// map can both appear in the same path segment the order does not matter in
/// practice.
pub static SKILL_TYPE_MAP: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("agents/", "agent_definition"),
        ("project/", "project_skill"),
        ("system/", "system_skill"),
    ])
});

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The result of a successful skill detection.
///
/// Mirrors the `(skill_name, skill_type)` tuple returned by Python
/// `_detect_skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedSkill {
    /// The stem (filename without extension) of the matched skill path.
    /// Example: `"foo-skill"` for `.claude/skills/agents/foo-skill.md`.
    pub name: String,

    /// The skill type derived from the path prefix, or `None` if the prefix
    /// did not match any entry in [`SKILL_TYPE_MAP`].
    pub skill_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract skill name and type from a text string, if a `.claude/skills/`
/// path is present.
///
/// Mirrors Python `_detect_skill(tool_input_raw)` in `ingest.py`:
///
/// * Returns `None` when no match is found (Python returns `(None, None)`).
/// * Returns `Some(DetectedSkill)` on a match, with `skill_type` set to
///   `None` when the path prefix is not in [`SKILL_TYPE_MAP`].
///
/// The caller is responsible for serialising non-string inputs to JSON before
/// calling this function (Python does `json.dumps(tool_input_raw)` for
/// non-string inputs; the Rust equivalent is [`serde_json::to_string`]).
pub fn detect_skill(text: &str) -> Option<DetectedSkill> {
    let m = SKILL_RE.captures(text)?;
    let raw_path = m.name("path")?.as_str();

    // Mirror Python: `re.split(r'["\'\s]', path)[0]`
    // Split on the first `"`, `'`, or whitespace character and take
    // everything before it.
    let path = raw_path
        .split(|c: char| c == '"' || c == '\'' || c.is_ascii_whitespace())
        .next()
        .unwrap_or(raw_path);

    // Mirror Python: `Path(path).stem`
    let name = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_owned();

    // Mirror Python: iterate _SKILL_TYPE_MAP, take first matching prefix.
    let skill_type = SKILL_TYPE_MAP.iter().find_map(|(prefix, stype)| {
        if path.contains(*prefix) {
            Some((*stype).to_owned())
        } else {
            None
        }
    });

    Some(DetectedSkill { name, skill_type })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// The first skill name in SKILL_TYPE_MAP that maps to "agent_definition".
    fn agent_skill_path() -> &'static str {
        "agents/foo-skill"
    }

    fn make_agent_input(quote: &str) -> String {
        format!("read_file {quote}.claude/skills/agents/foo-skill.md{quote}",)
    }

    // -----------------------------------------------------------------------
    // known_skill_with_double_quotes
    // -----------------------------------------------------------------------

    #[test]
    fn known_skill_with_double_quotes() {
        let input = make_agent_input("\"");
        let result = detect_skill(&input);
        assert!(result.is_some(), "expected Some, got None for: {input}");
        let ds = result.unwrap();
        assert_eq!(ds.name, "foo-skill");
        assert_eq!(ds.skill_type.as_deref(), Some("agent_definition"));
    }

    // -----------------------------------------------------------------------
    // known_skill_with_single_quotes
    // -----------------------------------------------------------------------

    #[test]
    fn known_skill_with_single_quotes() {
        let input = make_agent_input("'");
        let result = detect_skill(&input);
        assert!(result.is_some(), "expected Some, got None for: {input}");
        let ds = result.unwrap();
        assert_eq!(ds.name, "foo-skill");
        assert_eq!(ds.skill_type.as_deref(), Some("agent_definition"));
    }

    // -----------------------------------------------------------------------
    // known_skill_no_quotes
    // -----------------------------------------------------------------------

    #[test]
    fn known_skill_no_quotes() {
        let input = make_agent_input("");
        let result = detect_skill(&input);
        assert!(result.is_some(), "expected Some, got None for: {input}");
        let ds = result.unwrap();
        assert_eq!(ds.name, "foo-skill");
        assert_eq!(ds.skill_type.as_deref(), Some("agent_definition"));
    }

    // -----------------------------------------------------------------------
    // project_skill_type
    // -----------------------------------------------------------------------

    #[test]
    fn project_skill_type() {
        let input = "loading .claude/skills/project/my-project-skill.md";
        let ds = detect_skill(input).expect("should detect");
        assert_eq!(ds.name, "my-project-skill");
        assert_eq!(ds.skill_type.as_deref(), Some("project_skill"));
    }

    // -----------------------------------------------------------------------
    // system_skill_type
    // -----------------------------------------------------------------------

    #[test]
    fn system_skill_type() {
        let input = "loading .claude/skills/system/base-instructions";
        let ds = detect_skill(input).expect("should detect");
        assert_eq!(ds.name, "base-instructions");
        assert_eq!(ds.skill_type.as_deref(), Some("system_skill"));
    }

    // -----------------------------------------------------------------------
    // unknown_skill — path not in SKILL_TYPE_MAP prefix set
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_skill() {
        let input = ".claude/skills/other/nosuchskill";
        let ds = detect_skill(input).expect("should detect name even without type");
        assert_eq!(ds.name, "nosuchskill");
        assert!(
            ds.skill_type.is_none(),
            "expected no skill_type for unrecognised prefix, got {:?}",
            ds.skill_type
        );
    }

    // -----------------------------------------------------------------------
    // no_skill_in_text
    // -----------------------------------------------------------------------

    #[test]
    fn no_skill_in_text() {
        let input = "just some plain text without any skill reference";
        assert!(detect_skill(input).is_none());
    }

    // -----------------------------------------------------------------------
    // empty_input
    // -----------------------------------------------------------------------

    #[test]
    fn empty_input() {
        assert!(detect_skill("").is_none());
    }

    // -----------------------------------------------------------------------
    // mixed_content — skill embedded in prose
    // -----------------------------------------------------------------------

    #[test]
    fn mixed_content() {
        let input = concat!(
            "The agent will use the skill defined at ",
            ".claude/skills/agents/summariser.md to process the document.",
        );
        let ds = detect_skill(input).expect("should detect skill in prose");
        assert_eq!(ds.name, "summariser");
        assert_eq!(ds.skill_type.as_deref(), Some("agent_definition"));
    }

    // -----------------------------------------------------------------------
    // case_insensitive — Python uses re.IGNORECASE
    // -----------------------------------------------------------------------

    #[test]
    fn case_insensitive() {
        let input = ".CLAUDE/SKILLS/agents/uppercase-skill.md";
        let ds = detect_skill(input).expect("case-insensitive match should work");
        assert_eq!(ds.name, "uppercase-skill");
        assert_eq!(ds.skill_type.as_deref(), Some("agent_definition"));
    }

    // -----------------------------------------------------------------------
    // path_with_extension_stripped — stem extraction
    // -----------------------------------------------------------------------

    #[test]
    fn path_with_extension_stripped() {
        // Python uses Path(path).stem which strips the last extension.
        let input = ".claude/skills/project/my-skill.md";
        let ds = detect_skill(input).expect("should detect");
        assert_eq!(
            ds.name, "my-skill",
            "extension should be stripped from name"
        );
    }

    // -----------------------------------------------------------------------
    // path_without_extension
    // -----------------------------------------------------------------------

    #[test]
    fn path_without_extension() {
        let input = ".claude/skills/project/my-skill";
        let ds = detect_skill(input).expect("should detect");
        assert_eq!(ds.name, "my-skill");
    }

    // -----------------------------------------------------------------------
    // type_map_coverage — every SKILL_TYPE_MAP entry resolves correctly
    // -----------------------------------------------------------------------

    #[test]
    fn type_map_coverage() {
        let cases: &[(&str, &str, &str)] = &[
            ("agents/", "agent_definition", "agents/my-skill"),
            ("project/", "project_skill", "project/my-skill"),
            ("system/", "system_skill", "system/my-skill"),
        ];

        for (prefix, expected_type, path_suffix) in cases {
            let input = format!(".claude/skills/{path_suffix}");
            let ds = detect_skill(&input).unwrap_or_else(|| panic!("no match for prefix={prefix}"));
            let actual_type = ds
                .skill_type
                .as_deref()
                .unwrap_or_else(|| panic!("no skill_type for prefix={prefix}"));
            assert_eq!(
                actual_type, *expected_type,
                "prefix={prefix}: expected type={expected_type}, got={actual_type}"
            );
            // Verify the entry exists in the static map too.
            assert_eq!(
                SKILL_TYPE_MAP.get(prefix).copied(),
                Some(*expected_type),
                "SKILL_TYPE_MAP missing or wrong for prefix={prefix}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // skill_type_map_matches_python — parity test against ingest.py at runtime
    // -----------------------------------------------------------------------

    /// Parse the `_SKILL_TYPE_MAP = { ... }` block from Python `ingest.py` and
    /// assert it is identical to [`SKILL_TYPE_MAP`].
    ///
    /// If `ingest.py` is not present (e.g. CI without the dev env), the test
    /// is silently skipped — mirroring the DDL parity test in `schema.rs`.
    #[test]
    fn skill_type_map_matches_python() {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .expect("HOME must be set");
        let py_path = home.join(".claude/telemetry/ingest.py");

        if !py_path.exists() {
            eprintln!("skipping: {} not present", py_path.display());
            return;
        }

        let src = std::fs::read_to_string(&py_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", py_path.display()));

        // Locate `_SKILL_TYPE_MAP = {` … `}`
        let start_marker = "_SKILL_TYPE_MAP = {";
        let start = src
            .find(start_marker)
            .expect("ingest.py has no `_SKILL_TYPE_MAP = {`");
        let block_start = start + start_marker.len();
        let block_end = src[block_start..]
            .find('}')
            .expect("_SKILL_TYPE_MAP has no closing `}`")
            + block_start;

        let block = &src[block_start..block_end];

        // Parse lines of the form: `    "key": "value",`
        // Both key and value are delimited by double quotes.
        let py_map: HashMap<String, String> = block
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                // Expect:  "key": "value",
                let key_start = line.find('"')? + 1;
                let key_end = line[key_start..].find('"')? + key_start;
                let key = &line[key_start..key_end];

                let rest = &line[key_end + 1..]; // after closing quote of key
                let val_start = rest.find('"')? + 1;
                let val_end = rest[val_start..].find('"')? + val_start;
                let value = &rest[val_start..val_end];

                Some((key.to_owned(), value.to_owned()))
            })
            .collect();

        assert_eq!(
            py_map.len(),
            SKILL_TYPE_MAP.len(),
            "entry count mismatch: Python has {}, Rust has {}",
            py_map.len(),
            SKILL_TYPE_MAP.len()
        );

        for (py_key, py_val) in &py_map {
            let rust_val = SKILL_TYPE_MAP
                .get(py_key.as_str())
                .copied()
                .unwrap_or_else(|| {
                    panic!("Python key {py_key:?} missing from Rust SKILL_TYPE_MAP")
                });
            assert_eq!(
                rust_val,
                py_val.as_str(),
                "value mismatch for key {py_key:?}: Python={py_val:?}, Rust={rust_val:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // agent_skill_path consistency check
    // -----------------------------------------------------------------------

    #[test]
    fn agent_skill_path_helper_is_consistent() {
        // Sanity: the helper we use in multiple tests points at the right type.
        let path = agent_skill_path();
        assert!(
            path.starts_with("agents/"),
            "agent_skill_path must start with agents/"
        );
    }
}
