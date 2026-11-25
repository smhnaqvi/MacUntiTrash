use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, OsStr};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use objc2_foundation::{
    NSData, NSString, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions, NSURL,
};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_dialog::DialogExt;
#[cfg(target_os = "macos")]
use trash::macos::{DeleteMethod, TrashContextExtMacos};
use walkdir::WalkDir;

#[derive(Default)]
struct ScanState {
    active: Mutex<Option<Arc<AtomicBool>>>,
    choosing: AtomicBool,
    root: Mutex<Option<PathBuf>>,
    findings: Mutex<HashMap<PathBuf, (u64, u64)>>,
    home_scope: Mutex<Option<SecurityScope>>,
    progress_path: Mutex<Option<String>>,
}

#[tauri::command]
fn scan_status(state: tauri::State<'_, ScanState>) -> Result<serde_json::Value, String> {
    let active = scan_is_busy(&state);
    let path = state
        .progress_path
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    Ok(serde_json::json!({"active": active, "path": path}))
}

fn scan_is_busy(state: &ScanState) -> bool {
    state.choosing.load(Ordering::Relaxed) || state.active.lock().unwrap().is_some()
}

struct ScanRequest<'a> {
    app: &'a AppHandle,
    state: &'a ScanState,
}

impl<'a> ScanRequest<'a> {
    fn begin(app: &'a AppHandle, state: &'a ScanState) -> Result<Self, String> {
        if state
            .choosing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            return Err("A scan is already running".into());
        }
        let request = Self { app, state };
        if state.active.lock().map_err(|e| e.to_string())?.is_some() {
            return Err("A scan is already running".into());
        }
        emit_scan_status(app, true, None);
        Ok(request)
    }
}

impl Drop for ScanRequest<'_> {
    fn drop(&mut self) {
        self.state.choosing.store(false, Ordering::SeqCst);
        emit_scan_status(self.app, scan_is_busy(self.state), None);
    }
}

fn emit_scan_status(app: &AppHandle, active: bool, path: Option<&str>) {
    let _ = app.emit(
        "scan-status",
        serde_json::json!({"active": active, "path": path}),
    );
}

struct SecurityScope {
    url: objc2::rc::Retained<NSURL>,
    active: bool,
}

impl Drop for SecurityScope {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: This balances the successful access call for this URL.
            unsafe { self.url.stopAccessingSecurityScopedResource() };
        }
    }
}

fn bookmark_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("home.bookmark"))
}

fn user_home() -> Result<PathBuf, String> {
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0_u8; 16 * 1024];
    let mut result = std::ptr::null_mut();
    // SAFETY: all output pointers refer to live writable storage for the call.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return Err("Could not locate your Home folder".into());
    }
    // SAFETY: getpwuid_r succeeded and pw_dir points into the live buffer.
    let path = unsafe { CStr::from_ptr((*result).pw_dir) };
    Ok(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

fn resolve_home_bookmark(bytes: &[u8], home: &Path) -> Option<SecurityScope> {
    let data = NSData::with_bytes(bytes);
    let mut stale = objc2::runtime::Bool::NO;
    // SAFETY: `stale` is a valid mutable pointer for the duration of this call.
    let url = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &data,
            NSURLBookmarkResolutionOptions::WithSecurityScope,
            None,
            &mut stale,
        )
    }
    .ok()?;
    if stale.as_bool() {
        return None;
    }
    // SAFETY: The URL came from a stored app-scoped security bookmark.
    let active = unsafe { url.startAccessingSecurityScopedResource() };
    let resolved_home = url
        .path()
        .and_then(|path| PathBuf::from(path.to_string()).canonicalize().ok());
    if !active || resolved_home.as_deref() != Some(home) {
        if active {
            unsafe { url.stopAccessingSecurityScopedResource() };
        }
        return None;
    }
    Some(SecurityScope { url, active })
}

