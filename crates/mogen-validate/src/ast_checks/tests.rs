//! Validator tests live alongside the schema/walking code so they exercise
//! the same private helpers (`has_unknown_attr`, `diags_for_*`) without
//! widening visibility just for the test surface.

mod import_validator_tests {
    use super::super::*;
    use mogen_core::Diagnostic;
    use std::path::Path;

    fn diags_for_with_source(src: &str, base: Option<&Path>) -> Vec<Diagnostic> {
        let ast = mogen_dsl::parse(src).expect("parse");
        validate_ast_with_source(&ast, base)
    }

    fn write_tmp(label: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "mogen-validate-imports-{}-{}-{}",
            std::process::id(),
            id,
            label
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        dir
    }

    #[test]
    fn import_node_does_not_trigger_unknown_kind() {
        // Without a source dir we still parse `import`; the validator must
        // not flag it as an unknown kind.
        let diags = diags_for_with_source(
            r#"import "shared.mog" scene { box "b" (size=[1,1,1]) }"#,
            None,
        );
        assert!(
            !diags.iter().any(|d| d.code == "E0101"),
            "import should not be unknown-kind, got {diags:?}"
        );
    }

    #[test]
    fn import_with_attrs_or_block_is_rejected() {
        let diags = diags_for_with_source(
            r#"import "shared.mog" (foo=1) { box "x" (size=1) } scene {}"#,
            None,
        );
        assert!(diags.iter().any(|d| d.code == "E0308"), "got {diags:?}");
        assert!(diags.iter().any(|d| d.code == "E0309"), "got {diags:?}");
    }

    #[test]
    fn import_with_alias_is_accepted() {
        // `(as=ident)` is the one attribute `import` recognises — it renames
        // the synthesised module so two files with the same stem can coexist.
        let diags = diags_for_with_source(
            r#"import "shared.mog" (as=lib) scene {}"#,
            None,
        );
        assert!(
            !diags.iter().any(|d| d.code == "E0308"),
            "E0308 should not fire on `(as=…)`, got {diags:?}"
        );
    }

    #[test]
    fn imported_modules_validate_use_references() {
        let dir = write_tmp(
            "use_ok",
            &[(
                "lib.mog",
                r#"module "leg" (h=0.5) { cylinder "leg" (height=$h) }"#,
            )],
        );
        let src = r#"import "lib.mog" scene { use "leg" (h=1.0) }"#;
        let diags = diags_for_with_source(src, Some(dir.as_path()));
        // E0304 = unknown module. The imported `leg` should resolve.
        assert!(
            !diags.iter().any(|d| d.code == "E0304"),
            "imported modules should be visible to `use`, got {diags:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_module_check_suppressed_when_imports_present_without_source() {
        // Without a base dir we can't resolve imports, but we should not
        // false-flag `use` references that might come from imported files.
        let src = r#"import "lib.mog" scene { use "leg" () }"#;
        let diags = diags_for_with_source(src, None);
        assert!(
            !diags.iter().any(|d| d.code == "E0304"),
            "unknown-module must be suppressed when imports unresolved, got {diags:?}"
        );
    }

    #[test]
    fn missing_imported_file_surfaces_as_diagnostic() {
        let dir = write_tmp("missing", &[]);
        let src = r#"import "ghost.mog" scene { box "b" (size=[1,1,1]) }"#;
        let diags = diags_for_with_source(src, Some(dir.as_path()));
        assert!(
            diags.iter().any(|d| d.code == "E0306"),
            "missing import should surface E0306, got {diags:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

mod meta_block_tests {
    use super::super::*;
    use mogen_core::{Diagnostic, Severity};

    fn diags_for(src: &str) -> Vec<Diagnostic> {
        let ast = mogen_dsl::parse(src).expect("parse");
        validate_ast(&ast)
    }

    #[test]
    fn well_formed_meta_passes() {
        let src = r#"
            meta (
              name = "chair",
              version = "1.0",
              mogen_version = ""#.to_string()
            + env!("CARGO_PKG_VERSION")
            + r#"",
              description = "a chair",
              tags = ["furniture", "wood"],
            )
            scene { box "b" (size=[1,1,1]) }
        "#;
        let diags = diags_for(&src);
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "well-formed meta should produce no errors, got {diags:?}"
        );
        // And no version-mismatch warning when mogen_version matches.
        assert!(!diags.iter().any(|d| d.code == "W0107"), "got {diags:?}");
    }

    #[test]
    fn meta_style_accepts_known_key() {
        let src = r#"meta (style="ps1") scene { box "b" (size=[1,1,1]) }"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "meta(style=\"ps1\") should validate cleanly, got {diags:?}"
        );
        // No unknown-attr warning (the allowlist must include `style`).
        assert!(
            !diags.iter().any(|d| d.code == "W0102" && d.message.contains("\"style\"")),
            "got {diags:?}"
        );
    }

    #[test]
    fn meta_style_accepts_freeform_string() {
        // The validator deliberately leaves `style` as a free-form string so
        // hand-edited files with experimental keys still load. mogen-llm
        // narrows it to its own enum at the call site.
        let src = r#"meta (style="weird-experiment") scene { box "b" (size=[1,1,1]) }"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "meta(style=…) with an unknown value should still validate, got {diags:?}"
        );
    }

    #[test]
    fn meta_unknown_attr_warns() {
        let src = r#"meta (foo="bar") scene {}"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "W0102" && d.message.contains("\"foo\"")),
            "unknown meta attr should warn W0102, got {diags:?}"
        );
    }

