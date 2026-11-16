// SPDX-License-Identifier: GPL-3.0-or-later
//! Owns the Go `rootbox` child: start, probe readiness, stream stats, restart.
//!
//! No FFI: the core is an ordinary process. Control flows through the
//! `runtime/stop` file, readiness through `runtime/ready`, telemetry through
//! `WANBOX_STATS:` lines on stdout.

use crate::error::{self, Code};
use crate::process;
use crate::runtime::Runtime;
use crate::snapshot::Snapshot;
use crate::state::{ProcessIdentity, State, Stats};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const STATS_PREFIX: &str = "WANBOX_STATS:";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const READY_POLL: Duration = Duration::from_millis(100);
const STATS_MIN_INTERVAL: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(25);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RETRIES: u32 = 3;
/// A chatty `log.level: info` core can emit tens of MB per day.
const CORE_LOG_MAX_BYTES: u64 = 4 << 20;
const CORE_LOG_KEEP: usize = 2;

pub struct Supervisor {
    runtime: Arc<Runtime>,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

impl Supervisor {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self {
            runtime,
            state: Arc::new(Mutex::new(State::stopped())),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    fn persist(&self) {
        let state = self.state.lock().expect("state lock").clone();
        let _ = self.runtime.write_state(&state);
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        {
            let mut state = self.state.lock().expect("state lock");
            change(&mut state);
        }
        self.persist();
    }

    /// The supervisor stops on its own marker, on an explicit stop file, or
    /// when the module manager marks the module for removal. The last one
    /// matters: `uninstall.sh` only runs at the next boot, and leaving a live
    /// TUN until then would keep routing traffic for a module that is gone.
    fn should_stop(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
            || self.runtime.path(&["runtime", "stop"]).exists()
            || self.runtime.module_dir().join("remove").exists()
    }

    /// Clear the live fields so a stopped module never leaves a stale
    /// `connected` behind in `state.json`.
    fn mark_stopped(&self) {
        self.update(|state| {
            state.phase = "stopped".into();
            state.running_revision.clear();
            state.core = ProcessIdentity::default();
            state.stats = None;
            state.error.clear();
        });
    }

    /// Run until stopped. Retries the core with 1/2/4 s backoff.
    pub fn run(&self, revision: &str, snapshot: &Snapshot) -> Result<(), Code> {
        self.runtime.init().map_err(|_| error::STATE_WRITE_FAILED)?;
        process::independent_cgroup()?;
        let supervisor = process::identity(std::process::id() as i32)?;
        {
            let mut state = self.state.lock().expect("state lock");
            *state = State {
                schema_version: crate::state::PROTOCOL_VERSION,
                phase: "starting".into(),
                revision: revision.to_string(),
                running_revision: revision.to_string(),
                profile_id: snapshot.profile_id,
                profile_name: snapshot.profile_name.clone(),
                supervisor,
                ..Default::default()
            };
        }
        self.persist();

        let mut attempt = 0;
        loop {
            if self.should_stop() {
                self.mark_stopped();
                return Ok(());
            }
            let _ = std::fs::remove_file(self.runtime.path(&["runtime", "ready"]));
            self.update(|state| {
                state.phase = "starting".into();
                state.error.clear();
            });
            match self.run_generation(revision, snapshot) {
                Ok(()) => {
                    self.mark_stopped();
                    return Ok(());
                }
                Err(code) if code == error::CLEANUP_TIMEOUT => {
                    self.update(|state| {
                        state.phase = "failed".into();
                        state.error = "cleanup_required".into();
                    });
                    return Err(code);
                }
                Err(_) if attempt < MAX_RETRIES => {
                    self.update(|state| {
                        state.phase = "starting".into();
                        state.error = "core_retry".into();
                        state.stats = None;
                    });
                    std::thread::sleep(Duration::from_secs(1 << attempt));
                    attempt += 1;
                }
                Err(_) => {
                    self.update(|state| {
                        state.phase = "failed".into();
                        state.error = "retry_limit_reached".into();
                    });
                    self.runtime.record_event("retry_limit_reached");
                    return Err(error::RETRY_LIMIT);
                }
            }
        }
    }

