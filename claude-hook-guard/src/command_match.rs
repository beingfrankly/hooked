use crate::walker::CommandInfo;
use std::path::Path;

pub fn normalize_command(cmd: &CommandInfo) -> Vec<String> {
    let mut tokens = Vec::with_capacity(1 + cmd.args.len());
    let name = normalize_name(&cmd.name);
    tokens.push(name);
    tokens.extend(cmd.args.iter().cloned());
    normalize_known_patterns(&tokens)
}

pub fn matches_prefix(command: &[String], prefix: &[String]) -> bool {
    command.len() >= prefix.len() && command.iter().zip(prefix.iter()).all(|(a, b)| a == b)
}

fn normalize_name(name: &str) -> String {
    if name.contains('/') {
        Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(name)
            .to_string()
    } else {
        name.to_string()
    }
}

fn normalize_known_patterns(tokens: &[String]) -> Vec<String> {
    // Strip `-C <dir>` / `--directory <dir>` for git AND bd so an allowed
    // subcommand targeting another workspace still matches the allowlist.
    if matches!(tokens.first().map(|s| s.as_str()), Some("git" | "bd")) {
        let head = tokens[0].clone();
        let mut out = vec![head];
        let mut i = 1;
        while i < tokens.len() {
            if tokens[i] == "-C" || tokens[i] == "--directory" {
                i += 2;
                continue;
            }
            out.extend(tokens[i..].iter().cloned());
            return out;
        }
        return out;
    }

    if matches!(tokens.first().map(|s| s.as_str()), Some("mvn" | "mvnw")) {
        return normalize_maven(tokens);
    }

    tokens.to_vec()
}

fn normalize_maven(tokens: &[String]) -> Vec<String> {
    let mut out = vec![tokens[0].clone()];
    let mut i = 1;

    while i < tokens.len() {
        let token = &tokens[i];

        if token.starts_with('-') {
            if maven_flag_takes_value(token) && i + 1 < tokens.len() {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        out.push(token.clone());
        out.extend(tokens[i + 1..].iter().cloned());
        return out;
    }

    out
}

fn maven_flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-f" | "--file"
            | "-s"
            | "--settings"
            | "-gs"
            | "--global-settings"
            | "-t"
            | "--toolchains"
            | "-P"
            | "--activate-profiles"
            | "-pl"
            | "--projects"
            | "-rf"
            | "--resume-from"
    )
}

pub fn is_banned_wrapper(command: &[String]) -> bool {
    matches!(
        command.first().map(|s| s.as_str()),
        Some("bash" | "sh" | "zsh" | "sudo" | "env" | "command")
    )
}

#[cfg(test)]
mod tests {
    use super::normalize_known_patterns;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bd_strips_dash_c() {
        assert_eq!(
            normalize_known_patterns(&v(&["bd", "-C", "/some/dir", "create"])),
            v(&["bd", "create"])
        );
    }

    #[test]
    fn bd_strips_directory_long_form() {
        assert_eq!(
            normalize_known_patterns(&v(&["bd", "--directory", "/some/dir", "update", "x"])),
            v(&["bd", "update", "x"])
        );
    }

    #[test]
    fn git_strips_dash_c_regression() {
        assert_eq!(
            normalize_known_patterns(&v(&["git", "-C", "/repo", "status"])),
            v(&["git", "status"])
        );
    }

    #[test]
    fn bd_no_flag_unchanged() {
        assert_eq!(
            normalize_known_patterns(&v(&["bd", "create", "title"])),
            v(&["bd", "create", "title"])
        );
    }
}
