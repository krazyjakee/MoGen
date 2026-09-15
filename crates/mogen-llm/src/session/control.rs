use crate::{GenerateConfig, Usage};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLimits {
    pub calls: u32,
    pub iterations: u32,
    pub seconds: u64,
    /// Estimated USD; unknown pricing stops a money-limited session.
    pub spend_usd: Option<f64>,
    pub output_tokens: u32,
}
impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            calls: 12,
            iterations: 3,
            seconds: 600,
            spend_usd: None,
            output_tokens: 8192,
        }
    }
}
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionMeter {
    pub calls: u32,
    pub cancelled: bool,
    pub usage: Usage,
    pub estimated_usd: f64,
    pub unknown_cost: bool,
    pub stage: String,
    pub stage_started_seconds: f64,
    pub call_pending: bool,
    pub stopped: Option<String>,
}
#[derive(Debug, Clone)]
pub struct SessionControl(Arc<Control>);
#[derive(Debug)]
struct Control {
    limits: SessionLimits,
    started: Instant,
    prior_elapsed: Duration,
    state: Mutex<SessionMeter>,
    finished: Mutex<Option<Duration>>,
}
impl SessionControl {
    pub fn new(limits: SessionLimits) -> Self {
        Self(Arc::new(Control {
            limits,
            started: Instant::now(),
            prior_elapsed: Duration::ZERO,
            state: Mutex::new(SessionMeter::default()),
            finished: Mutex::new(None),
        }))
    }
    /// A deliberate resume clears the stop flag, preserving charged usage and
    /// elapsed budget. In-flight requests without a saved response are uncertain.
    pub fn resume(limits: SessionLimits, mut meter: SessionMeter, seconds: u64) -> Self {
        meter.cancelled = false;
        meter.stopped = None;
        Self(Arc::new(Control {
            limits,
            started: Instant::now(),
            prior_elapsed: Duration::from_secs(seconds),
            state: Mutex::new(meter),
            finished: Mutex::new(None),
        }))
    }
    pub fn limits(&self) -> &SessionLimits {
        &self.0.limits
    }
    pub fn meter(&self) -> SessionMeter {
        self.0.state.lock().unwrap().clone()
    }
    pub fn elapsed(&self) -> Duration {
        self.0
            .finished
            .lock()
            .unwrap()
            .unwrap_or_else(|| self.0.prior_elapsed + self.0.started.elapsed())
    }
    pub fn finish(&self) {
        self.0
            .finished
            .lock()
            .unwrap()
            .get_or_insert_with(|| self.0.prior_elapsed + self.0.started.elapsed());
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.state.lock().unwrap().cancelled
    }
    pub fn cancel(&self) {
        self.0.state.lock().unwrap().cancelled = true;
        self.stop("Cancelled; an in-flight provider request may still be billed");
    }
    pub fn stop(&self, reason: &str) {
        self.0
            .state
            .lock()
            .unwrap()
            .stopped
            .get_or_insert_with(|| reason.into());
    }
    pub fn check(&self) -> Result<(), String> {
        self.check_at(self.elapsed())
    }
    pub fn check_at(&self, elapsed: Duration) -> Result<(), String> {
        let mut state = self.0.state.lock().unwrap();
        if elapsed >= Duration::from_secs(self.0.limits.seconds) {
            state
                .stopped
                .get_or_insert_with(|| "Time limit reached".into());
        }
        match &state.stopped {
            Some(s) => Err(s.clone()),
            None => Ok(()),
        }
    }
    /// All derived configurations share this admission gate, including repair.
    pub fn before_call(
        &self,
        cfg: &GenerateConfig,
        price: Option<crate::spend::pricing::TextPricing>,
    ) -> Result<(), String> {
        self.check()?;
        let mut state = self.0.state.lock().unwrap();
        if let Some(reason) = &state.stopped {
            return Err(reason.clone());
        }
        let reason = if state.calls >= self.0.limits.calls {
            Some("Call limit reached")
        } else if let Some(limit) = self.0.limits.spend_usd {
            if !limit.is_finite() || limit <= 0.0 {
                Some("Spend limit reached")
            } else if let Some(p) = price {
                // Reserve generously from UTF-8 byte counts (not a token guarantee),
                // images and the output cap. Provider tokenization is not local.
                let input = cfg.user_prompt.len()
                    + cfg.system_instruction.as_ref().map_or(0, String::len)
                    + cfg.history.iter().map(|t| t.text.len()).sum::<usize>()
                    + cfg.user_images.len() * 8192;
                let reserve = (input as f64 * p.input_per_mtok.max(p.input_per_mtok_long)
                    + self.0.limits.output_tokens as f64
                        * p.output_per_mtok.max(p.output_per_mtok_long))
                    / 1_000_000.0;
                if state.unknown_cost || state.estimated_usd + reserve > limit {
                    Some("Estimated spend limit reached")
                } else {
                    None
                }
            } else {
                Some("Pricing unavailable; remove the USD limit or configure pricing")
            }
        } else {
            None
        };
        if let Some(reason) = reason {
            state.stopped = Some(reason.into());
            return Err(reason.into());
        }
        if price.is_none() {
            state.unknown_cost = true;
        }
        state.calls += 1;
        state.stage = cfg.spend_context.operation.clone();
        state.stage_started_seconds = self.elapsed().as_secs_f64();
        state.call_pending = true;
        Ok(())
    }
    pub fn after_call(
        &self,
        usage: Option<&Usage>,
        price: Option<crate::spend::pricing::TextPricing>,
    ) {
        let mut state = self.0.state.lock().unwrap();
        state.call_pending = false;
        if let Some(u) = usage {
            state.usage.add(u);
            if let Some(p) = price {
                state.estimated_usd += crate::spend::pricing::compute_cost(u, p);
            } else {
                state.unknown_cost = true;
            }
        } else {
            state.unknown_cost = true;
        }
        if self
            .0
            .limits
            .spend_usd
            .is_some_and(|limit| state.estimated_usd >= limit)
        {
            state.stopped.get_or_insert_with(|| {
                "Spend limit reached; in-flight usage may exceed the estimate".into()
            });
        }
    }
}

