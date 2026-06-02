use std::path::PathBuf;
use std::process::Command;

#[test]
fn agent_registry_is_consistent() {
    let home = std::env::var("HOME").expect("HOME should be set");
    let script = PathBuf::from(home).join(".claude/tools/agent-registry/validate.sh");

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
