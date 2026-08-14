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

use std::path::PathBuf;
use std::process::Stdio;

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

/// Run the scanner and forward each stdout line to the frontend as a
/// `scan-line` event, unparsed — the frontend owns the NDJSON schema.
#[tauri::command]
async fn run_scan(app: AppHandle, deep: bool) -> Result<(), String> {
    let script = resolve_script(&app)?;

    let mut cmd = Command::new("bash");
    cmd.arg(&script).arg("--json");
    if deep {
        cmd.arg("--deep");
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start scan: {e}"))?;

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
            let _ = app_handle.emit("scan-line", line);
        }
        let _ = child.wait().await;
    });

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
fn trash_paths(paths: Vec<String>) -> Result<Vec<String>, String> {
    let mut failures = Vec::new();

    for path in paths {
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
            Ok(s) if s.success() => {}
            _ => failures.push(path),
        }
    }

    Ok(failures)
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            run_scan,
            reveal_in_finder,
            open_fda_settings,
            trash_paths
        ])
        .run(tauri::generate_context!())
        .expect("error while running Disk Detective");
}