/// Isolate CLI launchers and their descendants so cancellation can stop the
/// actual provider process, even when the executable is a wrapper script.
pub fn configure_child(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    // configure_child creates a fresh group whose ID is the launcher's PID.
    // A negative PID targets only that group, including inherited pipe owners.
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// Drain both pipes while observing cancellation/deadline, then reap the child.
/// The provider adapter remains responsible for decoding any reported usage.
/// Call `configure_child` before spawning to enable process-tree cancellation.
pub fn wait_for_child(
    mut child: std::process::Child,
    control: Option<&SessionControl>,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut bytes = vec![];
        if let Some(mut s) = stdout {
            s.read_to_end(&mut bytes)?;
        }
        Ok::<_, std::io::Error>(bytes)
    });
    let err = std::thread::spawn(move || {
        let mut bytes = vec![];
        if let Some(mut s) = stderr {
            s.read_to_end(&mut bytes)?;
        }
        Ok::<_, std::io::Error>(bytes)
    });
    let status = loop {
        if control.is_some_and(|c| c.check().is_err()) {
            terminate_child(&mut child);
            break child.wait()?;
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // A launcher may exit before the provider. Continue observing the control
    // while descendants hold stdout/stderr open instead of blocking in join.
    while !out.is_finished() || !err.is_finished() {
        if control.is_some_and(|c| c.check().is_err()) {
            terminate_child(&mut child);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(std::process::Output {
        status,
        stdout: out
            .join()
            .map_err(|_| std::io::Error::other("stdout reader failed"))??,
        stderr: err
            .join()
            .map_err(|_| std::io::Error::other("stderr reader failed"))??,
    })
}
