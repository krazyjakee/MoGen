//! Parsing for `use "@user/slug[@version]"` references and AST walks that
//! collect them out of a parsed `.mog` source.
//!
//! Kept identical in semantics to MoGHub's former `src/mogen.rs` parser —
//! handle/slug character classes match GitHub-login conventions and the
//! moghub registry's slug grammar so a desktop-resolved ref and a
//! server-resolved ref agree on what's a registry token vs a local name.

use mogen_dsl::ast::Node;
use serde::{Deserialize, Serialize};

/// A `use "@user/slug[@version]"` reference to another author's published
/// model.
///
/// At publish time, MoGHub resolves the `(user, slug, version)` tuple to a
/// concrete `(model_id, version_id)` pair and writes both into
/// `model_versions.mog_lock`. At desktop build time, [`crate::client::RegistryClient`]
/// performs the same resolution against the public API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryRef {
    pub user: String,
    pub slug: String,
    /// Integer version constraint (e.g. `@alice/chairs@3`). `None` means
    /// "track latest" — the resolver pins to the model's current
    /// `latest_version_id` and writes that integer back into the
    /// dependent's `mog.lock`, so subsequent builds reproduce against the
    /// same version even though the source text didn't say so.
    pub version: Option<i32>,
    /// Original token text (e.g. `@alice/chairs@3`). Stored verbatim as
    /// `dependencies.version_constraint` server-side so the publisher's
    /// intent survives even if we later widen the matcher.
    pub raw: String,
}

/// Try to parse a `use` token as a cross-author registry ref. Returns
/// `None` for local/named uses (those stay in `mog_lock.uses`).
pub fn parse_registry_ref(token: &str) -> Option<RegistryRef> {
    let body = token.strip_prefix('@')?;
    // Two forms: <user>/<slug> or <user>/<slug>@<version>. Split on the
    // *last* '@' so handles containing the character in some future
    // identity scheme don't fight the parser — though today GitHub logins
    // are restricted enough that this is just defensive.
    let (head, version_s) = match body.rsplit_once('@') {
        Some((h, v)) => (h, Some(v)),
        None => (body, None),
    };
    let (user, slug) = head.split_once('/')?;
    if user.is_empty() || slug.is_empty() {
        return None;
    }
    if !is_handle_like(user) || !is_slug_like(slug) {
        return None;
    }
    let version = match version_s {
        Some(v) => Some(v.parse::<i32>().ok()?),
        None => None,
    };
    Some(RegistryRef {
        user: user.to_string(),
        slug: slug.to_string(),
        version,
        raw: token.to_string(),
    })
}

