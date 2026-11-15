// SPDX-License-Identifier: GPL-3.0-or-later
//! sb4a control CLI. Mirrors the Go `wanboxctl` command surface plus a
//! standard-config entry point for GUI-generated `config.json`.

use sb4a_core::error::{self, Code};
use sb4a_core::process;
use sb4a_core::runtime::Runtime;
use sb4a_core::snapshot::{self, Snapshot};
use sb4a_core::state::Response;
use sb4a_core::supervisor::Supervisor;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DATA_ROOT: &str = "/data/adb/sb4a";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = runtime_from_exe();

    let error = if !is_root() {
        Some(error::ROOT_REQUIRED)
    } else if let Err(code) = process::independent_cgroup() {
        Some(code)
    } else {
        run(&runtime, &args).err()
    };

    // `config convert` prints a config on stdout; a status envelope after it
    // would corrupt the pipe, so it reports through stderr and the exit code.
    if args.as_slice() == ["config", "convert"] {
        if let Some(code) = &error {
            eprintln!("error: {}", sanitize(code));
            std::process::exit(1);
        }
        return;
    }

    let mut state = runtime.status();
    if args.as_slice() == ["logs"] {
        state.events = runtime.events();
        // The full core output is too large for a JSON envelope; point at it.
        if runtime.path(&["runtime", "core.log"]).exists() {
            println!(
                "core.log: {}",
                runtime.path(&["runtime", "core.log"]).display()
            );
        }
    }
    let response = Response::new(state, error.map(|code| sanitize(&code)));
    println!("{}", serde_json::to_string(&response).unwrap_or_default());
    if !response.ok {
        std::process::exit(1);
    }
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn sanitize(code: &Code) -> String {
    if error::is_valid_code(code.0) {
        code.0.to_string()
    } else {
        error::MODULE_OPERATION_FAILED.0.to_string()
    }
}

fn runtime_from_exe() -> Runtime {
    let module_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|p| p.to_path_buf()))
        .and_then(|bin| bin.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/data/adb/modules/sb4a"));
    Runtime::new(DATA_ROOT, module_dir)
}

fn run(runtime: &Runtime, args: &[String]) -> Result<(), Code> {
    match args {
        [command] if command == "status" || command == "module" || command == "logs" => Ok(()),
        [command] if command == "start" => start(runtime),
        [command] if command == "stop" => Supervisor::stop(runtime),
        [command] if command == "restart" || command == "reload" => {
            // One lock for the whole stop-then-start window: another CLI must
            // not slip an `apply` in between.
            let guard = runtime.lock()?;
            Supervisor::stop(runtime)?;
            start_locked(runtime, guard)
        }
        [group, action] if group == "config" && action == "rollback" => rollback(runtime),
        [command, action] if command == "config" && action == "import" => import_config(runtime),
        // `config convert` prints a portable config to stdout without touching
        // the active generation, so it can be inspected before applying.
        [command, action] if command == "config" && action == "convert" => convert_config(),
        [group, action] if group == "config" && (action == "apply" || action == "validate") => {
            let mut raw = Vec::new();
            std::io::stdin()
                .take(snapshot::MAX_SNAPSHOT_BYTES + 1)
                .read_to_end(&mut raw)
                .map_err(|_| error::SNAPSHOT_INVALID)?;
            let snapshot = snapshot::decode_any(&raw)?;
            if action == "validate" {
                let revision = runtime.stage(&snapshot)?;
                sb4a_core::supervisor::check_generation(runtime, &revision)
            } else {
                apply(runtime, snapshot)
            }
        }
        [group, action] if group == "autostart" && (action == "on" || action == "off") => {
            let _guard = runtime.lock()?;
            sb4a_core::atomic::write_json(
                &runtime.path(&["boot.json"]),
                &serde_json::json!({ "autoStart": action == "on" }),
            )
            .map_err(|_| error::STATE_WRITE_FAILED)
        }
        [group, action] if group == "ruleset" && action == "update" => {
            sb4a_core::ruleset::update_all(runtime)
        }
        [group, action] if group == "ruleset" && action == "list" => {
            for name in sb4a_core::ruleset::list_cached(runtime) {
                println!("{name}");
            }
            Ok(())
        }
        // `ruleset download <family> <code> [url]` pulls one ruleset into the
        // active generation so a config can reference it offline. Supplying a URL
        // uses that source instead of the default mirror.
        [group, action, family, code] if group == "ruleset" && action == "download" => {
            let path = sb4a_core::ruleset::download(runtime, family, code)?;
            runtime.record_event("ruleset_downloaded");
            println!("{}", path.display());
            Ok(())
        }
        [group, action, family, code, url] if group == "ruleset" && action == "download" => {
            let path = sb4a_core::ruleset::download_from(runtime, family, code, Some(url))?;
            runtime.record_event("ruleset_downloaded");
            println!("{}", path.display());
            Ok(())
        }
        [internal, action] if internal == "__internal" && action == "supervise" => {
            supervise(runtime)
        }
        [internal, action] if internal == "__internal" && action == "boot" => boot(runtime),
        _ => Err(error::ARGUMENTS_INVALID),
    }
}

