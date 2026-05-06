//! Top-level FBX document assembly.
//!
//! The FBX 7.4 binary format is a flat tree of named nodes under a synthetic
//! root. The Blender importer expects, in this order:
//!
//! 1. `FBXHeaderExtension` — version + creator + scene info
//! 2. `GlobalSettings` — axis system, units, frame rate
//! 3. `Documents` — the top-level Document object
//! 4. `Definitions` — counts + Property templates per ObjectType
//! 5. `Objects` — every Geometry / Model / Material / Texture / etc.
//! 6. `Connections` — graph of `OO` (object-object) and `OP` (object-
//!    property) edges
//! 7. `Takes` — animation take metadata; we always emit an empty stub.
//!
//! This module owns the scaffolding (#1–#4 and #7) plus the entry point that
//! delegates each `Objects` section to its sibling module. A single
//! [`IdAllocator`](super::ids::IdAllocator) hands out object IDs across the
//! whole pass so connections can refer to anything by id.

use anyhow::Result;

use fbxcel::low::v7400::AttributeValue;
use fbxcel::tree::v7400::{NodeId, Tree};

use mogen_core::SceneGraph;

use crate::texture::TextureSource;
use crate::ExportOptions;

use super::ids::IdAllocator;

/// Build the in-memory FBX tree representing `scene`.
///
/// The shape this returns is the same regardless of `opts.include_textures`
/// — texture-related Objects/Connections are simply omitted when textures
/// are disabled, mirroring the GLB exporter.
pub(super) fn build_tree<F: Fn(&str)>(
    scene: &SceneGraph,
    opts: &ExportOptions,
    texture_source: &dyn TextureSource,
    progress: &F,
) -> Result<Tree> {
    let mut tree = Tree::default();
    let root = tree.root().node_id();
    let mut ids = IdAllocator::new();

    write_header_extension(&mut tree, root);
    write_global_settings(&mut tree, root);
    write_documents(&mut tree, root, &mut ids);

    // Plan: we run the object emitters in dependency order, each one
    // returning the IDs and connection edges it produced. The Definitions
    // block just needs final counts per type, which we accumulate as we
    // go and serialise *before* the Objects/Connections nodes.
    //
    // To keep the writer single-pass but the emitters self-contained, we
    // build Objects + Connections into intermediate `Vec`s first, then
    // commit them to the tree once Definitions is in place.

    let mut emit = ObjectEmitter::new();

    progress("collecting fbx objects");

    // 1. Models for every SceneNode (parents → children, but emission order
    //    doesn't matter for FBX — connections describe parentage).
    let model_ids = super::nodes::emit_models(scene, &mut ids, &mut emit);

    // 2. Geometry per (skinned mesh OR per unique non-skinned mesh-key).
    let mesh_table = super::mesh::emit_geometries(scene, &model_ids, &mut ids, &mut emit);

    // 3. Lights.
    super::light::emit_lights(scene, &model_ids, &mut ids, &mut emit);

    // 4. Materials.
    let texture_indices = super::material::emit_materials(
        scene,
        &model_ids,
        &mut ids,
        &mut emit,
        opts,
        texture_source,
    )?;

    // 5. Textures + Videos. Only when textures are enabled — the table just
    //    above is empty otherwise.
    super::texture::emit_textures_and_videos(scene, &texture_indices, &mut ids, &mut emit);

    // 6. Skin deformers + clusters.
    super::skin::emit_skins(scene, &model_ids, &mesh_table, &mut ids, &mut emit);

    // 7. Animation stacks/layers/curves.
    if opts.include_animations {
        super::anim::emit_animations(scene, &model_ids, &mut ids, &mut emit);
    }

    // Now write Definitions using the accumulated counts, then Objects and
    // Connections.
    write_definitions(&mut tree, root, &emit);
    write_objects(&mut tree, root, emit.objects);
    write_connections(&mut tree, root, &emit.connections);
    write_takes(&mut tree, root);

    Ok(tree)
}

/// Whether a connection is object-object (parent/child) or object-property
/// (a property slot on the parent).
#[derive(Debug, Clone, Copy)]
pub(super) enum ConnKind {
    /// `;OO`: child is the source, parent is the destination, no property name.
    ObjectObject,
    /// `;OP`: child is the source, parent is the destination, property
    /// identified by `name`.
    ObjectProperty,
}

