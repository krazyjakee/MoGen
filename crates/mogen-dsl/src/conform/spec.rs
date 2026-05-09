//! AST → `ConformSpec` lowering: collect every `conform` node (and the
//! `decal (on=…)` shortcut) from a parsed AST, validate the attribute
//! combinations, and emit a typed spec the dispatcher in [`super`]
//! can apply.

use anyhow::{anyhow, bail, Result};

use mogen_core::Span;
use mogen_geom::Axis;

use crate::ast::{Node, Value};

#[derive(Debug)]
pub(super) enum ConformMode {
    /// Strip stretched along a path between two connectors on the target.
    Path {
        from: String,
        to: String,
        along: Option<Axis>,
        width: Option<Axis>,
        height: Option<Axis>,
        samples: u32,
        twist_deg: f32,
    },
    /// Flat / disc child laid down at a single anchor connector on the target.
    Patch {
        at: String,
        up: Option<Axis>,
    },
}

#[derive(Debug)]
pub(super) struct ConformSpec {
    pub(super) target: String,
    pub(super) child: String,
    pub(super) lift: f32,
    pub(super) reparent: bool,
    pub(super) use_id: Option<u32>,
    #[allow(dead_code)]
    pub(super) span: Span,
    pub(super) mode: ConformMode,
}

pub(super) fn collect_conforms(ast: &[Node]) -> Result<Vec<ConformSpec>> {
    let mut out = Vec::new();
    for n in ast {
        walk(n, &mut out)?;
    }
    Ok(out)
}

pub(super) fn walk(n: &Node, out: &mut Vec<ConformSpec>) -> Result<()> {
    if n.kind == "conform" {
        out.push(build_spec(n)?);
        return Ok(());
    }
    // `decal (on=..., at=..., up=..., lift=...)` is sugar for an explicit
    // `conform` patch — the user authors a single decal node and we synthesize
    // the conform behind the scenes. Decals stay in the AST so downstream
    // passes that walk for transparent images (texture pipeline, span-aware
    // splicing of `image="…"`) still see them.
    if n.kind == "decal" && n.attr("on").is_some() {
        out.push(build_decal_spec(n)?);
        // Fall through into children — a decal with `on=` is rare to nest
        // children under, but be consistent with `conform` (which returns
        // early because conform never owns geometry of its own).
    }
    if n.kind == "array" || n.kind == "mirror" {
        return Ok(());
    }
    for c in &n.children {
        walk(c, out)?;
    }
    Ok(())
}

