use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};

/// Rotating "still working" lines shown after an LLM call has been running
/// for more than 10s. They're deliberately vague — we don't actually know
/// what the model is doing, we just want the wait to feel less dead.
pub(crate) const LLM_FLAVORS: &[&str] = &[
    "still thinking",
    "reasoning about geometry",
    "planning the scene",
    "picking materials",
    "wiring connectors",
    "working it out",
];

pub(crate) struct SpinnerState {
    base: String,
    since: Instant,
    flavors: &'static [&'static str],
}

#[derive(Clone)]
pub(crate) struct SpinnerHandle {
    pub(crate) pb: ProgressBar,
    state: Arc<Mutex<SpinnerState>>,
}

impl SpinnerHandle {
    pub(crate) fn set_message(&self, msg: impl Into<String>) {
        let msg = msg.into();
        {
            let mut s = self.state.lock().expect("spinner state mutex poisoned");
            s.base = msg.clone();
            s.since = Instant::now();
        }
        self.pb.set_message(msg);
    }
}

/// Terminal spinner with optional rotating "flavor text" on long waits.
/// Falls back silently to a no-op when stderr isn't a TTY (indicatif detects
/// this automatically), so piped/logged output stays clean.
pub(crate) struct Spinner {
    handle: SpinnerHandle,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Spinner {
    pub(crate) fn new(initial: &str, flavors: &'static [&'static str]) -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan.bold} [{elapsed:>3}] {msg}")
                .expect("static template")
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message(initial.to_string());
        let state = Arc::new(Mutex::new(SpinnerState {
            base: initial.to_string(),
            since: Instant::now(),
            flavors,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let join = if flavors.is_empty() {
            None
        } else {
            let pb_t = pb.clone();
            let state_t = state.clone();
            let stop_t = stop.clone();
            Some(std::thread::spawn(move || {
                while !stop_t.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    if stop_t.load(Ordering::Relaxed) {
                        break;
                    }
                    let (base, elapsed, flavors) = {
                        let s = state_t.lock().expect("spinner state mutex poisoned");
                        (s.base.clone(), s.since.elapsed(), s.flavors)
                    };
                    if elapsed >= Duration::from_secs(10) && !flavors.is_empty() {
                        let idx = ((elapsed.as_secs() - 10) / 4) as usize % flavors.len();
                        pb_t.set_message(format!("{base}  ·  {}…", flavors[idx]));
                    }
                }
            }))
        };
        Spinner {
            handle: SpinnerHandle { pb, state },
            stop,
            join,
        }
    }

    pub(crate) fn handle(&self) -> SpinnerHandle {
        self.handle.clone()
    }

    pub(crate) fn set_message(&self, msg: impl Into<String>) {
        self.handle.set_message(msg);
    }

    pub(crate) fn finish_with_message(&mut self, msg: String) {
        self.stop_thread();
        self.handle.pb.finish_with_message(msg);
    }

    pub(crate) fn abandon_with_message(&mut self, msg: String) {
        self.stop_thread();
        self.handle.pb.abandon_with_message(msg);
    }

    fn stop_thread(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop_thread();
    }
}