#[tauri::command]
async fn scan_my_files(
    app: AppHandle,
    state: tauri::State<'_, ScanState>,
) -> Result<Option<String>, String> {
    let _request = ScanRequest::begin(&app, &state)?;
    let home = user_home()?.canonicalize().map_err(|e| e.to_string())?;
    let saved = bookmark_file(&app)
        .ok()
        .and_then(|path| fs::read(path).ok());
    let scope = saved
        .as_deref()
        .and_then(|bytes| resolve_home_bookmark(bytes, &home));
    let scope = if let Some(scope) = scope {
        scope
    } else {
        let Some(chosen) = app
            .dialog()
            .file()
            .set_directory(&home)
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let chosen = chosen
            .into_path()
            .map_err(|e| e.to_string())?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if chosen != home {
            return Err("Select your Home folder to enable Scan My Files".into());
        }
        let url =
            NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&home.to_string_lossy()), true);
        let data = url
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::WithSecurityScope,
                None,
                None,
            )
            .map_err(|e| format!("Could not save Home folder access: {e}"))?;
        let path = bookmark_file(&app)?;
        fs::create_dir_all(path.parent().ok_or("Invalid bookmark location")?)
            .map_err(|e| e.to_string())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(&data.to_vec()).map_err(|e| e.to_string())?;
        // The open panel grants access for this session; a bookmark preserves it for future launches.
        let active = unsafe { url.startAccessingSecurityScopedResource() };
        SecurityScope { url, active }
    };
    start_scan(app.clone(), &state, home.clone(), Some(home), Some(scope)).map(Some)
}

fn emit_scan(app: &AppHandle, value: serde_json::Value) {
    let _ = app.emit("scan-line", value.to_string());
}

