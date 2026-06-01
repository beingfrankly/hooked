# Claude Hook Guard v1 Implementation Plan

## Overview

Replace the current mode-driven `claude-hook-guard` binary with a config-first rule engine driven by a shipped default `rules.toml`.

The new binary should:

1. Read hook input from stdin.
2. Load and validate config.
3. Resolve the calling profile.
4. Enforce native tool and MCP tool policy.
5. Validate `Agent` spawns.
6. Enforce `Bash` command policy through category matching, structure checks, and safety guards.

## Entry Point Changes

Update [src/main.rs](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/src/main.rs) to remove the legacy `--mode` flow entirely.

`parse_args()` should support only:

- `--config <path>`
- `--json`
- `--human`
- `--verbose`
- `--help`
- `--version`

The main runtime flow should become:

1. Read stdin
2. Parse `HookInput`
3. Load config
4. Resolve profile
5. Check tool or MCP allowlist
6. Validate `Agent` requests
7. For non-`Bash`, allow
8. For `Bash`, parse and evaluate command policy

## New Modules

Create these modules:

### `src/config.rs`

Responsibilities:

- Parse `rules.toml`
- Validate schema
- Validate known native tool names
- Build alias lookup tables
- Resolve default config path

### `src/engine.rs`

Responsibilities:

- Own evaluation order
- Implement first-deny behavior
- Coordinate profile resolution, tool gating, spawn validation, command checks, and safety checks

### `src/command_match.rs`

Responsibilities:

- Token-prefix matching for command categories
- Token-prefix matching for safety guards
- Normalize lexical command paths to basename where needed

### `src/agent_spawn.rs`

Responsibilities:

- Parse `Agent` tool payload fields from hook input
- Validate canonical target profile
- Validate caller spawn authorization
- Validate requested `bypassPermissions`

### `src/safety.rs`

Responsibilities:

- Evaluate global safety guards from config
- Implement v1 guard kinds such as:
  - deny flags
  - deny always
  - deny subcommands
  - require explicit push target
  - require bounded logs
  - require explicit pathspecs
  - deny protected branches

### `src/structure.rs`

Responsibilities:

- Enforce structural restrictions for narrow Bash profiles
- Support v1 rules such as:
  - single command only
  - no redirection

## Existing Module Updates

### `src/input.rs`

Expand [src/input.rs](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/src/input.rs) so `ToolInput` can read the fields needed for `Agent` validation.

At minimum, support extracting:

- target profile / subagent type
- `bypassPermissions`

Unknown extra fields should be ignored in v1.

### `src/walker.rs`

Keep tree-sitter-based command extraction in [src/walker.rs](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/src/walker.rs), but add helpers for:

- extracting token sequences for command-prefix matching
- detecting banned wrapper forms
- collecting command segments for pipelines and lists

The new engine should reject wrappers instead of recursively blessing them.

### `src/lib.rs`

Update [src/lib.rs](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/src/lib.rs) to export the new modules if the crate uses a library surface.

### `Cargo.toml`

Update [Cargo.toml](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/Cargo.toml) to add:

- `toml`
- any small serde helpers needed for schema parsing

## Legacy Code Retirement

These files should be retired after the new engine is covered by tests:

- [src/modes/safety.rs](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/src/modes/safety.rs)
- [src/modes/native_tools.rs](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/src/modes/native_tools.rs)
- [src/modes/delegation.rs](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/src/modes/delegation.rs)
- [src/lists.rs](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/src/lists.rs)

Do not delete them first. Migrate behavior into the new engine, then remove them after replacement tests pass.

## Default Policy File

Add a versioned shipped default policy file:

- [default-rules.toml](/Users/Frank.vanEldijk/.claude/tools/claude-hook-guard/default-rules.toml)

This file should be:

- source-controlled
- production-ready as shipped
- copied verbatim by the installer
- treated as user-owned after installation

## Test Strategy

Add two layers of tests.

### Engine Unit Tests

Cover:

- config parsing and validation
- alias resolution
- unknown profile handling
- native tool allowlisting
- MCP exact-match allowlisting
- `Agent` spawn authorization
- `bypassPermissions` validation
- command category token-prefix matching
- structural restriction enforcement
- safety guard evaluation

### Policy Contract Tests

Load `default-rules.toml` and assert representative allow/deny behavior for:

- `main`
- `explore`
- `worker`
- `reviewer`
- `plan`
- `git`
- `notes`
- `build-runner`
- `docker`
- `browser`
- `_default`

These tests should lock down the shipped default policy as product behavior, not just engine mechanics.

## Local Integration Follow-Up

After the engine and tests are complete, update local integration points before activation:

- [settings.json](/Users/Frank.vanEldijk/.claude/settings.json)
- [explore.md](/Users/Frank.vanEldijk/.claude/agents/explore.md)
- [worker.md](/Users/Frank.vanEldijk/.claude/agents/worker.md)
- [reviewer.md](/Users/Frank.vanEldijk/.claude/agents/reviewer.md)
- [build-runner.md](/Users/Frank.vanEldijk/.claude/agents/build-runner.md)
- [notes.md](/Users/Frank.vanEldijk/.claude/agents/notes.md)

The engine can be implemented before these files are cleaned up, but local activation should wait until prompts and settings match enforcement.

## Recommended Implementation Sequence

1. Implement config schema and loader.
2. Build the engine skeleton with profile and tool gating.
3. Add `Agent` spawn validation.
4. Add `Bash` command category matching.
5. Add structural checks.
6. Add safety guard evaluation.
7. Add the shipped default policy file and policy tests.
8. Update settings and agent markdown files.
