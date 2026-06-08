//! Typed carrier for enrichment-derived fields between enrich/apply
//! and the persistence boundary.  Avoids scattered serde_json::Value
//! mutations.
//!
//! At the persistence boundary, `merge_into` is called exactly once
//! per event to write these typed fields into `envelope.p`, in an
//! insertion order that preserves byte-identical Python parity.

#[derive(Debug, Clone, Default)]
pub struct EnrichedPayload {
    // Pass 1 — skill detection
    pub skill_name: Option<String>,
    pub skill_type: Option<String>,

    // apply_git_and_config
    pub config_version: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
}

impl EnrichedPayload {
    /// Merge the typed fields into the given JSON object.
    ///
    /// Insertion order (preserved across all callers — parity-critical):
    ///   1. skill_name, skill_type
    ///   2. config_version, git_branch, git_commit
    ///
    /// Fields that are `None` are NOT inserted (matches the current
    /// behaviour where empty values are simply not added).  An existing
    /// key in `obj` is overwritten — same as the in-place mutation
    /// previously did.
    pub fn merge_into(&self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        // 1. skill_name, skill_type — first to mirror Pass 1 ordering
        if let Some(sn) = &self.skill_name {
            obj.insert(
                "skill_name".to_owned(),
                serde_json::Value::String(sn.clone()),
            );
        }
        if let Some(st) = &self.skill_type {
            obj.insert(
                "skill_type".to_owned(),
                serde_json::Value::String(st.clone()),
            );
        }
        // 2. config_version, git_branch, git_commit — after, mirroring apply_git_and_config
        if let Some(cv) = &self.config_version {
            obj.insert(
                "config_version".to_owned(),
                serde_json::Value::String(cv.clone()),
            );
        }
        if let Some(gb) = &self.git_branch {
            obj.insert(
                "git_branch".to_owned(),
                serde_json::Value::String(gb.clone()),
            );
        }
        if let Some(gc) = &self.git_commit {
            obj.insert(
                "git_commit".to_owned(),
                serde_json::Value::String(gc.clone()),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // enriched_payload_default_is_all_none
    // -----------------------------------------------------------------------

    /// Sanity check: the Default impl must leave all fields as None.
    #[test]
    fn enriched_payload_default_is_all_none() {
        let ep = EnrichedPayload::default();
        assert!(ep.skill_name.is_none());
        assert!(ep.skill_type.is_none());
        assert!(ep.config_version.is_none());
        assert!(ep.git_branch.is_none());
        assert!(ep.git_commit.is_none());
    }

    // -----------------------------------------------------------------------
    // merge_into_inserts_in_correct_order
    // -----------------------------------------------------------------------

    /// Given an empty JSON object and an EnrichedPayload with all five fields
    /// populated, verify the iteration order of the resulting Map is:
    ///   skill_name, skill_type, config_version, git_branch, git_commit
    ///
    /// serde_json with `preserve_order` uses IndexMap, so insertion order is
    /// preserved.
    #[test]
    fn merge_into_inserts_in_correct_order() {
        let ep = EnrichedPayload {
            skill_name: Some("my-skill".to_owned()),
            skill_type: Some("agent_definition".to_owned()),
            config_version: Some("abcd1234".to_owned()),
            git_branch: Some("main".to_owned()),
            git_commit: Some("deadbeef".to_owned()),
        };

        let mut map = serde_json::Map::new();
        ep.merge_into(&mut map);

        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            &[
                "skill_name",
                "skill_type",
                "config_version",
                "git_branch",
                "git_commit"
            ],
            "insertion order must match parity-critical ordering"
        );
    }

    // -----------------------------------------------------------------------
    // merge_into_skips_none_fields
    // -----------------------------------------------------------------------

    /// Only `skill_name` set; verify only `skill_name` appears in the map.
    #[test]
    fn merge_into_skips_none_fields() {
        let ep = EnrichedPayload {
            skill_name: Some("only-skill".to_owned()),
            ..EnrichedPayload::default()
        };

        let mut map = serde_json::Map::new();
        ep.merge_into(&mut map);

        assert_eq!(map.len(), 1, "only skill_name must be present");
        assert!(map.contains_key("skill_name"));
        assert!(!map.contains_key("skill_type"));
        assert!(!map.contains_key("config_version"));
        assert!(!map.contains_key("git_branch"));
        assert!(!map.contains_key("git_commit"));
    }

    // -----------------------------------------------------------------------
    // merge_into_overwrites_existing_keys
    // -----------------------------------------------------------------------

    /// Pre-populate the map with `skill_name = "stale"`, run merge with
    /// `skill_name = Some("fresh")`, verify the result is "fresh".
    #[test]
    fn merge_into_overwrites_existing_keys() {
        let ep = EnrichedPayload {
            skill_name: Some("fresh".to_owned()),
            ..EnrichedPayload::default()
        };

        let mut map = serde_json::Map::new();
        map.insert(
            "skill_name".to_owned(),
            serde_json::Value::String("stale".to_owned()),
        );

        ep.merge_into(&mut map);

        assert_eq!(
            map.get("skill_name").and_then(|v| v.as_str()),
            Some("fresh"),
            "merge_into must overwrite existing keys"
        );
    }
}
