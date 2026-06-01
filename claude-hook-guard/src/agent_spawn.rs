use crate::config::{normalize_identity, ValidatedConfig};
use crate::input::HookInput;

pub fn validate_agent_spawn(
    config: &ValidatedConfig,
    profile_id: &str,
    input: &HookInput,
) -> Option<String> {
    let profile = config.rules.profiles.get(profile_id)?;
    let agent_policy = profile.agent.as_ref()?;
    let tool_input = input.tool_input.as_ref()?;
    let raw_target = tool_input
        .spawn_target()
        .ok_or_else(|| "BLOCKED: Agent spawn requires a target profile.".to_string())
        .ok()?;
    let normalized = normalize_identity(raw_target);
    let canonical_target = config
        .alias_to_profile
        .get(&normalized)
        .ok_or_else(|| format!("BLOCKED: Unknown spawn target '{}'.", raw_target))
        .ok()?;

    let target = agent_policy
        .spawn
        .allow
        .iter()
        .find(|target| target.profile == *canonical_target)
        .ok_or_else(|| {
            format!(
                "BLOCKED: Profile '{}' may not spawn '{}'.",
                profile_id, canonical_target
            )
        })
        .ok()?;

    if tool_input.requested_bypass() && !target.may_bypass {
        return Some(format!(
            "BLOCKED: Profile '{}' may not spawn '{}' with bypassPermissions.",
            profile_id, canonical_target
        ));
    }

    None
}
