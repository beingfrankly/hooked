use super::{ModeChecker, Violation};
use crate::walker::{self, CommandInfo};
use crate::lists;
use tree_sitter::Tree;

pub struct SafetyChecker {
    pub protected_branches: Vec<String>,
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn expand_short_flags(arg: &str) -> Vec<char> {
    if arg.starts_with("--") {
        return vec![]; // long flag, not expandable
    }
    if arg.starts_with('-') && arg.len() > 1 {
        arg[1..].chars().collect()
    } else {
        vec![]
    }
}

fn has_flag(args: &[String], short: char, long: &str) -> bool {
    args.iter().any(|a| a == long || expand_short_flags(a).contains(&short))
}

// ── ModeChecker impl ─────────────────────────────────────────────────────────

impl ModeChecker for SafetyChecker {
    fn check(&self, tree: &Tree, source: &[u8]) -> Option<Violation> {
        let commands = walker::collect_all_commands(tree.root_node(), source);
        for cmd in &commands {
            if let Some(v) = self.check_destructive(cmd) {
                return Some(v);
            }
        }
        None
    }
}

// ── core logic ────────────────────────────────────────────────────────────────

impl SafetyChecker {
    fn check_destructive(&self, cmd: &CommandInfo) -> Option<Violation> {
        // Always scan all args for destructive SQL patterns (every command).
        if let Some(v) = check_sql(&cmd.args) {
            return Some(v);
        }

        match cmd.name.as_str() {
            "bash" | "sh" | "zsh" => self.check_shell_exec(cmd),
            "sudo" | "env" | "command" => self.strip_prefix_command(cmd),
            "rm" => check_rm(&cmd.args),
            "git" => self.check_git(&cmd.args),
            "docker" => check_docker(&cmd.args),
            "kill" => check_kill(&cmd.args),
            "pkill" => Some(Violation::new(
                "BLOCKED: pkill. Use kill with a specific PID instead.",
            )),
            _ => None,
        }
    }

    // bash/sh/zsh -c "...": parse and recursively check the inner command string.
    fn check_shell_exec(&self, cmd: &CommandInfo) -> Option<Violation> {
        // Find -c flag and the command string after it.
        let mut iter = cmd.args.iter();
        let inner_cmd = loop {
            match iter.next() {
                None => return None,
                Some(a) if a == "-c" => {
                    match iter.next() {
                        Some(inner) => break inner.clone(),
                        None => return None,
                    }
                }
                _ => continue,
            }
        };

        // Strip surrounding quotes if present.
        let inner = inner_cmd.trim_matches(|c| c == '\'' || c == '"');
        if inner.is_empty() {
            return None;
        }

        // Parse and check the inner command.
        let tree = crate::parser::parse_bash(inner)?;
        let inner_commands = crate::walker::collect_all_commands(tree.root_node(), inner.as_bytes());
        for inner_cmd in &inner_commands {
            if let Some(v) = self.check_destructive(inner_cmd) {
                return Some(v);
            }
        }
        None
    }

    // sudo/env/command: strip prefix and any leading flags/env-vars, then re-check the inner command.
    fn strip_prefix_command(&self, cmd: &CommandInfo) -> Option<Violation> {
        // Find first non-flag, non-env-var arg — that is the real command name.
        let mut iter = cmd.args.iter();
        let inner_name = loop {
            match iter.next() {
                None => return None,
                Some(a) if a.starts_with('-') => continue, // skip flags
                Some(a) if a.contains('=') => continue,    // skip KEY=VALUE env vars
                Some(a) => break a.clone(),
            }
        };
        let inner_args: Vec<String> = iter.cloned().collect();
        let inner = CommandInfo {
            name: inner_name,
            args: inner_args,
        };
        self.check_destructive(&inner)
    }

    // git subcommand dispatch.
    fn check_git(&self, args: &[String]) -> Option<Violation> {
        // First non-flag arg is the git subcommand.
        let subcommand = args.iter().find(|a| !a.starts_with('-'))?;
        match subcommand.as_str() {
            "push" => self.check_git_push(args),
            "reset" => check_git_reset(args),
            "clean" => check_git_clean(args),
            _ => None,
        }
    }