/// Rewrite a desktop config into a module-portable one and print it.
fn convert_config() -> Result<(), Code> {
    let mut raw = Vec::new();
    std::io::stdin()
        .take(snapshot::MAX_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| error::CONFIG_INVALID)?;
    if raw.len() as u64 > snapshot::MAX_SNAPSHOT_BYTES {
        return Err(error::SNAPSHOT_TOO_LARGE);
    }
    let mut config: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| error::CONFIG_INVALID)?;
    let report = sb4a_core::portable::make_portable(&mut config)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&config).unwrap_or_default()
    );
    // Diagnostics go to stderr so stdout stays pipeable into `config import`.
    for item in &report.removed {
        eprintln!("removed: {item}");
    }
    for item in &report.normalised {
        eprintln!("normalised: {item}");
    }
    for item in &report.dropped {
        eprintln!("dropped rule_set (no portable source): {item}");
    }
    eprintln!("rule_sets rewritten: {}", report.rulesets);
    Ok(())
}

/// Accept a plain sing-box `config.json` and wrap it in the envelope the module
/// expects, so a config copied from a desktop client works unchanged.
fn import_config(runtime: &Runtime) -> Result<(), Code> {
    let mut raw = Vec::new();
    std::io::stdin()
        .take(snapshot::MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| error::CONFIG_INVALID)?;
    if raw.len() > snapshot::MAX_CONFIG_BYTES {
        return Err(error::SNAPSHOT_TOO_LARGE);
    }
    apply(runtime, snapshot::decode_any(&raw)?)
}

fn apply(runtime: &Runtime, snapshot: Snapshot) -> Result<(), Code> {
    let guard = runtime.lock()?;
    let (current_revision, current) = runtime.current()?;
    // An empty expectedRevision means "no precondition"; the desktop CLI has no
    // revision to echo back, while the app protocol still gets compare-and-swap.
    if !snapshot.expected_revision.is_empty() && snapshot.expected_revision != current_revision {
        return Err(error::REVISION_CONFLICT);
    }
    if let Some(existing) = &current {
        if same_content(existing, &snapshot) {
            return Ok(());
        }
    }
    let revision = runtime.stage(&snapshot)?;
    // Reject a config the core cannot even construct before activating it, so a
    // running core is never stopped for something that cannot start.
    sb4a_core::supervisor::check_generation(runtime, &revision)?;
    if !current_revision.is_empty() {
        sb4a_core::atomic::write(
            &runtime.path(&["previous"]),
            current_revision.as_bytes(),
            0o600,
        )
        .map_err(|_| error::STATE_WRITE_FAILED)?;
    }
    let was_running = process::same_process(&runtime.status().supervisor);
    if was_running {
        Supervisor::stop(runtime)?;
    }
    sb4a_core::atomic::write(&runtime.path(&["current"]), revision.as_bytes(), 0o600)
        .map_err(|_| error::STATE_WRITE_FAILED)?;
    if was_running {
        return start_locked(runtime, guard);
    }
    runtime.record_event("config_applied");
    runtime.prune_generations();
    Ok(())
}

fn same_content(a: &Snapshot, b: &Snapshot) -> bool {
    a.schema_version == b.schema_version
        && a.performance_priority == b.performance_priority
        && a.auto_start == b.auto_start
        && a.profile_id == b.profile_id
        && a.profile_name == b.profile_name
        && a.config == b.config
        && a.files == b.files
        && a.plugins == b.plugins
}