    #[test]
    fn meta_with_body_block_errors() {
        // The grammar lets any node have a body block; the validator must
        // reject it on `meta` since the metadata is attribute-only.
        let src = r#"meta { box "b" (size=[1,1,1]) } scene {}"#;
        let diags = diags_for(src);
        assert!(diags.iter().any(|d| d.code == "E0310"), "got {diags:?}");
    }

    #[test]
    fn meta_with_quoted_name_errors() {
        let src = r#"meta "x" (version="1") scene {}"#;
        let diags = diags_for(src);
        assert!(diags.iter().any(|d| d.code == "E0311"), "got {diags:?}");
    }

    #[test]
    fn duplicate_meta_errors() {
        let src = r#"meta (name="a") meta (name="b") scene {}"#;
        let diags = diags_for(src);
        assert!(diags.iter().any(|d| d.code == "E0312"), "got {diags:?}");
    }

    #[test]
    fn nested_meta_errors() {
        let src = r#"scene { meta (name="x") box "b" (size=[1,1,1]) }"#;
        let diags = diags_for(src);
        assert!(diags.iter().any(|d| d.code == "E0313"), "got {diags:?}");
    }

    #[test]
    fn version_mismatch_warns() {
        let src = r#"meta (mogen_version = "0.0.0-other") scene {}"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "W0107"),
            "stale mogen_version should warn W0107, got {diags:?}"
        );
    }

    #[test]
    fn version_patch_difference_is_silent() {
        // Patch bumps round-trip without semantic change — no warning, even
        // though the stamped string differs from the current toolchain.
        let current = env!("CARGO_PKG_VERSION");
        let (major, minor) = current
            .split(['-', '+'])
            .next()
            .and_then(|core| {
                let mut parts = core.split('.');
                let mj: u64 = parts.next()?.parse().ok()?;
                let mn: u64 = parts.next()?.parse().ok()?;
                Some((mj, mn))
            })
            .expect("CARGO_PKG_VERSION parses as MAJOR.MINOR.…");
        // Pick a patch number that cannot equal the current full version.
        let stamped = format!("{major}.{minor}.9999");
        assert_ne!(stamped, current);
        let src = format!(r#"meta (mogen_version = "{stamped}") scene {{}}"#);
        let diags = diags_for(&src);
        assert!(
            !diags.iter().any(|d| d.code == "W0107"),
            "patch-only difference should not warn, got {diags:?}"
        );
    }

    #[test]
    fn missing_meta_is_silent() {
        // Optional metadata: no warning for files that omit the block entirely.
        let diags = diags_for(r#"scene { box "b" (size=[1,1,1]) }"#);
        assert!(
            !diags.iter().any(|d| d.code == "W0107"),
            "missing meta should not warn, got {diags:?}"
        );
    }

    #[test]
    fn tags_must_be_string_list() {
        // Numeric items in tags should fail the list-of-string type check.
        let src = r#"meta (tags=[1,2,3]) scene {}"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E0103"),
            "numeric tags list should error E0103, got {diags:?}"
        );
    }
}

