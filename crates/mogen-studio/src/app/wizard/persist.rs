//! Load / save the wizard's `state.json` so a partial run survives Studio
//! restarts. All paths live under `<project>/wizard/`.

use std::fs;
use std::path::Path;

use super::state::WizardState;

pub fn state_file(project_dir: &Path) -> std::path::PathBuf {
    project_dir.join("state.json")
}

pub fn load(project_dir: &Path) -> Option<WizardState> {
    let bytes = fs::read(state_file(project_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write the wizard state to disk. Best-effort: failures are surfaced as
/// `Err(String)` but the caller usually just logs and carries on — losing
/// resume capability isn't worth blocking the active stage.
pub fn save(state: &WizardState) -> Result<(), String> {
    fs::create_dir_all(&state.project_dir)
        .map_err(|e| format!("mkdir {}: {e}", state.project_dir.display()))?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    let path = state_file(&state.project_dir);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    Ok(())
}
