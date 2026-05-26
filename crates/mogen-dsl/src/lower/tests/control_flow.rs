use super::*;
use crate::lower::*;
use crate::parser::parse;

#[test]
fn for_loop_emits_n_copies_with_loop_var_in_pos() {
    // Outside of a module, `for` still works because expand_modules walks
    // the top-level child list through expand_children_into.
    let g = lower_src(
        r#"scene {
            for (var="i", from=0, to=4) {
                box "leg" (size=[0.05, 0.5, 0.05], pos=[$i * 0.3, 0, 0])
            }
        }"#,
    );
    let legs: Vec<_> = g.nodes.iter().filter(|n| n.name == "leg").collect();
    assert_eq!(legs.len(), 4, "for(0..4) should emit 4 nodes");
    let xs: Vec<f32> = legs.iter().map(|n| n.transform.translation.x).collect();
    let mut want = vec![0.0, 0.3, 0.6, 0.9];
    let mut got = xs.clone();
    want.sort_by(|a, b| a.partial_cmp(b).unwrap());
    got.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for (a, b) in want.iter().zip(got.iter()) {
        assert!((a - b).abs() < 1e-5, "expected x={a}, got {b}");
    }
}

#[test]
fn for_loop_step_2_skips_odd_indices() {
    let g = lower_src(
        r#"scene {
            for (var="i", from=0, to=6, step=2) {
                box "even_$i" (size=[0.1, 0.1, 0.1])
            }
        }"#,
    );
    let names: Vec<_> = g.nodes.iter().filter(|n| n.name.starts_with("even_")).map(|n| n.name.clone()).collect();
    assert_eq!(names.len(), 3, "0,2,4 = 3 iterations");
    assert!(names.contains(&"even_0".to_string()));
    assert!(names.contains(&"even_2".to_string()));
    assert!(names.contains(&"even_4".to_string()));
}

#[test]
fn for_loop_zero_iterations_when_from_eq_to() {
    let g = lower_src(
        r#"scene {
            for (var="i", from=3, to=3) {
                box "shouldnt_appear" ()
            }
        }"#,
    );
    assert!(g.nodes.iter().all(|n| n.name != "shouldnt_appear"));
}

#[test]
fn for_loop_step_zero_errors() {
    let ast = parse(r#"scene { for (var="i", from=0, to=5, step=0) { box "b" () } }"#).expect("parse");
    let err = lower(&ast).expect_err("step=0 must error");
    assert!(format!("{err}").contains("step must not be zero"));
}

#[test]
fn for_loop_negative_step_iterates_downward() {
    // Mirrors Python's `range(5, 1, -1)` → 5, 4, 3, 2 (open on the bound).
    let g = lower_src(
        r#"scene {
            for (var="i", from=5, to=1, step=-1) {
                box "down_$i" (size=[0.1, 0.1, 0.1])
            }
        }"#,
    );
    let names: Vec<_> = g.nodes.iter().filter(|n| n.name.starts_with("down_")).map(|n| n.name.clone()).collect();
    assert_eq!(names.len(), 4, "from=5, to=1, step=-1 should emit 5,4,3,2");
    assert!(names.contains(&"down_5".to_string()));
    assert!(names.contains(&"down_2".to_string()));
    assert!(!names.contains(&"down_1".to_string()), "to is exclusive");
}

#[test]
fn for_loop_iteration_cap_protects_against_runaway_input() {
    // A `for` loop with a million iterations would grind the host. The
    // expand-time cap turns it into a friendly error instead.
    let ast = parse(r#"scene { for (var="i", from=0, to=1000000) { box "b_$i" () } }"#).expect("parse");
    let err = lower(&ast).expect_err("iteration cap must fire");
    let msg = format!("{err}");
    assert!(
        msg.contains("iteration cap") || msg.contains("cap of"),
        "expected cap error, got: {msg}"
    );
}

#[test]
fn if_truthy_cond_emits_then_branch() {
    let g = lower_src(
        r#"scene {
            if (cond=1) {
                box "yes" (size=[1, 1, 1])
            }
        }"#,
    );
    assert!(g.nodes.iter().any(|n| n.name == "yes"));
}

#[test]
fn if_falsy_cond_skips_then_branch() {
    let g = lower_src(
        r#"scene {
            if (cond=0) {
                box "no" (size=[1, 1, 1])
            }
            box "after" (size=[1, 1, 1])
        }"#,
    );
    assert!(g.nodes.iter().all(|n| n.name != "no"));
    assert!(g.nodes.iter().any(|n| n.name == "after"));
}

