//! Cinema mode: a director that drives the orbit camera through a sequence of
//! preset shots so the user can show off a model without touching the mouse.
//!
//! Each shot is an absolute pitch + zoom target plus a yaw delta to apply
//! over its duration. The director eases from the pose at shot-start to the
//! shot's target with an in/out cubic, so transitions never cut hard. When a
//! shot finishes the director latches the camera's current pose as the next
//! shot's start and rolls into the following entry, looping forever.
//!
//! All input (orbit, pan, zoom, gizmo) is suppressed by `Viewer::show` while
//! cinema is active; the grid and gizmo handles are skipped in the paint
//! callback so the framing reads as a clean presentation.

use std::f32::consts::TAU;

use super::camera::{CameraSnapshot, OrbitCamera};

#[derive(Clone, Copy)]
struct Shot {
    name: &'static str,
    duration: f32,
    /// Yaw added to the start yaw over the shot, eased. Use TAU for a full
    /// orbit, smaller values for a swing into a hero pose.
    yaw_delta: f32,
    /// Absolute pitch target.
    pitch: f32,
    /// Absolute zoom target (multiplier on `fit_distance`).
    zoom: f32,
}

const SHOTS: &[Shot] = &[
    Shot {
        name: "establishing",
        duration: 4.5,
        yaw_delta: 0.5,
        pitch: 0.50,
        zoom: 1.10,
    },
    Shot {
        name: "slow orbit",
        duration: 14.0,
        yaw_delta: TAU,
        pitch: 0.42,
        zoom: 1.00,
    },
    Shot {
        name: "crane up",
        duration: 5.0,
        yaw_delta: 0.4,
        pitch: 1.15,
        zoom: 0.95,
    },
    Shot {
        name: "push in",
        duration: 4.0,
        yaw_delta: -0.5,
        pitch: 0.45,
        zoom: 0.55,
    },
    Shot {
        name: "pull back",
        duration: 5.0,
        yaw_delta: 0.7,
        pitch: 0.50,
        zoom: 1.55,
    },
    Shot {
        name: "low hero",
        duration: 5.0,
        yaw_delta: -0.4,
        pitch: -0.15,
        zoom: 1.05,
    },
];

#[derive(Default)]
pub struct CinemaDirector {
    pub active: bool,
    current: usize,
    t: f32,
    start_yaw: f32,
    start_pitch: f32,
    start_zoom: f32,
    /// User's camera pose latched at activation; restored on deactivate so
    /// exiting cinema returns the viewport to where they left it.
    stashed: Option<CameraSnapshot>,
}

impl CinemaDirector {
    pub fn shot_label(&self) -> Option<&'static str> {
        if !self.active {
            return None;
        }
        SHOTS.get(self.current).map(|s| s.name)
    }

    pub fn activate(&mut self, camera: &OrbitCamera) {
        self.active = true;
        self.current = 0;
        self.t = 0.0;
        self.start_yaw = camera.yaw;
        self.start_pitch = camera.pitch;
        self.start_zoom = camera.zoom;
        self.stashed = Some(camera.snapshot());
    }

    pub fn deactivate(&mut self) -> Option<CameraSnapshot> {
        self.active = false;
        self.t = 0.0;
        self.current = 0;
        self.stashed.take()
    }

    pub fn tick(&mut self, dt: f32, camera: &mut OrbitCamera) {
        if !self.active {
            return;
        }
        let shot = match SHOTS.get(self.current) {
            Some(s) => *s,
            None => {
                self.current = 0;
                self.t = 0.0;
                return;
            }
        };
        self.t += dt.max(0.0);
        let u = (self.t / shot.duration.max(0.001)).clamp(0.0, 1.0);
        let eased = ease_in_out_cubic(u);
        camera.yaw = self.start_yaw + shot.yaw_delta * eased;
        camera.pitch = lerp(self.start_pitch, shot.pitch, eased);
        camera.zoom = lerp(self.start_zoom, shot.zoom, eased).clamp(0.1, 10.0);
        if self.t >= shot.duration {
            self.current = (self.current + 1) % SHOTS.len();
            self.t = 0.0;
            self.start_yaw = camera.yaw;
            self.start_pitch = camera.pitch;
            self.start_zoom = camera.zoom;
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn ease_in_out_cubic(u: f32) -> f32 {
    if u < 0.5 {
        4.0 * u * u * u
    } else {
        1.0 - (-2.0 * u + 2.0).powi(3) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_camera() -> OrbitCamera {
        OrbitCamera::default()
    }

    #[test]
    fn activate_and_deactivate_round_trip_pose() {
        let mut cam = fresh_camera();
        cam.yaw = 1.23;
        cam.pitch = 0.4;
        cam.zoom = 1.7;
        let mut dir = CinemaDirector::default();
        dir.activate(&cam);
        // Move the camera as the director would.
        cam.yaw = 9.0;
        cam.pitch = 1.1;
        cam.zoom = 0.5;
        let snap = dir.deactivate().expect("snapshot stashed");
        cam.restore(snap);
        assert!((cam.yaw - 1.23).abs() < 1e-5);
        assert!((cam.pitch - 0.4).abs() < 1e-5);
        assert!((cam.zoom - 1.7).abs() < 1e-5);
    }

    #[test]
    fn tick_advances_through_shots() {
        let mut cam = fresh_camera();
        let mut dir = CinemaDirector::default();
        dir.activate(&cam);
        // Run for far longer than every shot combined to confirm the loop
        // wraps without panicking and the camera stays in sane ranges.
        for _ in 0..2000 {
            dir.tick(0.05, &mut cam);
            assert!(cam.zoom.is_finite() && cam.zoom > 0.0);
            assert!(cam.pitch.is_finite());
            assert!(cam.yaw.is_finite());
        }
    }

    #[test]
    fn shot_label_is_none_when_inactive() {
        let dir = CinemaDirector::default();
        assert!(dir.shot_label().is_none());
    }

    #[test]
    fn shot_label_is_some_when_active() {
        let mut cam = fresh_camera();
        let mut dir = CinemaDirector::default();
        dir.activate(&cam);
        dir.tick(0.0, &mut cam);
        assert!(dir.shot_label().is_some());
    }
}