    fn check_git_push(&self, args: &[String]) -> Option<Violation> {
        // Check --delete first.
        if args.iter().any(|a| a == "--delete") {
            return Some(Violation::new(
                "BLOCKED: git push --delete. Ask the user to confirm branch deletion.",
            ));
        }

        // Check force push: --force or short -f, but NOT if --force-with-lease is present.
        let has_force_with_lease = args.iter().any(|a| a == "--force-with-lease");
        if !has_force_with_lease {
            let force = args.iter().any(|a| {
                a == "--force" || expand_short_flags(a).contains(&'f')
            });
            if force {
                return Some(Violation::new(
                    "BLOCKED: git push --force. Use --force-with-lease, or ask the user to confirm.",
                ));
            }
        }

        // Check push to protected branch.
        // Strip flags and the "push" subcommand word, then strip the remote
        // (first non-flag arg after "push"), remainder may contain branch names.
        let positional: Vec<&String> = args
            .iter()
            .filter(|a| !a.starts_with('-'))
            .collect();
        // positional[0] == "push" (the subcommand), positional[1] == remote (if present),
        // positional[2..] == branch refspecs.
        let branch_args = if positional.len() > 2 {
            &positional[2..]
        } else {
            &[]
        };

        // If no explicit branch is specified, block to avoid accidental pushes.
        if branch_args.is_empty() {
            return Some(Violation::new(
                "BLOCKED: git push without an explicit branch. Specify the branch explicitly (e.g. git push origin feature-branch) to avoid accidentally pushing to a protected branch.",
            ));
        }

        for arg in branch_args {
            // Refspecs can be "local:remote" — check both sides.
            let parts: Vec<&str> = arg.splitn(2, ':').collect();
            for part in parts {
                let branch = part.trim_start_matches('+'); // strip leading +
                if self.protected_branches.iter().any(|b| b == branch) {
                    return Some(Violation::new(format!(
                        "BLOCKED: git push to protected branch '{branch}'. Push to a feature branch instead.",
                    )));
                }
            }
        }

        None
    }
}

// ── free functions for individual checks ─────────────────────────────────────

fn check_rm(args: &[String]) -> Option<Violation> {
    let has_recursive = args.iter().any(|a| {
        a == "--recursive" || expand_short_flags(a).contains(&'r')
    });
    let has_force = args.iter().any(|a| {
        a == "--force" || expand_short_flags(a).contains(&'f')
    });
    if has_recursive && has_force {
        return Some(Violation::new(
            "BLOCKED: rm with recursive+force flags. Delete specific files instead, or ask the user to confirm.",
        ));
    }
    None
}

fn check_git_reset(args: &[String]) -> Option<Violation> {
    if has_flag(args, '\0', "--hard") {
        return Some(Violation::new(
            "BLOCKED: git reset --hard. Use git stash or ask the user to confirm.",
        ));
    }
    None
}

fn check_git_clean(args: &[String]) -> Option<Violation> {
    let force = has_flag(args, 'f', "--force");
    if force {
        return Some(Violation::new(
            "BLOCKED: git clean -f. Ask the user to confirm.",
        ));
    }
    None
}

fn check_docker(args: &[String]) -> Option<Violation> {
    // Collect positional (non-flag) subcommand words.
    let positional: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(|a| a.as_str())
        .collect();

    match positional.as_slice() {
        ["system", "prune", ..] => Some(Violation::new(
            "BLOCKED: docker system prune. Ask the user to confirm.",
        )),
        ["rm", ..] => Some(Violation::new(
            "BLOCKED: docker rm. Use docker stop / docker-compose down.",
        )),
        ["rmi", ..] => Some(Violation::new(
            "BLOCKED: docker rmi. Ask the user to confirm.",
        )),
        ["volume", "rm", ..] => Some(Violation::new(
            "BLOCKED: docker volume rm. Ask the user to confirm.",
        )),
        ["network", "rm", ..] => Some(Violation::new(
            "BLOCKED: docker network rm. Ask the user to confirm.",
        )),
        _ => None,
    }
}

fn check_kill(args: &[String]) -> Option<Violation> {
    // Direct signal flags: -9, -SIGKILL, -KILL
    let direct = args.iter().any(|a| {
        matches!(a.as_str(), "-9" | "-SIGKILL" | "-KILL")
    });
    if direct {
        return Some(Violation::new(
            "BLOCKED: kill -9 (SIGKILL). Use kill -15 (SIGTERM) for graceful shutdown.",
        ));
    }

    // -s <signal> style
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "-s" {
            if let Some(sig) = iter.peek() {
                if matches!(sig.as_str(), "9" | "SIGKILL" | "KILL") {
                    return Some(Violation::new(
                        "BLOCKED: kill -9 (SIGKILL). Use kill -15 (SIGTERM) for graceful shutdown.",
                    ));
                }
            }
        }
    }