mod common_attr_scope_tests {
    use super::super::*;
    use mogen_core::{Diagnostic, Severity};

    fn diags_for(src: &str) -> Vec<Diagnostic> {
        let ast = mogen_dsl::parse(src).expect("parse");
        validate_ast(&ast)
    }

    fn has_unknown_attr(diags: &[Diagnostic], attr: &str, kind: &str) -> bool {
        let needle = format!("attribute \"{attr}\" is not used by `{kind}`");
        diags.iter().any(|d| d.code == "W0102" && d.message == needle)
    }

    #[test]
    fn placement_shortcuts_are_rejected_on_animation_templates() {
        // This is the original bug: `from=[1,0,1]` was silently accepted on
        // `open_close` because it lived in the old blanket COMMON_ATTRS.
        let src = r#"
            material "wood" (color=[0.5, 0.3, 0.1])
            scene { box "lid" (size=[1,0.1,1], mat="wood") }
            open_close "swing" (target="lid", from=[1,0,1], axis=[1,0,0], angle=90, seconds=1.0)
        "#;
        let diags = diags_for(src);
        assert!(
            has_unknown_attr(&diags, "from", "open_close"),
            "expected W0102 for from= on open_close, got {diags:?}"
        );
    }

    #[test]
    fn placement_shortcuts_are_rejected_on_attach_joint_material() {
        // Attach, joint, clip, track, material: no implicit transforms or
        // placement shortcuts — only their kind-specific allowlist.
        let src = r#"
            material "wood" (color=[0.5, 0.3, 0.1], pos=[0,0,0])
            scene {
              box "a" (size=[1,1,1], mat="wood")
              box "b" (size=[1,1,1], mat="wood")
            }
            attach (parent="a", child="b", pos=[0,0,0])
            joint "j" (type=hinge, pivot="a", anchor="top")
        "#;
        let diags = diags_for(src);
        assert!(has_unknown_attr(&diags, "pos", "material"));
        assert!(has_unknown_attr(&diags, "pos", "attach"));
        assert!(has_unknown_attr(&diags, "anchor", "joint"));
    }

