use super::{dependencies, revision, LockKind, PartLock};
use anyhow::{bail, Context, Result};
use mogen_core::{NodeId, SceneGraph};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub use mogen_core::AssetView as View;
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelingTool {
    Inspect {
        revision: String,
        name: Option<String>,
    },
    Measure {
        revision: String,
        first: String,
        second: String,
        tolerance: Option<f64>,
        max_work: Option<usize>,
    },
    Documentation {
        topic: String,
    },
    Apply {
        revision: String,
        edits: String,
    },
    Compile {
        revision: String,
    },
    Render {
        revision: String,
        view: View,
        name: Option<String>,
    },
    Finish {
        revision: String,
        findings: String,
    },
}
/// Application-driven protocol works on every text backend; providers do not
/// receive filesystem or shell tools. Each response contains exactly one call.
pub const TOOL_INSTRUCTIONS: &str = r#"Modeling operations: return exactly one JSON object per turn.
{"tool":"inspect","revision":"current revision","name":null} returns hierarchy, transforms, bounds and materials.
{"tool":"measure","revision":"current revision","first":"foot","second":"leg","tolerance":0.002,"max_work":100000} returns current world triangle-surface distance, closest points and authored fit constraints; tolerance is in metres. Use this for suspected gaps, not bounding-box overlap.
{"tool":"documentation","topic":"loft"} returns relevant DSL documentation.
{"tool":"apply","revision":"current revision","edits":"SEARCH/REPLACE blocks or full DSL"} stages an atomic edit.
{"tool":"compile","revision":"current revision"} returns diagnostics.
{"tool":"render","revision":"current revision","view":"front|side|back|three_quarter|rear_three_quarter|presentation","name":null} returns the current render; set name to a uniquely named part for a close-up.
{"tool":"finish","revision":"current revision","findings":"concrete defects corrected or limitations"} finishes the candidate.
Inspect before editing. Use the returned revision for every subsequent operation.
Errors are recoverable: inspect the tool result and correct arguments. Preserve approved locks and unrelated source.
Do not claim quality based on compilation alone. Inspect silhouette, dimensions, required parts, joints, negative space, then surfaces and materials.
"#;