    None
}

fn check_sql(args: &[String]) -> Option<Violation> {
    if args.is_empty() {
        return None;
    }
    let joined = args.join(" ").to_uppercase();
    for pattern in lists::DESTRUCTIVE_SQL_PATTERNS {
        if joined.contains(*pattern) {
            return Some(Violation::new(
                "BLOCKED: Destructive SQL detected. Ask the user to confirm destructive SQL.",
            ));
        }
    }
    None
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn checker() -> SafetyChecker {
        SafetyChecker {
            protected_branches: lists::DEFAULT_PROTECTED_BRANCHES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }

    fn cmd(name: &str, args: &[&str]) -> CommandInfo {
        CommandInfo {
            name: name.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    // rm tests
    #[test]
    fn rm_rf_combined_blocked() {
        assert!(checker().check_destructive(&cmd("rm", &["-rf", "/tmp/x"])).is_some());
    }

    #[test]
    fn rm_fr_combined_blocked() {
        assert!(checker().check_destructive(&cmd("rm", &["-fr", "/tmp/x"])).is_some());
    }

    #[test]
    fn rm_separate_flags_blocked() {
        assert!(checker().check_destructive(&cmd("rm", &["-r", "-f", "dir"])).is_some());
    }

    #[test]
    fn rm_long_flags_blocked() {
        assert!(checker()
            .check_destructive(&cmd("rm", &["--recursive", "--force", "dir"]))
            .is_some());
    }

    #[test]
    fn rm_file_only_allowed() {
        assert!(checker().check_destructive(&cmd("rm", &["file.txt"])).is_none());
    }

    #[test]
    fn rm_recursive_no_force_allowed() {
        assert!(checker().check_destructive(&cmd("rm", &["-r", "dir"])).is_none());
    }

    // git push tests
    #[test]
    fn git_push_force_blocked() {
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "--force"]))
            .is_some());
    }

    #[test]
    fn git_push_short_f_blocked() {
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "-f"]))
            .is_some());
    }

    #[test]
    fn git_push_force_with_lease_blocked_no_branch() {
        // --force-with-lease without explicit branch is still blocked (no branch specified).
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "--force-with-lease"]))
            .is_some());
    }

    #[test]
    fn git_push_force_with_lease_allowed() {
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "--force-with-lease", "origin", "feature-branch"]))
            .is_none());
    }

    #[test]
    fn git_push_no_args_blocked() {
        assert!(checker()
            .check_destructive(&cmd("git", &["push"]))
            .is_some());
    }

    #[test]
    fn git_push_remote_only_blocked() {
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "origin"]))
            .is_some());
    }

    #[test]
    fn git_push_delete_blocked() {
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "--delete", "origin", "old-branch"]))
            .is_some());
    }

    #[test]
    fn git_push_protected_main_blocked() {
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "origin", "main"]))
            .is_some());
    }

    #[test]
    fn git_push_feature_branch_allowed() {
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "origin", "feature/my-thing"]))
            .is_none());
    }

    #[test]
    fn git_push_refspec_protected_remote_blocked() {
        // HEAD:main — remote side is protected
        assert!(checker()
            .check_destructive(&cmd("git", &["push", "origin", "HEAD:main"]))
            .is_some());
    }

    // git reset tests
    #[test]
    fn git_reset_hard_blocked() {
        assert!(checker()
            .check_destructive(&cmd("git", &["reset", "--hard"]))
            .is_some());
    }

    #[test]
    fn git_reset_soft_allowed() {
        assert!(checker()
            .check_destructive(&cmd("git", &["reset", "--soft", "HEAD~1"]))
            .is_none());
    }

    // git clean tests
    #[test]
    fn git_clean_f_blocked() {
        assert!(checker()
            .check_destructive(&cmd("git", &["clean", "-f"]))
            .is_some());
    }

    #[test]
    fn git_clean_no_force_allowed() {
        assert!(checker()
            .check_destructive(&cmd("git", &["clean", "-n"]))
            .is_none());
    }

    // docker tests
    #[test]
    fn docker_system_prune_blocked() {
        assert!(checker()
            .check_destructive(&cmd("docker", &["system", "prune"]))
            .is_some());
    }

    #[test]
    fn docker_rm_blocked() {
        assert!(checker()
            .check_destructive(&cmd("docker", &["rm", "my-container"]))
            .is_some());
    }

    #[test]
    fn docker_rmi_blocked() {
        assert!(checker()
            .check_destructive(&cmd("docker", &["rmi", "my-image"]))
            .is_some());
    }

    #[test]
    fn docker_volume_rm_blocked() {
        assert!(checker()
            .check_destructive(&cmd("docker", &["volume", "rm", "my-vol"]))
            .is_some());
    }

    #[test]
    fn docker_network_rm_blocked() {
        assert!(checker()
            .check_destructive(&cmd("docker", &["network", "rm", "my-net"]))
            .is_some());
    }

    #[test]
    fn docker_ps_allowed() {
        assert!(checker()
            .check_destructive(&cmd("docker", &["ps"]))
            .is_none());
    }

    // kill tests
    #[test]
    fn kill_9_blocked() {
        assert!(checker()
            .check_destructive(&cmd("kill", &["-9", "1234"]))
            .is_some());
    }

    #[test]
    fn kill_sigkill_blocked() {
        assert!(checker()
            .check_destructive(&cmd("kill", &["-SIGKILL", "1234"]))
            .is_some());
    }

    #[test]
    fn kill_s_9_blocked() {
        assert!(checker()
            .check_destructive(&cmd("kill", &["-s", "9", "1234"]))
            .is_some());
    }

    #[test]
    fn kill_15_allowed() {
        assert!(checker()
            .check_destructive(&cmd("kill", &["-15", "1234"]))
            .is_none());
    }

    #[test]
    fn kill_term_allowed() {
        assert!(checker()
            .check_destructive(&cmd("kill", &["-TERM", "1234"]))
            .is_none());
    }

    // pkill tests
    #[test]
    fn pkill_blocked() {
        assert!(checker()
            .check_destructive(&cmd("pkill", &["myapp"]))
            .is_some());
    }

    // SQL tests
    #[test]
    fn sql_drop_table_blocked() {
        assert!(checker()
            .check_destructive(&cmd("mysql", &["-e", "DROP TABLE users"]))
            .is_some());
    }

    #[test]
    fn sql_truncate_table_blocked() {
        assert!(checker()
            .check_destructive(&cmd("psql", &["-c", "TRUNCATE TABLE logs"]))
            .is_some());
    }

    #[test]
    fn sql_delete_from_blocked() {
        assert!(checker()
            .check_destructive(&cmd("psql", &["-c", "delete from sessions"]))
            .is_some());
    }

    #[test]
    fn sql_select_allowed() {
        assert!(checker()
            .check_destructive(&cmd("psql", &["-c", "SELECT * FROM users"]))
            .is_none());
    }

    // sudo wrapping
    #[test]
    fn sudo_rm_rf_blocked() {
        assert!(checker()
            .check_destructive(&cmd("sudo", &["rm", "-rf", "/tmp"]))
            .is_some());
    }

    #[test]
    fn sudo_rm_file_allowed() {
        assert!(checker()
            .check_destructive(&cmd("sudo", &["rm", "file.txt"]))
            .is_none());
    }

    // env wrapping
    #[test]
    fn env_rm_rf_blocked() {
        assert!(checker()
            .check_destructive(&cmd("env", &["rm", "-rf", "/tmp"]))
            .is_some());
    }

    #[test]
    fn env_with_var_rm_rf_blocked() {
        assert!(checker()
            .check_destructive(&cmd("env", &["FOO=bar", "rm", "-rf", "/tmp"]))
            .is_some());
    }

    #[test]
    fn command_rm_rf_blocked() {
        assert!(checker()
            .check_destructive(&cmd("command", &["rm", "-rf", "/tmp"]))
            .is_some());
    }

    // bash -c bypass
    #[test]
    fn bash_c_rm_rf_blocked() {
        assert!(checker()
            .check_destructive(&cmd("bash", &["-c", "rm -rf /"]))
            .is_some());
    }

    #[test]
    fn sh_c_rm_rf_blocked() {
        assert!(checker()
            .check_destructive(&cmd("sh", &["-c", "rm -rf /"]))
            .is_some());
    }

    #[test]
    fn zsh_c_rm_rf_blocked() {
        assert!(checker()
            .check_destructive(&cmd("zsh", &["-c", "rm -rf /"]))
            .is_some());
    }

    #[test]
    fn bash_script_allowed() {
        // bash without -c (running a script) should be allowed.
        assert!(checker()
            .check_destructive(&cmd("bash", &["script.sh"]))
            .is_none());
    }

    #[test]
    fn bash_c_safe_cmd_allowed() {
        assert!(checker()
            .check_destructive(&cmd("bash", &["-c", "echo hello"]))
            .is_none());
    }
}
