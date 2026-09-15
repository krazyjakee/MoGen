//! Opt-in reference-based evaluation. Default is credential-free fixture mode.
use anyhow::{bail, Context, Result};
use clap::Parser;
use mogen_llm::session::*;
use mogen_llm::{GenerateConfig, LlmClient, Provider};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "benches/quality/tasks.json")]
    manifest: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    live: bool,
    #[arg(long,default_value="baseline",value_parser=["baseline","refined","guidance"])]
    variant: String,
    #[arg(long, default_value = "openai")]
    provider: String,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, default_value_t = 1)]
    repeats: u32,
    #[arg(long, default_value_t = 12)]
    calls: u32,
    #[arg(long, default_value_t = 600)]
    seconds: u64,
    #[arg(long, default_value_t = 3)]
    iterations: u32,
    #[arg(long)]
    spend_usd: Option<f64>,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    #[arg(long)]
    no_render: bool,
}
#[derive(Deserialize)]
struct Manifest {
    version: u32,
    provenance: String,
    tasks: Vec<Task>,
}
#[derive(Deserialize)]
struct Task {
    id: String,
    source: PathBuf,
    /// Optional deterministic candidate compared against the source reference.
    #[serde(default)]
    candidate_source: Option<PathBuf>,
    prompt: String,
    required: Vec<String>,
    max_extent: [f32; 3],
    /// Optional, bounded surface measurements; no aesthetic pass/fail threshold.
    #[serde(default)]
    measure_pairs: Vec<[String; 2]>,
}
#[path = "../src/session_render.rs"]
mod session_render;
use session_render::Renderer;

