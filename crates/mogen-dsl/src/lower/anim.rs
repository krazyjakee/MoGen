use anyhow::Result;

use mogen_core::SceneGraph;

use crate::anim_lower::{lower_clip, lower_joint, lower_template};
use crate::ast::Node;

const ANIM_KINDS: &[&str] = &["joint", "clip", "spin", "open_close", "wave", "flap", "idle"];

pub(super) fn is_anim_decl(kind: &str) -> bool {
    ANIM_KINDS.contains(&kind)
}

/// Pass 3 of `lower`: joints first (clips may reference joint names), then
/// clips, then procedural templates (which can target either joints or
/// nodes). Imported animations live inside their synthesised module body, so
/// they arrive through `use` expansion and are already present in `ast` —
/// no separate walk needed.
pub(super) fn lower_animations(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    // Collect anim nodes by kind so ordering is deterministic regardless of
    // how the user wrote them in the file. The walk is recursive so that an
    // imported scene-as-module — whose body lives inside the synthesised
    // module and lands wherever the composing scene's `use` placed it —
    // still has its joints, clips, and templates discovered.
    let mut joints = Vec::new();
    let mut clips = Vec::new();
    let mut templates = Vec::new();
    for n in ast {
        collect_anim_decls(n, &mut joints, &mut clips, &mut templates);
    }
    for n in joints {
        lower_joint(n, graph)?;
    }
    for n in clips {
        lower_clip(n, graph)?;
    }
    for n in templates {
        lower_template(n, graph)?;
    }
    Ok(())
}

fn collect_anim_decls<'a>(
    n: &'a Node,
    joints: &mut Vec<&'a Node>,
    clips: &mut Vec<&'a Node>,
    templates: &mut Vec<&'a Node>,
) {
    match n.kind.as_str() {
        "joint" => joints.push(n),
        "clip" => clips.push(n),
        "spin" | "open_close" | "wave" | "flap" | "idle" => templates.push(n),
        _ => {}
    }
    for c in &n.children {
        collect_anim_decls(c, joints, clips, templates);
    }
}