/// One edge in the FBX `Connections` block.
pub(super) struct Connection {
    pub kind: ConnKind,
    pub child_id: i64,
    pub parent_id: i64,
    /// Only used when `kind == ObjectProperty`. Empty for `OO`.
    pub property: String,
}

/// One Objects-block node we've decided to emit. Each is built up to a
/// fully-shaped subtree — the doc module just splices them under the
/// `Objects` parent verbatim. `kind` is what gets counted in Definitions.
pub(super) struct ObjectNode {
    /// Object type as it appears in the `Objects` block (and counted in
    /// Definitions): "Geometry", "Model", "Material", "Texture", "Video",
    /// "NodeAttribute", "Deformer", "AnimationStack", "AnimationLayer",
    /// "AnimationCurveNode", "AnimationCurve".
    pub kind: &'static str,
    /// Build the subtree under `parent` in `tree`. The emitter is a
    /// boxed closure so callers can capture whatever owned data they
    /// need without the trait-object dance forcing `'static` references
    /// on the scene.
    pub emit: Box<dyn FnOnce(&mut Tree, NodeId)>,
}

/// Accumulator the per-object emitters push into. Holds Objects-block
/// children + Connections-block edges + per-type counts so Definitions can
/// be sized correctly.
pub(super) struct ObjectEmitter {
    pub objects: Vec<ObjectNode>,
    pub connections: Vec<Connection>,
}

impl ObjectEmitter {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
            connections: Vec::new(),
        }
    }

    pub fn push_object(&mut self, kind: &'static str, emit: Box<dyn FnOnce(&mut Tree, NodeId)>) {
        self.objects.push(ObjectNode { kind, emit });
    }

    pub fn connect_oo(&mut self, child_id: i64, parent_id: i64) {
        self.connections.push(Connection {
            kind: ConnKind::ObjectObject,
            child_id,
            parent_id,
            property: String::new(),
        });
    }

    pub fn connect_op(&mut self, child_id: i64, parent_id: i64, property: impl Into<String>) {
        self.connections.push(Connection {
            kind: ConnKind::ObjectProperty,
            child_id,
            parent_id,
            property: property.into(),
        });
    }

    /// Tally objects by `kind` for the Definitions block.
    fn type_counts(&self) -> Vec<(&'static str, i32)> {
        use std::collections::BTreeMap;
        let mut counts: BTreeMap<&'static str, i32> = BTreeMap::new();
        for obj in &self.objects {
            *counts.entry(obj.kind).or_insert(0) += 1;
        }
        counts.into_iter().collect()
    }
}

// ---------------------------------------------------------------------------
// Section emitters
// ---------------------------------------------------------------------------

fn write_header_extension(tree: &mut Tree, root: NodeId) {
    let hdr = tree.append_new(root, "FBXHeaderExtension");
    let v = tree.append_new(hdr, "FBXHeaderVersion");
    tree.append_attribute(v, 1003i32);
    let v = tree.append_new(hdr, "FBXVersion");
    tree.append_attribute(v, 7400i32);
    let v = tree.append_new(hdr, "EncryptionType");
    tree.append_attribute(v, 0i32);

    let creator = tree.append_new(hdr, "Creator");
    tree.append_attribute(creator, "MoGen FBX exporter");

    // SceneInfo block. Blender's importer reads this for the asset metadata
    // panel and tolerates a sparse one. We populate enough that
    // GlobalSettings doesn't end up alone with no document context.
    let info = tree.append_new(hdr, "SceneInfo");
    tree.append_attribute(info, "GlobalInfo\u{0}\u{1}SceneInfo");
    tree.append_attribute(info, "UserData");
    let ty = tree.append_new(info, "Type");
    tree.append_attribute(ty, "UserData");
    let ver = tree.append_new(info, "Version");
    tree.append_attribute(ver, 100i32);
    let meta = tree.append_new(info, "MetaData");
    let meta_v = tree.append_new(meta, "Version");
    tree.append_attribute(meta_v, 100i32);
    for (name, val) in [
        ("Title", ""),
        ("Subject", ""),
        ("Author", ""),
        ("Keywords", ""),
        ("Revision", ""),
        ("Comment", ""),
    ] {
        let n = tree.append_new(meta, name);
        tree.append_attribute(n, val);
    }
    write_properties70(tree, info, |t, props| {
        push_prop(t, props, "DocumentUrl", "KString", "Url", "", AttributeValue::String(String::new()));
        push_prop(t, props, "SrcDocumentUrl", "KString", "Url", "", AttributeValue::String(String::new()));
        push_prop(t, props, "Original", "Compound", "", "", AttributeValue::String(String::new()));
        push_prop(t, props, "LastSaved", "Compound", "", "", AttributeValue::String(String::new()));
    });
}

