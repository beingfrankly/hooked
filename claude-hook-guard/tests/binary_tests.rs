use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn run_guard(args: &[&str], input: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-hook-guard"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn binary");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn default_config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("default-rules.toml")
}

#[test]
fn binary_denies_missing_config() {
    let (code, stdout, _) = run_guard(
        &["--config", "/definitely/missing/rules.toml"],
        r#"{"tool_name":"Read","tool_input":{}}"#,
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("Missing config"));
}

#[test]
fn binary_uses_config_to_deny_main_glob() {
    let path = default_config_path();
    let path_str = path.to_str().unwrap();
    let (code, stdout, _) = run_guard(
        &["--config", path_str],
        r#"{"tool_name":"Glob","agent_type":"main","tool_input":{}}"#,
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("Delegate Glob"));
}
