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
#[cfg(target_os = "macos")]
use trash::macos::{DeleteMethod, TrashContextExtMacos};
use walkdir::WalkDir;

#[derive(Default)]
struct ScanState {
    active: Mutex<Option<Arc<AtomicBool>>>,
    root: Mutex<Option<PathBuf>>,
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

    start_scan(app, &state, root.clone(), Some(root)).map(Some)
}

fn validate_scan_folder(root: &Path, requested: &Path) -> Result<PathBuf, String> {
    let relative = requested
        .strip_prefix(root)
        .map_err(|_| "Folder is outside the selected location".to_string())?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("Invalid folder path".into());
        };
        current.push(name);
        let metadata = fs::symlink_metadata(&current).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("Choose a folder inside the selected location".into());
        }
    }
    Ok(current)
}

#[tauri::command]
fn scan_folder(
    app: AppHandle,
    state: tauri::State<'_, ScanState>,
    path: String,
) -> Result<String, String> {
    let root = state
        .root
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("Choose a folder to scan first")?;
    let folder = validate_scan_folder(&root, Path::new(&path))?;
    start_scan(app, &state, folder, None)
}

fn start_scan(
    app: AppHandle,
    state: &ScanState,
    folder: PathBuf,
    selected_root: Option<PathBuf>,
) -> Result<String, String> {
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut active = state.active.lock().map_err(|e| e.to_string())?;
        if active.is_some() {
            return Err("A scan is already running".into());
        }
        *active = Some(cancelled.clone());
    }
    if let Some(root) = selected_root {
        *state.root.lock().map_err(|e| e.to_string())? = Some(root);
    }
    state.findings.lock().map_err(|e| e.to_string())?.clear();
    let chosen_path = folder.display().to_string();
    let threshold =
        if state.root.lock().map_err(|e| e.to_string())?.as_deref() == Some(folder.as_path()) {
            10 * 1024 * 1024
        } else {
            0
        };
    let app_handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        emit_scan(
            &app_handle,
            serde_json::json!({"type":"stage","name":"Scanning selected folder…"}),
        );
        let outcome = scan_selected_folder(&app_handle, &folder, threshold, &cancelled);
        let scan_state = app_handle.state::<ScanState>();
        let mut active = scan_state.active.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
        {
            *active = None;
            if cancelled.load(Ordering::Relaxed) {
                scan_state.findings.lock().unwrap().clear();
                let _ = app_handle.emit("scan-stopped", ());
            } else {
                match outcome {
                    Ok(skipped) => emit_scan(
                        &app_handle,
                        serde_json::json!({"type":"done","skipped":skipped}),
                    ),
                    Err(error) => {
                        scan_state.findings.lock().unwrap().clear();
                        let _ = app_handle.emit("scan-failed", error);
                    }
                }
            }
        }
    });
    Ok(chosen_path)
}

fn scan_selected_folder(
    app: &AppHandle,
    root: &Path,
    threshold: u64,
    cancelled: &AtomicBool,
) -> Result<usize, String> {
    let entries = fs::read_dir(root).map_err(|e| format!("Cannot read selected folder: {e}"))?;
    let mut skipped = 0;
    for entry in entries {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(skipped);
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
            return Ok(skipped);
        }
        if bytes < threshold {
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
            serde_json::json!({"type":"finding","risk":"review","label":label,"path":path_string,"bytes":bytes,"isDir":meta.is_dir()}),
        );
    }
    Ok(skipped)
}

#[tauri::command]
fn stop_scan(app: AppHandle, state: tauri::State<'_, ScanState>) -> Result<(), String> {
    if let Some(cancelled) = state.active.lock().map_err(|e| e.to_string())?.as_ref() {
        cancelled.store(true, Ordering::Relaxed);
        let _ = app.emit("scan-stopping", ());
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
    let mut trash_context = trash::TrashContext::new();
    #[cfg(target_os = "macos")]
    trash_context.set_delete_method(DeleteMethod::NsFileManager);
    for path in paths {
        let item = PathBuf::from(&path);
        let valid = findings.get(&item).is_some_and(|&(dev, ino)| {
            fs::symlink_metadata(&item).is_ok_and(|meta| {
                !meta.file_type().is_symlink() && meta.dev() == dev && meta.ino() == ino
            })
        });
        if !valid || trash_context.delete(&item).is_err() {
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
            scan_folder,
            stop_scan,
            reveal_in_finder,
            trash_paths
        ])
        .run(tauri::generate_context!())
        .expect("error while running SpaceGlance");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn scanner_does_not_count_symlink_targets() {
        let base = std::env::temp_dir().join(format!(
            "spaceglance-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let selected = base.join("selected");
        fs::create_dir_all(&selected).unwrap();
        let outside = base.join("outside");
        fs::write(&outside, vec![1_u8; 1024 * 1024]).unwrap();
        symlink(&outside, selected.join("link")).unwrap();
        let (bytes, _) = allocated_bytes(&selected, &AtomicBool::new(false));
        fs::remove_dir_all(&base).unwrap();
        assert_eq!(bytes, 0);
    }

    #[test]
    fn navigation_stays_inside_selected_folder_and_rejects_symlinks() {
        let base = std::env::temp_dir().join(format!(
            "spaceglance-navigation-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("selected");
        let child = root.join("child");
        fs::create_dir_all(&child).unwrap();
        let outside = base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("link")).unwrap();

        assert_eq!(validate_scan_folder(&root, &child).unwrap(), child);
        assert!(validate_scan_folder(&root, &outside).is_err());
        assert!(validate_scan_folder(&root, &root.join("../outside")).is_err());
        assert!(validate_scan_folder(&root, &root.join("link")).is_err());

        fs::remove_dir_all(&base).unwrap();
    }
}
