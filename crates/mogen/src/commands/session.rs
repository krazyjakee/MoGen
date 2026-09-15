//! Supported headless entry point for the same runner used by Studio.
use crate::{
    common::{build_llm_client, resolve_model},
    session_render::Renderer,
};
use anyhow::{bail, Context, Result};
use clap::Args;
use mogen_llm::{session::*, GenerateConfig, GenerateResponse, ImageInput, Provider};
use serde_json::json;
use std::path::PathBuf;
#[derive(Args)]
pub(crate) struct SessionArgs {
    /// New output directory, or the original directory when resuming/inspecting.
    #[arg(long)]
    pub out_dir: PathBuf,
    /// Original brief for generation, or a correction when --input is supplied.
    #[arg(long)]
    pub prompt: Option<String>,
    /// JSON ModelingBrief containing persistent dimensions, style and constraints.
    #[arg(long, conflicts_with = "prompt")]
    pub brief: Option<PathBuf>,
    #[arg(long, conflicts_with = "resume")]
    pub input: Option<PathBuf>,
    #[arg(long)]
    pub resume: bool,
    /// Inspect saved state and artifact paths without making calls.
    #[arg(long)]
    pub inspect: bool,
    #[arg(long, default_value = "openai")]
    pub provider: crate::cli::ProviderArg,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub seed: Option<u64>,
    #[arg(long)]
    pub temperature: Option<f32>,
    #[arg(long,value_parser=["low","medium","high","xhigh"])]
    pub thinking: Option<String>,
    #[arg(long)]
    pub draft: bool,
    #[arg(long)]
    pub reference: Vec<PathBuf>,
    #[arg(long)]
    pub selected_part: Option<String>,
    /// Subtree lock on a uniquely named compiled part (repeatable).
    #[arg(long)]
    pub lock: Vec<String>,
    #[arg(long, default_value_t = 12)]
    pub calls: u32,
    #[arg(long, default_value_t = 3)]
    pub iterations: u32,
    #[arg(long, default_value_t = 600)]
    pub seconds: u64,
    #[arg(long)]
    pub spend_usd: Option<f64>,
    #[arg(long, default_value_t = 16384)]
    pub output_tokens: u32,
    #[arg(long)]
    pub glb: bool,
    #[arg(long)]
    pub experimental_guidance: bool,
    /// Credential-free provider transcript: JSON array of response strings.
    #[arg(long)]
    pub script: Option<PathBuf>,
}
pub(crate) fn run(args: SessionArgs) -> Result<()> {
    if (args.resume || args.inspect)
        && !ModelingProject::sidecar(&args.out_dir.join("final.mog")).is_file()
    {
        bail!(
            "No saved modeling session in {}; use a new --out-dir to start a session",
            args.out_dir.display()
        );
    }
    if args.inspect {
        let dir = args.out_dir.canonicalize()?;
        let project = ModelingProject::load(&dir.join("final.mog"))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&artifacts(&project, &dir, false)?)?
        );
        return Ok(());
    }
    if args.draft && args.input.is_some() {
        bail!("--draft creates a new asset; omit --draft to refine --input, or use ordinary modify for nonvisual edits");
    }
    if args.temperature.is_some_and(|v| !v.is_finite())
        || args.spend_usd.is_some_and(|v| !v.is_finite())
    {
        bail!("Temperature and USD limits must be finite");
    }
    let provider: Provider = args.provider.into();
    if args.resume
        && (args.prompt.is_some()
            || args.seed.is_some()
            || args.temperature.is_some()
            || args.thinking.is_some()
            || args.model.is_some()
            || args.brief.is_some()
            || !args.reference.is_empty()
            || args.selected_part.is_some()
            || !args.lock.is_empty())
    {
        bail!("Resume uses the saved brief, references, selection and locks; start a new session to change them");
    }
    if !args.resume && args.out_dir.exists() {
        bail!("Choose a new --out-dir, or use --resume/--inspect");
    }
    std::fs::create_dir_all(&args.out_dir)?;
    let dir = args.out_dir.canonicalize()?;
    let entry = dir.join("final.mog");
    let mut project = if args.resume {
        ModelingProject::load(&entry)?
    } else if let Some(input) = &args.input {
        ModelingProject::load(input)?
    } else {
        ModelingProject::default()
    };
    if args.resume && project.request_settings.is_none() {
        bail!("Saved project has no resumable execution; inspect its candidates or start a new session with --input");
    }
    let limits = SessionLimits {
        calls: args.calls,
        iterations: args.iterations,
        seconds: args.seconds,
        spend_usd: args.spend_usd,
        output_tokens: args.output_tokens,
    };
    if !args.resume {
        // An input sidecar supplies history and authoring constraints. Its
        // previous execution must not be resumed as this new refinement.
        project.session_initial = None;
        project.generation_response = None;
        project.generation_request.clear();
        project.input_source = None;
        if let Some(path) = &args.brief {
            project.brief = serde_json::from_slice(&std::fs::read(path)?)?;
        } else if project.brief.prompt.is_empty() {
            project.brief.prompt = args
                .prompt
                .clone()
                .context("--prompt or --brief is required for a new session")?;
        } else if let Some(correction) = &args.prompt {
            project.brief.corrections.push(correction.clone());
            project.brief.revision += 1;
        }
        project.mode = if args.draft {
            QualityMode::Draft
        } else {
            QualityMode::Refined
        };
        project.selected_part = args.selected_part.or(project.selected_part.clone());
        for name in args.lock {
            if !project
                .locks
                .iter()
                .any(|lock| lock.name == name && lock.kind == LockKind::Subtree)
            {
                project.locks.push(PartLock {
                    name,
                    kind: LockKind::Subtree,
                });
            }
        }
        project.experimental_guidance = args.experimental_guidance;
        project.limits = limits;
        for path in args.reference {
            let image = image::open(&path)?.into_rgba8();
            let mut png = std::io::Cursor::new(vec![]);
            image.write_to(&mut png, image::ImageFormat::Png)?;
            project.brief.add_reference(
                path.file_name().unwrap().to_string_lossy().into(),
                ImageInput {
                    mime_type: "image/png".into(),
                    data: png.into_inner(),
                },
            );
        }
    }
    let control = if args.resume {
        SessionControl::resume(
            project.limits.clone(),
            project.meter.clone(),
            project.elapsed_seconds,
        )
    } else {
        SessionControl::new(project.limits.clone())
    };
    let signal_control = control.clone();
    ctrlc::set_handler(move || signal_control.cancel())
        .context("Install session cancellation handler")?;
    let progress = Progress::start(control.clone());
    let mut cfg = GenerateConfig::new("");
    cfg.model = if args.resume {
        project
            .candidates
            .first()
            .map(|c| c.model.clone())
            .unwrap_or_else(|| resolve_model(provider, args.model.clone()))
    } else {
        resolve_model(provider, args.model)
    };
    if args.resume {
        if let Some(settings) = &project.request_settings {
            if provider.key() != settings.provider {
                bail!("Resume provider differs from saved {}", settings.provider);
            }
            settings.apply(&mut cfg);
        }
    } else {
        cfg.seed = args.seed;
        if let Some(t) = args.temperature {
            cfg.temperature = Some(t);
        }
        if let Some(thinking) = args.thinking {
            cfg.thinking_level = mogen_llm::ThinkingLevel::parse(&thinking);
        }
    }
    cfg.session_control = Some(control.clone());
    cfg.max_output_tokens = Some(project.limits.output_tokens);
    cfg.spend_context = mogen_llm::CallContext::new(mogen_llm::Operation::Generate)
        .with_scene(entry.display().to_string())
        .with_session(format!(
            "modeling-{}",
            identity(dir.to_string_lossy().as_bytes())
        ));
    cfg.system_instruction = Some(if project.experimental_guidance {
        experimental_system_instruction()
    } else {
        mogen_llm::system_instruction(&mogen_llm::StdlibIndex::from_registry(
            mogen_dsl::stdlib_registry(),
        ))
    });
    project.brief.attach(&mut cfg)?;
    if !args.resume {
        if let Some(input) = &args.input {
            let source = std::fs::read_to_string(input)?;
            let base = input
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            let deps = dependencies(&source, Some(base))?;
            // Output artifacts must never overwrite a dependency with the
            // same path. Fail before copying anything into the output project.
            for path in deps.keys() {
                let name = path
                    .components()
                    .next()
                    .unwrap()
                    .as_os_str()
                    .to_string_lossy();
                if matches!(
                    name.as_ref(),
                    "final.mog"
                        | "final.glb"
                        | "final.mog.modeling.json"
                        | "report.json"
                        | "generation-response.txt"
                ) || name.starts_with("candidate-")
                    || name.starts_with("revision-")
                {
                    bail!("Input dependency {} conflicts with session output artifacts; rename it before refining", path.display());
                }
            }
            for (path, data) in deps {
                let target = dir.join(path);
                std::fs::create_dir_all(target.parent().unwrap())?;
                std::fs::write(target, data)?;
            }
            compile(&source, Some(&dir))?;
            write_source(&entry, &source)?;
            project.input_source = Some(source);
        }
        project.request_settings = Some(SessionRequestSettings::new(&cfg, provider.key()));
        project.sync_control(&cfg);
        project.save(&entry)?;
    }
    let mut renderer = Renderer {
        base: dir.clone(),
        framing: None,
        front_yaw: None,
        last_capture: None,
        fit_image: None,
        capture_part: None,
    };
    let source_guard = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let check_source = || -> Result<()> {
        if let Some(expected) = source_guard.borrow().as_ref() {
            if std::fs::read_to_string(&entry).ok().as_ref() != Some(expected) {
                bail!("Stale source file: final.mog changed during the session; retained snapshots remain available");
            }
        }
        Ok(())
    };
    let mut checkpoint_error = None;
    let mut checkpoint = |p: &ModelingProject| {
        if let Err(e) = check_source() {
            control.stop(&e.to_string());
        }
        if let Err(e) = p.save(&entry) {
            checkpoint_error = Some(e.to_string());
            control.stop("Checkpoint write failed");
        }
        eprintln!(
            "{} | {} calls | {}s | saved {}",
            p.stage,
            control.meter().calls,
            control.elapsed().as_secs(),
            ModelingProject::sidecar(&entry).display()
        );
    };
    let outcome: Result<()> = (|| {
        if project.mode == QualityMode::Refined {
            if args.script.is_none()
                && (!provider.supports_images()
                    || (provider == Provider::Zai && !cfg.model.contains("5v")))
            {
                bail!("render_unavailable: selected provider/model cannot review images; select a vision model or --draft");
            }
            // Verify headless GL before any model call. Discard probe framing.
            let probe = "scene { box \"probe\" }";
            renderer.render(probe,&revision(probe,&dependencies(probe,Some(&dir))?),View::Front).context("render_unavailable: install EGL/OpenGL drivers (Mesa software rendering is supported)")?;
            renderer.framing = None;
            renderer.front_yaw = None;
        }
        let mut script: Option<std::collections::VecDeque<String>> = args
            .script
            .as_ref()
            .map(|p| -> Result<_> {
                Ok(serde_json::from_slice::<Vec<String>>(&std::fs::read(p)?)?.into())
            })
            .transpose()?;
        let client = if script.is_none() {
            Some(build_llm_client(provider, None, args.provider.into())?)
        } else {
            None
        };
        let mut call = |cfg: &GenerateConfig| -> Result<GenerateResponse> {
            check_source()?;
            if let Some(script) = &mut script {
                control.before_call(cfg, None).map_err(anyhow::Error::msg)?;
                let text = script.pop_front().context("Scripted provider exhausted")?;
                let usage = mogen_llm::Usage {
                    prompt_tokens: 1,
                    response_tokens: 1,
                    total_tokens: 2,
                    cached_tokens: 0,
                };
                control.after_call(Some(&usage), None);
                Ok(GenerateResponse { text, usage })
            } else {
                Ok(client.as_ref().unwrap().generate(cfg)?)
            }
        };
        let source = if args.resume && project.session_initial.is_some() {
            std::fs::read_to_string(&entry)
                .context("Missing final.mog; inspect retained candidates in sidecar")?
        } else if let Some(source) = &project.input_source {
            source.clone()
        } else {
            cfg.spend_context.operation = "generate".into();
            let source = generate_candidate(
                &mut project,
                &cfg,
                provider.key(),
                &mut call,
                &mut checkpoint,
            )?;
            validate_generated_candidate(
                &mut project,
                &source,
                Some(&dir),
                &cfg,
                provider.key(),
                &mut call,
                &mut renderer,
                &mut checkpoint,
            )?
        };
        compile(&source, Some(&dir))?;
        if !entry.exists() {
            write_source(&entry, &source)?;
        }
        *source_guard.borrow_mut() = Some(source.clone());
        check_source()?;
        cfg.spend_context.operation = "refine".into();
        let result = if project.mode == QualityMode::Draft {
            let i = project.record(
                source.clone(),
                Some(&dir),
                &cfg,
                provider.key(),
                "Draft; unreviewed".into(),
                vec![],
            )?;
            project.selected_candidate = Some(i);
            project.stop_reason = "Draft complete; unreviewed".into();
            source
        } else if args.resume && project.session_initial.is_some() {
            resume_session(
                &mut project,
                &source,
                Some(&dir),
                &cfg,
                provider.key(),
                &mut call,
                &mut renderer,
                &mut checkpoint,
            )?
        } else {
            refine_session(
                &mut project,
                &source,
                Some(&dir),
                &cfg,
                provider.key(),
                &mut call,
                &mut renderer,
                &mut checkpoint,
            )?
        };
        check_source()?;
        write_source(&entry, &result)?;
        *source_guard.borrow_mut() = Some(result);
        if args.glb {
            let mut scene = compile(&std::fs::read_to_string(&entry)?, Some(&dir))?;
            scene.resolve_texture_paths(&dir);
            mogen_export::write_glb(&scene, &dir.join("final.glb"))?;
        }
        Ok(())
    })();
    if let Err(e) = &outcome {
        project.stop_reason = format!("Stopped: {e:#}");
    }
    control.finish();
    drop(progress);
    project.meter = control.meter();
    project.elapsed_seconds = control.elapsed().as_secs();
    project.save(&entry)?;
    let report = artifacts(&project, &dir, true)?;
    std::fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if let Some(e) = checkpoint_error {
        bail!("Checkpoint failed: {e}");
    }
    outcome
}
fn artifacts(
    project: &ModelingProject,
    dir: &std::path::Path,
    write: bool,
) -> Result<serde_json::Value> {
    let selected = project
        .selected_candidate
        .and_then(|i| project.candidates.get(i));
    if let Some(response) = &project.generation_response {
        if write {
            std::fs::write(dir.join("generation-response.txt"), &response.text)?;
        }
    }
    let mut candidates = vec![];
    for (i, c) in project.candidates.iter().enumerate() {
        let path = dir.join(format!("candidate-{i}.mog"));
        if write {
            std::fs::write(&path, &c.source)?;
        }
        let snapshot = dir.join(format!("revision-{i}"));
        if write && !snapshot.exists() {
            c.restore_copy(&snapshot)?;
        }
        let mut views = vec![];
        for view in &c.views {
            let path = dir.join(format!("candidate-{i}-{}.png", view.label));
            if write {
                std::fs::write(&path, &view.image.data)?;
            }
            views.push(json!({"path":path,"revision":view.revision,"camera":view.camera}));
        }
        candidates.push(json!({"path":path,"snapshot_directory":snapshot,"revision":c.revision,"reviewed":c.reviewed,"selected":project.selected_candidate==Some(i),"views":views}));
    }
    let reason = &project.stop_reason;
    let status = if reason.contains("Cancelled") {
        "canceled"
    } else if reason.contains("render_unavailable") {
        "render_unavailable"
    } else if reason.contains("format recovery") {
        "review_format_failed"
    } else if reason.contains("limit reached") {
        "budget_exhausted"
    } else if reason.starts_with("Stopped:") {
        "stopped"
    } else {
        "completed"
    };
    Ok(
        json!({"version":1,"status":status,"stop_reason":reason,"final_mog":selected.filter(|c|std::fs::read_to_string(dir.join("final.mog")).is_ok_and(|s|s==c.source)).map(|_|dir.join("final.mog")),
        "selected_mog":project.selected_candidate.map(|i|dir.join(format!("candidate-{i}.mog"))),"latest_mog":project.candidates.len().checked_sub(1).map(|i|dir.join(format!("candidate-{i}.mog"))),"selected_revision":selected.map(|c|&c.revision),"latest_revision":project.candidates.last().map(|c|&c.revision),
        "sidecar":ModelingProject::sidecar(&dir.join("final.mog")),"report":dir.join("report.json"),"candidates":candidates,
        "stage":project.stage,"saved_responses":project.attempts.len(),"usage":project.meter,"elapsed_seconds":project.elapsed_seconds,
        "visual_quality":"Requires human review; orchestration success does not establish aesthetic improvement"}),
    )
}

struct Progress {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Progress {
    fn start(control: SessionControl) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut ticks = 0;
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(100));
                ticks += 1;
                if ticks % 20 == 0 {
                    let meter = control.meter();
                    eprintln!(
                        "Session pending: {} | {} calls | {:.1}s elapsed | {:.1}s in current call",
                        meter.stage,
                        meter.calls,
                        control.elapsed().as_secs_f64(),
                        if meter.call_pending {
                            (control.elapsed().as_secs_f64() - meter.stage_started_seconds).max(0.0)
                        } else {
                            0.0
                        }
                    );
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn write_source(path: &std::path::Path, source: &str) -> Result<()> {
    use std::io::Write;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temp.write_all(source.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