    fn run_generation(&self, revision: &str, snapshot: &Snapshot) -> Result<(), Code> {
        let generation = self.runtime.path(&["generations", revision]);
        let mut plugins = Vec::new();
        for plugin in &snapshot.plugins {
            plugins.push(self.spawn_plugin(&generation, plugin)?);
        }
        let result = self.spawn_core(&generation);
        for mut child in plugins {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }

    fn spawn_plugin(
        &self,
        generation: &std::path::Path,
        plugin: &crate::snapshot::Plugin,
    ) -> Result<std::process::Child, Code> {
        let binary = generation.join(&plugin.binary);
        let config = generation.join(&plugin.config);
        let mut command = Command::new(&binary);
        match plugin.kind.as_str() {
            "trojan-go" => {
                command.args(["-config"]).arg(&config);
            }
            "mieru" => {
                command.arg("run").env("MIERU_CONFIG_JSON_FILE", &config);
            }
            "naive" => {
                command.arg(&config);
                if !plugin.certificate.is_empty() {
                    command.env("SSL_CERT_FILE", generation.join(&plugin.certificate));
                }
            }
            "hysteria" => {
                command.args(["--no-check", "--config"]).arg(&config).args([
                    "--log-level",
                    "warn",
                    "client",
                ]);
            }
            _ => return Err(error::PLUGIN_UNSUPPORTED),
        }
        command
            .current_dir(generation.join("run"))
            .env("LD_LIBRARY_PATH", binary.parent().unwrap_or(generation))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        process::parent_death_signal(&mut command);
        command.spawn().map_err(|_| error::CORE_START_FAILED)
    }

    fn spawn_core(&self, generation: &std::path::Path) -> Result<(), Code> {
        let binary = self.runtime.module_dir().join("bin/rootbox");
        let assets = generation.join("assets");
        // `geosite:cn`-style aliases only resolve once the .srs is on disk, so
        // the core gets a copy with absolute paths instead of the raw config.
        let mut config: serde_json::Value = crate::atomic::read_json(
            &generation.join("run/config.json"),
            crate::snapshot::MAX_CONFIG_BYTES as u64,
        )
        .map_err(|_| error::CONFIG_MISSING)?;
        crate::ruleset::prepare(&assets, &mut config)?;
        crate::endpoint::prepare(self.runtime.root(), &mut config)?;
        let effective = generation.join("run/effective.json");
        crate::atomic::write_json(&effective, &config).map_err(|_| error::STATE_WRITE_FAILED)?;
        // The core writes its diagnostics here; without it a bad config only
        // shows up as a generic `core_start_failed`. Rotate instead of truncating
        // so the previous crash survives one restart, and so a long-lived core
        // cannot fill the data partition.
        let log_path = self.runtime.path(&["runtime", "core.log"]);
        crate::atomic::rotate(&log_path, CORE_LOG_MAX_BYTES, CORE_LOG_KEEP)
            .map_err(|_| error::STATE_WRITE_FAILED)?;
        let core_log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|_| error::STATE_WRITE_FAILED)?;
        let mut command = Command::new(binary);
        command
            .arg(&effective)
            .arg(&assets)
            .arg(self.runtime.path(&["runtime", "core.pid"]))
            .arg(self.runtime.path(&["runtime", "ready"]))
            .arg(self.runtime.path(&["runtime", "stop"]))
            .arg(std::process::id().to_string())
            .current_dir(generation.join("run"))
            .env("WANBOX_MODULE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(core_log));
        process::parent_death_signal(&mut command);
        let mut child = command.spawn().map_err(|_| error::CORE_START_FAILED)?;
        let identity = process::identity(child.id() as i32)?;
        self.update(|state| state.core = identity.clone());

        let stdout = child.stdout.take().ok_or(error::CORE_START_FAILED)?;
        let stats_state = Arc::clone(&self.state);
        let stats_runtime = Arc::clone(&self.runtime);
        let reader = std::thread::spawn(move || {
            let mut last = Instant::now() - STATS_MIN_INTERVAL;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some(raw) = line.strip_prefix(STATS_PREFIX) else {
                    continue;
                };
                let Ok(stats) = serde_json::from_str::<Stats>(raw) else {
                    continue;
                };
                if stats.tag.len() > 512
                    || stats.tx < 0
                    || stats.rx < 0
                    || stats.direct_tx < 0
                    || stats.direct_rx < 0
                {
                    continue;
                }
                if last.elapsed() < STATS_MIN_INTERVAL {
                    continue;
                }
                last = Instant::now();
                let mut state = stats_state.lock().expect("state lock");
                state.stats = Some(stats);
                let _ = stats_runtime.write_state(&state);
            }
        });

        let started = Instant::now();
        loop {
            if self.should_stop() {
                process::terminate(&identity).ok();
                let _ = child.wait();
                let _ = reader.join();
                return Ok(());
            }
            if self.runtime.path(&["runtime", "ready"]).exists() {
                self.update(|state| {
                    if state.phase != "connected" {
                        state.phase = "connected".into();
                    }
                });
                // Keep polling instead of blocking in `wait()`: a stop request or
                // the manager's `remove` marker must be able to end a live core.
                let status = loop {
                    if self.should_stop() {
                        process::terminate(&identity).ok();
                        let _ = child.wait();
                        break None;
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) => std::thread::sleep(READY_POLL),
                        Err(_) => break Some(std::process::ExitStatus::default()),
                    }
                };
                let _ = reader.join();
                self.update(|state| {
                    state.core = ProcessIdentity::default();
                    state.stats = None;
                });
                return match status {
                    // Asked to stop: not a failure.
                    None => Ok(()),
                    // Exited 0 on its own: also a clean end, no retry storm.
                    Some(status) if status.success() => Ok(()),
                    Some(_) => Err(error::CORE_START_FAILED),
                };
            }
            if started.elapsed() > STARTUP_TIMEOUT {
                process::terminate(&identity).ok();
                let _ = child.wait();
                let _ = reader.join();
                return Err(error::STARTUP_TIMEOUT);
            }
            std::thread::sleep(READY_POLL);
        }
    }

