use super::super::*;
use super::support::*;
use crate::GenerateConfig;
use serde_json::json;

#[test]
fn atomic_edits_preserve_locks_and_unrelated_text() {
    let lock = PartLock {
        name: "seat".into(),
        kind: LockKind::Subtree,
    };
    let mut w =
        ModelingWorkspace::new(ORIGINAL.into(), None, vec![lock], Some("arm".into())).unwrap();
    let old = w.revision().unwrap();
    let next = ORIGINAL.replace("size=[0.1,0.1,1]", "size=[0.2,0.1,1]");
    assert!(w.apply("stale", &next).is_err());
    assert_eq!(w.source, ORIGINAL);
    assert!(w
        .apply(&old, &ORIGINAL.replace("size=[1,0.2,1]", "size=[2,0.2,1]"))
        .is_err());
    assert!(w
        .apply(
            &old,
            &ORIGINAL.replace("color=[0.5,0.3,0.1]", "color=[1,0,0]")
        )
        .is_err());
    assert!(w
        .apply(
            &old,
            "<<<<<<< SEARCH\nnot present\n=======\nx\n>>>>>>> REPLACE"
        )
        .is_err());
    assert_eq!(w.source, ORIGINAL);
    w.apply(&old, &next).unwrap();
    assert_eq!(w.source, next);
    assert!(w.apply(&old, ORIGINAL).is_err());
}

#[test]
fn shared_material_and_parent_transform_cannot_bypass_locks() {
    let locks = vec![PartLock {
        name: "seat".into(),
        kind: LockKind::Subtree,
    }];
    assert!(enforce_locks(
        ORIGINAL,
        &ORIGINAL.replace("color=[0.5,0.3,0.1]", "color=[1,0,0]"),
        &locks,
        None
    )
    .is_err());
    let src = "scene { group \"frame\" { box \"seat\" (size=[1,1,1]) } }";
    assert!(enforce_locks(
        src,
        &src.replace("group \"frame\"", "group \"frame\" (pos=[0,1,0])"),
        &locks,
        None
    )
    .is_err());
}

#[test]
fn relational_driver_changes_cannot_bypass_dependent_geometry_locks() {
    let source = "scene { box \"seat\" (pos=[0,0.6,0],size=[1,0.1,1]) spline_tube \"leg\" (points=[[0,0,0],[0,0.3,0]],radius=0.025) } relate (child=\"leg\",target=\"seat\",mode=\"endpoint\",socket=\"bottom\",insertion=0.01)";
    let changed = source.replace("pos=[0,0.6,0]", "pos=[0,0.9,0]");
    let locks = vec![PartLock {
        name: "leg".into(),
        kind: LockKind::Geometry,
    }];
    let error = enforce_locks(source, &changed, &locks, None).unwrap_err();
    assert!(error.to_string().contains("locked"));
}

#[test]
fn guided_details_respect_dependent_geometry_locks() {
    let source = include_str!("../../../../../examples/furniture/guided_cushion.mog");
    let changed = source.replace("use \"cushion\" ()", "use \"cushion\" (w=1.2)");
    let locks = vec![PartLock {
        name: "welt".into(),
        kind: LockKind::Geometry,
    }];
    assert!(enforce_locks(source, &changed, &locks, None)
        .unwrap_err()
        .to_string()
        .contains("locked"));
}

#[test]
fn raw_dsl_recovery_cannot_change_locked_material_indirectly() {
    let mut workspace = ModelingWorkspace::new(
        ORIGINAL.into(),
        None,
        vec![PartLock {
            name: "seat".into(),
            kind: LockKind::Material,
        }],
        None,
    )
    .unwrap();
    let mut calls = 0;
    tool_session(
        &mut workspace,
        &GenerateConfig::new("chair"),
        "change arm finish",
        &mut |cfg| {
            calls += 1;
            if calls == 1 {
                Ok(response(ORIGINAL.replace("0.5,0.3,0.1", "0.9,0.1,0.1")))
            } else {
                assert!(cfg.user_prompt.contains("locked"));
                Ok(response(
                    json!({"tool":"finish","revision":rev(ORIGINAL),"findings":"Lock preserved"})
                        .to_string(),
                ))
            }
        },
        &mut Renderer {
            seen: vec![],
            fail: false,
        },
    )
    .unwrap();
    assert_eq!(workspace.source, ORIGINAL);
}