fn build_spec(n: &Node) -> Result<ConformSpec> {
    let target = str_attr(n, "target")
        .ok_or_else(|| anyhow!("conform requires target=\"<node name>\""))?;
    let child = str_attr(n, "child")
        .ok_or_else(|| anyhow!("conform requires child=\"<node name>\""))?;

    // Reserved attributes for future modes — reject early so users get a
    // clear error rather than silent fallback to defaults.
    if n.attr("direction").is_some() {
        bail!(
            "conform: direction= projection mode is not yet implemented (v1 supports path mode via from=/to= and patch mode via at=)"
        );
    }
    if let Some(curve) = str_attr(n, "curve") {
        if curve != "geodesic_lerp" {
            bail!(
                "conform: curve=\"{curve}\" is not yet implemented (v1 supports curve=\"geodesic_lerp\" only)"
            );
        }
    }
    if n.attr("via").is_some() {
        bail!("conform: via= multi-segment paths are not yet implemented");
    }

    // Mode discrimination: `at` selects patch mode, `from`/`to` selects path
    // mode. Mixing or omitting both is a hard error so authors get an
    // actionable diagnostic.
    let has_at = n.attr("at").is_some();
    let has_from = n.attr("from").is_some();
    let has_to = n.attr("to").is_some();
    if has_at && (has_from || has_to) {
        bail!(
            "conform: cannot combine patch-mode (at=) with path-mode (from=/to=); pick one"
        );
    }
    if !has_at && !(has_from || has_to) {
        bail!(
            "conform requires either at=\"<connector>\" (patch mode) or from=\"<connector>\" to=\"<connector>\" (path mode)"
        );
    }

    let lift = n.attr_number("lift").unwrap_or(0.0);
    // `reparent` defaults to true. Author can pass `reparent=0` to disable.
    let reparent = n.attr_number("reparent").map(|v| v != 0.0).unwrap_or(true);

    let mode = if has_at {
        // Reject path-mode-only attrs so authors don't accidentally write
        // attrs that get silently ignored.
        for k in ["along", "width", "height", "samples", "twist"] {
            if n.attr(k).is_some() {
                bail!(
                    "conform: attribute `{k}` is path-mode only (use it with from=/to=, not at=)"
                );
            }
        }
        let at = str_attr(n, "at").unwrap();
        let up = parse_axis(n, "up");
        ConformMode::Patch { at, up }
    } else {
        if n.attr("up").is_some() {
            bail!(
                "conform: attribute `up` is patch-mode only (use it with at=, not from=/to=)"
            );
        }
        let from = str_attr(n, "from")
            .ok_or_else(|| anyhow!("conform requires from=\"<connector>\" on the target"))?;
        let to = str_attr(n, "to")
            .ok_or_else(|| anyhow!("conform requires to=\"<connector>\" on the target"))?;
        let along = parse_axis(n, "along");
        let width = parse_axis(n, "width");
        let height = parse_axis(n, "height");
        let samples = n
            .attr_number("samples")
            .map(|v| v.max(2.0) as u32)
            .unwrap_or(64);
        let twist_deg = n.attr_number("twist").unwrap_or(0.0);
        ConformMode::Path { from, to, along, width, height, samples, twist_deg }
    };

    Ok(ConformSpec {
        target,
        child,
        lift,
        reparent,
        use_id: n.use_id,
        span: n.span,
        mode,
    })
}

/// Build a synthesized patch-mode `ConformSpec` from a `decal` node carrying
/// the `on=`/`at=`/`up=`/`lift=` shortcut. Mirrors `build_spec` for explicit
/// `conform` nodes but knows the decal's contract: target = `on=`, child =
/// the decal's own name, mode = patch, default `up` is +Z (the decal quad's
/// face normal — picked up automatically from `default_up_for("decal")`
/// when `up=` is omitted).
fn build_decal_spec(n: &Node) -> Result<ConformSpec> {
    let target = str_attr(n, "on")
        .ok_or_else(|| anyhow!("decal `on=` shortcut requires a target node name"))?;
    // The decal's own name is the conform child. Validation already requires
    // `at=` when `on=` is set; the runtime error here is a defensive fallback
    // in case `validate_ast` was bypassed.
    let at = str_attr(n, "at").ok_or_else(|| {
        anyhow!(
            "decal \"{}\" has `on=\"{}\"` but no `at=\"<connector>\"` — \
             the patch needs an anchor connector on the target",
            n.name.clone().unwrap_or_else(|| "decal".to_string()),
            target,
        )
    })?;
    let child = n.name.clone().ok_or_else(|| {
        anyhow!(
            "decal with `on=` must have a name so the synthesized conform can \
             reference it (got an unnamed decal targeting \"{target}\")"
        )
    })?;
    let up = parse_axis(n, "up");
    let lift = n.attr_number("lift").unwrap_or(0.0);
    Ok(ConformSpec {
        target,
        child,
        lift,
        // Reparent under the target so the conformed decal moves with it,
        // matching the explicit-conform default and the prior decal-as-child
        // authoring pattern.
        reparent: true,
        use_id: n.use_id,
        span: n.span,
        mode: ConformMode::Patch { at, up },
    })
}

fn str_attr(n: &Node, key: &str) -> Option<String> {
    match n.attr(key)? {
        Value::String(s) | Value::Ident(s) => Some(s.clone()),
        _ => None,
    }
}

fn parse_axis(n: &Node, key: &str) -> Option<Axis> {
    match n.attr(key)? {
        Value::String(s) | Value::Ident(s) => match s.as_str() {
            "x" | "X" => Some(Axis::X),
            "y" | "Y" => Some(Axis::Y),
            "z" | "Z" => Some(Axis::Z),
            _ => None,
        },
        _ => None,
    }
}
