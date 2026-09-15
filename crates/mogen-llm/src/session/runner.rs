use super::*;
use crate::{GenerateConfig, GenerateResponse, ImageInput};
use anyhow::{bail, Result};
use serde_json::json;
use std::path::Path;

/// Frontends render immutable snapshots using their own GL scheduling. A render
/// response is paired with the requested revision before reaching the model.
pub trait SessionRenderer {
    fn restore_camera(&mut self, _capture: &mogen_core::views::CaptureInfo) -> Result<()> {
        Ok(())
    }
    fn capture_info(&self) -> Option<mogen_core::views::CaptureInfo> {
        None
    }
    fn diagnostic_fit(&self) -> Option<ImageInput> {
        None
    }
    fn render_part(
        &mut self,
        _source: &str,
        _revision: &str,
        _view: View,
        _name: &str,
    ) -> Result<ImageInput> {
        bail!("This rendering host does not support selected-part close-ups")
    }
    fn render(&mut self, source: &str, revision: &str, view: View) -> Result<ImageInput>;
}
fn captures(
    renderer: &mut dyn SessionRenderer,
    workspace: &ModelingWorkspace,
    revision: &str,
    cfg: &GenerateConfig,
    project: &mut ModelingProject,
    candidate: usize,
    checkpoint: &mut dyn FnMut(&ModelingProject),
) -> Result<Vec<RenderedView>> {
    let mut views = project.candidates[candidate].views.clone();
    for view in View::ALL.into_iter().skip(views.len()) {
        if let Some(c) = &cfg.session_control {
            c.check().map_err(anyhow::Error::msg)?;
        }
        workspace.check_revision(revision)?;
        let image = renderer.render(&workspace.source, revision, view)?;
        workspace.check_revision(revision)?;
        views.push(RenderedView {
            label: view.label().into(),
            revision: revision.into(),
            image,
            camera: renderer.capture_info(),
            diagnostic_fit: renderer.diagnostic_fit(),
        });
        project.candidates[candidate].views = views.clone();
        project.stage = format!("captured {}", view.label());
        project.sync_control(&cfg);
        checkpoint(project);
    }
    Ok(views)
}
pub fn tool_session(
    workspace: &mut ModelingWorkspace,
    base: &GenerateConfig,
    instruction: &str,
    call: &mut dyn FnMut(&GenerateConfig) -> Result<GenerateResponse>,
    renderer: &mut dyn SessionRenderer,
) -> Result<String> {
    let mut project = ModelingProject::default();
    tool_session_saved(
        workspace,
        base,
        instruction,
        call,
        renderer,
        &mut project,
        &mut 0,
        "custom",
        &mut |_| {},
    )
}
fn tool_session_saved(
    workspace: &mut ModelingWorkspace,
    base: &GenerateConfig,
    instruction: &str,
    call: &mut dyn FnMut(&GenerateConfig) -> Result<GenerateResponse>,
    renderer: &mut dyn SessionRenderer,
    project: &mut ModelingProject,
    cursor: &mut usize,
    provider: &str,
    checkpoint: &mut dyn FnMut(&ModelingProject),
) -> Result<String> {
    let mut cfg = base.clone();
    cfg.cached_content = None;
    cfg.response_schema = None;
    cfg.system_instruction = Some(format!(
        "{}\n{}",
        if project.experimental_guidance {
            crate::prompt::experimental_modeling_guidance()
        } else {
            crate::prompt::modeling_guidance(&crate::prompt::StdlibIndex::from_registry(
                mogen_dsl::stdlib_registry(),
            ))
        },
        TOOL_INSTRUCTIONS
    ));
    cfg.history.clear();
    cfg.user_prompt = format!(
        "Modeling request and original target context:\n{}\n\nCurrent correction (use the source below):\n{instruction}\nCurrent revision: {}\nSource:\n{}",
        base.user_prompt,
        workspace.revision()?,
        workspace.source
    );
    cfg.spend_context.operation = "refine".into();
    // Each iteration makes at most one provider call, so bound the loop by
    // the session's own configured call budget rather than an arbitrary
    // constant — otherwise a larger `--calls` budget than this cap would be
    // truncated early, and a smaller one would let this loop run past what
    // `SessionControl::before_call` already intends to allow. Fall back to a
    // fixed cap only when there's no session control to read a budget from
    // (e.g. fixture clients in tests).
    let max_steps = cfg
        .session_control
        .as_ref()
        .map(|c| c.limits().calls)
        .unwrap_or(24);
    for _ in 0..max_steps {
        if let Some(c) = &cfg.session_control {
            c.check().map_err(anyhow::Error::msg)?;
        }
        let request_revision = workspace.revision()?;
        let (attempt, response) =
            super::journal::receive(project, cursor, workspace, &cfg, provider, call, checkpoint)?;
        if let Some(c) = &cfg.session_control {
            c.check().map_err(anyhow::Error::msg)?;
        }
        workspace.check_revision(&request_revision)?;
        let parsed = parse_tool(&response.text, &request_revision);
        project.attempts[attempt].provenance =
            parsed.as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
        project.attempts[attempt].state =
            if project.attempts[attempt].provenance.contains("raw DSL") {
                "staged"
            } else {
                "interpreted"
            }
            .into();
        project.sync_control(&cfg);
        checkpoint(project);
        let result: Result<serde_json::Value> = (|| {
            match parsed?.0 {
                ModelingTool::Inspect { revision, name } => {
                    workspace.check_revision(&revision)?;
                    workspace.inspect(name.as_deref())
                }
                ModelingTool::Measure {
                    revision,
                    first,
                    second,
                    tolerance,
                    max_work,
                } => workspace.measure(&revision, &first, &second, tolerance, max_work),
                ModelingTool::Documentation { topic } => {
                    Ok(json!({"documentation":documentation(&topic)?}))
                }
                ModelingTool::Apply { revision, edits } => {
                    let result = workspace.apply_controlled(
                        &revision,
                        &edits,
                        cfg.session_control.as_ref(),
                    )?;
                    project.attempts[attempt].state = "applied".into();
                    if !project
                        .candidates
                        .iter()
                        .skip(project.session_initial.unwrap_or(0))
                        .any(|c| c.revision == result["revision"].as_str().unwrap_or(""))
                    {
                        project.record(
                            workspace.source.clone(),
                            workspace.base.as_deref(),
                            base,
                            provider,
                            "Unreviewed validated Apply".into(),
                            vec![],
                        )?;
                    }
                    project.attempts[attempt].outcome = Some(result.clone());
                    project.sync_control(&cfg);
                    checkpoint(project);
                    Ok(result)
                }
                ModelingTool::Compile { revision } => {
                    workspace.check_revision(&revision)?;
                    compile(&workspace.source, workspace.base.as_deref())?;
                    Ok(json!({"revision":revision,"valid":true}))
                }
                ModelingTool::Render {
                    revision,
                    view,
                    name,
                } => {
                    workspace.check_revision(&revision)?;
                    let saved = project.attempts[attempt].render.clone();
                    let capture = if let Some(saved) = saved {
                        saved
                    } else {
                        let image = if let Some(name) = name.as_deref() {
                            renderer.render_part(&workspace.source, &revision, view, name)?
                        } else {
                            renderer.render(&workspace.source, &revision, view)?
                        };
                        let captured = RenderedView {
                            label: view.label().into(),
                            revision: revision.clone(),
                            image,
                            camera: renderer.capture_info(),
                            diagnostic_fit: renderer.diagnostic_fit(),
                        };
                        workspace.check_revision(&revision)?;
                        project.attempts[attempt].render = Some(captured.clone());
                        project.sync_control(&cfg);
                        checkpoint(project);
                        captured
                    };
                    let image = capture.image;
                    workspace.check_revision(&revision)?;
                    // Replace previous tool render, retaining original references.
                    cfg.user_images = base.user_images.clone();
                    cfg.user_images.push(image);
                    if let Some(fit) = capture.diagnostic_fit.clone() {
                        cfg.user_images.push(fit);
                    }
                    Ok(
                        json!({"revision":revision,"view":view.label(),"camera":capture.camera,
                            "diagnostic_fit":capture.diagnostic_fit.clone().is_some(),
                            "image_roles":"original target references first, then fixed comparison render; optional last image is diagnostic_fit, not a matched comparison"}),
                    )
                }
                ModelingTool::Finish { revision, findings } => {
                    workspace.check_revision(&revision)?;
                    Ok(json!({"finished":true,"findings":findings}))
                }
            }
        })();
        project.attempts[attempt].outcome = Some(match &result {
            Ok(v) => v.clone(),
            Err(e) => json!({"error":format!("{e:#}")}),
        });
        project.sync_control(&cfg);
        checkpoint(project);
        if let Ok(value) = &result {
            if value["finished"] == true {
                return Ok(value["findings"].as_str().unwrap_or("").into());
            }
        }
        let value = match result {
            Ok(v) => v,
            Err(e) => json!({"error":format!("{e:#}"),"revision":workspace.revision()?}),
        };
        cfg.history.push(crate::Turn {
            role: crate::Role::User,
            text: cfg.user_prompt.clone(),
        });
        cfg.history.push(crate::Turn {
            role: crate::Role::Model,
            text: response.text,
        });
        cfg.user_prompt=format!("Tool result: {value}\nContinue the requested modeling correction using this current revision. Return one JSON tool call.");
    }
    bail!("Modeling tool step limit reached")
}
/// Refine a valid generated asset, retaining every candidate. Visual judgments
/// are advisory and recorded for human comparison, never advertised as proof.
pub fn refine_session(
    project: &mut ModelingProject,
    source: &str,
    base_dir: Option<&Path>,
    cfg: &GenerateConfig,
    provider: &str,
    call: &mut dyn FnMut(&GenerateConfig) -> Result<GenerateResponse>,
    renderer: &mut dyn SessionRenderer,
    checkpoint: &mut dyn FnMut(&ModelingProject),
) -> Result<String> {
    run_session(
        project, source, base_dir, cfg, provider, call, renderer, checkpoint, false,
    )
}
/// Resume a saved transcript with exactly matching context and dependencies.
pub fn resume_session(
    project: &mut ModelingProject,
    source: &str,
    base_dir: Option<&Path>,
    cfg: &GenerateConfig,
    provider: &str,
    call: &mut dyn FnMut(&GenerateConfig) -> Result<GenerateResponse>,
    renderer: &mut dyn SessionRenderer,
    checkpoint: &mut dyn FnMut(&ModelingProject),
) -> Result<String> {
    run_session(
        project, source, base_dir, cfg, provider, call, renderer, checkpoint, true,
    )
}
fn run_session(
    project: &mut ModelingProject,
    source: &str,
    base_dir: Option<&Path>,
    cfg: &GenerateConfig,
    provider: &str,
    call: &mut dyn FnMut(&GenerateConfig) -> Result<GenerateResponse>,
    renderer: &mut dyn SessionRenderer,
    checkpoint: &mut dyn FnMut(&ModelingProject),
    resume: bool,
) -> Result<String> {
    let context=identity(serde_json::to_string(&json!({"brief":project.brief,"locks":project.locks,
        "selected":project.selected_part,"provider":provider,"model":cfg.model,"prompt":cfg.user_prompt,
        "guidance":project.experimental_guidance,"settings":SessionRequestSettings::new(cfg,provider)}))?.as_bytes());
    let retained_selection = if resume {
        project.selected_candidate
    } else {
        None
    };
    let replay_count = if resume { project.attempts.len() } else { 0 };
    if resume {
        if project.session_context != context {
            bail!("Resume context changed (brief, model, locks or selection); start a new session");
        }
        let current = revision(source, &dependencies(source, base_dir)?);
        if !project.candidates.iter().any(|c| c.revision == current) {
            bail!("Stale source/dependencies; restore a saved candidate or start a new session");
        }
    } else {
        project.previous_attempts.append(&mut project.attempts);
        while project.previous_attempts.len() > 256
            || project
                .previous_attempts
                .iter()
                .map(|a| a.response.len())
                .sum::<usize>()
                > super::journal::JOURNAL_BYTES
        {
            project.previous_attempts.remove(0);
        }
        project.session_initial = None;
        project.session_context = context;
        project.request_settings = Some(SessionRequestSettings::new(cfg, provider));
        project.session_prompt = cfg.user_prompt.clone();
        project.session_images = cfg.user_images.clone();
    }
    let source = if resume {
        project
            .candidates
            .get(project.session_initial.ok_or_else(|| {
                anyhow::anyhow!("Legacy session has no resumable journal; start a new session")
            })?)
            .ok_or_else(|| anyhow::anyhow!("Invalid initial candidate"))?
            .source
            .clone()
    } else {
        source.into()
    };
    let source = source.as_str();
    let mut cursor = 0;
    compile(source, base_dir)?;
    let mut workspace = ModelingWorkspace::new(
        source.into(),
        base_dir.map(Path::to_path_buf),
        project.locks.clone(),
        project.selected_part.clone(),
    )?;
    let initial = if resume {
        project.session_initial.unwrap()
    } else {
        project.record(
            source.into(),
            base_dir,
            cfg,
            provider,
            "Initial valid candidate; awaiting visual review".into(),
            vec![],
        )?
    };
    project.session_initial = Some(initial);
    project.selected_candidate = retained_selection.or(Some(initial));
    project.sync_control(&cfg);
    checkpoint(project);
    if resume {
        if let Some(info) = project.candidates[initial]
            .views
            .first()
            .and_then(|v| v.camera.as_ref())
        {
            renderer.restore_camera(info)?;
        }
    }
    let mut best = initial;
    let mut candidate = initial;
    let result: Result<()> = (|| {
        for iteration in 0..=project.limits.iterations {
            if let Some(c) = &cfg.session_control {
                c.check().map_err(anyhow::Error::msg)?;
            }
            // The saved candidate owns the revision. Rehashing live files here
            // could silently label changed dependencies as the saved snapshot.
            let rev = project.candidates[candidate].revision.clone();
            workspace.check_revision(&rev)?;
            let views = if project.candidates[candidate].views.len() == View::ALL.len() {
                project.candidates[candidate].views.clone()
            } else {
                captures(
                    renderer, &workspace, &rev, cfg, project, candidate, checkpoint,
                )?
            };
            project.candidates[candidate].views = views.clone();
            project.stage = "captured; awaiting review".into();
            project.sync_control(&cfg);
            checkpoint(project);
            let mut review_cfg = cfg.clone();
            review_cfg.cached_content = None;
            review_cfg.history.clear();
            review_cfg.spend_context.operation = "review".into();
            review_cfg.system_instruction = Some(review_instructions());
            review_cfg.response_schema = if matches!(provider, "openai" | "gemini") {
                Some(review_schema())
            } else {
                None
            };
            review_cfg.user_prompt=format!("{}\nReview revision {rev}. Original target references: images 1–{}. Current views follow in neutral front, side, back, three_quarter order, followed by a presentation view with authored materials. Retained candidate views follow those when present.\nCurrent source:\n{}",project.brief.context(),cfg.user_images.len(),workspace.source);
            review_cfg.user_images = cfg.user_images.clone();
            review_cfg
                .user_images
                .extend(views.iter().map(|v| v.image.clone()));
            if candidate != best {
                review_cfg.user_images.extend(
                    project.candidates[best]
                        .views
                        .iter()
                        .map(|v| v.image.clone()),
                );
            }
            let camera_info: Vec<_> = views.iter().map(|v| &v.camera).collect();
            review_cfg.user_prompt.push_str(&format!("\nCamera metadata: {}. Out-of-frame counts identify cropped geometry; do not infer that those parts are missing. Separately labeled diagnostic_fit images follow all matched views, in the order listed below.\n", serde_json::to_string(&camera_info)?));
            for view in &views {
                if let Some(fit) = &view.diagnostic_fit {
                    review_cfg.user_prompt.push_str(&format!(
                        "diagnostic_fit: {} revision {} (independent framing)\n",
                        view.label, view.revision
                    ));
                    review_cfg.user_images.push(fit.clone());
                }
            }
            let (attempt, response) = super::journal::receive(
                project,
                &mut cursor,
                &workspace,
                &review_cfg,
                provider,
                call,
                checkpoint,
            )?;
            if let Some(c) = &cfg.session_control {
                c.check().map_err(anyhow::Error::msg)?;
            }
            workspace.check_revision(&rev)?;
            let review = match parse_review(&response.text) {
                Ok((review, provenance)) => {
                    project.attempts[attempt].provenance = provenance;
                    project.attempts[attempt].state = "normalized".into();
                    project.sync_control(cfg);
                    checkpoint(project);
                    review
                }
                Err(error) => {
                    project.attempts[attempt].outcome =
                        Some(json!({"parse_error":error.to_string()}));
                    project.attempts[attempt].state = "format_failed".into();
                    project.sync_control(&cfg);
                    checkpoint(project);
                    let mut repair = review_cfg.clone();
                    repair.user_images.clear();
                    repair.spend_context.operation = "review_format_repair".into();
                    repair.user_prompt=format!("Format-only repair. Preserve every observation and judgment in this response; do not invent missing booleans or reassess geometry. If required judgments are missing, return the original unchanged. Parse error: {error}. Original response:\n{}",response.text);
                    let (repair_attempt, fixed) = super::journal::receive(
                        project,
                        &mut cursor,
                        &workspace,
                        &repair,
                        provider,
                        call,
                        checkpoint,
                    )?;
                    if let Some(c) = &cfg.session_control {
                        c.check().map_err(anyhow::Error::msg)?;
                    }
                    workspace.check_revision(&rev)?;
                    let parsed = parse_review(&fixed.text);
                    project.attempts[repair_attempt].outcome = Some(match &parsed {
                        Ok((r, _)) => json!(r),
                        Err(e) => json!({"parse_error":e.to_string()}),
                    });
                    project.sync_control(&cfg);
                    checkpoint(project);
                    let (review,provenance)=parsed.map_err(|e|anyhow::anyhow!("Review format recovery exhausted: {e}; raw responses saved in modeling sidecar"))?;
                    validate_review_repair(&response.text, &review)
                        .map_err(|e| anyhow::anyhow!("Review format recovery exhausted: {e}"))?;
                    project.attempts[repair_attempt].provenance =
                        format!("format-only repair of attempt {attempt}; {provenance}");
                    project.attempts[repair_attempt].state = "normalized".into();
                    project.attempts[attempt].provenance =
                        format!("format-only repair by attempt {repair_attempt}");
                    review
                }
            };
            project.attempts[attempt].state = "reviewed".into();
            let mut outcome = json!(review);
            if let Some(error) = project.attempts[attempt]
                .outcome
                .as_ref()
                .and_then(|v| v.get("parse_error"))
            {
                outcome["parse_error"] = error.clone();
            }
            project.attempts[attempt].outcome = Some(outcome);
            project.candidates[candidate].reviewed = true;
            project.candidates[candidate].findings = review.findings.clone();
            project.candidates[candidate].usage = cfg
                .session_control
                .as_ref()
                .map(|c| c.meter().usage)
                .unwrap_or_else(|| response.usage.clone());
            let accepted = candidate == initial || review.improved;
            if accepted {
                best = candidate;
            }
            // Reconstructing earlier judgments must not downgrade the durable
            // selection if cancellation or a second crash interrupts replay.
            if cursor >= replay_count {
                project.selected_candidate = Some(best);
            }
            project.sync_control(&cfg);
            checkpoint(project);
            if !accepted {
                project.stop_reason =
                    "No useful improvement; retained the earlier candidate".into();
                return Ok(());
            }
            if review.complete {
                project.stop_reason =
                    "Reviewer found no further defects; inspect the result before accepting".into();
                return Ok(());
            }
            if review.correction.trim().is_empty() {
                project.stop_reason = "No actionable correction".into();
                return Ok(());
            }
            if iteration == project.limits.iterations {
                project.stop_reason = "Iteration limit reached".into();
                return Ok(());
            }
            let tool_result = tool_session_saved(
                &mut workspace,
                cfg,
                &review.correction,
                call,
                renderer,
                project,
                &mut cursor,
                provider,
                checkpoint,
            );
            if workspace.source != project.candidates[best].source {
                let findings = tool_result
                    .as_ref()
                    .map(|s| s.clone())
                    .unwrap_or_else(|e| format!("Unreviewed completed edit: {e}"));
                candidate = if let Some(index) = project
                    .candidates
                    .iter()
                    .enumerate()
                    .skip(initial)
                    .find(|(_, c)| c.revision == workspace.revision().unwrap_or_default())
                    .map(|(index, _)| index)
                {
                    index
                } else {
                    project.record(
                        workspace.source.clone(),
                        base_dir,
                        cfg,
                        provider,
                        findings,
                        vec![],
                    )?
                };
                project.sync_control(&cfg);
                checkpoint(project);
            }
            tool_result?;
            if workspace.source == project.candidates[best].source {
                project.stop_reason = "No source improvement".into();
                return Ok(());
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        project.stop_reason = format!("Stopped: {e:#}; retained the best completed candidate");
        if cursor <= replay_count {
            best = retained_selection.unwrap_or(best);
        }
    }
    project.selected_candidate = Some(best);
    project.stage = "stopped".into();
    if let Some(c) = &cfg.session_control {
        project.meter = c.meter();
        project.elapsed_seconds = c.elapsed().as_secs();
    }
    project.sync_control(&cfg);
    checkpoint(project);
    Ok(project.candidates[best].source.clone())
}

/// Generation receipt shares the durable project and provider admission gate.
/// Resume reuses the received source verbatim, including an invalid response
/// retained for inspection; it never silently regenerates a different asset.
pub fn generate_candidate(
    project: &mut ModelingProject,
    cfg: &GenerateConfig,
    provider: &str,
    call: &mut dyn FnMut(&GenerateConfig) -> Result<GenerateResponse>,
    checkpoint: &mut dyn FnMut(&ModelingProject),
) -> Result<String> {
    let request=identity(serde_json::to_string(&json!({"prompt":cfg.user_prompt,"settings":SessionRequestSettings::new(cfg,provider),"model":cfg.model,"provider":provider,"system":cfg.system_instruction,"images":cfg.user_images.iter().map(|i|identity(&i.data)).collect::<Vec<_>>()}))?.as_bytes());
    if project.generation_response.is_some() && project.generation_request != request {
        bail!("Saved generation context differs; start a new session");
    }
    if project.generation_response.is_none() {
        if let Some(c) = &cfg.session_control {
            c.check().map_err(anyhow::Error::msg)?;
        }
        project.stage = "generate: awaiting provider response".into();
        project.request_settings = Some(SessionRequestSettings::new(cfg, provider));
        project.sync_control(cfg);
        project.meter.calls = project.meter.calls.saturating_add(1);
        project.meter.unknown_cost = true;
        checkpoint(project);
        let mut receipt_cfg = cfg.clone();
        receipt_cfg.retain_stopped_response = true;
        let response = call(&receipt_cfg)?;
        project.generation_response = Some(response);
        project.request_settings = Some(SessionRequestSettings::new(cfg, provider));
        project.generation_request = request;
        project.stage = "generate: response saved".into();
        project.sync_control(cfg);
        checkpoint(project);
    }
    if let Some(c) = &cfg.session_control {
        c.check().map_err(anyhow::Error::msg)?;
    }
    let raw = &project.generation_response.as_ref().unwrap().text;
    if raw.len() > MAX_RESPONSE_BYTES {
        bail!("Generation exceeds 1 MiB response limit; raw response retained");
    }
    Ok(document_text(raw, "mog").to_string())
}

/// Build recovery uses the same atomic tool dispatcher as visual corrections.
pub fn validate_generated_candidate(
    project: &mut ModelingProject,
    source: &str,
    base: Option<&Path>,
    cfg: &GenerateConfig,
    provider: &str,
    call: &mut dyn FnMut(&GenerateConfig) -> Result<GenerateResponse>,
    renderer: &mut dyn SessionRenderer,
    checkpoint: &mut dyn FnMut(&ModelingProject),
) -> Result<String> {
    let error = match compile(source, base) {
        Ok(_) => return Ok(source.into()),
        Err(e) => format!("{e:#}"),
    };
    let start = if dependencies(source, base).is_ok() {
        source
    } else {
        "scene {}"
    };
    let mut workspace = ModelingWorkspace::new(
        start.into(),
        base.map(Path::to_path_buf),
        project.locks.clone(),
        project.selected_part.clone(),
    )?;
    tool_session_saved(&mut workspace,cfg,&format!("Repair the generated document to satisfy compiler diagnostics. Preserve the original brief. Diagnostics: {error}. Received document:\n{source}"),call,renderer,project,&mut 0,provider,checkpoint)?;
    compile(&workspace.source, base)?;
    Ok(workspace.source)
}