    #[test]
    fn geometry_still_accepts_common_attrs() {
        // Regression guard: the split must NOT reject legitimate uses of
        // placement shortcuts on primitives.
        let src = r#"
            material "wood" (color=[0.5, 0.3, 0.1])
            scene {
              box "a" (size=[1,1,1], mat="wood", pos=[0,0,0], anchor="bottom", tags="floating")
              slab "b" (size=[1,0.1,1], mat="wood", above="a", gap=0.05)
            }
        "#;
        let diags = diags_for(src);
        let warnings: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Warning))
            .collect();
        assert!(
            warnings.is_empty(),
            "expected no attr warnings on valid geometry, got {warnings:?}"
        );
    }

    #[test]
    fn bones_still_accept_transform_attrs_but_not_placement() {
        // Bones legitimately use pos= (parent-relative offset). They must NOT
        // accept anchor/from/above etc — those are meaningless on joints.
        let ok_src = r#"
            scene {
              skeleton "rig" {
                bone "root" (pos=[0, 1, 0]) {
                  bone "child" (pos=[0, 0.5, 0], envelope=0.2)
                }
              }
            }
        "#;
        let diags = diags_for(ok_src);
        let warns: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Warning))
            .collect();
        assert!(warns.is_empty(), "valid bone attrs should not warn: {warns:?}");

        let bad_src = r#"
            scene {
              skeleton "rig" {
                bone "root" (pos=[0, 1, 0], anchor="bottom") {
                  bone "child" (pos=[0, 0.5, 0], tags="foo")
                }
              }
            }
        "#;
        let diags = diags_for(bad_src);
        assert!(has_unknown_attr(&diags, "anchor", "bone"));
        assert!(has_unknown_attr(&diags, "tags", "bone"));
    }

    #[test]
    fn light_requires_kind() {
        let src = r#"scene { light "x" () }"#;
        let diags = diags_for(src);
        assert!(diags.iter().any(|d| d.code == "E0801"), "got {diags:?}");
    }

    #[test]
    fn light_unknown_kind_rejected() {
        let src = r#"scene { light "x" (kind=floodlamp) }"#;
        let diags = diags_for(src);
        assert!(diags.iter().any(|d| d.code == "E0802"), "got {diags:?}");
    }

    #[test]
    fn light_inner_must_not_exceed_outer() {
        let src = r#"scene { light "s" (kind=spot, inner_cone=40, outer_cone=20) }"#;
        let diags = diags_for(src);
        assert!(diags.iter().any(|d| d.code == "E0808"), "got {diags:?}");
    }

    #[test]
    fn light_cone_on_non_spot_warns() {
        let src = r#"scene { light "s" (kind=point, outer_cone=30) }"#;
        let diags = diags_for(src);
        assert!(diags.iter().any(|d| d.code == "W0807"), "got {diags:?}");
    }

    #[test]
    fn light_accepts_pos_and_rot_but_not_anchor() {
        let src = r#"scene { light "s" (kind=point, pos=[0,2,0], anchor=bottom) }"#;
        let diags = diags_for(src);
        assert!(
            has_unknown_attr(&diags, "anchor", "light"),
            "anchor should be rejected on light, got {diags:?}",
        );
        // pos= must NOT warn — lights are placed in the scene.
        assert!(
            !has_unknown_attr(&diags, "pos", "light"),
            "pos= must be accepted on light, got {diags:?}",
        );
    }

    #[test]
    fn track_from_to_still_accepted() {
        // `track` has its own `from`/`to` (scalar keyframe values) in its
        // kind-specific allowlist — the split must not break these.
        let src = r#"
            scene {
              group "door" { box "panel" (size=[1, 2, 0.1]) }
            }
            joint "h" (type=hinge, pivot="door", axis=[0,1,0])
            clip "swing" (seconds=1.0) {
              track "h" (from=0, to=90)
            }
        "#;
        let diags = diags_for(src);
        let warns: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Warning))
            .collect();
        assert!(warns.is_empty(), "track from/to must pass: {warns:?}");
    }

    #[test]
    fn decal_kind_is_known() {
        let src = r#"scene { decal "logo" (prompt="x", size=[0.2,0.1]) }"#;
        let diags = diags_for(src);
        assert!(
            !diags.iter().any(|d| d.code == "E0101"),
            "decal must not be unknown-kind: {diags:?}"
        );
    }

    #[test]
    fn decal_rejects_mat() {
        let src = r#"
            material "fabric" (color=[0.1,0.2,0.6])
            scene { decal "logo" (prompt="x", size=[0.2,0.1], mat="fabric") }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E0901"),
            "expected E0901 on decal with mat=, got {diags:?}"
        );
    }
}

mod building_validator_tests {
    use super::super::*;
    use mogen_core::{Diagnostic, Severity};

    fn diags_for(src: &str) -> Vec<Diagnostic> {
        let ast = mogen_dsl::parse(src).expect("parse");
        validate_ast(&ast)
    }

