use serde::{Deserialize, Serialize};
use serde_json::Value;

// --- Input structs (deserialized from stdin) ---

#[derive(Deserialize, Default)]
pub struct HookInput {
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<ToolInput>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct ToolInput {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub subagent_type: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default, rename = "bypassPermissions")]
    pub bypass_permissions: Option<bool>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

impl ToolInput {
    pub fn spawn_target(&self) -> Option<&str> {
        self.profile
            .as_deref()
            .or(self.subagent_type.as_deref())
            .or_else(|| self.extra.get("agent_type").and_then(|v| v.as_str()))
    }

    pub fn requested_bypass(&self) -> bool {
        self.bypass_permissions.unwrap_or(false)
            || self
                .extra
                .get("permissionMode")
                .and_then(|v| v.as_str())
                .map(|s| s == "bypassPermissions")
                .unwrap_or(false)
    }
}

// --- Output structs (serialized to stdout) ---

#[derive(Serialize)]
pub struct HookOutput {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: HookDecision,
}

#[derive(Serialize)]
pub struct HookDecision {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    #[serde(rename = "permissionDecision")]
    pub permission_decision: String,
    #[serde(rename = "permissionDecisionReason")]
    pub permission_decision_reason: String,
}

impl HookOutput {
    pub fn deny(reason: &str) -> Self {
        HookOutput {
            hook_specific_output: HookDecision {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: "deny".to_string(),
                permission_decision_reason: reason.to_string(),
            },
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Internal serialization error"}}"#.to_string()
        })
    }
}
