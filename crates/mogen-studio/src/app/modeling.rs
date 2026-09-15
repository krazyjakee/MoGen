//! Studio adapter for the shared modeling session and immutable GL snapshots.
use crate::app::types::{LlmMessage, LlmProgress};
use eframe::egui;
use mogen_llm::session::{ModelingProject, QualityMode, SessionControl, SessionRenderer, View};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub(super) struct RenderJob {
    mesh: Arc<mogen_render::FlatMesh>,
    camera: mogen_render::OrbitCamera,
    reply: mpsc::Sender<Result<Vec<u8>, String>>,
    control: SessionControl,
}
pub(in crate::app) struct WorkerRenderer {
    pub tx: mpsc::Sender<LlmMessage>,
    pub control: SessionControl,
    pub base_dir: Option<std::path::PathBuf>,
    pub framing: Option<(glam::Vec3, f32)>,
    pub front_yaw: Option<f32>,
    pub last_capture: Option<mogen_core::views::CaptureInfo>,
    pub fit_image: Option<mogen_llm::ImageInput>,
    pub capture_part: Option<String>,
}
impl SessionRenderer for WorkerRenderer {
    fn restore_camera(&mut self, info: &mogen_core::views::CaptureInfo) -> anyhow::Result<()> {
        if info.convention_version != mogen_core::views::CAMERA_CONVENTION_VERSION
            || info.view != "front"
        {
            anyhow::bail!("Unsupported saved camera");
        }
        self.framing = Some((info.target.into(), info.distance / 2.8));
        self.front_yaw = Some(info.yaw);
        Ok(())
    }
    fn capture_info(&self) -> Option<mogen_core::views::CaptureInfo> {
        self.last_capture.clone()
    }
    fn diagnostic_fit(&self) -> Option<mogen_llm::ImageInput> {
        self.fit_image.clone()
    }
    fn render_part(
        &mut self,
        source: &str,
        revision: &str,
        view: View,
        name: &str,
    ) -> anyhow::Result<mogen_llm::ImageInput> {
        let scene = mogen_llm::session::compile(source, self.base_dir.as_deref())?;
        let (center, radius) = mogen_llm::session::part_framing(&scene, name)?;
        let old = self.framing.replace((center.into(), radius));
        let old_front = self.front_yaw;
        let old_part = self.capture_part.replace(name.into());
        let result = self.render(source, revision, view);
        self.framing = old;
        self.front_yaw = old_front;
        self.capture_part = old_part;
        result
    }
    fn render(
        &mut self,
        source: &str,
        revision: &str,
        view: View,
    ) -> anyhow::Result<mogen_llm::ImageInput> {
        self.control.check().map_err(anyhow::Error::msg)?;
        let scene = mogen_llm::session::inspection_scene(
            mogen_llm::session::compile(source, self.base_dir.as_deref())?,
            view,
        );
        let mesh = Arc::new(mogen_render::flatten(&scene, self.base_dir.as_deref()));
        let (center, radius) = *self
            .framing
            .get_or_insert((mesh.center, mesh.radius.max(0.001)));
        let front = mogen_core::asset_front_yaw(&scene).map_err(anyhow::Error::msg)?;
        let (yaw, pitch) = view.camera_from_front(*self.front_yaw.get_or_insert(front));
        let camera = mogen_render::OrbitCamera {
            yaw,
            pitch,
            fit_distance: radius * 2.8,
            zoom: 1.0,
            target: center,
        };
        self.fit_image = None;
        let info = mogen_render::capture_info(
            &scene,
            &camera,
            revision,
            view.label(),
            self.capture_part.as_deref(),
        );
        let comparison = self.render_image(mesh.clone(), camera, view.label())?;
        if info.out_of_frame_vertices != 0 {
            self.tx
                .send(LlmMessage::Progress(LlmProgress::Status(format!(
                    "{} comparison crops {} vertices; rendering separate diagnostic fit",
                    view.label(),
                    info.out_of_frame_vertices
                ))))?;
            let fit = mogen_render::OrbitCamera {
                yaw,
                pitch,
                fit_distance: mesh.radius.max(0.001) * 2.8,
                zoom: 1.0,
                target: mesh.center,
            };
            self.fit_image = Some(self.render_image(mesh, fit, "diagnostic_fit")?);
        }
        self.last_capture = Some(info);
        Ok(comparison)
    }
}
impl WorkerRenderer {
    fn render_image(
        &self,
        mesh: Arc<mogen_render::FlatMesh>,
        camera: mogen_render::OrbitCamera,
        label: &str,
    ) -> anyhow::Result<mogen_llm::ImageInput> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(LlmMessage::Progress(LlmProgress::Status(format!(
                "Rendering {} view",
                label
            ))))?;
        self.tx.send(LlmMessage::Render(RenderJob {
            mesh,
            camera,
            reply,
            control: self.control.clone(),
        }))?;
        let pixels = loop {
            self.control.check().map_err(anyhow::Error::msg)?;
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => break result.map_err(anyhow::Error::msg)?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(e) => return Err(e.into()),
            }
        };
        let image = image::RgbaImage::from_raw(512, 512, pixels)
            .ok_or_else(|| anyhow::anyhow!("Invalid render dimensions"))?;
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image).write_to(&mut encoded, image::ImageFormat::Png)?;
        Ok(mogen_llm::ImageInput {
            mime_type: "image/png".into(),
            data: encoded.into_inner(),
        })
    }
}
/// The app's GL context executes the same renderer used by CLI thumbnails.
/// Each callback uses an immutable mesh and its own renderer, so tab switches
/// and edits cannot change the capture or replace the user's viewport scene.
pub(super) fn schedule_render(ctx: &egui::Context, job: RenderJob) {
    let once = Arc::new(Mutex::new(Some(job)));
    let repaint = ctx.clone();
    let callback = egui_glow::CallbackFn::new(move |_info, painter| {
        let Some(job) = once.lock().unwrap().take() else {
            return;
        };
        let result = (|| -> anyhow::Result<Vec<u8>> {
            job.control.check().map_err(anyhow::Error::msg)?;
            let gl = painter.gl();
            let mut renderer = mogen_render::Renderer::new(gl)?;
            renderer.upload(gl, &job.mesh);
            let result = renderer.render_to_pixels(
                gl,
                512,
                job.camera.view_proj(1.0),
                job.camera.eye(),
                [42, 45, 51, 255],
            );
            renderer.destroy(gl);
            result
        })();
        let _ = job.reply.send(result.map_err(|e| e.to_string()));
        repaint.request_repaint();
    });
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("modeling_capture"),
    ))
    .add(egui::PaintCallback {
        rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)),
        callback: Arc::new(callback),
    });
    ctx.request_repaint();
}
pub(super) fn quality_controls(ui: &mut egui::Ui, project: &mut ModelingProject) {
    ui.horizontal(|ui| {
        ui.selectable_value(&mut project.mode, QualityMode::Draft, "Draft");
        ui.selectable_value(&mut project.mode, QualityMode::Refined, "Refined");
    });
    ui.label(
        egui::RichText::new(match project.mode {
            QualityMode::Draft => "Generate and repair once.",
            QualityMode::Refined => {
                "Inspect four views and correct defects automatically; uses additional model calls."
            }
        })
        .weak(),
    );
    egui::Grid::new("modeling_limits").show(ui, |ui| {
        ui.label("Maximum calls");
        ui.add(egui::DragValue::new(&mut project.limits.calls).range(1..=100));
        ui.end_row();
        ui.label("Refinement rounds");
        ui.add(egui::DragValue::new(&mut project.limits.iterations).range(0..=10));
        ui.end_row();
        ui.label("Seconds");
        ui.add(egui::DragValue::new(&mut project.limits.seconds).range(10..=3600));
        ui.end_row();
        let mut limit = project.limits.spend_usd.is_some();
        if ui.checkbox(&mut limit, "Estimated USD limit").changed() {
            project.limits.spend_usd = limit.then_some(2.0);
        }
        if let Some(value) = &mut project.limits.spend_usd {
            ui.add(egui::DragValue::new(value).range(0.01..=100.0).speed(0.1));
        }
        ui.end_row();
    });
    ui.label(egui::RichText::new("USD limits use conservative estimates; in-flight usage can exceed them. Unknown pricing or subscriptions require call/time limits. Stop prevents further calls; some in-flight requests cannot be aborted.").weak());
}
impl super::MogenStudioApp {
    pub(super) fn ui_modeling(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.active().modeling_load_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            ui.label("Repair or move the .modeling.json sidecar and reopen the project to resume AI editing.");
            return;
        }
        let shared = self.active().modeling.clone();
        let busy = self.active().llm_in_flight.is_some();
        let Ok(mut project) = shared.try_lock() else {
            ui.label("Saving modeling checkpoint…");
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            return;
        };
        let mut resume_requested = false;
        ui.collapsing("Modeling session",|ui| {
            ui.add_enabled_ui(!busy,|ui| {
                quality_controls(ui,&mut project);
                ui.label("Original target");ui.text_edit_multiline(&mut project.brief.prompt);
                ui.label("Dimensions and units");ui.text_edit_singleline(&mut project.brief.dimensions);
                ui.label("Intended use");ui.text_edit_singleline(&mut project.brief.intended_use);
                ui.label("Required details");ui.text_edit_multiline(&mut project.brief.required_details);
                ui.label("Approved constraints");ui.text_edit_multiline(&mut project.brief.constraints);
                ui.checkbox(&mut project.experimental_guidance,"Try experimental shape guidance");
                ui.label(format!("{} stored reference image(s)",project.brief.references.len()));
                let selected=self.viewer.primary_selection().and_then(|id|self.active().last_result.as_ref()?.scene.as_ref().map(|s|s.get(id).name.clone()));
                if let Some(name)=selected {
                    if ui.button(format!("Focus edits on {name}")).clicked() { project.selected_part=Some(name.clone()); }
                    for kind in [mogen_llm::session::LockKind::Subtree,mogen_llm::session::LockKind::Geometry,mogen_llm::session::LockKind::Transform,mogen_llm::session::LockKind::Material] {
                        if ui.button(format!("Lock {kind:?}")).clicked() && !project.locks.iter().any(|l|l.name==name && l.kind==kind) {
                            project.locks.push(mogen_llm::session::PartLock{name:name.clone(),kind});
                        }
                    }
                }
                if let Some(name)=project.selected_part.clone() {
                    ui.label(format!("Focused part: {name}"));if ui.button("Edit whole model").clicked() { project.selected_part=None; }
                }
                let mut remove=None;
                for (i,lock) in project.locks.iter().enumerate() {
                    ui.horizontal(|ui| {ui.label(format!("{}: {:?}",lock.name,lock.kind));if ui.button("Unlock").clicked(){remove=Some(i);}});
                }
                if let Some(i)=remove {project.locks.remove(i);}
            });
            if let Some(control)=&self.active().modeling_control {
                let meter=control.meter();
                if meter.call_pending {ui.label(format!("{} pending · {:.1}s in current call",meter.stage,(control.elapsed().as_secs_f64()-meter.stage_started_seconds).max(0.0)));}
                ui.label(format!("{} calls · {} tokens · {:.1}s · {}",meter.calls,meter.usage.total_tokens,control.elapsed().as_secs_f64(),
                    if meter.unknown_cost {"cost unavailable/subscription".into()} else {format!("estimated ${:.3}",meter.estimated_usd)}));
            }
            ui.label(format!("Stage: {} · {} saved responses",project.stage,project.attempts.len()));
            if ui.add_enabled(!busy && project.session_initial.is_some(),egui::Button::new("Resume saved refinement")).clicked(){resume_requested=true;}
            ui.collapsing("Saved responses and recovery",|ui| {
                for (i,a) in project.attempts.iter().enumerate().rev().take(20) {
                    ui.collapsing(format!("{} · {} · {} · {}",i+1,a.phase,a.state,a.model),|ui|{
                        ui.label(&a.provenance);if let Some(outcome)=&a.outcome {ui.label(outcome.to_string());}
                        if ui.button("Copy raw response").clicked(){ui.ctx().copy_text(a.response.clone());}
                        ui.label(format!("{} bytes saved for revision {}",a.response.len(),&a.revision[..12]));
                    });
                }
            });
            if !project.stop_reason.is_empty(){ui.label(&project.stop_reason);}
            ui.label(format!("{} recoverable candidates",project.candidates.len()));
            if self.active().path.is_none(){ui.label(egui::RichText::new("Unsaved candidates are recoverable from ~/.mogen/modeling-recovery; save the asset to keep its session beside the project.").weak());}
            let mut restore=None;
            let mut compare=None;
            let mut restore_copy=None;
            for (i,candidate) in project.candidates.iter().enumerate().rev().take(20) {
                ui.horizontal(|ui| {
                    ui.label(format!("Revision {} · {} · {}",i+1,candidate.model,if candidate.reviewed{"reviewed"}else{"unreviewed"}));
                    if ui.button("Compare").clicked(){compare=Some(i);}
                    if ui.add_enabled(!busy,egui::Button::new("Restore as copy")).clicked(){restore_copy=Some(i);}
                    if ui.add_enabled(!busy,egui::Button::new("Restore / Keep")).clicked(){restore=Some(i);}
                });
                ui.label(egui::RichText::new(&candidate.findings).weak());
            }
            if let Some(i)=restore_copy {
                if let Some(path)=self.active().path.clone() {
                    let destination=path.parent().unwrap_or(std::path::Path::new(".")).join(".modeling-revisions").join(format!("{}-{}",&project.candidates[i].revision[..12],std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
                    match project.candidates[i].restore_copy(&destination) {
                        Ok(path)=>self.active_mut().status=format!("Recovered complete snapshot: {}",path.display()),
                        Err(e)=>self.active_mut().status=format!("Restore failed: {e}"),
                    }
                }else{self.active_mut().status="Save the project first to recover a copy next to it".into();}
            }
            if let Some(i)=compare { self.active_mut().modeling_compare=Some(i); }
            if let Some(i)=restore {
                let candidate=&project.candidates[i];
                let base=self.active().path.as_ref().and_then(|p|p.parent());
                match mogen_llm::session::dependencies(&candidate.source,base) {
                    Ok(deps) if deps==candidate.dependencies => {
                        let before=self.active().source.clone();
                        let source=candidate.source.clone();
                        let tab=self.active().tab_id;
                        self.active_mut().source=source.clone();
                        self.active_mut().dirty=source!=self.active().last_saved_source;
                        self.active_mut().needs_compile=true;
                        super::llm::poll::push_llm_change_to_editor_history(ui.ctx(),egui::Id::new(("mog_editor_textedit",tab)),before.clone(),source);
                        self.push_undo(self.active,before,super::types::UndoKey{surface:"modeling_restore",attr:None,node_path:vec![]});
                        project.selected_candidate=Some(i);
                    },
                    _=>self.active_mut().status="Restore blocked: a dependency changed or is missing; restore matching project assets first".into(),
                }
            }
        });
        if let Some(i) = self.active().modeling_compare {
            if let Some(candidate) = project.candidates.get(i) {
                let mut open = true;
                egui::Window::new("Compare AI candidates")
                    .open(&mut open)
                    .show(ui.ctx(), |ui| {
                        let best = project
                            .selected_candidate
                            .and_then(|i| project.candidates.get(i));
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for view in View::ALL {
                                ui.label(view.label());
                                ui.horizontal(|ui| {
                                    for c in [best, Some(candidate)].into_iter().flatten() {
                                        if let Some(v) =
                                            c.views.iter().find(|v| v.label == view.label())
                                        {
                                            if let Some(camera) = &v.camera {
                                                ui.label(format!("Camera v{} · {} cropped vertices",camera.convention_version,camera.out_of_frame_vertices));
                                            } else { ui.label("Legacy camera convention (unversioned)"); }
                                            if let Some(fit) = &v.diagnostic_fit {
                                                ui.collapsing("Diagnostic fit · independent framing", |ui| {
                                                    if let Ok(img) = image::load_from_memory(&fit.data) {
                                                        let img = img.to_rgba8();
                                                        let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize,img.height() as usize],img.as_raw());
                                                        let texture = ui.ctx().load_texture(format!("fit-{}-{}",c.revision,v.label),color,egui::TextureOptions::LINEAR);
                                                        ui.image((texture.id(),egui::vec2(220.0,220.0)));
                                                    }
                                                });
                                            }
                                            let key = egui::Id::new((
                                                "modeling_preview",
                                                &c.revision,
                                                &v.label,
                                            ));
                                            let handle = ui.ctx().data_mut(|d| {
                                                d.get_temp::<egui::TextureHandle>(key)
                                            });
                                            let handle = handle.or_else(|| {
                                                let img = image::load_from_memory(&v.image.data)
                                                    .ok()?
                                                    .to_rgba8();
                                                let color =
                                                    egui::ColorImage::from_rgba_unmultiplied(
                                                        [
                                                            img.width() as usize,
                                                            img.height() as usize,
                                                        ],
                                                        img.as_raw(),
                                                    );
                                                let handle = ui.ctx().load_texture(
                                                    format!("{key:?}"),
                                                    color,
                                                    egui::TextureOptions::LINEAR,
                                                );
                                                ui.ctx().data_mut(|d| {
                                                    d.insert_temp(key, handle.clone())
                                                });
                                                Some(handle)
                                            });
                                            if let Some(h) = handle {
                                                ui.image((h.id(), egui::vec2(220.0, 220.0)));
                                            }
                                        }
                                    }
                                });
                            }
                        });
                    });
                if !open {
                    self.active_mut().modeling_compare = None;
                }
            }
        }
        if resume_requested {
            drop(project);
            self.spawn_modeling_resume(ui.ctx().clone());
            return;
        }
        if !busy {
            if let Some(path) = self.active().path.clone() {
                let bytes=serde_json::to_vec(&serde_json::json!({"brief":{
                    "prompt":project.brief.prompt,"corrections":project.brief.corrections,"style":project.brief.style,
                    "dimensions":project.brief.dimensions,"intended_use":project.brief.intended_use,"details":project.brief.required_details,"constraints":project.brief.constraints,
                    "references":project.brief.references.iter().map(|r|&r.digest).collect::<Vec<_>>()},
                    "mode":project.mode,"limits":project.limits,"locks":project.locks,"selected":project.selected_part,
                    "candidate_count":project.candidates.len(),"keep":project.selected_candidate,"experimental":project.experimental_guidance})).unwrap_or_default();
                let fingerprint = mogen_llm::session::identity(&bytes);
                if self.active().modeling_saved_hash != fingerprint {
                    match project.save(&path) {
                        Ok(()) => self.active_mut().modeling_saved_hash = fingerprint,
                        Err(e) => self.active_mut().status = format!("Session save failed: {e}"),
                    }
                }
            }
        }
    }
}
