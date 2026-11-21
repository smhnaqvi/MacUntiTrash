use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use walkdir::WalkDir;

#[derive(Default)]
struct ScanState {
    active: Mutex<Option<Arc<AtomicBool>>>,
    findings: Mutex<HashMap<PathBuf, (u64, u64)>>,
}

fn emit_scan(app: &AppHandle, value: serde_json::Value) {
    let _ = app.emit("scan-line", value.to_string());
}

fn allocated_bytes(path: &Path, cancelled: &AtomicBool) -> (u64, usize) {
    let mut bytes = 0_u64;
    let mut skipped = 0;
    let mut seen = HashSet::new();
    for entry in WalkDir::new(path).follow_links(false) {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        match entry {
            Ok(entry) if entry.file_type().is_symlink() => continue,
            Ok(entry) => match entry.metadata() {
                Ok(meta) if meta.is_file() && seen.insert((meta.dev(), meta.ino())) => {
                    bytes = bytes.saturating_add(meta.blocks().saturating_mul(512));
                }
                Err(_) => skipped += 1,
                _ => {}
            },
            Err(_) => skipped += 1,
        }
    }
    (bytes, skipped)
}

/// The native open panel grants sandbox access to the selected folder for this session.
#[tauri::command]
async fn run_scan(
    app: AppHandle,
    state: tauri::State<'_, ScanState>,
) -> Result<Option<String>, String> {
    if state.active.lock().map_err(|e| e.to_string())?.is_some() {
        return Err("A scan is already running".into());
    }
    let Some(chosen) = app.dialog().file().blocking_pick_folder() else {
        return Ok(None);
    };
    let root = chosen
        .into_path()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("Select a folder to scan".into());
    }

    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut active = state.active.lock().map_err(|e| e.to_string())?;
        if active.is_some() {
            return Err("A scan is already running".into());
        }
        *active = Some(cancelled.clone());
    }
    state.findings.lock().map_err(|e| e.to_string())?.clear();
    let chosen_path = root.display().to_string();
    let app_handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        emit_scan(
            &app_handle,
            serde_json::json!({"type":"stage","name":"Scanning selected folder…"}),
        );
        let outcome = scan_selected_folder(&app_handle, &root, &cancelled);
        let scan_state = app_handle.state::<ScanState>();
        let mut active = scan_state.active.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
        {
            *active = None;
            if let Err(error) = outcome {
                let _ = app_handle.emit("scan-failed", error);
            }
        }
    });
    Ok(Some(chosen_path))
}

fn scan_selected_folder(
    app: &AppHandle,
    root: &Path,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let entries = fs::read_dir(root).map_err(|e| format!("Cannot read selected folder: {e}"))?;
    let mut skipped = 0;
    for entry in entries {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(());
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.file_type().is_symlink() => meta,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let (bytes, unreadable) = allocated_bytes(&path, cancelled);
        skipped += unreadable;
        if cancelled.load(Ordering::Relaxed) {
            return Ok(());
        }
        if bytes < 10 * 1024 * 1024 {
            continue;
        }
        let label = entry.file_name().to_string_lossy().into_owned();
        let path_string = path.display().to_string();
        app.state::<ScanState>()
            .findings
            .lock()
            .unwrap()
            .insert(path.clone(), (meta.dev(), meta.ino()));
        emit_scan(
            app,
            serde_json::json!({"type":"finding","risk":"review","label":label,"path":path_string,"bytes":bytes}),
        );
    }
    emit_scan(app, serde_json::json!({"type":"done","skipped":skipped}));
    Ok(())
}

#[tauri::command]
fn stop_scan(app: AppHandle, state: tauri::State<'_, ScanState>) -> Result<(), String> {
    if let Some(cancelled) = state.active.lock().map_err(|e| e.to_string())?.take() {
        cancelled.store(true, Ordering::Relaxed);
        state.findings.lock().map_err(|e| e.to_string())?.clear();
        let _ = app.emit("scan-stopped", ());
    }
    Ok(())
}

#[tauri::command]
fn reveal_in_finder(path: String) -> Result<(), String> {
    let status = Command::new("open")
        .arg("-R")
        .arg(&path)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Finder could not reveal this item".into())
    }
}

#[tauri::command]
fn trash_paths(
    paths: Vec<String>,
    state: tauri::State<'_, ScanState>,
) -> Result<Vec<String>, String> {
    if state.active.lock().map_err(|e| e.to_string())?.is_some() {
        return Err("Wait for the scan to finish before moving items".into());
    }
    let mut failures = Vec::new();
    let mut findings = state.findings.lock().map_err(|e| e.to_string())?;
    for path in paths {
        let item = PathBuf::from(&path);
        let valid = findings.get(&item).is_some_and(|&(dev, ino)| {
            fs::symlink_metadata(&item).is_ok_and(|meta| {
                !meta.file_type().is_symlink() && meta.dev() == dev && meta.ino() == ino
            })
        });
        if !valid || trash::delete(&item).is_err() {
            failures.push(path);
        } else {
            findings.remove(&item);
        }
    }
    Ok(failures)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(ScanState::default())
        .invoke_handler(tauri::generate_handler![
            run_scan,
            stop_scan,
            reveal_in_finder,
            trash_paths
        ])
        .run(tauri::generate_context!())
        .expect("error while running Disk Detective");
}