pub fn compile(source: &str, base: Option<&Path>) -> Result<SceneGraph> {
    compile_with_connectivity(source, base, true)
}
fn compile_with_connectivity(
    source: &str,
    base: Option<&Path>,
    strict: bool,
) -> Result<SceneGraph> {
    // Check dependency scope before the compiler opens imports or textures.
    dependencies(source, base)?;
    let preview = mogen_dsl::synthesise_standalone_module_use(source);
    let ast = mogen_dsl::parse(preview.as_deref().unwrap_or(source))?;
    let diagnostics = mogen_validate::validate_ast_with_source(&ast, base);
    if mogen_core::has_errors(&diagnostics) {
        bail!(
            "{}",
            mogen_validate::render_json("session.mog", &diagnostics)
        );
    }
    let scene = mogen_dsl::lower_with_source(&ast, base)?;
    let diagnostics = mogen_validate::validate_graph(&scene);
    if mogen_core::has_mesh_contract_errors(&diagnostics) {
        return Err(mogen_core::MeshContractError { diagnostics }.into());
    }
    if diagnostics
        .iter()
        .any(|d| d.severity == mogen_core::Severity::Error && (strict || d.code != "E1101"))
    {
        bail!(
            "{}",
            mogen_validate::render_json("session.mog", &diagnostics)
        );
    }
    Ok(scene)
}
fn unique_node(scene: &SceneGraph, name: &str) -> Result<NodeId> {
    let ids: Vec<_> = scene
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.name == name)
        .map(|(i, _)| NodeId(i as u32))
        .collect();
    if ids.len() != 1 {
        bail!("Part '{name}' is missing or ambiguous; select a uniquely named part");
    }
    Ok(ids[0])
}
fn fingerprint(scene: &SceneGraph, id: NodeId, kind: LockKind) -> Value {
    let node = scene.get(id);
    let world = scene.world_transforms()[id.0 as usize];
    let material = node.material.map(|id| {
        let mut v = serde_json::to_value(&scene.materials[id.0 as usize]).unwrap();
        if let Some(o) = v.as_object_mut() {
            o.remove("source_span");
            o.remove("origin");
        }
        v
    });
    let children: Vec<_> = node
        .children
        .iter()
        .map(|id| fingerprint(scene, *id, kind))
        .collect();
    match kind {
        LockKind::Geometry => {
            json!({"name":node.name,"geometry":node.mesh.as_ref().map(|m|json!({"positions":m.positions,"normals":m.normals,"indices":m.indices})),"children":children})
        }
        LockKind::Transform => json!({"name":node.name,"world":world,"children":children}),
        LockKind::Material => json!({"name":node.name,"material":material,"children":children}),
        LockKind::Subtree => {
            json!({"name":node.name,"mesh":node.mesh,"world":world,"material":material,"children":children})
        }
    }
}
pub fn enforce_locks(
    before: &str,
    after: &str,
    locks: &[PartLock],
    base: Option<&Path>,
) -> Result<()> {
    if locks.is_empty() {
        return Ok(());
    }
    let a = compile(before, base)?;
    let b = compile(after, base)?;
    for lock in locks {
        let ai = unique_node(&a, &lock.name)?;
        let bi = unique_node(&b, &lock.name)?;
        if fingerprint(&a, ai, lock.kind) != fingerprint(&b, bi, lock.kind) {
            bail!("Edit changes locked {:?} on '{}', possibly through a shared material or dependency", lock.kind, lock.name);
        }
    }
    Ok(())
}
pub fn enforce_scope(before: &str, after: &str, selected: Option<&str>) -> Result<()> {
    let Some(name) = selected else {
        return Ok(());
    };
    fn span(src: &str, name: &str) -> Result<mogen_core::Span> {
        fn walk(ns: &[mogen_dsl::ast::Node], name: &str, found: &mut Vec<mogen_core::Span>) {
            for n in ns {
                if n.name.as_deref() == Some(name) {
                    found.push(n.span);
                }
                walk(&n.children, name, found);
            }
        }
        let mut found = vec![];
        walk(&mogen_dsl::parse(src)?, name, &mut found);
        if found.len() != 1 {
            bail!("Focused edit requires one authored part named '{name}'");
        }
        Ok(found[0])
    }
    let a = span(before, name)?;
    let b = span(after, name)?;
    if before[..a.start] != after[..b.start] || before[a.end..] != after[b.end..] {
        bail!("Focused edit changes source outside '{name}'; shared dependencies must be edited separately");
    }
    Ok(())
}
pub struct ModelingWorkspace {
    pub source: String,
    pub base: Option<PathBuf>,
    pub locks: Vec<PartLock>,
    pub selected: Option<String>,
    initial_dependencies: std::collections::BTreeMap<PathBuf, Vec<u8>>,
}
impl ModelingWorkspace {
    pub fn new(
        source: String,
        base: Option<PathBuf>,
        locks: Vec<PartLock>,
        selected: Option<String>,
    ) -> Result<Self> {
        let initial_dependencies = dependencies(&source, base.as_deref())?;
        Ok(Self {
            source,
            base,
            locks,
            selected,
            initial_dependencies,
        })
    }
    pub fn revision(&self) -> Result<String> {
        Ok(revision(
            &self.source,
            &dependencies(&self.source, self.base.as_deref())?,
        ))
    }
    pub fn check_revision(&self, expected: &str) -> Result<()> {
        if self.revision()? != expected {
            bail!("Stale source/dependency revision; inspect again");
        }
        for (path, bytes) in &self.initial_dependencies {
            if std::fs::read(self.base.as_deref().unwrap_or(Path::new(".")).join(path))? != *bytes {
                bail!("Dependency changed during session: {}", path.display());
            }
        }
        Ok(())
    }
    pub fn apply(&mut self, expected: &str, response: &str) -> Result<Value> {
        self.check_revision(expected)?;
        let next = if response.contains("<<<<<<< SEARCH") {
            crate::repair::apply_edit_blocks(
                &self.source,
                &crate::repair::parse_edit_blocks(response)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?,
            )
            .map_err(|e| anyhow::anyhow!("{e:?}"))?
        } else {
            crate::repair::strip_markdown_fences(response)
        };
        enforce_scope(&self.source, &next, self.selected.as_deref())?;
        enforce_locks(&self.source, &next, &self.locks, self.base.as_deref())?;
        compile(&next, self.base.as_deref()).context("Candidate rejected; source unchanged")?;
        self.source = next;
        Ok(json!({"revision":self.revision()?,"applied":true}))
    }
    pub fn inspect(&self, name: Option<&str>) -> Result<Value> {
        let revision = self.revision()?;
        self.check_revision(&revision)?;
        let scene = compile_with_connectivity(&self.source, self.base.as_deref(), false)?;
        let measurements = mogen_core::world_part_measurements(&scene);
        if let Some(n) = name {
            unique_node(&scene, n)?;
        }
        let world = scene.world_transforms();
        let parts:Vec<_>=scene.nodes.iter().enumerate().filter(|(_,n)|name.is_none_or(|s|s==n.name)).map(|(i,n)|json!({
            "name":n.name,"kind":n.kind,"parent":n.parent.map(|id|scene.get(id).name.clone()),
            "children":n.children.iter().map(|id|scene.get(*id).name.clone()).collect::<Vec<_>>(),
            "transform":n.transform,"world":world[i],"bounds":n.mesh.as_ref().map(mogen_core::Aabb::from_mesh),
            "world_measurements":measurements[i],
            "material":n.material.map(|id|&scene.materials[id.0 as usize]),"path_frame":n.path_frame
        })).collect();
        self.check_revision(&revision)?;
        Ok(
            json!({"revision":revision,"units":"m","space":"world","ground_plane":"Y=0","parts":parts,"diagnostics":mogen_validate::validate_graph(&scene),"relationship_checks":mogen_core::relationship_measurements(&scene),"relationships":scene.relationships,"guides":scene.guides.iter().map(|g|json!({"name":g.name,"target":scene.get(g.target).name,"section":g.section,"closed":g.closed,"tolerance":g.tolerance,"samples":g.points.len()})).collect::<Vec<_>>(),"locks":self.locks,"selected":self.selected}),
        )
    }

