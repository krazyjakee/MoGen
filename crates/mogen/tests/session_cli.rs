use std::process::Command;
#[test]
fn draft_cli_writes_machine_readable_artifacts_and_inspects_saved_state() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("responses.json");
    std::fs::write(&script, r#"["scene { box \"asset\" }"]"#).unwrap();
    let out = dir.path().join("result");
    let output = Command::new(env!("CARGO_BIN_EXE_mogen"))
        .args(["session", "--prompt", "a box", "--draft", "--script"])
        .arg(script)
        .arg("--out-dir")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "completed");
    assert!(report["final_mog"].as_str().unwrap().starts_with('/'));
    assert_eq!(report["candidates"][0]["reviewed"], false);
    let project = mogen_llm::session::ModelingProject::load(&out.join("final.mog")).unwrap();
    assert!(project.generation_response.is_some());
    assert_eq!(project.meter.calls, 1);
    let inspected = Command::new(env!("CARGO_BIN_EXE_mogen"))
        .args(["session", "--inspect", "--out-dir"])
        .arg(&out)
        .output()
        .unwrap();
    let inspected: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(report, inspected);
}