    /// Write the stop marker and wait for supervisor and core to disappear.
    pub fn stop(runtime: &Runtime) -> Result<(), Code> {
        let state = runtime.status();
        if !process::same_process(&state.supervisor) && !process::same_process(&state.core) {
            return Ok(());
        }
        crate::atomic::write(&runtime.path(&["runtime", "stop"]), b"stop", 0o600)
            .map_err(|_| error::STATE_WRITE_FAILED)?;
        if process::same_process(&state.supervisor) {
            process::terminate(&state.supervisor).map_err(|_| error::CLEANUP_TIMEOUT)?;
        }
        if process::same_process(&state.core) {
            process::terminate(&state.core).map_err(|_| error::CLEANUP_TIMEOUT)?;
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if !process::same_process(&state.supervisor) && !process::same_process(&state.core) {
                let _ = std::fs::remove_file(runtime.path(&["runtime", "ready"]));
                let mut final_state = runtime.read_state();
                final_state.schema_version = crate::state::PROTOCOL_VERSION;
                final_state.phase = "stopped".into();
                final_state.revision = runtime.current_revision().unwrap_or_default();
                final_state.running_revision.clear();
                final_state.supervisor = ProcessIdentity::default();
                final_state.core = ProcessIdentity::default();
                final_state.stats = None;
                final_state.error.clear();
                let _ = runtime.write_state(&final_state);
                runtime.record_event("core_stopped");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(error::CLEANUP_TIMEOUT)
    }
}

/// Decode and construct the core for a staged generation without starting it.
/// `rootbox --check` validates the real sing-box schema, which the snapshot
/// layer cannot do; a typo like `auto_detect_auto` is caught here instead of at
/// start time.
pub fn check_generation(runtime: &Runtime, revision: &str) -> Result<(), Code> {
    let generation = runtime.path(&["generations", revision]);
    let config = generation.join("run/config.json");
    if !config.is_file() {
        return Err(error::CONFIG_MISSING);
    }
    // The core validates a config it can actually load, so resolve ruleset
    // aliases and consume module-only hints (`url`) exactly like a real start.
    let assets = generation.join("assets");
    let mut resolved: serde_json::Value =
        crate::atomic::read_json(&config, crate::snapshot::MAX_CONFIG_BYTES as u64)
            .map_err(|_| error::CONFIG_MISSING)?;
    crate::ruleset::prepare(&assets, &mut resolved)?;
    crate::endpoint::prepare(runtime.root(), &mut resolved)?;
    let checked = generation.join("run/checked.json");
    crate::atomic::write_json(&checked, &resolved).map_err(|_| error::STATE_WRITE_FAILED)?;
    let output = Command::new(runtime.module_dir().join("bin/rootbox"))
        .arg("--check")
        .arg(&checked)
        .arg(generation.join("assets"))
        .current_dir(generation.join("run"))
        .env("WANBOX_MODULE", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|_| error::CORE_START_FAILED)?;
    if output.status.success() {
        return Ok(());
    }
    // Keep the core's exact reason in the log; the caller gets a stable code
    // because error codes never carry raw text.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::write(runtime.path(&["runtime", "check.log"]), stderr.as_bytes());
    Err(error::CONFIG_INVALID)
}

pub fn generation_dir(runtime: &Runtime, revision: &str) -> PathBuf {
    runtime.path(&["generations", revision])
}

pub fn cleanup_wait() -> Duration {
    CLEANUP_TIMEOUT
}