    pub fn measure(
        &self,
        expected: &str,
        first: &str,
        second: &str,
        tolerance: Option<f64>,
        max_work: Option<usize>,
    ) -> Result<Value> {
        self.check_revision(expected)?;
        let scene = compile_with_connectivity(&self.source, self.base.as_deref(), false)?;
        let a = unique_node(&scene, first)?;
        let b = unique_node(&scene, second)?;
        let options = mogen_geom::measure::MeasureOptions {
            tolerance: tolerance.unwrap_or(0.002),
            max_work: max_work.unwrap_or(100_000),
            ..Default::default()
        };
        let distance = mogen_geom::measure::measure_surfaces(&scene, a, b, options)?;
        let parts = mogen_core::world_part_measurements(&scene);
        let checks: Vec<_> = mogen_core::relationship_measurements(&scene)
            .into_iter()
            .filter(|r| {
                (r.child == first && r.target == second) || (r.child == second && r.target == first)
            })
            .collect();
        self.check_revision(expected)?;
        Ok(
            json!({"revision":expected,"units":"m","space":"world","ground_plane":"Y=0",
            "first":first,"second":second,"first_measurements":parts[a.0 as usize],"second_measurements":parts[b.0 as usize],
            "surface":distance,"relationship_checks":checks,
            "policy":"unsigned triangle-surface distance; intentional insertion is allowed; containment, penetration depth and aesthetic fit are not inferred",
            "evidence":"current static tessellated geometry","cached":false,
            "limits":{"max_work":options.max_work,"max_triangles_per_part":options.max_triangles}}),
        )
    }
}
pub fn documentation(topic: &str) -> Result<String> {
    match topic.to_ascii_lowercase().as_str() {
        "measure" | "measurements" | "fit" => {
            return Ok(include_str!("../../../../docs/fit-measurements.md").into())
        }
        "guide" | "welt" | "surface details" => {
            return Ok(include_str!("../../../../docs/surface-guides.md").into())
        }
        "relate" | "relationships" => {
            return Ok(include_str!("../../../../docs/relational-modeling.md").into())
        }
        "frame_up" | "path frames" => {
            return Ok(include_str!("../../../../docs/sweep-frames.md").into())
        }
        _ => {}
    }

    let topic = topic.trim().to_lowercase();
    if topic.len() < 3 || topic.len() > 80 {
        bail!("Use a specific DSL operation or technique (3–80 characters)");
    }
    let recipe = match topic.as_str() {
        "upholstery" | "cushion" => Some(include_str!(
            "../../../../benches/quality/targets/upholstery.mog"
        )),
        "hollow vessel" | "vessel" => Some(include_str!(
            "../../../../benches/quality/targets/hollow_vessel.mog"
        )),
        "organic" => Some(include_str!(
            "../../../../benches/quality/targets/organic.mog"
        )),
        "curved frame" | "sweep" => Some(include_str!(
            "../../../../examples/features/curved_moulding.mog"
        )),
        "shaped surface" | "loft" => {
            Some(include_str!("../../../../examples/vehicles/boat_hull.mog"))
        }
        _ => None,
    };
    let docs = include_str!("../../../../docs/dsl.md");
    let lines: Vec<_> = docs.lines().collect();
    let mut out = recipe
        .map(|s| format!("Technique example (validate fit against the target):\n{s}\n"))
        .unwrap_or_default();
    for (i, line) in lines.iter().enumerate() {
        if line.to_lowercase().contains(&topic) {
            out.push_str(&lines[i.saturating_sub(2)..(i + 18).min(lines.len())].join("\n"));
            out.push('\n');
            if out.len() > 12000 {
                break;
            }
        }
    }
    if out.is_empty() {
        bail!("No documentation for {topic}");
    }
    Ok(out)
}

