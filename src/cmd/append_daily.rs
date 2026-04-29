//! Implementation of the `append-daily` subcommand.
//!
//! Mirrors Python `cmd_append_daily` in query.py.
//!
//! Queries today's session stats from SQLite (or falls back to JSONL) and
//! appends a Markdown section to the daily note at:
//!   `<vault>/Journal/Daily notes/<YYYY-MM-DD>.md`
//!
//! The appended format exactly mirrors Python:
//!
//! ```markdown
//!
//! ## Claude Code Session Metrics (HH:MM)
//!
//! | Metric | Value |
//! |--------|-------|
//! | Sessions | N |
//! | Tool calls | N |
//! | Failures | N |
//! | Prompts | N |
//! | Subagents | N |
//! ```
//!
//! If the daily note does not exist, it is created with a `# YYYY-MM-DD` heading
//! prepended (mirrors Python).

use std::fs;
use std::path::PathBuf;

use chrono::Local;

use crate::cli::AppendDailyArgs;
use crate::dbh::open_db;
use crate::paths::vault_path;

/// Resolve the vault root: --vault flag > default vault_path().
fn resolve_vault(args: &AppendDailyArgs) -> PathBuf {
    match &args.vault {
        Some(v) => PathBuf::from(v),
        None => vault_path(),
    }
}

/// Build the Markdown content to append.
///
/// Mirrors Python's `content` string (note: leading newline before `##`).
fn build_content(
    now_str: &str,
    sessions: i64,
    tool_calls: i64,
    failures: i64,
    prompts: i64,
    subagents: i64,
) -> String {
    format!(
        "\n## Claude Code Session Metrics ({now_str})\n\n\
| Metric | Value |\n\
|--------|-------|\n\
| Sessions | {sessions} |\n\
| Tool calls | {tool_calls} |\n\
| Failures | {failures} |\n\
| Prompts | {prompts} |\n\
| Subagents | {subagents} |\n"
    )
}

pub fn append_daily(args: &AppendDailyArgs) -> anyhow::Result<()> {
    let vault = resolve_vault(args);
    let today_str = Local::now().format("%Y-%m-%d").to_string();
    let now_str = Local::now().format("%H:%M").to_string();

    // Python: daily_note = vault / "Journal" / "Daily notes" / f"{today_str}.md"
    let daily_note = vault
        .join("Journal")
        .join("Daily notes")
        .join(format!("{}.md", today_str));

    // Collect stats — try SQLite first (mirrors Python: use DB data if available).
    let (sessions, tool_calls, failures, prompts, subagents) =
        query_today_stats(&today_str).unwrap_or((0, 0, 0, 0, 0));

    let content = build_content(&now_str, sessions, tool_calls, failures, prompts, subagents);

    if daily_note.exists() {
        // Append to existing note.
        let mut file = fs::OpenOptions::new().append(true).open(&daily_note)?;
        use std::io::Write;
        file.write_all(content.as_bytes())?;
        println!("Appended to: {}", daily_note.display());
    } else {
        // Create new note with heading.
        if let Some(parent) = daily_note.parent() {
            fs::create_dir_all(parent)?;
        }
        let full = format!("# {}\n{}", today_str, content);
        fs::write(&daily_note, &full)?;
        println!("Created: {}", daily_note.display());
    }

    Ok(())
}

