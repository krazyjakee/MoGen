//! Give every node, mesh and material a unique `.mog`-safe name.
//!
//! Readers only record `source_name`; this is the one place names are made.
//! Sanitising maps anything outside `[A-Za-z0-9_]` to `_`, so the result is a
//! valid identifier as well as a string (handy if the user later wants to use
//! it as one). Collisions get `_2`, `_3`, … in source order, which is what
//! keeps the result stable across runs.
//!
//! Nodes and materials are separate namespaces in `.mog`. Meshes only name
//! modules and generated `poly` nodes, so they share the node namespace.

use std::collections::HashSet;

use crate::ir::MeshScene;

/// Assign `name` on every node, mesh and material.
pub fn normalize(scene: &mut MeshScene) {
    let mut nodes = Namer::default();
    for node in &mut scene.nodes {
        node.name = nodes.claim(node.source_name.as_deref(), "node");
    }
    for mesh in &mut scene.meshes {
        mesh.name = nodes.claim(mesh.source_name.as_deref(), "mesh");
    }
    let mut mats = Namer::default();
    for mat in &mut scene.materials {
        mat.name = mats.claim(mat.source_name.as_deref(), "material");
    }
}

/// `raw` as a `[A-Za-z0-9_]+` identifier that does not start with a digit, or
/// `fallback` if nothing usable is left.
pub fn sanitize(raw: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_underscore = false;
    for c in raw.chars() {
        let c = if c.is_ascii_alphanumeric() { c } else { '_' };
        if c == '_' {
            if last_underscore {
                continue;
            }
            last_underscore = true;
        } else {
            last_underscore = false;
        }
        out.push(c);
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        return fallback.to_string();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("{fallback}_{out}");
    }
    out.to_string()
}

/// Hands out unique names. Membership only — the set is never iterated, so
/// its hash order cannot leak into the output.
#[derive(Default)]
pub(crate) struct Namer {
    taken: HashSet<String>,
}

impl Namer {
    pub(crate) fn claim(&mut self, raw: Option<&str>, fallback: &str) -> String {
        let base = match raw {
            Some(r) => sanitize(r, fallback),
            None => fallback.to_string(),
        };
        if self.taken.insert(base.clone()) {
            return base;
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}_{n}");
            if self.taken.insert(candidate.clone()) {
                return candidate;
            }
            n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_identifiers_and_rewrites_the_rest() {
        assert_eq!(sanitize("Chair Leg.001", "node"), "Chair_Leg_001");
        assert_eq!(sanitize("  --  ", "node"), "node");
        assert_eq!(sanitize("3dModel", "node"), "node_3dModel");
        assert_eq!(sanitize("ümlaut", "node"), "mlaut");
    }

    #[test]
    fn collisions_are_numbered_in_order() {
        let mut n = Namer::default();
        assert_eq!(n.claim(Some("leg"), "node"), "leg");
        assert_eq!(n.claim(Some("leg"), "node"), "leg_2");
        assert_eq!(n.claim(None, "node"), "node");
        assert_eq!(n.claim(None, "node"), "node_2");
        // A source name that happens to look like a generated suffix still
        // gets a unique result.
        assert_eq!(n.claim(Some("leg_2"), "node"), "leg_2_2");
    }
}
