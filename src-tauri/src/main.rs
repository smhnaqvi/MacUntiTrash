// Disk Detective — Tauri backend.
//
// This process is deliberately thin: all detection logic lives in
// mac-disk-scan.sh, bundled as a resource and run with `--json`. Rust's job
// is to spawn it, stream its NDJSON stdout to the frontend as events, and
// handle the few actions the frontend can't do itself (move to Trash,
// reveal in Finder, open System Settings).
//
// NOTE: written against the Tauri v2 API from documentation, but not
// compiled here (no macOS/Rust toolchain in the environment that produced
// it). Run `npm install && npm run tauri dev` and fix whatever the compiler
// flags — the most likely friction points are noted inline.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;

use tauri::path::BaseDirectory;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Locate mac-disk-scan.sh: inside the app bundle's Resources when packaged,
/// or straight from src-tauri/bin during `tauri dev` (resources aren't
/// copied out in dev mode).
fn resolve_script(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(p) = app
        .path()
        .resolve("bin/mac-disk-scan.sh", BaseDirectory::Resource)
    {
        if p.exists() {
            return Ok(p);
        }
    }

    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin/mac-disk-scan.sh");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    Err("mac-disk-scan.sh not found (checked bundled resources and src-tauri/bin/)".into())
}

/// Tracks the running scan's process group so `stop_scan` can kill it (and
/// the `find`/`du` children mac-disk-scan.sh spawns) rather than just the
/// top-level bash process.
#[derive(Default)]
struct ScanState {
    pgid: Mutex<Option<i32>>,
    findings: Mutex<HashMap<String, (u64, u64)>>,
}

/// Run the scanner and forward each stdout line to the frontend as a
/// `scan-line` event, unparsed — the frontend owns the NDJSON schema.
#[tauri::command]
async fn run_scan(
    app: AppHandle,
    deep: bool,
    state: tauri::State<'_, ScanState>,
) -> Result<(), String> {
    let mut running = state.pgid.lock().map_err(|e| e.to_string())?;
    if running.is_some() {
        return Err("A scan is already running".into());
    }
    state.findings.lock().map_err(|e| e.to_string())?.clear();
    let script = resolve_script(&app)?;

    let mut cmd = Command::new("bash");
    cmd.arg(&script).arg("--json");
    if deep {
        cmd.arg("--deep");
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Make bash the leader of a new process group so stop_scan can signal
    // the whole tree (bash + the find/du/etc. it spawns) at once instead of
    // just the immediate child, which would otherwise be left running.
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start scan: {e}"))?;

    let pid = child.id().map(|id| id as i32);
    *running = pid;
    drop(running);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "scan process had no stdout".to_string())?;
    let mut lines = BufReader::new(stdout).lines();

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            // Best-effort: a dropped event just means one row is missing,
            // not worth failing the whole scan over.
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                if value["type"] == "finding" {
                    if let Some(path) = value["path"].as_str() {
                        if let Ok(meta) = std::fs::symlink_metadata(path) {
                            if !meta.file_type().is_symlink() {
                                app_handle
                                    .state::<ScanState>()
                                    .findings
                                    .lock()
                                    .unwrap()
                                    .insert(path.to_owned(), (meta.dev(), meta.ino()));
                            }
                        }
                    }
                }
            }
            let _ = app_handle.emit("scan-line", line);
        }
        let result = child.wait().await;
        let mut running = app_handle.state::<ScanState>().pgid.lock().unwrap();
        // Stop may have cleared this scan while another was started.
        if *running == pid {
            *running = None;
            if !matches!(result, Ok(status) if status.success()) {
                let _ = app_handle.emit("scan-failed", "Scan process failed");
            }
        }
    });

    Ok(())
}

/// Kill the in-flight scan's whole process group. A no-op if nothing is
/// running (e.g. the user double-clicks Stop, or it finished just before).
#[tauri::command]
fn stop_scan(app: AppHandle, state: tauri::State<'_, ScanState>) -> Result<(), String> {
    let pgid = state.pgid.lock().unwrap().take();
    let Some(pgid) = pgid else {
        return Ok(());
    };

    // Negative pid = signal the whole process group, not just the leader.
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pgid}"))
        .status();

    let _ = app.emit("scan-stopped", ());
    Ok(())
}

/// Reveal a path in Finder (works for files, folders, and the report/
/// cleanup.sh paths emitted in the `done` event).
#[tauri::command]
fn reveal_in_finder(path: String) -> Result<(), String> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(&path)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Deep-link straight to the Full Disk Access pane.
#[tauri::command]
fn open_fda_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Move each path to Trash via Finder/AppleScript rather than `rm -rf`.
/// This is the whole safety model for the delete action: it's the same
/// operation as a normal Finder delete, so it's reversible until Trash is
/// emptied, and Finder — not us — handles name collisions and permissions.
/// Returns the subset of paths that failed, so the UI can report them.
#[tauri::command]
fn trash_paths(
    paths: Vec<String>,
    state: tauri::State<'_, ScanState>,
) -> Result<Vec<String>, String> {
    let mut failures = Vec::new();
    let mut findings = state.findings.lock().map_err(|e| e.to_string())?;

    for path in paths {
        let valid = findings.get(&path).is_some_and(|&(dev, ino)| {
            !path.chars().any(char::is_control)
                && std::fs::symlink_metadata(&path).is_ok_and(|meta| {
                    !meta.file_type().is_symlink() && meta.dev() == dev && meta.ino() == ino
                })
        });
        if !valid {
            failures.push(path);
            continue;
        }
        let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "tell application \"Finder\" to delete POSIX file \"{}\"",
            escaped
        );
        let status = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .status();

        match status {
            Ok(s) if s.success() => {
                findings.remove(&path);
            }
            _ => failures.push(path),
        }
    }

    Ok(failures)
}

fn main() {
    tauri::Builder::default()
        .manage(ScanState::default())
        .invoke_handler(tauri::generate_handler![
            run_scan,
            stop_scan,
            reveal_in_finder,
            open_fda_settings,
            trash_paths
        ])
        .run(tauri::generate_context!())
        .expect("error while running Disk Detective");
}
