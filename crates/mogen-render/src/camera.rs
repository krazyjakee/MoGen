use glam::{Mat4, Vec2, Vec3};

pub struct OrbitCamera {
    pub yaw: f32,
    pub pitch: f32,
    /// Distance that exactly fits the current model at the fixed FOV. Derived
    /// from the mesh's bounding sphere on every framing pass.
    pub fit_distance: f32,
    /// User-controlled multiplier on top of `fit_distance`. 1.0 = auto-fit;
    /// scroll tweaks this. `Viewer::set_scene` resets it to 1.0 when the
    /// caller asks for a refit so different models render at the same
    /// apparent size.
    pub zoom: f32,
    pub target: Vec3,
}

/// Lightweight snapshot of camera pose, suitable for persisting per-file so
/// that switching tabs preserves the user's framing.
#[derive(Clone, Copy, Debug)]
pub struct CameraSnapshot {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub target: Vec3,
    pub fit_distance: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            yaw: std::f32::consts::FRAC_PI_4,
            // Positive pitch lifts the eye above the target so we get a
            // classic 3/4 view looking slightly down at the model.
            pitch: 0.5,
            fit_distance: 4.0,
            zoom: 1.0,
            target: Vec3::ZERO,
        }
    }
}

impl OrbitCamera {
    pub fn snapshot(&self) -> CameraSnapshot {
        CameraSnapshot {
            yaw: self.yaw,
            pitch: self.pitch,
            zoom: self.zoom,
            target: self.target,
            fit_distance: self.fit_distance,
        }
    }

    pub fn restore(&mut self, snap: CameraSnapshot) {
        self.yaw = snap.yaw;
        self.pitch = snap.pitch;
        self.zoom = snap.zoom;
        self.target = snap.target;
        self.fit_distance = snap.fit_distance;
    }

    /// Translate the orbit target along the camera's right/up axes by the
    /// given screen-space delta in pixels. Scales by `fit_distance` so the
    /// pan feels consistent across models of wildly different sizes.
    pub fn pan(&mut self, delta_px: Vec2, viewport_height: f32) {
        // World units per pixel at the focal plane, derived from the FOV used
        // in `view_proj`. Same heuristic as Blender's middle-button pan.
        let dist = self.distance().max(0.001);
        let height = viewport_height.max(1.0);
        let world_per_px = 2.0 * dist * (45.0_f32.to_radians() * 0.5).tan() / height;
        let eye = self.eye();
        let forward = (self.target - eye).normalize_or_zero();
        // Right-handed: right = forward × up; recompute up so it stays planar.
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        self.target -= right * delta_px.x * world_per_px;
        self.target += up * delta_px.y * world_per_px;
    }

    pub fn distance(&self) -> f32 {
        self.fit_distance * self.zoom
    }

    pub fn eye(&self) -> Vec3 {
        let dist = self.distance();
        self.target
            + Vec3::new(
                dist * self.pitch.cos() * self.yaw.sin(),
                dist * self.pitch.sin(),
                dist * self.pitch.cos() * self.yaw.cos(),
            )
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let dist = self.distance();
        let eye = self.eye();
        let view = Mat4::look_at_rh(eye, self.target, Vec3::Y);
        let near = (dist * 0.01).max(0.01);
        let far = (dist * 10.0).max(10.0);
        let proj = Mat4::perspective_rh_gl(45.0_f32.to_radians(), aspect.max(0.01), near, far);
        proj * view
    }
}
