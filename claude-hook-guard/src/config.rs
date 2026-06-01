use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const KNOWN_NATIVE_TOOLS: &[&str] = &[
    "Agent",
    "AskUserQuestion",
    "Bash",
    "Edit",
    "EnterPlanMode",
    "ExitPlanMode",
    "Glob",
    "Grep",
    "Read",
    "Search",
    "Skill",
    "Task",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskOutput",
    "TaskUpdate",
    "TodoRead",
    "TodoWrite",
    "WebFetch",
    "Write",
    "ast_grep_search",
    "lsp_diagnostics",
    "lsp_document_symbols",
    "lsp_find_references",
    "lsp_goto_definition",
    "lsp_hover",
    "lsp_servers",
    "lsp_workspace_symbols",
];

#[derive(Debug, Clone, Deserialize)]
pub struct RulesConfig {
    pub profiles: HashMap<String, Profile>,
    #[serde(rename = "command-categories")]
    pub command_categories: HashMap<String, CommandCategory>,
    pub safety: HashMap<String, SafetyGuard>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default, rename = "mcp-tools")]
    pub mcp_tools: Vec<String>,
    #[serde(default)]
    pub commands: CommandsPolicy,
    #[serde(default, rename = "safety-overrides")]
    pub safety_overrides: Vec<String>,
    #[serde(default)]
    pub agent: Option<AgentPolicy>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CommandsPolicy {
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub structure: Option<StructurePolicy>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StructurePolicy {
    #[serde(default, rename = "single-command")]
    pub single_command: bool,
    #[serde(default, rename = "no-redirection")]
    pub no_redirection: bool,
    #[serde(default, rename = "allow-pipeline")]
    pub allow_pipeline: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentPolicy {
    pub spawn: SpawnPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnPolicy {
    pub allow: Vec<SpawnTarget>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnTarget {
    pub profile: String,
    #[serde(default, rename = "may-bypass")]
    pub may_bypass: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommandCategory {
    pub allow: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SafetyGuard {
    pub kind: SafetyGuardKind,
    pub command: GuardCommandMatch,
    #[serde(default, rename = "allow-exact")]
    pub allow_exact: Vec<Vec<String>>,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub prefixes: Vec<String>,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub forbid: Vec<String>,
    #[serde(default)]
    pub subcommands: Vec<Vec<String>>,
    #[serde(default)]
    pub branches: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuardCommandMatch {
    pub prefix: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SafetyGuardKind {
    DenyFlags,
    RequirePositionalPrefix,
    RequireCurlUrlPrefix,
    AllowCurlHeaders,
    AllowCurlMethods,
    AllowCurlForms,
    RequireExplicitPathspecs,
    RequireExplicitPushTarget,
    DenyProtectedBranch,
    RequireBoundedLogs,
    DenyAlways,
    DenySubcommands,
}

#[derive(Debug)]
pub struct ValidatedConfig {
    pub rules: RulesConfig,
    pub alias_to_profile: HashMap<String, String>,
}

impl ValidatedConfig {
    pub fn resolve_profile_id(&self, agent_type: Option<&str>) -> Option<&str> {
        let raw = agent_type.unwrap_or("");
        let key = normalize_identity(raw);
        self.alias_to_profile.get(&key).map(|s| s.as_str())
    }
}

pub fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".claude/hooks/rules.toml"))
}

pub fn load_config(path: Option<&Path>) -> Result<ValidatedConfig, String> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => default_config_path().ok_or("Could not determine default config path")?,
    };

    let content =
        fs::read_to_string(&path).map_err(|_| format!("Missing config: {}", path.display()))?;
    let rules: RulesConfig = toml::from_str(&content)
        .map_err(|e| format!("Invalid config {}: {}", path.display(), e))?;
    validate_rules(rules)
}

fn validate_rules(rules: RulesConfig) -> Result<ValidatedConfig, String> {
    let mut alias_to_profile = HashMap::new();
    let mut seen_names: HashMap<String, String> = HashMap::new();

    if !rules.profiles.contains_key("_default") {
        return Err("Config must define profiles._default".to_string());
    }

    for (profile_id, profile) in &rules.profiles {
        for tool in &profile.tools {
            if !KNOWN_NATIVE_TOOLS.contains(&tool.as_str()) {
                return Err(format!(
                    "Unknown native tool '{}' in profile '{}'",
                    tool, profile_id
                ));
            }
        }

        for category in &profile.commands.categories {
            if !rules.command_categories.contains_key(category) {
                return Err(format!(
                    "Unknown command category '{}' in profile '{}'",
                    category, profile_id
                ));
            }
        }

        for guard in &profile.safety_overrides {
            if !rules.safety.contains_key(guard) {
                return Err(format!(
                    "Unknown safety override '{}' in profile '{}'",
                    guard, profile_id
                ));
            }
        }

        let canonical = normalize_identity(profile_id);
        if let Some(existing) = seen_names.get(&canonical) {
            if existing != profile_id {
                return Err(format!(
                    "Duplicate canonical profile identity '{}'",
                    profile_id
                ));
            }
        }
        seen_names.insert(canonical.clone(), profile_id.clone());
        alias_to_profile.insert(canonical, profile_id.clone());

        for alias in &profile.aliases {
            let normalized = normalize_identity(alias);
            if let Some(existing) = seen_names.get(&normalized) {
                if existing != profile_id {
                    return Err(format!("Duplicate profile alias '{}'", alias));
                }
            }
            seen_names.insert(normalized.clone(), profile_id.clone());
            alias_to_profile.insert(normalized, profile_id.clone());
        }

        if let Some(agent) = &profile.agent {
            for target in &agent.spawn.allow {
                if !rules.profiles.contains_key(&target.profile) {
                    return Err(format!(
                        "Unknown spawn target '{}' referenced by profile '{}'",
                        target.profile, profile_id
                    ));
                }
            }
        }
    }

    for (profile_id, profile) in &rules.profiles {
        for guard_name in &profile.safety_overrides {
            let guard = rules.safety.get(guard_name).unwrap();
            let reachable = profile
                .commands
                .categories
                .iter()
                .filter_map(|name| rules.command_categories.get(name))
                .flat_map(|category| category.allow.iter())
                .any(|prefix| prefix_starts_with(prefix, &guard.command.prefix));
            if !reachable {
                return Err(format!(
                    "Unreachable safety override '{}' in profile '{}'",
                    guard_name, profile_id
                ));
            }
        }
    }

    Ok(ValidatedConfig {
        rules,
        alias_to_profile,
    })
}

fn prefix_starts_with(candidate: &[String], prefix: &[String]) -> bool {
    candidate.len() >= prefix.len() && candidate.iter().zip(prefix.iter()).all(|(a, b)| a == b)
}

pub fn normalize_identity(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_default_rules() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-rules.toml");
        let validated = load_config(Some(&path)).expect("default-rules.toml should validate");
        assert!(validated.rules.profiles.contains_key("main"));
        assert_eq!(validated.resolve_profile_id(Some("search")), Some("search"));
        assert_eq!(
            validated.resolve_profile_id(Some("ast-search")),
            Some("ast-search")
        );
        assert_eq!(
            validated.resolve_profile_id(Some("lsp-search")),
            Some("lsp-search")
        );
        assert_eq!(validated.resolve_profile_id(None), Some("orchestrator"));
    }
}
