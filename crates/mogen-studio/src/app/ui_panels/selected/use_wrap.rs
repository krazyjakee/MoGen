//! Helpers for the "Wrap `use` in a group" affordance shown when the
//! inspector lands on an imported-via-`use` node. Resolves the active
//! source span of the originating `use` call, plus the kind-keyword
//! rewrite that backs the CSG op switch.

/// Resolve the active-source span of the `use "..." (...)` call that brought
/// `node` into the scene, plus a sensible group name to wrap it under. Used
/// by the "Wrap `use` in a group" affordance shown next to the
/// imported-via-use warning.
///
/// Two cases:
///   - **Imported file (`origin = Some`)**: the file's stem (e.g.
///     `humanoid_full.mog` → `humanoid_full`) matches the use's name. The
///     first `use "<stem>"` AST node in the active source wins.
///   - **Local module (`origin = None`)**: the use call lives in the body of
///     the closest user-authored ancestor (the first ancestor in the chain
///     with `use_id != node.use_id` that has a span). We pick the first
///     `use` AST child within that ancestor's span. With multiple use calls
///     the first match isn't always *the* call that minted this node, but
///     undo recovers and the warning only fires when there's no wrapper, so
///     ambiguity is uncommon.
///
/// Returns `None` when the source no longer parses or no candidate `use`
/// declaration is found.
pub(super) fn resolve_use_wrap_target(
    scene: &mogen_core::SceneGraph,
    sel: mogen_core::NodeId,
    node: &mogen_core::SceneNode,
    source: &str,
) -> Option<(mogen_core::Span, String)> {
    if let Some(stem) = node
        .origin
        .as_deref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
    {
        let span = crate::app::util::find_use_source_span(source, stem)?;
        return Some((span, stem.to_string()));
    }

    // Local-module case: walk up to the closest ancestor whose `use_id`
    // differs from the selected node's. That ancestor's source span (in the
    // active file) bounds where the originating `use` call lives.
    let mut cur = node.parent;
    let mut ancestor_span: Option<mogen_core::Span> = None;
    while let Some(pid) = cur {
        let parent = scene.nodes.get(pid.0 as usize)?;
        if parent.use_id != node.use_id {
            ancestor_span = parent.source_span;
            break;
        }
        cur = parent.parent;
    }
    let ancestor_span = ancestor_span?;
    let ast = mogen_dsl::parse(source).ok()?;
    let _ = sel;

    fn find_first_use_within(
        nodes: &[mogen_dsl::ast::Node],
        bounds: mogen_core::Span,
    ) -> Option<&mogen_dsl::ast::Node> {
        for n in nodes {
            if n.span.start < bounds.start || n.span.end > bounds.end {
                continue;
            }
            if n.kind == "use" {
                return Some(n);
            }
            if let Some(child) = find_first_use_within(&n.children, bounds) {
                return Some(child);
            }
        }
        None
    }
    let use_node = find_first_use_within(&ast, ancestor_span)?;
    let name = use_node.name.clone()?;
    Some((use_node.span, name))
}

/// Rewrite the keyword (kind identifier) at the very start of the node
/// covered by `span`. Used by the CSG op switch in the inspector — the
/// keyword sits before the optional `"name" (...)` header, and existing
/// edit helpers don't expose a kind rewrite. Bytes-only; no AST round-trip.
pub(super) fn rewrite_node_kind(src: &str, span: mogen_core::Span, new_kind: &str) -> String {
    let bytes = src.as_bytes();
    let start = span.start.min(src.len());
    // Skip leading whitespace inside the span — pest spans typically begin
    // exactly at the kind keyword, but be defensive.
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let kind_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == kind_start {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len() + new_kind.len());
    out.push_str(&src[..kind_start]);
    out.push_str(new_kind);
    out.push_str(&src[i..]);
    out
}
