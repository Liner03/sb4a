// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration generations, atomic activation and status reporting.

use crate::atomic;
use crate::error::{self, Code};
use crate::process;
use crate::snapshot::{self, Snapshot};
use crate::state::{Event, State, PROTOCOL_VERSION};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOCK_TIMEOUT: Duration = Duration::from_secs(65);
const MAX_EVENTS: usize = 40;

#[derive(Clone)]
pub struct Runtime {
    root: PathBuf,
    module_dir: PathBuf,
}

impl Runtime {
    pub fn new(root: impl Into<PathBuf>, module_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            module_dir: module_dir.into(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn module_dir(&self) -> &Path {
        &self.module_dir
    }

    pub fn path(&self, parts: &[&str]) -> PathBuf {
        let mut path = self.root.clone();
        for part in parts {
            path.push(part);
        }
        path
    }

    pub fn init(&self) -> io::Result<()> {
        atomic::private_dir(&self.root)?;
        atomic::private_dir(&self.path(&["generations"]))?;
        atomic::private_dir(&self.path(&["runtime"]))
    }

    pub fn lock(&self) -> Result<atomic::Lock, Code> {
        self.init().map_err(|_| error::STATE_WRITE_FAILED)?;
        atomic::Lock::acquire(&self.path(&["control.lock"]), LOCK_TIMEOUT)
            .map_err(|_| error::MODULE_BUSY)
    }

    /// Non-blocking variant: a concurrent operation must report `module_busy`
    /// immediately instead of stalling the caller for the full lock timeout.
    pub fn try_lock(&self) -> Result<atomic::Lock, Code> {
        self.init().map_err(|_| error::STATE_WRITE_FAILED)?;
        atomic::Lock::try_acquire(&self.path(&["control.lock"])).map_err(|_| error::MODULE_BUSY)
    }

    pub fn enabled(&self) -> bool {
        if !self.module_dir.join("module.prop").is_file() {
            return false;
        }
        !self.module_dir.join("disable").exists() && !self.module_dir.join("remove").exists()
    }

    pub fn current_revision(&self) -> Result<String, Code> {
        let path = self.path(&["current"]);
        match fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).trim().to_string();
                if snapshot::is_revision(&text) {
                    Ok(text)
                } else {
                    Err(error::CURRENT_INVALID)
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(_) => Err(error::CURRENT_INVALID),
        }
    }

    pub fn current(&self) -> Result<(String, Option<Snapshot>), Code> {
        let revision = self.current_revision()?;
        if revision.is_empty() {
            return Ok((revision, None));
        }
        let path = self.path(&["generations", &revision, "snapshot.json"]);
        let snapshot: Snapshot = atomic::read_json(&path, snapshot::MAX_SNAPSHOT_BYTES)
            .map_err(|_| error::SNAPSHOT_INVALID)?;
        snapshot::check(&snapshot)?;
        Ok((revision, Some(snapshot)))
    }

    /// Materialise a validated generation and return its content hash.
    pub fn stage(&self, snapshot: &Snapshot) -> Result<String, Code> {
        snapshot::check(snapshot)?;
        self.init().map_err(|_| error::STATE_WRITE_FAILED)?;
        let encoded = serde_json::to_vec(snapshot).map_err(|_| error::SNAPSHOT_INVALID)?;
        let revision = hex::encode(Sha256::digest(&encoded));
        let staging = self.path(&["generations", &format!(".stage-{}", std::process::id())]);
        let _ = fs::remove_dir_all(&staging);
        atomic::private_dir(&staging).map_err(|_| error::STATE_WRITE_FAILED)?;
        let result = (|| -> Result<(), Code> {
            for dir in ["run", "assets", "files", "plugins"] {
                atomic::private_dir(&staging.join(dir)).map_err(|_| error::STATE_WRITE_FAILED)?;
            }
            for (name, data) in &snapshot.files {
                let target = staging.join(name);
                if let Some(parent) = target.parent() {
                    atomic::private_dir(parent).map_err(|_| error::STATE_WRITE_FAILED)?;
                }
                let mode = if name.starts_with("plugins/") {
                    0o700
                } else {
                    0o600
                };
                atomic::write(&target, data, mode).map_err(|_| error::STATE_WRITE_FAILED)?;
            }
            atomic::write(
                &staging.join("run/config.json"),
                &snapshot.config_bytes(),
                0o600,
            )
            .map_err(|_| error::STATE_WRITE_FAILED)?;
            atomic::write_json(&staging.join("snapshot.json"), snapshot)
                .map_err(|_| error::STATE_WRITE_FAILED)?;
            Ok(())
        })();
        if let Err(code) = result {
            let _ = fs::remove_dir_all(&staging);
            return Err(code);
        }
        let destination = self.path(&["generations", &revision]);
        if destination.exists() {
            let _ = fs::remove_dir_all(&staging);
            return Ok(revision);
        }
        fs::rename(&staging, &destination).map_err(|_| error::STATE_WRITE_FAILED)?;
        atomic::sync_dir(&self.path(&["generations"])).map_err(|_| error::STATE_WRITE_FAILED)?;
        Ok(revision)
    }

    pub fn write_state(&self, state: &State) -> Result<(), Code> {
        atomic::write_json(&self.path(&["state.json"]), state)
            .map_err(|_| error::STATE_WRITE_FAILED)
    }

    pub fn read_state(&self) -> State {
        atomic::read_json::<State>(&self.path(&["state.json"]), 1 << 20)
            .unwrap_or_else(|_| State::stopped())
    }

    /// Report only what this process can prove: stale `connected` never survives.
    pub fn status(&self) -> State {
        let stored = self.read_state();
        let state = self.reconcile(stored.clone());
        // A process killed with SIGKILL never runs its cleanup, so the record on
        // disk can still claim `connected`. Repair it here, otherwise every later
        // reader (and the module manager UI) sees a lie. Only write when the
        // proven state actually differs, so `status` stays read-mostly.
        if state.phase != stored.phase || state.error != stored.error {
            let _ = self.write_state(&state);
        }
        state
    }

    /// Apply liveness checks to a stored state without touching the disk.
    fn reconcile(&self, mut state: State) -> State {
        if state.schema_version == 0 {
            state.schema_version = PROTOCOL_VERSION;
        }
        state.revision = self.current_revision().unwrap_or_default();
        if !self.enabled() {
            state.phase = "disabled".into();
            state.stats = None;
            return state;
        }
        let supervisor_live = process::same_process(&state.supervisor);
        let core_live = process::same_process(&state.core);
        if !supervisor_live {
            if core_live {
                state.phase = "cleanup_required".into();
                state.error = "supervisor_missing".into();
            } else if state.phase != "failed" {
                state.phase = "stopped".into();
                state.running_revision.clear();
                state.supervisor = crate::state::ProcessIdentity::default();
                state.core = crate::state::ProcessIdentity::default();
                state.error.clear();
            }
        } else if state.phase == "connected" {
            if !core_live {
                state.phase = "failed".into();
                state.error = "core_exited".into();
            } else if !self.path(&["runtime", "ready"]).exists() {
                state.phase = "starting".into();
            }
        }
        if state.phase != "connected" {
            state.stats = None;
        }
        state
    }

    pub fn events(&self) -> Vec<Event> {
        atomic::read_json(&self.path(&["events.json"]), 64 << 10).unwrap_or_default()
    }

    pub fn record_event(&self, code: &str) {
        if !error::is_valid_code(code) {
            return;
        }
        let mut events = self.events();
        events.push(Event {
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            code: code.to_string(),
        });
        if events.len() > MAX_EVENTS {
            events.drain(0..events.len() - MAX_EVENTS);
        }
        let _ = atomic::write_json(&self.path(&["events.json"]), &events);
    }

    /// Keep current/previous/running generations only.
    pub fn prune_generations(&self) {
        let current = self.current_revision().unwrap_or_default();
        let state = self.status();
        let mut keep = vec![current, state.running_revision];
        if let Ok(raw) = fs::read(self.path(&["previous"])) {
            keep.push(String::from_utf8_lossy(&raw).trim().to_string());
        }
        let Ok(entries) = fs::read_dir(self.path(&["generations"])) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.path().is_dir() && snapshot::is_revision(&name) && !keep.contains(&name) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_runtime() -> (Runtime, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "sb4a-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let module = base.join("module");
        fs::create_dir_all(&module).unwrap();
        fs::write(module.join("module.prop"), "id=sb4a").unwrap();
        (Runtime::new(base.join("data"), module), base)
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            schema_version: 1,
            expected_revision: String::new(),
            config: serde_json::json!({"log": {"level": "warn"}}),
            files: Default::default(),
            plugins: Vec::new(),
            performance_priority: false,
            auto_start: false,
            profile_id: 7,
            profile_name: "fixture".into(),
        }
    }

    #[test]
    fn stage_is_content_addressed_and_idempotent() {
        let (runtime, base) = temp_runtime();
        let first = runtime.stage(&snapshot()).unwrap();
        let second = runtime.stage(&snapshot()).unwrap();
        assert_eq!(first, second);
        assert!(runtime
            .path(&["generations", &first, "run", "config.json"])
            .is_file());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn status_repairs_state_left_by_a_killed_supervisor() {
        let (runtime, base) = temp_runtime();
        runtime.stage(&snapshot()).unwrap();
        // A supervisor killed with SIGKILL leaves `connected` behind.
        let mut stale = State::stopped();
        stale.phase = "connected".into();
        stale.running_revision = "a".repeat(64);
        stale.supervisor = crate::state::ProcessIdentity {
            pid: 2_000_000,
            start: "1".into(),
            executable: "/nonexistent".into(),
        };
        runtime.write_state(&stale).unwrap();

        let reported = runtime.status();
        assert_eq!(reported.phase, "stopped");
        assert!(reported.running_revision.is_empty());
        assert_eq!(reported.supervisor.pid, 0);

        // The repair is persisted, not just computed.
        let on_disk = runtime.read_state();
        assert_eq!(on_disk.phase, "stopped");
        assert!(on_disk.running_revision.is_empty());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn status_reports_stopped_without_supervisor() {
        let (runtime, base) = temp_runtime();
        runtime.stage(&snapshot()).unwrap();
        let state = runtime.status();
        assert_eq!(state.phase, "stopped");
        assert!(state.stats.is_none());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn disabled_module_reports_disabled() {
        let (runtime, base) = temp_runtime();
        fs::write(runtime.module_dir().join("disable"), "").unwrap();
        assert_eq!(runtime.status().phase, "disabled");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn lock_is_exclusive() {
        let (runtime, base) = temp_runtime();
        let guard = runtime.lock().unwrap();
        // A concurrent attempt must fail fast, not burn the whole 65s timeout.
        let started = std::time::Instant::now();
        assert!(runtime.try_lock().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(guard);
        assert!(runtime.lock().is_ok());
        let _ = fs::remove_dir_all(base);
    }
}