/// Shape inspection uses neutral opaque surfaces under the same fixed lighting.
/// The presentation view retains authored materials and texture scale.
pub fn inspection_scene(mut scene: SceneGraph, view: View) -> SceneGraph {
    if view != View::Presentation {
        for node in &mut scene.nodes {
            if let Some(mesh) = &mut node.mesh {
                mesh.colors.clear();
            }
        }
        for material in &mut scene.materials {
            material.base_color = [0.65, 0.65, 0.65, 1.0];
            material.metallic = 0.0;
            material.roughness = 0.8;
            material.transmission = 0.0;
            material.emissive = [0.0; 3];
            material.emissive_strength = 0.0;
            material.alpha_mode = mogen_core::AlphaMode::Opaque;
            material.shader_name = None;
            material.gradient = None;
            material.shader_params.clear();
            for slot in material.texture_slots_mut() {
                *slot = None;
            }
        }
    }
    scene
}

/// World-space framing for an authored part and its descendants.
pub fn part_framing(scene: &SceneGraph, name: &str) -> Result<([f32; 3], f32)> {
    let id = unique_node(scene, name)?;
    let local =
        mogen_core::subtree_local_aabb(scene, id).context("Selected part has no geometry")?;
    let world = scene.world_transforms()[id.0 as usize];
    let mut bounds = mogen_core::Aabb::empty();
    for corner in local.corners() {
        bounds.expand(world.transform_point3(corner));
    }
    Ok((
        bounds.center().to_array(),
        (bounds.max - bounds.min).length().max(0.002) * 0.5,
    ))
}

#[cfg(test)]
mod mesh_contract_tests {
    #[test]
    fn session_preserves_mesh_contract_diagnostics() {
        let error = super::compile(
            "scene { box \"collapsed\" (size=[1,1,1],scale=[0,1,1]) }",
            None,
        )
        .unwrap_err();
        let contract = error
            .downcast_ref::<mogen_core::MeshContractError>()
            .unwrap();
        assert!(contract
            .diagnostics
            .iter()
            .any(|d| d.code == "E1201" && d.message.contains("collapsed")));
        let serialized: serde_json::Value = serde_json::from_str(&contract.to_string()).unwrap();
        assert_eq!(serialized["mesh_contract"], "failed");
    }
}