fn allocated_bytes(
    path: &Path,
    cancelled: &AtomicBool,
    mut on_visit: impl FnMut(&Path),
) -> (u64, usize) {
    let mut bytes = 0_u64;
    let mut skipped = 0;
    let mut seen = HashSet::new();
    for entry in WalkDir::new(path).follow_links(false) {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        match entry {
            Ok(entry) if entry.file_type().is_symlink() => continue,
            Ok(entry) => {
                on_visit(entry.path());
                match entry.metadata() {
                    Ok(meta) if meta.is_file() && seen.insert((meta.dev(), meta.ino())) => {
                        bytes = bytes.saturating_add(meta.blocks().saturating_mul(512));
                    }
                    Err(_) => skipped += 1,
                    _ => {}
                }
            }
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
    let _request = ScanRequest::begin(&app, &state)?;
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

    start_scan(app.clone(), &state, root.clone(), Some(root), None).map(Some)
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
    start_scan(app, &state, folder, None, None)
}

fn start_scan(
    app: AppHandle,
    state: &ScanState,
    folder: PathBuf,
    selected_root: Option<PathBuf>,
    scope: Option<SecurityScope>,
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
        *state.home_scope.lock().map_err(|e| e.to_string())? = scope;
    }
    *state.progress_path.lock().map_err(|e| e.to_string())? = None;
    emit_scan_status(&app, true, None);
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
            *scan_state.progress_path.lock().unwrap() = None;
            emit_scan_status(&app_handle, false, None);
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
    let mut last_progress = Instant::now() - Duration::from_millis(150);
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
        let (bytes, unreadable) = allocated_bytes(&path, cancelled, |visited| {
            if last_progress.elapsed() >= Duration::from_millis(150) {
                let path = visited.display().to_string();
                *app.state::<ScanState>().progress_path.lock().unwrap() = Some(path.clone());
                emit_scan(app, serde_json::json!({"type":"progress","path":path}));
                emit_scan_status(app, true, Some(&path));
                last_progress = Instant::now();
            }
        });
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

fn volume_breakdown(
    blocks: u64,
    free: u64,
    available: u64,
    block_size: u64,
) -> Result<(u64, u64, u64), String> {
    if blocks == 0 || block_size == 0 || free > blocks || available > free {
        return Err("Inconsistent volume information".into());
    }
    let bytes = |count: u64| {
        count
            .checked_mul(block_size)
            .ok_or_else(|| "Volume size exceeds supported range".to_string())
    };
    Ok((bytes(blocks)?, bytes(blocks - free)?, bytes(available)?))
}

#[tauri::command]
fn volume_stats(state: tauri::State<'_, ScanState>) -> Result<serde_json::Value, String> {
    let root = state
        .root
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("Choose a folder to scan first")?;
    let (total, used, available) = read_volume_stats(&root)?;
    Ok(serde_json::json!({"total": total, "used": used, "available": available}))
}

fn read_volume_stats(root: &Path) -> Result<(u64, u64, u64), String> {
    let path = CString::new(root.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL-terminated and stats points to writable memory.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: statvfs returned success and initialized stats.
    let stats = unsafe { stats.assume_init() };
    volume_breakdown(
        stats.f_blocks as u64,
        stats.f_bfree as u64,
        stats.f_bavail as u64,
        stats.f_frsize as u64,
    )
}

#[tauri::command]
fn system_volume_stats() -> Result<serde_json::Value, String> {
    let (total, used, available) = read_volume_stats(Path::new("/"))?;
    Ok(serde_json::json!({"total": total, "used": used, "available": available}))
}

#[tauri::command]
fn open_main_window(app: AppHandle, scan_my_files: bool) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or("Main window is unavailable")?;
    main.show().map_err(|e| e.to_string())?;
    main.set_focus().map_err(|e| e.to_string())?;
    if let Some(storage) = app.get_webview_window("storage") {
        let _ = storage.hide();
    }
    if scan_my_files && !scan_is_busy(&app.state::<ScanState>()) {
        app.emit_to("main", "scan-my-files-requested", ())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn setup_storage_tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    WebviewWindowBuilder::new(
        app,
        "storage",
        WebviewUrl::App("index.html?view=storage".into()),
    )
    .title("SpaceGlance storage")
    .inner_size(336.0, 328.0)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(false)
    .build()?;

    let open = MenuItem::with_id(app, "open", "Open SpaceGlance", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit SpaceGlance", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let icon = app
        .default_window_icon()
        .ok_or(tauri::Error::WindowNotFound)?
        .clone();
    TrayIconBuilder::with_id("storage")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("SpaceGlance · checking disk space")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                let _ = open_main_window(app.clone(), false);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                position,
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("storage") {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let size = window.outer_size().ok();
                        let monitor = window.current_monitor().ok().flatten();
                        let width = size.map(|s| s.width as f64).unwrap_or(336.0);
                        let height = size.map(|s| s.height as f64).unwrap_or(328.0);
                        let (x, y) = if let Some(monitor) = monitor {
                            let left = monitor.position().x as f64;
                            let top = monitor.position().y as f64;
                            let right = left + monitor.size().width as f64;
                            let bottom = top + monitor.size().height as f64;
                            (
                                (position.x - width / 2.0).clamp(left, right - width),
                                (position.y + 16.0).clamp(top, bottom - height),
                            )
                        } else {
                            (position.x - width / 2.0, position.y + 16.0)
                        };
                        let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    let handle = app.handle().clone();
    std::thread::spawn(move || loop {
        if let Ok(stats) = system_volume_stats() {
            if let Some(available) = stats["available"].as_u64() {
                if let Some(tray) = handle.tray_by_id("storage") {
                    let gib = available as f64 / 1024_f64.powi(3);
                    let _ = tray.set_tooltip(Some(format!("SpaceGlance · {gib:.1} GiB available")));
                }
            }
            let _ = handle.emit("storage-updated", stats);
        }
        std::thread::sleep(Duration::from_secs(60));
    });
    Ok(())
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
            scan_my_files,
            scan_folder,
            stop_scan,
            scan_status,
            reveal_in_finder,
            volume_stats,
            system_volume_stats,
            open_main_window,
            trash_paths
        ])
        .setup(setup_storage_tray)
        .on_window_event(|window, event| match (window.label(), event) {
            ("main", WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
            }
            ("storage", WindowEvent::Focused(false)) => {
                let _ = window.hide();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running SpaceGlance");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn permission_picker_counts_as_busy_for_menu_bar() {
        let state = ScanState::default();
        assert!(!scan_is_busy(&state));
        state.choosing.store(true, Ordering::SeqCst);
        assert!(scan_is_busy(&state));
    }

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
        let (bytes, _) = allocated_bytes(&selected, &AtomicBool::new(false), |_| {});
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

    #[test]
    fn volume_stats_keep_unavailable_space_separate() {
        let (total, used, available) =
            volume_breakdown(239_362_496, 52_562_496, 11_360_000, 1024).unwrap();
        assert!(total > used + available);
        assert_eq!(total - used - available, 41_202_496 * 1024);
    }

    #[test]
    fn reads_volume_stats_for_a_local_folder() {
        let (total, used, available) = read_volume_stats(&std::env::temp_dir()).unwrap();
        assert!(total > 0 && used <= total && available <= total - used);
    }
}