fn write_global_settings(tree: &mut Tree, root: NodeId) {
    let gs = tree.append_new(root, "GlobalSettings");
    let v = tree.append_new(gs, "Version");
    tree.append_attribute(v, 1000i32);
    write_properties70(tree, gs, |t, props| {
        // Y-up, right-handed, -Z forward, meters. Matches glTF and the
        // codebase's de-facto convention.
        push_prop(t, props, "UpAxis", "int", "Integer", "", AttributeValue::I32(1));
        push_prop(t, props, "UpAxisSign", "int", "Integer", "", AttributeValue::I32(1));
        push_prop(t, props, "FrontAxis", "int", "Integer", "", AttributeValue::I32(2));
        push_prop(t, props, "FrontAxisSign", "int", "Integer", "", AttributeValue::I32(1));
        push_prop(t, props, "CoordAxis", "int", "Integer", "", AttributeValue::I32(0));
        push_prop(t, props, "CoordAxisSign", "int", "Integer", "", AttributeValue::I32(1));
        push_prop(t, props, "OriginalUpAxis", "int", "Integer", "", AttributeValue::I32(1));
        push_prop(t, props, "OriginalUpAxisSign", "int", "Integer", "", AttributeValue::I32(1));
        push_prop(t, props, "UnitScaleFactor", "double", "Number", "", AttributeValue::F64(1.0));
        push_prop(t, props, "OriginalUnitScaleFactor", "double", "Number", "", AttributeValue::F64(1.0));
        push_prop(t, props, "AmbientColor", "ColorRGB", "Color", "", AttributeValue::F64(0.0));
        push_prop(t, props, "DefaultCamera", "KString", "", "", AttributeValue::String("Producer Perspective".into()));
        push_prop(t, props, "TimeMode", "enum", "", "", AttributeValue::I32(11));
        push_prop(t, props, "TimeProtocol", "enum", "", "", AttributeValue::I32(2));
        push_prop(t, props, "SnapOnFrameMode", "enum", "", "", AttributeValue::I32(0));
        push_prop(t, props, "TimeSpanStart", "KTime", "Time", "", AttributeValue::I64(0));
        push_prop(t, props, "TimeSpanStop", "KTime", "Time", "", AttributeValue::I64(super::anim::FBX_TICKS_PER_SECOND));
        push_prop(t, props, "CustomFrameRate", "double", "Number", "", AttributeValue::F64(30.0));
        push_prop(t, props, "TimeMarker", "Compound", "", "", AttributeValue::String(String::new()));
        push_prop(t, props, "CurrentTimeMarker", "int", "Integer", "", AttributeValue::I32(-1));
    });
}

fn write_documents(tree: &mut Tree, root: NodeId, ids: &mut IdAllocator) {
    let docs = tree.append_new(root, "Documents");
    let count = tree.append_new(docs, "Count");
    tree.append_attribute(count, 1i32);

    let document_id = ids.alloc();
    let doc = tree.append_new(docs, "Document");
    tree.append_attribute(doc, document_id);
    tree.append_attribute(doc, "Scene\u{0}\u{1}Document");
    tree.append_attribute(doc, "Scene");
    write_properties70(tree, doc, |t, props| {
        push_prop(t, props, "SourceObject", "object", "", "", AttributeValue::String(String::new()));
        push_prop(t, props, "ActiveAnimStackName", "KString", "", "", AttributeValue::String(String::new()));
    });
    let root_node = tree.append_new(doc, "RootNode");
    tree.append_attribute(root_node, 0i64);
}