    const MIN_BUILDING: &str = r#"
        material "concrete" (color=[0.8, 0.8, 0.8])
        building "x" (
          seed=1, style="grid", floor_area=40, rooms=2,
          entrances=1, mat="concrete",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;

    #[test]
    fn minimum_building_is_valid() {
        let diags = diags_for(MIN_BUILDING);
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Error))
            .collect();
        assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
    }

    #[test]
    fn rejects_unknown_room_type_kind() {
        let src = r#"
            material "c" (color=[0.5, 0.5, 0.5])
            building "x" (seed=1, style="grid", floor_area=20, rooms=1, mat="c") {
              room_type "office" (kind=bogus, density=1)
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1123"),
            "expected E1123, got: {diags:?}"
        );
    }

    #[test]
    fn rejects_adjacency_referencing_unknown_type() {
        let src = r#"
            material "c" (color=[0.5, 0.5, 0.5])
            building "x" (seed=1, style="grid", floor_area=20, rooms=2, mat="c") {
              room_type "office" (kind=staff_only, density=1)
              adjacency "office" (adjacent_to=["lobby"])
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1108"),
            "expected E1108, got: {diags:?}"
        );
    }

    #[test]
    fn t2_multi_floor_without_stairs_warns() {
        // Tranche 2 unlocks multi-storey but still expects the author to
        // wire a staircase (otherwise the upper floor is visually
        // disconnected). Tranche 4 implements all roof shapes — pairing a
        // non-flat roof with skylights now produces W1114 (skylights are
        // ignored under non-flat roofs) instead of the retired E1111.
        let src = r#"
            material "c" (color=[0.5, 0.5, 0.5])
            building "x" (
              seed=1, style="grid", floor_area=40, rooms=4,
              floors_above=2, skylights=1, roof="gabled",
              mat="c",
            ) {
              room_type "office" (kind=staff_only, density=1)
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "W1113"),
            "expected W1113 for multi-floor without stairs, got: {diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.code == "W1114"),
            "expected W1114 for skylights under non-flat roof, got: {diags:?}"
        );
        assert!(
            !diags.iter().any(|d| d.code == "E1111"),
            "E1111 was retired in T4; got: {diags:?}"
        );
    }

    #[test]
    fn t2_multi_floor_with_stair_is_clean() {
        let src = r#"
            material "c" (color=[0.5, 0.5, 0.5])
            building "x" (
              seed=1, style="grid", floor_area=60, rooms=6,
              floors_above=2, staircases=1, skylights=1,
              mat="c",
            ) {
              room_type "office" (kind=staff_only, density=1)
            }
        "#;
        let diags = diags_for(src);
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Error))
            .collect();
        assert!(errs.is_empty(), "T2 multi-floor + stair should be clean, got errors: {errs:?}");
        assert!(
            !diags.iter().any(|d| d.code == "W1113"),
            "W1113 should not fire when a stair is present: {diags:?}"
        );
    }

    #[test]
    fn rejects_unknown_style() {
        let src = r#"
            material "c" (color=[0.5, 0.5, 0.5])
            building "x" (seed=1, style="bogus", floor_area=20, rooms=1, mat="c") {
              room_type "office" (kind=staff_only, density=1)
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1102"),
            "expected E1102, got: {diags:?}"
        );
    }

    #[test]
    fn rejects_geometry_child_in_building() {
        let src = r#"
            material "c" (color=[0.5, 0.5, 0.5])
            building "x" (seed=1, style="grid", floor_area=20, rooms=1, mat="c") {
              room_type "office" (kind=staff_only, density=1)
              box "intruder" (size=[1,1,1])
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1101"),
            "expected E1101 for non-room_type/adjacency child, got: {diags:?}"
        );
    }

    #[test]
    fn decal_warns_on_image_and_prompt_together() {
        let src = r#"
            scene {
              decal "logo" (
                prompt="ignored", image="textures/x/logo_decal.png",
                size=[0.2,0.1]
              )
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "W0902"),
            "expected W0902 when both image= and prompt= present: {diags:?}"
        );
    }

    #[test]
    fn decal_size_must_be_two_element_list() {
        // Four-element list takes the parser's `list` rule (vec3 is 3 only),
        // so the arity check fires as E0903.
        let src = r#"scene { decal "logo" (prompt="x", size=[0.2, 0.1, 0.05, 0.0]) }"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E0903"),
            "expected E0903 on bad decal size arity: {diags:?}"
        );
    }

    #[test]
    fn decal_size_rejects_vec3_via_type_check() {
        // `[0.2, 0.1, 0.05]` parses as Vec3 (grammar prefers it over List for
        // 3-element forms). The expected-type check fires E0103 telling the
        // author size must be a list — which is the right hint.
        let src = r#"scene { decal "logo" (prompt="x", size=[0.2, 0.1, 0.05]) }"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E0103"),
            "expected E0103 on Vec3 size: {diags:?}"
        );
    }

    #[test]
    fn decal_rejects_non_positive_size_components() {
        let src = r#"scene { decal "logo" (prompt="x", size=[0.2, 0]) }"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E0903"),
            "expected E0903 on zero size component: {diags:?}"
        );
    }

    #[test]
    fn decal_on_requires_at() {
        let src = r#"
            scene {
              ellipsoid "bag" (size=[1,0.5,0.5]) {
                connector "spot" (at=[0.4,0.2,0.3], dir=[0,0,1])
              }
              decal "logo" (prompt="x", size=[0.2,0.1], on="bag")
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E0904"),
            "expected E0904 when on= is set without at=: {diags:?}"
        );
    }

    #[test]
    fn decal_at_without_on_is_rejected() {
        // `at=` only makes sense paired with `on=`; alone it's silent noise.
        let src = r#"scene { decal "logo" (prompt="x", size=[0.2,0.1], at="spot") }"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E0905"),
            "expected E0905 when at= used without on=: {diags:?}"
        );
    }

    #[test]
    fn decal_lift_without_on_is_rejected() {
        let src = r#"scene { decal "logo" (prompt="x", size=[0.2,0.1], lift=0.005) }"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E0905"),
            "expected E0905 when lift= used without on=: {diags:?}"
        );
    }

    #[test]
    fn decal_on_with_at_validates_clean() {
        let src = r#"
            scene {
              ellipsoid "bag" (size=[1,0.5,0.5]) {
                connector "spot" (at=[0.4,0.2,0.3], dir=[0,0,1])
              }
              decal "logo" (prompt="x", size=[0.2,0.1], on="bag", at="spot", lift=0.002)
            }
        "#;
        let diags = diags_for(src);
        let errs: Vec<_> = diags.iter().filter(|d| matches!(d.severity, Severity::Error)).collect();
        assert!(errs.is_empty(), "no errors expected on valid on= shortcut: {errs:?}");
    }

    #[test]
    fn decal_accepts_transform_attrs_but_not_skin_or_bind() {
        // Decal accepts pos/rot/scale and placement helpers; skin/bind don't
        // make sense on a non-skinned overlay.
        let src = r#"scene {
            decal "logo" (
                prompt="x", size=[0.2,0.1],
                pos=[0,0.1,0.1], rot=[0,0,0],
                anchor="center", above="other",
                skin="rig", bind="root"
            )
        }"#;
        let diags = diags_for(src);
        assert!(
            !diags.iter().any(|d| d.code == "W0102" && d.message.contains("\"pos\"")),
            "pos= must be accepted on decal: {diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.code == "W0102" && d.message.contains("\"skin\"")),
            "skin= must be rejected on decal: {diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.code == "W0102" && d.message.contains("\"bind\"")),
            "bind= must be rejected on decal: {diags:?}"
        );
    }

    #[test]
    fn deform_modifiers_accepted_on_primitives() {
        let src = r#"scene {
            sphere "s" (
                radius=0.5,
                noise=0.3, jitter=0.1, seed=7,
                bend_x=10, bend_y=15, bend_z=20,
                twist_y=45, taper=0.6, droop=0.2,
                faceted=1
            )
        }"#;
        let diags = diags_for(src);
        assert!(
            !diags.iter().any(|d| d.code == "W0102"),
            "deformation modifiers should be common attrs on geometry primitives: {diags:?}"
        );
    }

    #[test]
    fn out_of_range_noise_warns() {
        let src = r#"scene { sphere "s" (radius=0.5, noise=1.5) }"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "W1002"),
            "expected W1002 for out-of-range noise: {diags:?}"
        );
    }
}

