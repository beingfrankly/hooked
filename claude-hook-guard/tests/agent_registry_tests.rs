use std::path::PathBuf;
use std::process::Command;

#[test]
fn agent_registry_is_consistent() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate should live under tools/claude-hook-guard")
        .to_path_buf();
    let script = root.join("tools/agent-registry/validate.sh");

    let output = Command::new("bash")
        .arg(&script)
        .output()
        .expect("validator should execute");

    assert!(
        output.status.success(),
        "validator failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
