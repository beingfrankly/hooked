use claude_hook_guard::config::load_config;
use claude_hook_guard::engine::Engine;
use claude_hook_guard::input::{HookInput, ToolInput};
use std::path::PathBuf;

fn default_config() -> claude_hook_guard::config::ValidatedConfig {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-rules.toml");
    load_config(Some(&path)).expect("default rules should load")
}

fn hook(tool_name: &str, agent_type: Option<&str>, tool_input: ToolInput) -> HookInput {
    HookInput {
        tool_name: Some(tool_name.to_string()),
        tool_input: Some(tool_input),
        agent_type: agent_type.map(|s| s.to_string()),
        agent_id: Some("test-agent".to_string()),
    }
}

#[test]
fn default_rules_resolve_main_and_aliases() {
    let config = default_config();
    assert_eq!(config.resolve_profile_id(None), Some("orchestrator"));
    assert_eq!(config.resolve_profile_id(Some("search")), Some("search"));
    assert_eq!(
        config.resolve_profile_id(Some("ast-search")),
        Some("ast-search")
    );
    assert_eq!(
        config.resolve_profile_id(Some("lsp-search")),
        Some("lsp-search")
    );
    assert_eq!(config.resolve_profile_id(Some("curl")), Some("curl"));
}

#[test]
fn subagents_can_use_skill() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook("Skill", Some("browser"), ToolInput::default());
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn main_must_delegate_glob() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook("Glob", Some("main"), ToolInput::default());
    let reason = engine.evaluate(&input).expect("main glob should be denied");
    assert!(reason.contains("Delegate Glob"));
}

#[test]
fn main_can_use_search() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook("Search", Some("main"), ToolInput::default());
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn orchestrator_can_use_hubspotdev_mcp_docs_tools() {
    let config = default_config();
    let engine = Engine { config: &config };

    let fetch_input = hook(
        "mcp__HubSpotDev__fetch-doc",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&fetch_input).is_none());

    let search_input = hook(
        "mcp__HubSpotDev__search-doc",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&search_input).is_none());
}

#[test]
fn orchestrator_can_use_atlassian_mcp_tools_via_prefix_rule() {
    let config = default_config();
    let engine = Engine { config: &config };

    let jira_input = hook(
        "mcp__atlassian__search",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&jira_input).is_none());

    let confluence_input = hook(
        "mcp__atlassian__getConfluencePage",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&confluence_input).is_none());

    let teamwork_graph_input = hook(
        "mcp__atlassian__getTeamworkGraphContext",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&teamwork_graph_input).is_none());

    let configured_alias_input = hook(
        "mcp__mcp-atlassian__getJiraIssue",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&configured_alias_input).is_none());

    let display_name_input = hook(
        "mcp__Atlassian_MCP_Server__getJiraIssue",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&display_name_input).is_none());
}

#[test]
fn orchestrator_can_use_hfs_mcp_tools_via_prefix_rules() {
    let config = default_config();
    let engine = Engine { config: &config };

    let hyphenated_input = hook(
        "mcp__hfs-api__hfs_login",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&hyphenated_input).is_none());

    let normalized_input = hook(
        "mcp__hfs_api__hfs_login",
        Some("orchestrator"),
        ToolInput::default(),
    );
    assert!(engine.evaluate(&normalized_input).is_none());
}