mod cave_validator_tests {
    use super::super::*;
    use mogen_core::Severity;

    fn diags_for(src: &str) -> Vec<mogen_core::Diagnostic> {
        let ast = mogen_dsl::parse(src).expect("parse");
        validate_ast(&ast)
    }

    const MIN_CAVE: &str = r#"
        cave "den" (
          seed=3,
          size=[20, 9, 20],
          chambers=5,
          levels=2,
          resolution=48,
          entrances=1,
        )
    "#;

    #[test]
    fn minimum_cave_is_valid() {
        let diags = diags_for(MIN_CAVE);
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Error))
            .collect();
        assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
    }

    #[test]
    fn rejects_geometry_child_in_cave() {
        let src = r#"
            cave "den" (seed=1, chambers=4) {
              box "intruder" (size=[1,1,1])
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1201"),
            "expected E1201 for non-feature child in cave, got: {diags:?}"
        );
    }

    #[test]
    fn rejects_unknown_feature_kind() {
        let src = r#"
            cave "den" (seed=1, chambers=4) {
              feature "x" (kind=lava, count=3)
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1212"),
            "expected E1212 for unknown feature kind, got: {diags:?}"
        );
    }

    #[test]
    fn rejects_feature_without_kind() {
        let src = r#"
            cave "den" (seed=1, chambers=4) {
              feature "x" (count=3)
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1211"),
            "expected E1211 when feature.kind= is absent, got: {diags:?}"
        );
    }

    #[test]
    fn rejects_feature_with_body_block() {
        let src = r#"
            cave "den" (seed=1, chambers=4) {
              feature "x" (kind=stalagmite) { box "b" (size=[1,1,1]) }
            }
        "#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1210"),
            "expected E1210 for feature with a body block, got: {diags:?}"
        );
    }

    #[test]
    fn warns_when_chamber_min_exceeds_chamber_max() {
        let src = r#"cave "den" (seed=1, chambers=4, chamber_min=8, chamber_max=2)"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "W1205"),
            "expected W1205 when chamber_min > chamber_max, got: {diags:?}"
        );
    }

    #[test]
    fn warns_when_seed_is_zero() {
        let src = r#"cave "den" (seed=0, chambers=4)"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "W1209"),
            "expected W1209 for seed=0, got: {diags:?}"
        );
    }

    #[test]
    fn rejects_negative_decoration_count() {
        let src = r#"cave "den" (seed=1, chambers=4, stalagmites=-1)"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1202"),
            "expected E1202 for negative decoration count, got: {diags:?}"
        );
    }

    #[test]
    fn rejects_non_positive_size_components() {
        let src = r#"cave "den" (seed=1, chambers=4, size=[20, 0, 20])"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1203"),
            "expected E1203 for zero size component, got: {diags:?}"
        );
    }

    #[test]
    fn rejects_max_slope_out_of_range() {
        let src = r#"cave "den" (seed=1, chambers=4, max_slope=0)"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "E1204"),
            "expected E1204 for max_slope=0, got: {diags:?}"
        );
    }

    #[test]
    fn warns_out_of_range_roughness() {
        let src = r#"cave "den" (seed=1, chambers=4, roughness=1.5)"#;
        let diags = diags_for(src);
        assert!(
            diags.iter().any(|d| d.code == "W1206"),
            "expected W1206 for roughness > 1, got: {diags:?}"
        );
    }

    #[test]
    fn valid_feature_block_is_clean() {
        let src = r#"
            cave "den" (seed=2, chambers=5, stalagmites=3) {
              feature "spikes" (kind=stalagmite, min_size=0.4, max_size=0.9)
            }
        "#;
        let diags = diags_for(src);
        let errs: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, Severity::Error))
            .collect();
        assert!(errs.is_empty(), "valid feature should produce no errors: {errs:?}");
    }
}