fn is_handle_like(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_slug_like(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Result of walking a parsed `.mog` AST for cross-file references.
///
/// Mirrors what MoGHub stores in `model_versions.mog_lock` minus the
/// post-resolution fields (`resolved_version_id`, transitive hoists). The
/// JSON shape is the same so a lock written by either side round-trips
/// through the other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UseGraph {
    /// Local `import "x.mog"` paths, sorted + deduped.
    pub imports: Vec<String>,
    /// Local `use "name"` references that aren't registry refs. Sorted +
    /// deduped.
    pub uses: Vec<String>,
    /// Cross-author `use "@user/slug[@v]"` registry refs, sorted by `raw`
    /// + deduped.
    pub registry: Vec<RegistryRef>,
}

impl UseGraph {
    /// Render as the JSON shape MoGHub stores in
    /// `model_versions.mog_lock`. The server reads this verbatim; keep
    /// the field set in lockstep.
    pub fn to_mog_lock_json(&self) -> serde_json::Value {
        let registry: Vec<serde_json::Value> = self
            .registry
            .iter()
            .map(|r| {
                serde_json::json!({
                    "user": r.user,
                    "slug": r.slug,
                    "version": r.version,
                    "raw": r.raw,
                })
            })
            .collect();
        serde_json::json!({
            "imports": self.imports,
            "uses": self.uses,
            "registry": registry,
        })
    }
}

/// Walk the AST collecting `import "x.mog"` (local file imports), `use "name"`
/// (local module instantiations), and `use "@user/slug[@v]"` (cross-author
/// registry refs).
pub fn extract_use_graph(ast: &[Node]) -> UseGraph {
    let mut g = UseGraph::default();
    walk(ast, &mut g);
    g.imports.sort();
    g.imports.dedup();
    g.uses.sort();
    g.uses.dedup();
    g.registry.sort_by(|a, b| a.raw.cmp(&b.raw));
    g.registry.dedup_by(|a, b| a.raw == b.raw);
    g
}

fn walk(nodes: &[Node], out: &mut UseGraph) {
    for n in nodes {
        match n.kind.as_str() {
            // `import "shared.mog"` — path is in `Node::name`.
            "import" => {
                if let Some(path) = &n.name {
                    out.imports.push(path.clone());
                }
            }
            // `use "name" (...)`. A leading '@' promotes the entry to a
            // registry ref; bare names stay local (resolved via the
            // entry's own `module "..."` blocks or its imported files).
            "use" => {
                if let Some(name) = &n.name {
                    if let Some(r) = parse_registry_ref(name) {
                        out.registry.push(r);
                    } else {
                        out.uses.push(name.clone());
                    }
                }
            }
            _ => {}
        }
        walk(&n.children, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registry_ref_with_version() {
        let r = parse_registry_ref("@alice/chairs@3").unwrap();
        assert_eq!(r.user, "alice");
        assert_eq!(r.slug, "chairs");
        assert_eq!(r.version, Some(3));
        assert_eq!(r.raw, "@alice/chairs@3");
    }

    #[test]
    fn parses_registry_ref_without_version() {
        let r = parse_registry_ref("@alice/chairs").unwrap();
        assert_eq!(r.user, "alice");
        assert_eq!(r.slug, "chairs");
        assert_eq!(r.version, None);
    }

    #[test]
    fn rejects_local_module_names() {
        assert!(parse_registry_ref("chair_leg").is_none());
        assert!(parse_registry_ref("@nope").is_none());
        assert!(parse_registry_ref("alice/chairs").is_none());
    }

    #[test]
    fn rejects_invalid_handles_or_slugs() {
        assert!(parse_registry_ref("@alice/Chair_Legs").is_none());
        assert!(parse_registry_ref("@alice//chairs").is_none());
        assert!(parse_registry_ref("@alice/chairs@notaversion").is_none());
    }

    #[test]
    fn extract_picks_up_registry_and_local_refs() {
        let src = r#"
            import "shared.mog"
            scene {
                use "leg" (h=0.5)
                use "@alice/chairs@2" ()
                use "@bob/lamps" ()
            }
        "#;
        let ast = mogen_dsl::parse(src).unwrap();
        let g = extract_use_graph(&ast);
        assert_eq!(g.imports, vec!["shared.mog".to_string()]);
        assert_eq!(g.uses, vec!["leg".to_string()]);
        assert_eq!(g.registry.len(), 2);
        assert_eq!(g.registry[0].raw, "@alice/chairs@2");
        assert_eq!(g.registry[1].raw, "@bob/lamps");
        assert_eq!(g.registry[1].version, None);
    }

    #[test]
    fn mog_lock_json_shape_matches_server() {
        let src = r#"scene { use "@alice/chairs@2" () }"#;
        let ast = mogen_dsl::parse(src).unwrap();
        let g = extract_use_graph(&ast);
        let v = g.to_mog_lock_json();
        let r = &v["registry"][0];
        assert_eq!(r["user"], "alice");
        assert_eq!(r["slug"], "chairs");
        assert_eq!(r["version"], 2);
        assert_eq!(r["raw"], "@alice/chairs@2");
    }
}
