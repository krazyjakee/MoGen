use super::*;
use crate::{GenerateConfig, GenerateResponse, ImageInput};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

/// Frontends render immutable snapshots using their own GL scheduling. A render
/// response is paired with the requested revision before reaching the model.
pub trait SessionRenderer {
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
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    findings: String,
    complete: bool,
    improved: bool,
    correction: String,
}
fn captures(
    renderer: &mut dyn SessionRenderer,
    workspace: &ModelingWorkspace,
    revision: &str,
    cfg: &GenerateConfig,
) -> Result<Vec<RenderedView>> {
    let mut views = vec![];
    for view in View::ALL {
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
        });
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
    let mut cfg = base.clone();
    cfg.cached_content = None;
    cfg.system_instruction = Some(format!(
        "{}\n{}",
        base.system_instruction.as_deref().unwrap_or(""),
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
    // Also bounded without a session control (e.g. fixture clients).
    for _ in 0..24 {
        if let Some(c) = &cfg.session_control {
            c.check().map_err(anyhow::Error::msg)?;
        }
        let response = call(&cfg)?;
        let parsed = serde_json::from_str::<ModelingTool>(&crate::repair::strip_markdown_fences(
            &response.text,
        ));
        let result: Result<serde_json::Value> = (|| {
            match parsed? {
                ModelingTool::Inspect { revision, name } => {
                    workspace.check_revision(&revision)?;
                    workspace.inspect(name.as_deref())
                }
                ModelingTool::Documentation { topic } => {
                    Ok(json!({"documentation":documentation(&topic)?}))
                }
                ModelingTool::Apply { revision, edits } => workspace.apply(&revision, &edits),
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
                    let image = if let Some(name) = name.as_deref() {
                        renderer.render_part(&workspace.source, &revision, view, name)?
                    } else {
                        renderer.render(&workspace.source, &revision, view)?
                    };
                    workspace.check_revision(&revision)?;
                    // Replace previous tool render, retaining original references.
                    cfg.user_images = base.user_images.clone();
                    cfg.user_images.push(image);
                    Ok(
                        json!({"revision":revision,"view":view.label(),"image_roles":"original target references first; current render last"}),
                    )
                }
                ModelingTool::Finish { revision, findings } => {
                    workspace.check_revision(&revision)?;
                    Ok(json!({"finished":true,"findings":findings}))
                }
            }
        })();
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
    compile(source, base_dir)?;
    let mut workspace = ModelingWorkspace::new(
        source.into(),
        base_dir.map(Path::to_path_buf),
        project.locks.clone(),
        project.selected_part.clone(),
    )?;
    let initial = project.record(
        source.into(),
        base_dir,
        cfg,
        provider,
        "Initial valid candidate; awaiting visual review".into(),
        vec![],
    )?;
    project.selected_candidate = Some(initial);
    checkpoint(project);
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
            let views = captures(renderer, &workspace, &rev, cfg)?;
            project.candidates[candidate].views = views.clone();
            let mut review_cfg = cfg.clone();
            review_cfg.cached_content = None;
            review_cfg.history.clear();
            review_cfg.spend_context.operation = "review".into();
            review_cfg.system_instruction=Some("Review a 3D asset against the original brief and target references. Return only JSON with findings (concrete observed defects and uncertainty), complete (boolean), improved (boolean compared with retained candidate), correction (targeted edit instructions). Assess silhouette/proportions and required parts, then joints/negative space, geometry finish, materials/UV scale and reference/style fidelity. Compilation or confidence alone is not quality. Set improved false for regressions or uncertainty; do not assign numeric self-scores.".into());
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
            let response = call(&review_cfg)?;
            workspace.check_revision(&rev)?;
            let review: Review =
                serde_json::from_str(&crate::repair::strip_markdown_fences(&response.text))?;
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
            project.selected_candidate = Some(best);
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
            let tool_result = tool_session(&mut workspace, cfg, &review.correction, call, renderer);
            if workspace.source != project.candidates[best].source {
                let findings = tool_result
                    .as_ref()
                    .map(|s| s.clone())
                    .unwrap_or_else(|e| format!("Unreviewed completed edit: {e}"));
                candidate = project.record(
                    workspace.source.clone(),
                    base_dir,
                    cfg,
                    provider,
                    findings,
                    vec![],
                )?;
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
    }
    project.selected_candidate = Some(best);
    checkpoint(project);
    Ok(project.candidates[best].source.clone())
}