fn write_definitions(tree: &mut Tree, root: NodeId, emit: &ObjectEmitter) {
    let defs = tree.append_new(root, "Definitions");
    let v = tree.append_new(defs, "Version");
    tree.append_attribute(v, 100i32);

    let counts = emit.type_counts();
    // +1 for the implicit GlobalSettings ObjectType, which we always count
    // even though it's not in the Objects block — Blender's importer is
    // tolerant of mismatches but treats it as the canonical answer.
    let total: i32 = counts.iter().map(|(_, c)| c).sum::<i32>() + 1;
    let total_node = tree.append_new(defs, "Count");
    tree.append_attribute(total_node, total);

    // GlobalSettings ObjectType always present.
    let ot = tree.append_new(defs, "ObjectType");
    tree.append_attribute(ot, "GlobalSettings");
    let c = tree.append_new(ot, "Count");
    tree.append_attribute(c, 1i32);

    for (kind, count) in counts {
        let ot = tree.append_new(defs, "ObjectType");
        tree.append_attribute(ot, kind);
        let c = tree.append_new(ot, "Count");
        tree.append_attribute(c, count);
        // We deliberately omit PropertyTemplate per type. Blender tolerates
        // its absence; our object emitters spell out every Properties70
        // entry inline so the importer never needs to consult a template.
    }
}

fn write_objects(tree: &mut Tree, root: NodeId, objects: Vec<ObjectNode>) {
    let parent = tree.append_new(root, "Objects");
    for obj in objects {
        (obj.emit)(tree, parent);
    }
}

fn write_connections(tree: &mut Tree, root: NodeId, edges: &[Connection]) {
    let parent = tree.append_new(root, "Connections");
    for edge in edges {
        let c = tree.append_new(parent, "C");
        match edge.kind {
            ConnKind::ObjectObject => {
                tree.append_attribute(c, "OO");
                tree.append_attribute(c, edge.child_id);
                tree.append_attribute(c, edge.parent_id);
            }
            ConnKind::ObjectProperty => {
                tree.append_attribute(c, "OP");
                tree.append_attribute(c, edge.child_id);
                tree.append_attribute(c, edge.parent_id);
                tree.append_attribute(c, edge.property.clone());
            }
        }
    }
}

fn write_takes(tree: &mut Tree, root: NodeId) {
    let takes = tree.append_new(root, "Takes");
    let cur = tree.append_new(takes, "Current");
    tree.append_attribute(cur, "");
}

// ---------------------------------------------------------------------------
// Properties70 helpers
// ---------------------------------------------------------------------------

/// Build a `Properties70` child under `parent` and pass it to `body` so the
/// caller can push individual `P` rows. Matches the FBX convention that
/// every property block lives under a `Properties70` wrapper.
pub(super) fn write_properties70<F>(tree: &mut Tree, parent: NodeId, body: F)
where
    F: FnOnce(&mut Tree, NodeId),
{
    let props = tree.append_new(parent, "Properties70");
    body(tree, props);
}

/// Append one `P` (property) row. Layout: `name | type | secondary_type | flags | value...`.
/// Numeric properties (Vector3D, Color, Number, etc.) take three or one
/// f64 values. We expose the value as a single `AttributeValue` for
/// scalar/string props; vector helpers below handle the multi-value case.
pub(super) fn push_prop(
    tree: &mut Tree,
    props: NodeId,
    name: &str,
    ty: &str,
    secondary: &str,
    flags: &str,
    value: AttributeValue,
) {
    let p = tree.append_new(props, "P");
    tree.append_attribute(p, name);
    tree.append_attribute(p, ty);
    tree.append_attribute(p, secondary);
    tree.append_attribute(p, flags);
    tree.append_attribute(p, value);
}

/// 3-component f64 property (Vector3D, Color, ColorRGB, Lcl Translation/Rotation/Scaling).
pub(super) fn push_prop_vec3(
    tree: &mut Tree,
    props: NodeId,
    name: &str,
    ty: &str,
    secondary: &str,
    flags: &str,
    value: [f64; 3],
) {
    let p = tree.append_new(props, "P");
    tree.append_attribute(p, name);
    tree.append_attribute(p, ty);
    tree.append_attribute(p, secondary);
    tree.append_attribute(p, flags);
    tree.append_attribute(p, value[0]);
    tree.append_attribute(p, value[1]);
    tree.append_attribute(p, value[2]);
}