#[test]
fn orchestrator_can_use_bd_cli_via_bash() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("orchestrator"),
        ToolInput {
            command: Some("bd list".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn bd_cli_is_allowed_for_all_registered_profiles() {
    let config = default_config();
    let engine = Engine { config: &config };

    for profile in [
        "main",
        "search",
        "ast-search",
        "lsp-search",
        "curl",
        "worker",
        "reviewer",
        "codex-review",
        "Plan",
        "git",
        "notes",
        "build-runner",
        "docker",
        "browser",
    ] {
        let input = hook(
            "Bash",
            Some(profile),
            ToolInput {
                command: Some("bd list".to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "profile {profile} should be allowed to run bd"
        );
    }
}

#[test]
fn bd_read_commands_can_take_arguments_globally() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        "bd show project-123 --json",
        "bd stats",
        "bd search login --status all",
        "bd blocked --json",
        "bd graph project-123 --compact",
        "bd count --status open",
        "bd children epic-123",
        "bd history project-123",
        "bd types",
        "bd statuses",
        "bd context",
        "bd where",
        "bd info",
        "bd recall project-memory",
        "bd state workflow",
        "bd quickstart",
        "bd human",
        "bd version",
        "bd preflight",
        "bd lint",
        "bd stale",
        "bd defer project-123 --until tomorrow",
        "bd diff main feature",
        "bd dep list project-123 --json",
        "bd dep tree project-123",
        "bd dep cycles",
    ] {
        let input = hook(
            "Bash",
            Some("main"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "main should be allowed to run read-only beads command: {command}"
        );
    }
}

#[test]
fn bd_write_commands_are_limited_to_orchestrator_and_worker() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        "bd update project-123 --claim",
        "bd dep add nvim-cl2 nvim-bv2",
        "bd dep remove nvim-cl2 nvim-bv2",
        "bd dep relate nvim-cl2 nvim-bv2",
        "bd dep unrelate nvim-cl2 nvim-bv2",
    ] {
        for profile in ["orchestrator", "worker"] {
            let input = hook(
                "Bash",
                Some(profile),
                ToolInput {
                    command: Some(command.to_string()),
                    ..ToolInput::default()
                },
            );
            assert!(
                engine.evaluate(&input).is_none(),
                "profile {profile} should be allowed to run beads write command: {command}"
            );
        }
    }

    for command in [
        "bd update project-123 --claim",
        "bd dep add nvim-cl2 nvim-bv2",
    ] {
        for profile in ["main", "search", "reviewer", "codex-review", "git"] {
            let input = hook(
                "Bash",
                Some(profile),
                ToolInput {
                    command: Some(command.to_string()),
                    ..ToolInput::default()
                },
            );
            assert!(
                engine.evaluate(&input).is_some(),
                "profile {profile} should not be allowed to mutate beads with: {command}"
            );
        }
    }
}

#[test]
fn orchestrator_bash_is_limited_to_bd_cli() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("orchestrator"),
        ToolInput {
            command: Some("git status".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("orchestrator bash should only allow bd");
    assert!(reason.contains("Command 'git status' not allowed"));
}

#[test]
fn reviewer_global_bd_access_does_not_enable_git_bash() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("reviewer"),
        ToolInput {
            command: Some("git status".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("reviewer should not get general Bash access");
    assert!(reason.contains("Tool Bash not allowed"));
}

#[test]
fn global_bd_access_rejects_shell_composition() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("main"),
        ToolInput {
            command: Some("bd list && bd status".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("global bd access should remain a single command");
    assert!(reason.contains("Tool Bash not allowed"));
}

#[test]
fn blocked_worker_shell_substitutes_suggest_native_tools() {
    let config = default_config();
    let engine = Engine { config: &config };

    for (command, expected) in [
        (
            r#"grep -n "Lazy" /tmp/snacks.lua"#,
            "Use the native Grep tool instead.",
        ),
        (
            r#"find /tmp -name snacks.lua"#,
            "Use the native Glob tool instead.",
        ),
        (
            r#"cat /tmp/snacks.lua"#,
            "Use the native Read tool for file inspection.",
        ),
        (
            r#"readlink -f /tmp/snacks.lua"#,
            "Use Read, Glob, and task context for path inspection",
        ),
        (
            r#"python3 -c "import os; print(os.path.realpath('/tmp/snacks.lua'))""#,
            "Do not use scripting languages as shell substitutes",
        ),
        (
            r#"nvim --headless -u /tmp/init.lua +q"#,
            "Use Edit/Write for file changes",
        ),
    ] {
        let input = hook(
            "Bash",
            Some("worker"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        let reason = engine
            .evaluate(&input)
            .unwrap_or_else(|| panic!("worker command should be denied: {command}"));
        assert!(
            reason.contains(expected),
            "denial for {command:?} should contain {expected:?}, got {reason:?}"
        );
    }
}

#[test]
fn docs_agent_can_use_ctx7_and_defuddle_cli() {
    let config = default_config();
    let engine = Engine { config: &config };

    let library_input = hook(
        "Bash",
        Some("docs"),
        ToolInput {
            command: Some("ctx7 library react useEffect".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&library_input).is_none());

    let docs_input = hook(
        "Bash",
        Some("docs"),
        ToolInput {
            command: Some("ctx7 docs /facebook/react useEffect".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&docs_input).is_none());

    let defuddle_input = hook(
        "Bash",
        Some("docs"),
        ToolInput {
            command: Some("defuddle parse https://example.com/docs --md".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&defuddle_input).is_none());
}

#[test]
fn docs_agent_denies_shell_search() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("docs"),
        ToolInput {
            command: Some("grep -R useEffect .".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("docs should not be able to run shell grep");
    assert!(reason.contains("not allowed"));
}

#[test]
fn search_denies_ctx7_docs_via_bash() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("search"),
        ToolInput {
            command: Some("ctx7 docs /facebook/react useEffect".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("search should not allow docs lookup via Bash");
    assert!(reason.contains("Command 'ctx7 docs /facebook/react useEffect' not allowed"));
}

#[test]
fn search_denies_hs_via_bash() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("search"),
        ToolInput {
            command: Some("hs project list".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("search should not allow HubSpot CLI via Bash");
    assert!(reason.contains("Command 'hs project list' not allowed"));
}

#[test]
fn search_allows_read_only_path_introspection() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        "readlink -f /tmp/snacks.lua",
        "greadlink -f /tmp/snacks.lua",
        "realpath /tmp/snacks.lua",
        "ls -la /tmp/snacks.lua",
        "stat /tmp/snacks.lua",
        "file /tmp/snacks.lua",
    ] {
        let input = hook(
            "Bash",
            Some("search"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "search should allow read-only path introspection command: {command}"
        );
    }
}

#[test]
fn search_allows_shell_search_fallbacks() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        r#"grep -n "Lazy" /tmp/snacks.lua"#,
        r#"egrep -n "Lazy|VeryLazy" /tmp/snacks.lua"#,
        r#"fgrep -n "Lazy" /tmp/snacks.lua"#,
        r#"rg -n "Lazy" /tmp"#,
        r#"find /tmp -name snacks.lua"#,
        r#"fd snacks /tmp"#,
    ] {
        let input = hook(
            "Bash",
            Some("search"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "search should allow shell search fallback command: {command}"
        );
    }
}

#[test]
fn search_denies_shell_search_exec_or_write_actions() {
    let config = default_config();
    let engine = Engine { config: &config };

    for (command, expected) in [
        (
            r#"find /tmp -name snacks.lua -delete"#,
            "find actions that execute commands or write files",
        ),
        (
            r#"find /tmp -name snacks.lua -exec rm {} +"#,
            "find actions that execute commands or write files",
        ),
        (r#"fd snacks /tmp -x rm"#, "fd exec actions are not allowed"),
        (
            r#"rg --pre ./render-markdown "Lazy" /tmp"#,
            "rg preprocessors are not allowed",
        ),
    ] {
        let input = hook(
            "Bash",
            Some("search"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        let reason = engine
            .evaluate(&input)
            .expect("search should deny unsafe shell search command");
        assert!(
            reason.contains(expected),
            "expected {expected:?} in denial for {command:?}, got {reason:?}"
        );
    }
}

#[test]
fn curl_agent_can_use_hubapi_get_with_auth_and_jq_pipeline() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -s -H \"Authorization: Bearer $TOKEN\" \"https://api.hubapi.com/marketing/v3/emails\" | jq ."
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn curl_agent_can_use_hubapi_post_search_with_json_and_jq_pipeline() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -s -X POST -H \"Authorization: Bearer <YOUR_TOKEN>\" -H \"Content-Type: application/json\" -d '{\"query\":\"cor.netto@gmail.com\"}' \"https://api.hubapi.com/crm/v3/objects/contacts/search\" | jq ."
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn curl_agent_denies_curl_to_other_domains() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some("curl https://example.com".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("curl outside hubapi should be denied");
    assert!(reason.contains("https://api.hubapi.com"));
}

#[test]
fn curl_agent_denies_curl_output_flag_equals_form() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl --output=result.json https://api.hubapi.com/marketing/v3/emails".to_string(),
            ),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("curl output flag should be denied");
    assert!(reason.contains("BLOCKED"));
}

#[test]
fn curl_agent_denies_curl_with_unapproved_header() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -H \"X-Test: nope\" https://api.hubapi.com/marketing/v3/emails".to_string(),
            ),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("curl header should be denied");
    assert!(reason.contains("Authorization Bearer"));
}

#[test]
fn curl_agent_can_use_hubapi_put_with_json() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -X PUT -H \"Authorization: Bearer $TOKEN\" -H \"Content-Type: application/json\" -d '{\"name\":\"Updated\"}' \"https://api.hubapi.com/marketing/v3/emails/123\""
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn curl_agent_can_use_hubapi_patch_with_json() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -X PATCH -H \"Authorization: Bearer $TOKEN\" -H \"Content-Type: application/json\" -d '{\"name\":\"Updated\"}' \"https://api.hubapi.com/marketing/v3/emails/123\""
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn curl_agent_can_use_hubapi_import_multipart_from_tmp() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -s -X POST -H \"Authorization: Bearer $TOKEN\" -F \"importRequest=</tmp/hubspot-import-request.json;type=application/json\" -F \"files=@/tmp/hubspot-import.csv;type=text/csv\" \"https://api.hubapi.com/crm/v3/imports\" | jq ."
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn curl_agent_denies_unapproved_multipart_form_usage() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -s -X POST -H \"Authorization: Bearer $TOKEN\" -F \"other=@/tmp/anything.csv\" \"https://api.hubapi.com/crm/v3/imports\" | jq ."
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("unexpected multipart form should be denied");
    assert!(reason.contains("multipart curl"));
}

#[test]
fn curl_agent_denies_curl_with_unapproved_method() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -X DELETE -H \"Authorization: Bearer $TOKEN\" \"https://api.hubapi.com/marketing/v3/emails\""
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("curl method should be denied");
    assert!(reason.contains("GET, POST, PUT, and PATCH"));
}

#[test]
fn curl_agent_still_denies_shell_lists_with_curl() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("curl"),
        ToolInput {
            command: Some(
                "curl -s -H \"Authorization: Bearer $TOKEN\" \"https://api.hubapi.com/marketing/v3/emails\" && jq ."
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("shell lists should still be denied");
    assert!(reason.contains("shell composition"));
}

#[test]
fn search_denies_curl_commands() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("search"),
        ToolInput {
            command: Some(
                "curl -s -H \"Authorization: Bearer $TOKEN\" \"https://api.hubapi.com/marketing/v3/emails\""
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("search should not allow curl");
    assert!(reason.contains("not allowed"));
}

#[test]
fn worker_can_use_hs_via_bash() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("worker"),
        ToolInput {
            command: Some("hs project list".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn approved_spawn_with_optional_bypass_is_allowed() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Agent",
        None,
        ToolInput {
            profile: Some("search".to_string()),
            bypass_permissions: Some(true),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn curl_spawn_with_optional_bypass_is_allowed() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Agent",
        None,
        ToolInput {
            profile: Some("curl".to_string()),
            bypass_permissions: Some(true),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn task_tool_alias_is_allowed_for_orchestrator_profile() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Task",
        None,
        ToolInput {
            profile: Some("search".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn todo_aliases_are_allowed_via_legacy_task_permissions() {
    let config = default_config();
    let engine = Engine { config: &config };

    let read_input = hook("TodoRead", None, ToolInput::default());
    assert!(engine.evaluate(&read_input).is_none());

    let write_input = hook("TodoWrite", None, ToolInput::default());
    assert!(engine.evaluate(&write_input).is_none());
}

#[test]
fn unapproved_bypass_is_denied() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Agent",
        None,
        ToolInput {
            profile: Some("worker".to_string()),
            bypass_permissions: Some(true),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("worker bypass should be denied");
    assert!(reason.contains("bypassPermissions"));
}

#[test]
fn notes_profile_allows_obsidian_create() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("notes"),
        ToolInput {
            command: Some("obsidian create name=\"Note\" content=\"hello\"".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn notes_profile_denies_redirection() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("notes"),
        ToolInput {
            command: Some("obsidian read file=\"Note\" > out.txt".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("redirection should be denied");
    assert!(reason.contains("redirection"));
}

#[test]
fn worker_denies_recursive_rm() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("worker"),
        ToolInput {
            command: Some("rm -rf tmp".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("recursive rm should be denied");
    assert!(reason.contains("recursive rm"));
}

#[test]
fn git_denies_force_push() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("git push --force origin feature".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("force push should be denied");
    assert!(reason.contains("force/delete push"));
}

#[test]
fn git_allows_explicit_push() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("git push origin feature-branch".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn git_allows_fetch() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("git fetch origin main".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn git_allows_rebase_origin_main() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("git rebase origin/main".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn git_rebase_other_target_requires_confirmation() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("git rebase origin/develop".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("rebase should require confirmation");
    assert!(reason.contains("git rebase requires explicit user confirmation"));
}

#[test]
fn worker_cannot_rebase_origin_main() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("worker"),
        ToolInput {
            command: Some("git rebase origin/main".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("worker should not be able to rebase");
    assert!(reason.contains("not allowed"));
}

#[test]
fn git_fetch_then_rebase_then_force_with_lease_blocks_on_rebase() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some(
                "git fetch origin main && git rebase origin/develop && git push --force-with-lease origin feature-branch"
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("rebase should stop the compound command");
    assert!(reason.contains("git rebase requires explicit user confirmation"));
}

#[test]
fn git_allows_worktree_list() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("git worktree list --porcelain".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_cd_preamble_then_pnpm_run() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("cd /tmp/project && pnpm run build:libs".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn git_allows_worktree_add() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("git worktree add /tmp/test-wt feature-branch".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn git_allows_merge_for_git_profile() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("git merge origin/main".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn git_profile_allows_read_only_github_cli() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        "gh auth status",
        "gh pr view 123 --json headRefName,baseRefName",
        "gh pr checks 123",
        "gh pr diff 123",
        "gh issue list --state open",
        "gh repo view owner/repo",
        "gh run view 456 --log",
    ] {
        let input = hook(
            "Bash",
            Some("git"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "git profile should allow GitHub CLI read command: {command}"
        );
    }
}

#[test]
fn github_cli_is_not_global_bash_access() {
    let config = default_config();
    let engine = Engine { config: &config };

    for profile in ["main", "worker", "reviewer"] {
        let input = hook(
            "Bash",
            Some(profile),
            ToolInput {
                command: Some("gh pr view 123".to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_some(),
            "profile {profile} should not get gh access from git profile rules"
        );
    }
}

#[test]
fn git_profile_denies_mutating_github_cli_by_default() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("gh pr close 123".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("mutating gh command should be denied");
    assert!(reason.contains("not allowed"));
}

#[test]
fn git_profile_allows_worktrunk_core_commands() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        "wt list",
        "wt switch feature-branch",
        "wt switch --create feature-branch --no-cd --no-hooks",
        "wt remove feature-branch",
        "wt merge main",
        "wt config show",
        "wt step commit --show-prompt",
        "wt hook pre-merge",
        "git-wt list",
        "git-wt --help",
        "git-wt switch --help",
        "git-wt create --help",
    ] {
        let input = hook(
            "Bash",
            Some("git"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "git profile should allow {command}"
        );
    }
}

#[test]
fn worker_profile_denies_worktrunk_commands() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("worker"),
        ToolInput {
            command: Some("wt merge main".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("worker should not run worktrunk git operations");
    assert!(reason.contains("not allowed"));
}

#[test]
fn git_profile_denies_unregistered_worktrunk_aliases() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("git"),
        ToolInput {
            command: Some("wt deploy".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("unregistered wt aliases should not be broadly allowed");
    assert!(reason.contains("not allowed"));
}

#[test]
fn worker_allows_chmod_executable_only() {
    let config = default_config();
    let engine = Engine { config: &config };
    let executable = hook(
        "Bash",
        Some("worker"),
        ToolInput {
            command: Some("chmod +x scripts/run.sh".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&executable).is_none());

    let broad = hook(
        "Bash",
        Some("worker"),
        ToolInput {
            command: Some("chmod 777 scripts/run.sh".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&broad)
        .expect("broad chmod should remain denied");
    assert!(reason.contains("not allowed"));
}

#[test]
fn build_runner_allows_pnpm_script_shorthand() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("pnpm test-super-striive".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_plain_cargo() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("cargo fmt".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_select_application_compile_with_flags() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("mvn -pl select-application -am -q compile".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_maven_test_with_arbitrary_args() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("mvn -pl any-module -q test -Dtest=AnyTest -DskipITs".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_select_application_test_class() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some(
                "mvn -pl select-application test -Dtest=JobMatchActivatedEventPublisherTest"
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_pitest_mutation_coverage_with_maven_flags() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some(
                "mvn -pl select-application org.pitest:pitest-maven:mutationCoverage -DtargetClasses=com.example.Service -DtargetTests=com.example.ServiceTest -DoutputFormats=HTML,XML"
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_short_pitest_mutation_coverage_goal() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("mvn -pl select-application pitest:mutationCoverage".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_maven_install_and_test_compile_with_flags() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        "mvn install",
        "mvn -pl select-application -am install",
        "mvn -pl select-application test-compile",
    ] {
        let input = hook(
            "Bash",
            Some("build-runner"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "build-runner should allow Maven build lifecycle command: {command}"
        );
    }
}

#[test]
fn build_runner_allows_mvnw_compile_with_flags() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("./mvnw -pl another-module -am compile".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_stryker_and_typescript_quality_commands() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        "STRYKER_TEST_MATCH=libs/striive-common/src/foo.spec.ts npx stryker run --mutate libs/striive-common/src/foo.ts",
        "npx @stryker-mutator/core run --mutate libs/striive-common/src/foo.ts",
        "npx tsc --noEmit",
    ] {
        let input = hook(
            "Bash",
            Some("build-runner"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "build-runner should allow JS/TS quality command: {command}"
        );
    }
}

#[test]
fn build_runner_allows_go_hfs_build_wrapper() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("go-hfs build".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_rustup() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("build-runner"),
        ToolInput {
            command: Some("rustup target list".to_string()),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn build_runner_allows_neovim_headless_load_gates() {
    let config = default_config();
    let engine = Engine { config: &config };

    for command in [
        "nvim --headless -l tests/health.lua",
        "nvim --headless -u /Users/Frank.vanEldijk/.config/nvim/init.lua +qall",
        "nvim --headless -u /Users/Frank.vanEldijk/.config/nvim/init.lua --cmd 'lua require(\"custom.plugins.worktree\")' +qall",
    ] {
        let input = hook(
            "Bash",
            Some("build-runner"),
            ToolInput {
                command: Some(command.to_string()),
                ..ToolInput::default()
            },
        );
        assert!(
            engine.evaluate(&input).is_none(),
            "build-runner should allow Neovim headless load gate: {command}"
        );
    }
}

#[test]
fn codex_review_allows_codex_companion_review_command() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("codex-review"),
        ToolInput {
            command: Some(
                "cd /tmp/project && node \"/Users/Frank.vanEldijk/.claude/plugins/marketplaces/openai-codex/plugins/codex/scripts/codex-companion.mjs\" review \"\""
                    .to_string(),
            ),
            ..ToolInput::default()
        },
    );
    assert!(engine.evaluate(&input).is_none());
}

#[test]
fn codex_review_denies_other_node_commands() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("codex-review"),
        ToolInput {
            command: Some("node /tmp/other-script.mjs review".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("non-companion node command should be denied");
    assert!(reason.contains("not allowed"));
}

#[test]
fn docker_logs_requires_tail() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook(
        "Bash",
        Some("docker"),
        ToolInput {
            command: Some("docker logs my-container".to_string()),
            ..ToolInput::default()
        },
    );
    let reason = engine
        .evaluate(&input)
        .expect("unbounded logs should be denied");
    assert!(reason.contains("requires --tail"));
}

#[test]
fn unknown_agent_type_is_denied() {
    let config = default_config();
    let engine = Engine { config: &config };
    let input = hook("Read", Some("mystery"), ToolInput::default());
    let reason = engine
        .evaluate(&input)
        .expect("unknown agents should be denied");
    assert!(reason.contains("unregistered agent type"));
}
