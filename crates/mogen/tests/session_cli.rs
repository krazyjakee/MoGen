use std::process::Command;

#[test]
fn resume_and_inspect_require_an_existing_session() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("responses.json");
    std::fs::write(&script, r#"["scene { box \"unexpected\" }"]"#).unwrap();
    for existing in [false, true] {
        for mode in ["--resume", "--inspect"] {
            let out = dir.path().join(format!("{existing}-{mode}"));
            if existing {
                std::fs::create_dir(&out).unwrap();
                std::fs::write(out.join("final.mog"), "scene { box \"original\" }").unwrap();
            }
            let output = Command::new(env!("CARGO_BIN_EXE_mogen"))
                .args(["session", mode, "--script"])
                .arg(&script)
                .arg("--out-dir")
                .arg(&out)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("No saved modeling session"));
            assert!(!mogen_llm::session::ModelingProject::sidecar(&out.join("final.mog")).exists());
            if existing {
                assert_eq!(
                    std::fs::read_to_string(out.join("final.mog")).unwrap(),
                    "scene { box \"original\" }"
                );
                assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
            } else {
                assert!(!out.exists());
            }
        }
    }
}

#[test]
fn input_is_saved_before_render_preflight_and_restored_without_generation() {
    use mogen_llm::session::{ModelingProject, QualityMode};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mog");
    let source = "scene { box \"supplied\" }";
    std::fs::write(&input, source).unwrap();
    let out = dir.path().join("result");
    let output = Command::new(env!("CARGO_BIN_EXE_mogen"))
        .args([
            "session",
            "--prompt",
            "refine this box",
            "--provider",
            "ollama",
            "--input",
        ])
        .arg(&input)
        .arg("--out-dir")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("render_unavailable"));
    let entry = out.join("final.mog");
    let mut project = ModelingProject::load(&entry).unwrap();
    assert_eq!(project.input_source.as_deref(), Some(source));
    assert_eq!(project.meter.calls, 0);
    assert!(project.session_initial.is_none());
    assert!(project.generation_response.is_none());
    // Exercise CLI source recovery without requiring a display in this test.
    project.mode = QualityMode::Draft;
    project.save(&entry).unwrap();
    let script = dir.path().join("empty.json");
    std::fs::write(&script, "[]").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mogen"))
        .args(["session", "--resume", "--provider", "ollama", "--script"])
        .arg(&script)
        .arg("--out-dir")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(&entry).unwrap(), source);
    let project = ModelingProject::load(&entry).unwrap();
    assert_eq!(project.meter.calls, 0);
    assert!(project.generation_response.is_none());
    assert_eq!(
        project.candidates[project.selected_candidate.unwrap()].source,
        source
    );
}

#[test]
fn legacy_project_resume_cannot_start_generation() {
    use mogen_llm::session::ModelingProject;
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("final.mog");
    ModelingProject::default().save(&entry).unwrap();
    let saved = std::fs::read(ModelingProject::sidecar(&entry)).unwrap();
    let script = dir.path().join("responses.json");
    std::fs::write(&script, r#"["scene { box \"unexpected\" }"]"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_mogen"))
        .args(["session", "--resume", "--script"])
        .arg(&script)
        .arg("--out-dir")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no resumable execution"));
    assert!(!entry.exists());
    assert_eq!(
        std::fs::read(ModelingProject::sidecar(&entry)).unwrap(),
        saved
    );
}

#[test]
fn input_dependencies_cannot_be_overwritten_by_session_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mog");
    std::fs::write(&input, "import \"report.json\"\nscene { use \"part\" }").unwrap();
    let dependency = "module \"part\" { box \"supplied\" }";
    std::fs::write(dir.path().join("report.json"), dependency).unwrap();
    let out = dir.path().join("result");
    let output = Command::new(env!("CARGO_BIN_EXE_mogen"))
        .args(["session", "--prompt", "refine", "--input"])
        .arg(&input)
        .arg("--out-dir")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("conflicts with session output artifacts")
    );
    assert!(!out.join("report.json").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("report.json")).unwrap(),
        dependency
    );
}

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
