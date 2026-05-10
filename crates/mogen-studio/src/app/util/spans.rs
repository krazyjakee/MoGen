/// Locate the DSL source span for the authored `material "name" (...)`
/// declaration. Materials can live at the top level or inside `scene { … }` —
/// both are checked. Returns `None` if the source no longer parses or the
/// material wasn't authored in the active file (e.g. it came from a module).
pub(in crate::app) fn find_material_source_span(src: &str, name: &str) -> Option<mogen_core::Span> {
    let ast = mogen_dsl::parse(src).ok()?;
    for n in &ast {
        if n.kind == "material" && n.name.as_deref() == Some(name) {
            return Some(n.span);
        }
        if n.kind == "scene" {
            for c in &n.children {
                if c.kind == "material" && c.name.as_deref() == Some(name) {
                    return Some(c.span);
                }
            }
        }
    }
    None
}

/// Locate the DSL source span for the first `use "<stem>" (...)` declaration
/// in the active source. Used by the inspector's "Wrap `use` in a group"
/// affordance: a click resolves the imported node's origin file stem to the
/// `use` line that brought it in, then the wrap helper splices a group
/// around it. Returns `None` when the source no longer parses or no
/// matching `use` exists. When multiple `use "<stem>"` calls exist, the
/// first match (depth-first walk) is returned — picking the wrong instance
/// is recoverable via undo, and the warning that triggers this affordance
/// already implies there's no wrapper to disambiguate against.
pub(in crate::app) fn find_use_source_span(src: &str, stem: &str) -> Option<mogen_core::Span> {
    let ast = mogen_dsl::parse(src).ok()?;
    fn walk(node: &mogen_dsl::ast::Node, stem: &str) -> Option<mogen_core::Span> {
        if node.kind == "use" && node.name.as_deref() == Some(stem) {
            return Some(node.span);
        }
        for c in &node.children {
            if let Some(s) = walk(c, stem) {
                return Some(s);
            }
        }
        None
    }
    for n in &ast {
        if let Some(s) = walk(n, stem) {
            return Some(s);
        }
    }
    None
}

/// Locate the DSL source span for the `clip` (or procedural-template) node
/// whose resulting clip has `clip_name`. Scans the parsed AST recursively so
/// scene-nested clips are found alongside top-level ones. Returns `None` if
/// the source no longer parses or no matching authored node exists (e.g. for
/// multi-target templates whose clip names carry an `_{i}` suffix).
pub(in crate::app) fn find_clip_source_span(src: &str, clip_name: &str) -> Option<mogen_core::Span> {
    let ast = mogen_dsl::parse(src).ok()?;
    // Kinds that lower to `Clip` entries. Procedural templates take their
    // name from `node.name`; literal clips likewise. Multi-target templates
    // produce `{name}_{i}` clips — those won't match a bare name here and
    // the Delete button is disabled upstream.
    const ANIM_KINDS: &[&str] = &["clip", "spin", "open_close", "wave", "flap", "idle"];
    fn walk(
        node: &mogen_dsl::ast::Node,
        target: &str,
        kinds: &[&str],
    ) -> Option<mogen_core::Span> {
        if kinds.contains(&node.kind.as_str())
            && node.name.as_deref() == Some(target)
        {
            return Some(node.span);
        }
        for c in &node.children {
            if let Some(s) = walk(c, target, kinds) {
                return Some(s);
            }
        }
        None
    }
    for n in &ast {
        if let Some(s) = walk(n, clip_name, ANIM_KINDS) {
            return Some(s);
        }
    }
    None
}
