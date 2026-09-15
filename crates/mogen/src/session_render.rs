use anyhow::{bail, Result};
use mogen_llm::{session::*, ImageInput};
use std::path::{Path, PathBuf};
pub(crate) struct Renderer {
    pub(crate) base: PathBuf,
    pub(crate) framing: Option<([f32; 3], f32)>,
    pub(crate) front_yaw: Option<f32>,
    pub(crate) last_capture: Option<mogen_core::views::CaptureInfo>,
    pub(crate) fit_image: Option<ImageInput>,
    pub(crate) capture_part: Option<String>,
}
impl SessionRenderer for Renderer {
    fn restore_camera(&mut self, info: &mogen_core::views::CaptureInfo) -> Result<()> {
        if info.convention_version != mogen_core::views::CAMERA_CONVENTION_VERSION
            || info.view != "front"
        {
            bail!("Saved camera is not a supported front capture");
        }
        self.framing = Some((info.target, info.distance / 2.8));
        self.front_yaw = Some(info.yaw);
        Ok(())
    }
    fn capture_info(&self) -> Option<mogen_core::views::CaptureInfo> {
        self.last_capture.clone()
    }
    fn diagnostic_fit(&self) -> Option<ImageInput> {
        self.fit_image.clone()
    }
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
        let old_front = self.front_yaw;
        let old_part = self.capture_part.replace(name.into());
        let result = self.render(source, revision, view);
        self.framing = old;
        self.front_yaw = old_front;
        self.capture_part = old_part;
        result
    }
    fn render(&mut self, source: &str, revision: &str, view: View) -> Result<ImageInput> {
        if revision_of(source, &self.base)? != revision {
            bail!("Stale capture revision");
        }
        let scene = inspection_scene(compile(source, Some(&self.base))?, view);
        let mesh = mogen_render::flatten(&scene, Some(&self.base));
        let framing = *self
            .framing
            .get_or_insert((mesh.center.to_array(), mesh.radius));
        let front = mogen_core::asset_front_yaw(&scene).map_err(anyhow::Error::msg)?;
        let (yaw, pitch) = view.camera_from_front(*self.front_yaw.get_or_insert(front));
        let opts = mogen_render::headless::ThumbnailOptions {
            yaw,
            pitch,
            base_dir: Some(self.base.clone()),
            ..Default::default()
        };
        let camera = mogen_render::OrbitCamera {
            yaw,
            pitch,
            target: framing.0.into(),
            fit_distance: framing.1.max(0.001) * 2.8,
            zoom: 1.0,
        };
        let info = mogen_render::capture_info(
            &scene,
            &camera,
            revision,
            view.label(),
            self.capture_part.as_deref(),
        );
        let image = render_png(&scene, &opts, Some(framing))?;
        self.fit_image = if info.out_of_frame_vertices != 0 {
            Some(render_png(&scene, &opts, None)?)
        } else {
            None
        };
        if revision_of(source, &self.base)? != revision {
            bail!("Dependencies changed during capture");
        }
        self.last_capture = Some(info);
        Ok(image)
    }
}
fn render_png(
    scene: &mogen_core::SceneGraph,
    opts: &mogen_render::headless::ThumbnailOptions,
    framing: Option<([f32; 3], f32)>,
) -> Result<ImageInput> {
    let pixels = mogen_render::headless::render_thumbnail_framed(scene, opts, framing)?;
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

fn revision_of(source: &str, base: &Path) -> Result<String> {
    Ok(revision(source, &dependencies(source, Some(base))?))
}