fn start(runtime: &Runtime) -> Result<(), Code> {
    let guard = runtime.lock()?;
    start_locked(runtime, guard)
}

/// Caller already owns the control lock.
fn start_locked(runtime: &Runtime, guard: sb4a_core::atomic::Lock) -> Result<(), Code> {
    if !runtime.enabled() {
        return Err(error::MODULE_DISABLED);
    }
    let status = runtime.status();
    if process::same_process(&status.supervisor) {
        return if status.phase == "connected" {
            Ok(())
        } else {
            Err(error::MODULE_BUSY)
        };
    }
    if process::same_process(&status.core) {
        return Err(error::CLEANUP_REQUIRED);
    }
    let (revision, snapshot) = runtime.current()?;
    snapshot.ok_or(error::CONFIG_MISSING)?;
    let _ = std::fs::remove_file(runtime.path(&["runtime", "stop"]));
    let _ = std::fs::remove_file(runtime.path(&["runtime", "ready"]));

    let mut child = spawn_supervisor()?;
    drop(guard);

    let deadline = Instant::now() + Duration::from_secs(270);
    while Instant::now() < deadline {
        let state = runtime.status();
        if state.running_revision == revision && state.phase == "connected" {
            runtime.record_event("core_started");
            return Ok(());
        }
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            return Err(error::CORE_START_FAILED);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(error::STARTUP_TIMEOUT)
}

/// The supervisor must outlive the short CLI call that starts it.
fn spawn_supervisor() -> Result<std::process::Child, Code> {
    let exe = std::env::current_exe().map_err(|_| error::SUPERVISOR_START_FAILED)?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["__internal", "supervise"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    process::detach(&mut command);
    command.spawn().map_err(|_| error::SUPERVISOR_START_FAILED)
}

/// Boot runs from `service.sh`, so it returns as soon as the supervisor is
/// spawned instead of waiting for the core to connect.
fn boot(runtime: &Runtime) -> Result<(), Code> {
    if !runtime.enabled() {
        return Ok(());
    }
    let auto_start = std::fs::read(runtime.path(&["boot.json"]))
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("autoStart").and_then(|flag| flag.as_bool()))
        .unwrap_or(false);
    if !auto_start {
        return Ok(());
    }
    let guard = runtime.lock()?;
    if process::same_process(&runtime.status().supervisor) {
        return Ok(());
    }
    if runtime.current()?.1.is_none() {
        return Ok(());
    }
    let _ = std::fs::remove_file(runtime.path(&["runtime", "stop"]));
    let _ = std::fs::remove_file(runtime.path(&["runtime", "ready"]));
    spawn_supervisor()?;
    drop(guard);
    Ok(())
}

fn supervise(runtime: &Runtime) -> Result<(), Code> {
    let (revision, snapshot) = runtime.current()?;
    let snapshot = snapshot.ok_or(error::CONFIG_MISSING)?;
    Supervisor::new(Arc::new(runtime.clone())).run(&revision, &snapshot)
}

fn rollback(runtime: &Runtime) -> Result<(), Code> {
    let guard = runtime.lock()?;
    let raw = std::fs::read(runtime.path(&["previous"])).map_err(|_| error::ROLLBACK_MISSING)?;
    let previous = String::from_utf8_lossy(&raw).trim().to_string();
    if !snapshot::is_revision(&previous) {
        return Err(error::ROLLBACK_INVALID);
    }
    let path = runtime.path(&["generations", &previous, "snapshot.json"]);
    let snapshot: Snapshot = sb4a_core::atomic::read_json(&path, snapshot::MAX_SNAPSHOT_BYTES)
        .map_err(|_| error::ROLLBACK_INVALID)?;
    snapshot::check(&snapshot)?;
    let status = runtime.status();
    let was_running =
        process::same_process(&status.supervisor) || process::same_process(&status.core);
    if was_running {
        Supervisor::stop(runtime)?;
    }
    sb4a_core::atomic::write(&runtime.path(&["current"]), previous.as_bytes(), 0o600)
        .map_err(|_| error::STATE_WRITE_FAILED)?;
    if was_running {
        start_locked(runtime, guard)?;
    }
    runtime.record_event("config_rolled_back");
    Ok(())
}