/// Query today's aggregated session stats from SQLite.
///
/// Returns `(sessions, tool_calls, failures, prompts, subagents)`.
/// Falls back to `(0, 0, 0, 0, 0)` on any error.
fn query_today_stats(today_str: &str) -> anyhow::Result<(i64, i64, i64, i64, i64)> {
    let conn = open_db()?;

    struct Stats {
        sessions: Option<i64>,
        tool_calls: Option<i64>,
        failures: Option<i64>,
        prompts: Option<i64>,
        subagents: Option<i64>,
    }

    let stats = conn.query_row(
        "SELECT COUNT(*) AS sessions,
                SUM(total_tool_calls) AS tool_calls,
                SUM(total_failures) AS failures,
                SUM(total_prompts) AS prompts,
                SUM(total_subagents) AS subagents
         FROM sessions WHERE date(started_at) = ?1",
        rusqlite::params![today_str],
        |row| {
            Ok(Stats {
                sessions: row.get(0)?,
                tool_calls: row.get(1)?,
                failures: row.get(2)?,
                prompts: row.get(3)?,
                subagents: row.get(4)?,
            })
        },
    )?;

    Ok((
        stats.sessions.unwrap_or(0),
        stats.tool_calls.unwrap_or(0),
        stats.failures.unwrap_or(0),
        stats.prompts.unwrap_or(0),
        stats.subagents.unwrap_or(0),
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn build_content_format_matches_python() {
        let content = build_content("14:30", 3, 42, 1, 7, 2);
        assert!(content.starts_with('\n'), "should start with newline");
        assert!(content.contains("## Claude Code Session Metrics (14:30)"));
        assert!(content.contains("| Sessions | 3 |"));
        assert!(content.contains("| Tool calls | 42 |"));
        assert!(content.contains("| Failures | 1 |"));
        assert!(content.contains("| Prompts | 7 |"));
        assert!(content.contains("| Subagents | 2 |"));
        assert!(content.contains("|--------|-------|"));
    }

    #[test]
    fn append_daily_creates_new_file_when_missing() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            // Use the tempdir as the vault root.
            let vault = tmp.path().join("vault");
            let args = AppendDailyArgs {
                vault: Some(vault.display().to_string()),
            };

            let result = append_daily(&args);
            assert!(result.is_ok(), "append_daily should succeed: {:?}", result);

            let today_str = Local::now().format("%Y-%m-%d").to_string();
            let daily_note = vault
                .join("Journal")
                .join("Daily notes")
                .join(format!("{}.md", today_str));

            assert!(daily_note.exists(), "daily note should be created");
            let content = fs::read_to_string(&daily_note).expect("read daily note");
            assert!(
                content.starts_with(&format!("# {}", today_str)),
                "new note should start with heading"
            );
            assert!(content.contains("## Claude Code Session Metrics"));
        });
    }

    #[test]
    fn append_daily_appends_to_existing_file() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            let vault = tmp.path().join("vault");
            let today_str = Local::now().format("%Y-%m-%d").to_string();
            let daily_dir = vault.join("Journal").join("Daily notes");
            fs::create_dir_all(&daily_dir).expect("create dirs");
            let note_path = daily_dir.join(format!("{}.md", today_str));
            fs::write(&note_path, "# existing content\n").expect("write existing note");

            let args = AppendDailyArgs {
                vault: Some(vault.display().to_string()),
            };

            let result = append_daily(&args);
            assert!(result.is_ok(), "append_daily should succeed: {:?}", result);

            let content = fs::read_to_string(&note_path).expect("read note");
            assert!(
                content.starts_with("# existing content"),
                "existing content should be preserved"
            );
            assert!(
                content.contains("## Claude Code Session Metrics"),
                "metrics section should be appended"
            );
        });
    }

    #[test]
    fn resolve_vault_uses_arg_when_provided() {
        let args = AppendDailyArgs {
            vault: Some("/custom/vault".to_string()),
        };
        let vault = resolve_vault(&args);
        assert_eq!(vault, PathBuf::from("/custom/vault"));
    }

    #[test]
    fn resolve_vault_uses_default_when_none() {
        let tmp = tempdir().expect("tempdir");
        crate::test_utils::with_fake_home(tmp.path(), || {
            let args = AppendDailyArgs { vault: None };
            let vault = resolve_vault(&args);
            assert!(vault.starts_with(tmp.path()));
            assert!(vault.ends_with("Sync/Obsidian/Second Brain"));
        });
    }
}