fn asset_checks(source: &str, base: &Path, task: &Task) -> Result<serde_json::Value> {
    let snapshot = dependencies(source, Some(base))?;
    let source_revision = revision(source, &snapshot);
    let scene = match compile(source, Some(base)) {
        Ok(scene) => scene,
        Err(error) => {
            if let Some(contract) = error.downcast_ref::<mogen_core::MeshContractError>() {
                return Ok(json!({"compiles":false,"asset_pass":false,
                    "mesh_contract":{"pass":false,"diagnostics":contract.diagnostics},
                    "visual_quality":"not evaluated: mesh contract failed"}));
            }
            return Err(error);
        }
    };
    let contract = mogen_core::validate_renderable_scene(&scene);
    let missing: Vec<_> = task
        .required
        .iter()
        .filter(|name| !scene.nodes.iter().any(|n| &n.name == *name))
        .collect();
    let mesh = mogen_render::flatten(&scene, Some(base));
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    // The renderer's bounding sphere is supplemented by transformed positions
    // so dimensions are measured in world coordinates, not local primitive space.
    let worlds = scene.world_transforms();
    for (i, node) in scene.nodes.iter().enumerate() {
        if let Some(m) = &node.mesh {
            for p in &m.positions {
                let q = worlds[i].transform_point3((*p).into()).to_array();
                for k in 0..3 {
                    min[k] = min[k].min(q[k]);
                    max[k] = max[k].max(q[k]);
                }
            }
        }
    }
    let extent = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let finite = extent.iter().all(|v| v.is_finite());
    let dimensions_ok = finite && extent.iter().zip(task.max_extent).all(|(a, b)| *a <= b);
    let relationship_checks = mogen_core::relationship_measurements(&scene);
    let constraints_ok = relationship_checks.iter().all(|r| r.satisfied);
    let mut surface_measurements = Vec::new();
    if task.measure_pairs.len() > 16 {
        bail!("quality task permits at most 16 requested measurement pairs");
    }
    for [first, second] in &task.measure_pairs {
        let find = |name: &str| -> Result<mogen_core::NodeId> {
            let matches: Vec<_> = scene
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| n.name == name)
                .collect();
            if matches.len() != 1 {
                bail!("Measurement part {name:?} is missing or ambiguous");
            }
            Ok(mogen_core::NodeId(matches[0].0 as u32))
        };
        let result = mogen_geom::measure::measure_surfaces(
            &scene,
            find(first)?,
            find(second)?,
            Default::default(),
        )?;
        surface_measurements.push(json!({"first":first,"second":second,"surface":result}));
    }
    if dependencies(source, Some(base))? != snapshot {
        bail!("Dependencies changed during measurement; retry evaluation");
    }
    Ok(
        json!({"compiles":true,"mesh_contract":{"pass":true,"diagnostics":contract},"finite":finite,"missing_parts":missing,"extent":extent,"dimensions_ok":dimensions_ok,
        "asset_pass":missing.is_empty()&&dimensions_ok&&constraints_ok,
        "measurements":{"revision":source_revision,"units":"m","space":"world","ground_plane":"Y=0","parts":mogen_core::world_part_measurements(&scene),"relationships":relationship_checks,"constraints_ok":constraints_ok,"surface_pairs":surface_measurements,"evidence":"static tessellated geometry; unsigned distance does not infer penetration or aesthetic fit"},"framing_radius":mesh.radius,"visual_quality":"requires blinded human review"}),
    )
}
fn main() -> Result<()> {
    let args = Args::parse();
    if args.repeats == 0 {
        bail!("repeats must be positive");
    }
    if args.out.exists() {
        bail!("Choose a new output directory so previous evaluation artifacts remain intact");
    }
    let manifest_bytes = std::fs::read(&args.manifest)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.version != 1 {
        bail!("Unsupported manifest version");
    }
    let root = args
        .manifest
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    std::fs::create_dir_all(&args.out)?;
    std::fs::write(args.out.join("manifest.json"), &manifest_bytes)?;
    let provider = Provider::parse(&args.provider).context("Unknown provider")?;
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| provider.default_model().into());
    let client = if args.live {
        let key = std::env::var(provider.env_var()).unwrap_or_default();
        if !provider.is_keyless() && key.is_empty() {
            bail!("Set {} before running --live", provider.env_var());
        }
        let c = LlmClient::new(provider, key);
        if let Ok(rec) = mogen_llm::SqliteRecorder::open_default() {
            let _ = mogen_llm::spend::install_global(std::sync::Arc::new(rec));
        }
        Some(c)
    } else {
        None
    };
    let mut rows = vec![];
    let mut html=String::from("<!doctype html><meta charset=utf-8><title>MoGen quality evaluation</title><style>body{font:16px system-ui;max-width:1100px;margin:auto}img{width:220px}article{border-top:1px solid #aaa;padding:20px}textarea{width:95%;height:60px}</style><h1>MoGen reference evaluation</h1><p>Judge silhouette, proportions, completeness, joints/negative space, finish, materials/UV scale, and reference fidelity. Record preference, uncertainty and disagreement before opening run metadata. Fixture outputs are authored controls, not AI performance measurements.</p>");
    for task in manifest.tasks {
        if !task
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            bail!("Invalid task id");
        }
        let source = std::fs::read_to_string(root.join(&task.source))?;
        let base = root.join(&task.source).parent().unwrap().to_path_buf();
        for repeat in 0..args.repeats {
            let dir = args.out.join(format!("{}-{repeat}", task.id));
            std::fs::create_dir_all(&dir)?;
            let mut renderer = Renderer {
                base: base.clone(),
                framing: None,
                front_yaw: None,
                last_capture: None,
                fit_image: None,
                capture_part: None,
            };
            let mut project = ModelingProject::default();
            project.brief.prompt = task.prompt.clone();
            project.limits = SessionLimits {
                calls: args.calls,
                seconds: args.seconds,
                iterations: args.iterations,
                spend_usd: args.spend_usd,
                ..Default::default()
            };
            let mut reference_views = vec![];
            if !args.no_render {
                for view in View::ALL {
                    let img = renderer.render(&source, &capture_revision(&source, &base)?, view)?;
                    std::fs::write(
                        dir.join(format!("reference-{}.png", view.label())),
                        &img.data,
                    )?;
                    std::fs::write(
                        dir.join(format!("reference-{}.camera.json", view.label())),
                        serde_json::to_vec_pretty(&renderer.last_capture)?,
                    )?;
                    reference_views.push(img);
                }
                project
                    .brief
                    .add_reference("reference presentation".into(), reference_views[4].clone());
            }
            let control = SessionControl::new(project.limits.clone());
            let mut cfg = GenerateConfig::new(&task.prompt);
            cfg.model = model.clone();
            cfg.seed = Some(args.seed + repeat as u64);
            cfg.session_control = Some(control.clone());
            cfg.max_output_tokens = Some(project.limits.output_tokens);
            cfg.system_instruction = Some(if args.variant == "guidance" {
                experimental_system_instruction()
            } else {
                mogen_llm::system_instruction(&mogen_llm::StdlibIndex::from_registry(
                    mogen_dsl::stdlib_registry(),
                ))
            });
            cfg.spend_context = mogen_llm::CallContext::new(mogen_llm::Operation::Generate)
                .with_scene(dir.join("candidate.mog").display().to_string())
                .with_session(format!("quality-{}-{}-{repeat}", args.variant, task.id));
            project.brief.attach(&mut cfg)?;
            std::fs::write(
                dir.join("system-prompt.txt"),
                cfg.system_instruction.as_deref().unwrap(),
            )?;
            let started = std::time::Instant::now();
            let result = (|| -> Result<String> {
                let Some(client) = &client else {
                    return if let Some(path) = &task.candidate_source {
                        Ok(std::fs::read_to_string(base.join(path))?)
                    } else {
                        Ok(source.clone())
                    };
                };
                let generated =
                    mogen_llm::generate_with_repair(client, cfg.clone(), &Default::default())?;
                std::fs::write(dir.join("generation.mog"), &generated.dsl)?;
                if !generated.is_ok() {
                    bail!(
                        "Generation invalid after repairs: {:?}",
                        generated.diagnostics
                    );
                }
                if args.variant == "refined" {
                    if args.no_render {
                        bail!("Refined evaluation requires renders");
                    }
                    refine_session(
                        &mut project,
                        &generated.dsl,
                        Some(&base),
                        &cfg,
                        provider.key(),
                        &mut |cfg| client.generate(cfg).map_err(Into::into),
                        &mut renderer,
                        &mut |p| {
                            let _ = p.save(&dir.join("candidate.mog"));
                        },
                    )
                } else {
                    Ok(generated.dsl)
                }
            })();
            let mut row = json!({"task":task.id,"repeat":repeat,"variant":args.variant,"mode":if args.live{"live"}else{"fixture"},"provider":if args.live {args.provider.as_str()} else {"none"},"model":if args.live {model.as_str()} else {"authored fixture"},"seed":args.seed+repeat as u64,"temperature":cfg.temperature,"thinking":"high","limits":project.limits,"elapsed_seconds":started.elapsed().as_secs_f64(),"meter":control.meter(),"stop_reason":project.stop_reason,"manifest_sha256":identity(&manifest_bytes),"prompt_sha256":identity(cfg.system_instruction.as_ref().unwrap().as_bytes()),"human_review":null});
            match result {
                Ok(candidate) => {
                    std::fs::write(dir.join("candidate.mog"), &candidate)?;
                    row["checks"] = match asset_checks(&candidate, &base, &task) {
                        Ok(v) => v,
                        Err(e) => json!({"asset_pass":false,"error":format!("{e:#}")}),
                    };
                    if let Ok(scene) = compile(&candidate, Some(&base)) {
                        match mogen_export::write_glb(&scene, &dir.join("candidate.glb")) {
                            Ok(()) => row["export_ok"] = json!(true),
                            Err(error) => {
                                row["export_ok"] = json!(false);
                                row["export_error"] = json!(format!("{error:#}"));
                                if let Some(contract) =
                                    error.downcast_ref::<mogen_core::MeshContractError>()
                                {
                                    row["checks"]["asset_pass"] = json!(false);
                                    row["checks"]["mesh_contract"] = json!({"pass":false,"stage":"export","diagnostics":contract.diagnostics});
                                }
                            }
                        }
                        if !args.no_render {
                            for view in View::ALL {
                                let image = renderer.render(
                                    &candidate,
                                    &capture_revision(&candidate, &base)?,
                                    view,
                                )?;
                                std::fs::write(
                                    dir.join(format!("candidate-{}.png", view.label())),
                                    &image.data,
                                )?;
                                std::fs::write(
                                    dir.join(format!("candidate-{}.camera.json", view.label())),
                                    serde_json::to_vec_pretty(&renderer.last_capture)?,
                                )?;
                                if let Some(fit) = &renderer.fit_image {
                                    std::fs::write(
                                        dir.join(format!(
                                            "candidate-{}-diagnostic-fit.png",
                                            view.label()
                                        )),
                                        &fit.data,
                                    )?;
                                }
                            }
                        }
                    }
                }
                Err(e) => row["error"] = json!(format!("{e:#}")),
            }
            let folder = dir.file_name().unwrap().to_string_lossy();
            html.push_str(&format!(
                "<article><h2>{}-{repeat}</h2><p>{}</p>",
                task.id,
                escape(&task.prompt)
            ));
            if !args.no_render {
                for view in View::ALL {
                    html.push_str(&format!("<div>{}<br><img src='{folder}/reference-{}.png'><img src='{folder}/candidate-{}.png'></div>",view.label(),view.label(),view.label()));
                }
            }
            html.push_str("<p>Completeness / silhouette / proportions / joints / finish / materials / fidelity:</p><textarea placeholder='Record observations, uncertainty, and evaluator ID; save judgments in judgments.json'></textarea></article>");
            row["wall_seconds"] = json!(started.elapsed().as_secs_f64());
            std::fs::write(dir.join("run.json"), serde_json::to_vec_pretty(&row)?)?;
            rows.push(row);
        }
    }
    // A valid scene that misses every required chair part is an asset-quality failure.
    let negative = "scene { box \"wrong_object\" (size=[0.1,0.1,0.1]) }";
    let negative_task = Task {
        id: "negative".into(),
        source: PathBuf::new(),
        candidate_source: None,
        prompt: String::new(),
        required: vec!["seat".into(), "back".into()],
        max_extent: [1.0; 3],
        measure_pairs: Vec::new(),
    };
    let negative_check = asset_checks(negative, &root, &negative_task)?;
    if negative_check["asset_pass"] != false {
        bail!("Negative control was not detected");
    }
    std::fs::write(
        args.out.join("negative-control.json"),
        serde_json::to_vec_pretty(&negative_check)?,
    )?;
    let contract_controls = mesh_contract_controls()?;
    std::fs::write(
        args.out.join("mesh-contract-controls.json"),
        serde_json::to_vec_pretty(&contract_controls)?,
    )?;
    std::fs::write(
        args.out.join("report.json"),
        serde_json::to_vec_pretty(
            &json!({"version":1,"provenance":manifest.provenance,"runs":rows,"mesh_contract_controls":contract_controls,"camera_convention_version":mogen_core::CAMERA_CONVENTION_VERSION,"human_review_status":"pending","renderer_caveats":["#105 UV seams","#107 water shader"]}),
        )?,
    )?;
    std::fs::write(args.out.join("review.html"), html)?;
    std::fs::write(args.out.join("judgments.json"),"{\"evaluators\":[],\"preferences\":[],\"uncertainty\":[],\"disagreements\":[],\"follow_up_issues\":[]}")?;
    println!("Evaluation artifact: {}", args.out.display());
    Ok(())
}
fn rev(source: &str) -> String {
    revision(source, &Default::default())
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// Malformed compiled geometry cannot be expressed reliably as valid DSL. Mutate
// a known-good compiled control, retaining the source revision and mutation id.
fn mesh_contract_controls() -> Result<serde_json::Value> {
    let source = "scene { box \"contract_control\" (size=[1,1,1]) }";
    let original = compile(source, None)?;
    let mut rows = vec![];
    for (mutation, expected) in [
        ("zero_normals", "E1204"),
        ("nan_position", "E1202"),
        ("missing_normals", "E1200"),
        ("invalid_index", "E1203"),
        ("nan_uv", "E1205"),
    ] {
        let mut scene = original.clone();
        let mesh = scene
            .nodes
            .iter_mut()
            .find_map(|n| n.mesh.as_mut())
            .unwrap();
        match mutation {
            "zero_normals" => mesh.normals.fill([0.0; 3]),
            "nan_position" => mesh.positions[0][0] = f32::NAN,
            "missing_normals" => mesh.normals.clear(),
            "invalid_index" => mesh.indices[0] = u32::MAX,
            _ => mesh.uvs = vec![[f32::NAN, 0.0]; mesh.positions.len()],
        }
        let diagnostics = mogen_core::validate_renderable_scene(&scene);
        let export = mogen_export::build_glb_with_options(&scene, &Default::default(), |_| {});
        let detected = diagnostics.iter().any(|d| d.code == expected) && export.is_err();
        if !detected {
            bail!("Mesh-contract control {mutation} was not rejected");
        }
        rows.push(
            json!({"mutation":mutation,"source":source,"source_revision":rev(source),
            "expected_code":expected,"detected":detected,"diagnostics":diagnostics,
            "visual_quality":"not applicable: malformed geometry control"}),
        );
    }
    Ok(json!(rows))
}

fn capture_revision(source: &str, base: &Path) -> Result<String> {
    Ok(revision(source, &dependencies(source, Some(base))?))
}
