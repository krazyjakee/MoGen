//! Opt-in reference-based evaluation. Default is credential-free fixture mode.
use anyhow::{bail, Context, Result};
use clap::Parser;
use mogen_llm::session::*;
use mogen_llm::{GenerateConfig, ImageInput, LlmClient, Provider};
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
    prompt: String,
    required: Vec<String>,
    max_extent: [f32; 3],
}
struct Renderer {
    base: PathBuf,
    framing: Option<([f32; 3], f32)>,
}
impl SessionRenderer for Renderer {
    fn render_part(
        &mut self,
        source: &str,
        revision: &str,
        view: View,
        name: &str,
    ) -> Result<ImageInput> {
        let scene = compile(source, Some(&self.base))?;
        let framing = part_framing(&scene, name)?;
        let old = self.framing.replace(framing);
        let result = self.render(source, revision, view);
        self.framing = old;
        result
    }
    fn render(&mut self, source: &str, _revision: &str, view: View) -> Result<ImageInput> {
        let scene = inspection_scene(compile(source, Some(&self.base))?, view);
        let mesh = mogen_render::flatten(&scene, Some(&self.base));
        let framing = *self
            .framing
            .get_or_insert((mesh.center.to_array(), mesh.radius));
        let (yaw, pitch) = view.camera();
        let opts = mogen_render::headless::ThumbnailOptions {
            yaw,
            pitch,
            base_dir: Some(self.base.clone()),
            ..Default::default()
        };
        let pixels = mogen_render::headless::render_thumbnail_framed(&scene, &opts, Some(framing))?;
        let mut png = std::io::Cursor::new(vec![]);
        image::write_buffer_with_format(
            &mut png,
            &pixels,
            opts.size,
            opts.size,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )?;
        Ok(ImageInput {
            mime_type: "image/png".into(),
            data: png.into_inner(),
        })
    }
}
fn asset_checks(source: &str, base: &Path, task: &Task) -> Result<serde_json::Value> {
    let scene = compile(source, Some(base))?;
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
    Ok(
        json!({"compiles":true,"finite":finite,"missing_parts":missing,"extent":extent,"dimensions_ok":dimensions_ok,
        "asset_pass":missing.is_empty()&&dimensions_ok,"framing_radius":mesh.radius,"visual_quality":"requires blinded human review"}),
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
                    let img = renderer.render(&source, &rev(&source), view)?;
                    std::fs::write(
                        dir.join(format!("reference-{}.png", view.label())),
                        &img.data,
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
                    return Ok(source.clone());
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
                        row["export_ok"] =
                            json!(
                                mogen_export::write_glb(&scene, &dir.join("candidate.glb")).is_ok()
                            );
                        if !args.no_render {
                            for view in View::ALL {
                                let image = renderer.render(&candidate, &rev(&candidate), view)?;
                                std::fs::write(
                                    dir.join(format!("candidate-{}.png", view.label())),
                                    &image.data,
                                )?;
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
        prompt: String::new(),
        required: vec!["seat".into(), "back".into()],
        max_extent: [1.0; 3],
    };
    let negative_check = asset_checks(negative, &root, &negative_task)?;
    if negative_check["asset_pass"] != false {
        bail!("Negative control was not detected");
    }
    std::fs::write(
        args.out.join("negative-control.json"),
        serde_json::to_vec_pretty(&negative_check)?,
    )?;
    std::fs::write(
        args.out.join("report.json"),
        serde_json::to_vec_pretty(
            &json!({"version":1,"provenance":manifest.provenance,"runs":rows,"human_review_status":"pending","renderer_caveats":["#105 UV seams","#107 water shader"]}),
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
