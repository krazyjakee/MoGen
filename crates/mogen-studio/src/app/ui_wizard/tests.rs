//! Unit tests for the batch claim/reservation helpers that keep the
//! auto-continue poll from double-issuing in-flight worker ids.

use super::*;
use std::path::PathBuf;

fn obj(id: &str) -> ObjectEntry {
    ObjectEntry {
        id: id.to_string(),
        name: id.to_string(),
        role: String::new(),
        prompt: String::new(),
        size: [1.0, 1.0, 1.0],
        position: [0.0, 0.0, 0.0],
        rotation_y_deg: 0.0,
        reference_image: None,
        mog_path: None,
        thumb_path: None,
        position_guide: None,
    }
}

// RAII guard: removes the temp file on drop, even if the test panics.
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn existing_file() -> TempFile {
    let dir = std::env::temp_dir().join("mogen-studio-batch-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!(
        "{}-{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, b"x").unwrap();
    TempFile(path)
}

// Regression: batch was selected without reservation, so the 150ms poll re-issued the same ids.
#[test]
fn second_claim_before_completion_is_empty() {
    let manifest = vec![obj("a"), obj("b"), obj("c")];
    let mut in_flight = HashSet::new();

    let first = claim_pending_batch(&manifest, &mut in_flight, 3, reference_missing);
    assert_eq!(first.len(), 3, "first claim takes the whole batch");
    assert_eq!(in_flight.len(), 3, "claimed ids are reserved");

    let second = claim_pending_batch(&manifest, &mut in_flight, 3, reference_missing);
    assert!(
        second.is_empty(),
        "nothing left to claim while the first batch is still in flight"
    );
    assert_eq!(in_flight.len(), 3, "no double-issue");
}

// poll_wizard checks next_pending_reference_skipping; once claimed, that probe must return None.
#[test]
fn poll_gate_sees_no_pending_after_claim() {
    let mut state = WizardState::default();
    state.manifest = vec![obj("a"), obj("b"), obj("c")];
    let mut in_flight = HashSet::new();

    assert!(
        next_pending_reference_skipping(&state, &in_flight).is_some(),
        "work exists before any claim"
    );
    let _ = claim_pending_batch(&state.manifest, &mut in_flight, 3, reference_missing);
    assert!(
        next_pending_reference_skipping(&state, &in_flight).is_none(),
        "poll gate must report no pending work once the batch is reserved"
    );
}

#[test]
fn claim_never_exceeds_target_concurrency() {
    let manifest: Vec<_> = (0..10).map(|i| obj(&format!("o{i}"))).collect();
    let mut in_flight = HashSet::new();
    let claimed = claim_pending_batch(&manifest, &mut in_flight, 3, reference_missing);
    assert_eq!(claimed.len(), 3, "capacity caps the batch at target_concurrency");
    assert_eq!(in_flight.len(), 3);
}

#[test]
fn capacity_accounts_for_already_in_flight() {
    let manifest: Vec<_> = (0..10).map(|i| obj(&format!("o{i}"))).collect();
    let mut in_flight = HashSet::new();
    in_flight.insert("o0".to_string());
    in_flight.insert("o1".to_string());
    let claimed = claim_pending_batch(&manifest, &mut in_flight, 3, reference_missing);
    assert_eq!(claimed.len(), 1, "only one free slot remains under a target of 3");
    assert_eq!(in_flight.len(), 3, "o0, o1 pre-existing + o2 newly claimed");
}

// ReferenceDone removes the id; a still-missing object becomes claimable again.
#[test]
fn completed_id_can_be_reclaimed() {
    let manifest = vec![obj("a")];
    let mut in_flight = HashSet::new();
    let first = claim_pending_batch(&manifest, &mut in_flight, 3, reference_missing);
    assert_eq!(first.len(), 1);
    in_flight.remove("a");
    let again = claim_pending_batch(&manifest, &mut in_flight, 3, reference_missing);
    assert_eq!(again.len(), 1, "a freed, still-missing object is reclaimable");
}

#[test]
fn objects_with_existing_reference_are_not_claimed() {
    let _guard = existing_file();
    let mut done = obj("done");
    done.reference_image = Some(_guard.0.clone());
    let manifest = vec![done, obj("todo")];
    let mut in_flight = HashSet::new();
    let claimed = claim_pending_batch(&manifest, &mut in_flight, 3, reference_missing);
    assert_eq!(claimed.len(), 1, "only the object missing its PNG is claimed");
    assert_eq!(claimed[0].id, "todo");
}

#[test]
fn objects_with_existing_mog_are_not_claimed() {
    let _guard = existing_file();
    let mut built = obj("built");
    built.mog_path = Some(_guard.0.clone());
    let manifest = vec![built, obj("todo")];
    let mut in_flight = HashSet::new();
    let claimed = claim_pending_batch(&manifest, &mut in_flight, 3, object_missing);
    assert_eq!(claimed.len(), 1, "only the object missing its .mog is claimed");
    assert_eq!(claimed[0].id, "todo");
}

#[test]
fn object_missing_tracks_mog_path() {
    let _guard = existing_file();
    let mut built = obj("built");
    built.mog_path = Some(_guard.0.clone());
    assert!(!object_missing(&built), "an on-disk .mog is not missing");
    assert!(object_missing(&obj("fresh")), "no .mog means missing");
}