#[test]
fn if_else_picks_else_branch_when_falsy() {
    let g = lower_src(
        r#"scene {
            if (cond=0) {
                box "then_branch" ()
            }
            else {
                box "else_branch" ()
            }
        }"#,
    );
    assert!(g.nodes.iter().all(|n| n.name != "then_branch"));
    assert!(g.nodes.iter().any(|n| n.name == "else_branch"));
}

#[test]
fn else_without_preceding_if_errors() {
    let ast = parse(r#"scene { else { box "lonely" () } }"#).expect("parse");
    let err = lower(&ast).expect_err("else without if must error");
    assert!(format!("{err}").contains("`else` must immediately follow"));
}

#[test]
fn comparison_operator_in_cond_works() {
    // `cond=$n > 1` is the canonical authoring shape for "draw extras only
    // when there's more than one of something".
    let make = |n: i32| {
        let src = format!(
            r#"module "demo" (n=1) {{
                box "always" ()
                if (cond=$n > 1) {{ box "many" () }}
            }}
            scene {{ use "demo" (n={n}) }}"#
        );
        lower_src(&src)
    };
    let one = make(1);
    let two = make(2);
    assert!(one.nodes.iter().all(|n| n.name != "many"));
    assert!(two.nodes.iter().any(|n| n.name == "many"));
}

#[test]
fn string_interpolation_in_node_name() {
    let g = lower_src(
        r#"scene {
            for (var="i", from=0, to=3) {
                box "leg_$i" (size=[0.05, 0.5, 0.05])
            }
        }"#,
    );
    let names: Vec<_> = g.nodes.iter().map(|n| n.name.clone()).collect();
    assert!(names.contains(&"leg_0".to_string()));
    assert!(names.contains(&"leg_1".to_string()));
    assert!(names.contains(&"leg_2".to_string()));
    // Integer-valued var should NOT render as "leg_0.0".
    assert!(!names.contains(&"leg_0.0".to_string()));
}

#[test]
fn string_interpolation_with_braces() {
    // ${name} form delimits the binding so authors can compose names like
    // "${prefix}_panel" without the parser mistaking the underscore for
    // part of the binding name.
    let g = lower_src(
        r#"module "panel" (i=0) {
            box "${i}_panel" (size=[0.5, 0.1, 0.5])
        }
        scene { use "panel" (i=7) }"#,
    );
    assert!(g.nodes.iter().any(|n| n.name == "7_panel"));
}

#[test]
fn string_interpolation_preserves_multibyte_utf8() {
    // Regression test for the second-pass review: `interpolate_string`
    // walked source bytes and used `byte as char`, which corrupts every
    // UTF-8 continuation byte (0x80..=0xBF) into a U+0080..U+00FF
    // codepoint. `"pièce_$i"` would render as `"piÃ¨ce_0"` etc. The fix
    // flushes literal runs via string slicing so multi-byte chars stay
    // intact.
    let g = lower_src(
        r#"scene {
            for (var="i", from=0, to=2) {
                box "pièce_$i" (size=[0.05, 0.5, 0.05])
            }
        }"#,
    );
    let names: Vec<_> = g.nodes.iter().map(|n| n.name.clone()).collect();
    assert!(
        names.contains(&"pièce_0".to_string()),
        "multi-byte char must survive interpolation; got names: {names:?}"
    );
    assert!(names.contains(&"pièce_1".to_string()));
    // Also exercise the `${name}` branch with non-ASCII content around it.
    let g2 = lower_src(
        r#"module "p" (i=0) {
            box "${i}号_部品" (size=[0.5, 0.1, 0.5])
        }
        scene { use "p" (i=42) }"#,
    );
    assert!(
        g2.nodes.iter().any(|n| n.name == "42号_部品"),
        "CJK + ${{name}} interpolation must preserve trailing multi-byte chars; got: {:?}",
        g2.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
}

#[test]
fn module_with_for_inside_expands_per_use() {
    // `for` inside a module body should expand against the call's params.
    let g = lower_src(
        r#"module "fence" (posts=1) {
            for (var="i", from=0, to=$posts) {
                box "post_$i" (size=[0.05, 0.6, 0.05], pos=[$i * 0.4, 0, 0])
            }
        }
        scene { use "fence" (posts=3) }"#,
    );
    let posts: Vec<_> = g.nodes.iter().filter(|n| n.name.starts_with("post_")).collect();
    assert_eq!(posts.len(), 3);
}
