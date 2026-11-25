import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";

// Scan events from the Rust backend
type Risk = "review";

interface Finding {
  key: string; // path, used as the row/selection key
  label: string;
  path: string;
  bytes: number;
  risk: Risk;
  isDir: boolean;
}

interface VolumeStats {
  total: number;
  used: number;
  available: number;
}

type ScanEvent =
  | { type: "stage"; name: string }
  | { type: "finding"; risk: Risk; label: string; path: string; bytes: number; isDir: boolean }
  | { type: "done"; skipped: number };

function formatBytes(n: number): string {
  if (!n || n <= 0) return "0 B";
  const gb = n / 1024 ** 3;
  if (gb >= 1) return `${gb.toFixed(1)} GiB`;
  const mb = n / 1024 ** 2;
  if (mb >= 1) return `${mb.toFixed(mb < 10 ? 1 : 0)} MiB`;
  if (n < 1024) return `${n} B`;
  return `${Math.round(n / 1024)} KiB`;
}

function ItemIcon({ directory }: { directory: boolean }) {
  return directory ? (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden="true">
      <path d="M3.5 7a2 2 0 0 1 2-2h4l2.1 2.3h6.9a2 2 0 0 1 2 2v8.2a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2V7Z" />
    </svg>
  ) : (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden="true">
      <path d="M6 3.5h7l5 5V19a1.5 1.5 0 0 1-1.5 1.5h-10A1.5 1.5 0 0 1 5 19V5a1.5 1.5 0 0 1 1-1.5Z" />
      <path d="M13 3.5V9h5" />
    </svg>
  );
}

export default function App() {
  const [version, setVersion] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [picking, setPicking] = useState(false);
  const [stage, setStage] = useState<string | null>(null);
  const [findings, setFindings] = useState<Finding[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [folder, setFolder] = useState<string | null>(null);
  const [currentFolder, setCurrentFolder] = useState<string | null>(null);
  const [volume, setVolume] = useState<VolumeStats | null>(null);
  const [volumeUnavailable, setVolumeUnavailable] = useState(false);
  const [done, setDone] = useState<{ skipped: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null); // "trash" | "reveal:<path>" | null
  const [confirmTrash, setConfirmTrash] = useState<{ paths: string[]; bytes: number } | null>(null);

  const unlistenRefs = useRef<Array<() => void>>([]);

  useEffect(() => {
    getVersion().then(setVersion).catch(() => setVersion(null));
  }, []);

  useEffect(() => {
    if (!folder) return;
    let cancelled = false;
    invoke<VolumeStats>("volume_stats")
      .then((stats) => { if (!cancelled) setVolume(stats); })
      .catch(() => { if (!cancelled) setVolumeUnavailable(true); });
    return () => { cancelled = true; };
  }, [folder]);

  useEffect(() => {
    listen<string>("scan-line", (e) => {
      let evt: ScanEvent;
      try {
        evt = JSON.parse(e.payload);
      } catch {
        return; // ignore any stray non-JSON stdout noise
      }
      switch (evt.type) {
        case "stage":
          setStage(evt.name);
          break;
        case "finding":
          setFindings((prev) => [
            ...prev,
            { key: evt.path, label: evt.label, path: evt.path, bytes: evt.bytes, risk: evt.risk, isDir: evt.isDir },
          ]);
          break;
        case "done":
          setDone({ skipped: evt.skipped });
          setStage(null);
          setScanning(false);
          break;
      }
    }).then((un) => {
      unlistenRefs.current.push(un);
    });
    listen("scan-stopped", () => {
      setScanning(false);
      setStage(null);
      setFindings([]);
      setSelected(new Set());
    }).then((un) => {
      unlistenRefs.current.push(un);
    });
    listen("scan-stopping", () => {
      setStage("Stopping scan…");
    }).then((un) => {
      unlistenRefs.current.push(un);
    });
    listen<string>("scan-failed", (e) => {
      setError(e.payload);
      setScanning(false);
      setStage(null);
      setFindings([]);
      setSelected(new Set());
    }).then((un) => {
      unlistenRefs.current.push(un);
    });
    return () => unlistenRefs.current.forEach((un) => un());
  }, []);

  const runScan = useCallback(async () => {
    setPicking(true);
    setScanning(true);
    setError(null);
    setStage("Starting…");
    setFindings([]);
    setSelected(new Set());
    setFolder(null);
    setCurrentFolder(null);
    setVolume(null);
    setVolumeUnavailable(false);
    setDone(null);
    try {
      const selectedFolder = await invoke<string | null>("run_scan");
      setPicking(false);
      if (selectedFolder) { setFolder(selectedFolder); setCurrentFolder(selectedFolder); }
      else { setScanning(false); setStage(null); }
    } catch (err) {
      setPicking(false);
      setError(String(err));
      setScanning(false);
      setStage(null);
    }
  }, []);

  const openFolder = useCallback(async (path: string) => {
    if (scanning || !folder || !currentFolder || path === currentFolder) return;
    const previous = currentFolder;
    setScanning(true);
    setStage("Opening folder…");
    setFindings([]);
    setSelected(new Set());
    setDone(null);
    setError(null);
    setCurrentFolder(path);
    try {
      await invoke("scan_folder", { path });
    } catch (err) {
      setCurrentFolder(previous);
      setError(String(err));
      setScanning(false);
      setStage(null);
    }
  }, [scanning, folder, currentFolder]);

  const stopScan = useCallback(async () => {
    try {
      await invoke("stop_scan");
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const sorted = useMemo(() => [...findings].sort((a, b) => b.bytes - a.bytes), [findings]);
  const unavailableBytes = volume ? Math.max(0, volume.total - volume.used - volume.available) : 0;
  const isRootView = currentFolder === folder;
  const breadcrumbs = useMemo(() => {
    if (!folder || !currentFolder) return [];
    const rootName = folder.split("/").filter(Boolean).pop() ?? folder;
    const relative = currentFolder.slice(folder.length).split("/").filter(Boolean);
    return [{ label: rootName, path: folder }, ...relative.map((label, index) => ({
      label, path: `${folder}/${relative.slice(0, index + 1).join("/")}`,
    }))];
  }, [folder, currentFolder]);
  const selectedBytes = useMemo(
    () => sorted.filter((f) => selected.has(f.key)).reduce((sum, f) => sum + f.bytes, 0),
    [sorted, selected]
  );

  function toggle(key: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      next.has(key) ? next.delete(key) : next.add(key);
      return next;
    });
  }

  function clearSelection() {
    setSelected(new Set());
  }

  function moveSelectedToTrash() {
    const paths = sorted.filter((f) => selected.has(f.key)).map((f) => f.path);
    if (paths.length === 0) return;
    // A custom modal, not window.confirm() — Tauri's macOS webview (WKWebView)
    // doesn't implement native JS confirm dialogs, so confirm() returns
    // false immediately and silently no-ops the whole action.
    setConfirmTrash({ paths, bytes: selectedBytes });
  }

  async function performTrash() {
    if (!confirmTrash) return;
    const { paths } = confirmTrash;
    setConfirmTrash(null);
    setBusyAction("trash");
    setError(null);
    try {
      const failures = await invoke<string[]>("trash_paths", { paths });
      if (failures.length > 0) {
        setError(`Couldn't trash ${failures.length} item(s): ${failures.join(", ")}`);
      }
      setFindings((prev) => prev.filter((f) => !paths.includes(f.path) || failures.includes(f.path)));
      setSelected(new Set());
    } catch (err) {
      setError(String(err));
    } finally {
      setBusyAction(null);
    }
  }

  async function revealInFinder(path: string) {
    setBusyAction(`reveal:${path}`);
    try {
      await invoke("reveal_in_finder", { path });
    } catch (err) {
      setError(String(err));
    } finally {
      setBusyAction(null);
    }
  }

  return (
    <div className="app-shell">
      <header className="app-header">
        <div className="brand">
          <div>
            <h1>SpaceGlance <span className="version">{version && "v" + version}</span></h1>
            <p>Scan a folder. Review items before moving them to Trash.</p>
          </div>
        </div>
        <div className="header-actions">
          {scanning && !picking ? (
            <button className="button button-secondary" onClick={stopScan}>Stop scan</button>
          ) : (
            <button className="button button-primary" onClick={runScan} disabled={picking}>
              {picking ? "Choosing folder…" : folder ? "Scan another folder" : "Choose a folder"}
            </button>
          )}
        </div>
      </header>

      <div className="workspace">
        {currentFolder && (
          <nav className="location-strip" aria-label="Current folder">
              <span className="location-label">Selected folder:</span>
              <div className="breadcrumbs" aria-label="Folder path">
                {breadcrumbs.map((crumb, index) => (
                  <span className="crumb" key={crumb.path}>
                    {index > 0 && <span className="crumb-divider" aria-hidden="true">/</span>}
                    {index === breadcrumbs.length - 1 ? (
                      <span aria-current="page">{crumb.label}</span>
                    ) : (
                      <button onClick={() => openFolder(crumb.path)} disabled={scanning}>{crumb.label}</button>
                    )}
                  </span>
                ))}
              </div>
              <span className="full-path" title={currentFolder}>{currentFolder}</span>
              {done && <span className="status-pill"><span className="status-dot" /> Complete</span>}
              {scanning && <span className="status-pill status-active"><span className="status-dot" /> Scanning</span>}
          </nav>
        )}

        {folder && (
          <section className="volume-panel" aria-label="Volume storage summary">
            <div className="volume-heading">
              <div>
                <h2>System storage</h2>
                <p>Volume containing the selected folder</p>
              </div>
              {volume && <div className="volume-total"><strong>{formatBytes(volume.total)}</strong><span>capacity</span></div>}
            </div>
            {volume ? (
              <>
                <div className="volume-track" role="img" aria-label={
                  "Volume capacity " + formatBytes(volume.total) + ", used " + formatBytes(volume.used) +
                  ", available " + formatBytes(volume.available) + ", other or unavailable " + formatBytes(unavailableBytes)
                }>
                  <span className="volume-used" style={{ width: (volume.used / volume.total * 100) + "%" }} />
                  <span className="volume-available" style={{ width: (volume.available / volume.total * 100) + "%" }} />
                  <span className="volume-other" style={{ width: (unavailableBytes / volume.total * 100) + "%" }} />
                </div>
                <div className="volume-legend">
                  <span><i className="legend-used" />Used <strong>{formatBytes(volume.used)}</strong></span>
                  <span><i className="legend-available" />Available <strong>{formatBytes(volume.available)}</strong></span>
                  <span><i className="legend-other" />Other or unavailable <strong>{formatBytes(unavailableBytes)}</strong></span>
                </div>
                <p className="volume-note">“Other” is the difference between capacity, this volume’s used space, and available space. Shared APFS storage and system reserves can affect it. Values are rounded for display.</p>
              </>
            ) : <p className="volume-loading">{volumeUnavailable ? "Volume information is unavailable on this Mac." : "Reading volume information…"}</p>}
          </section>
        )}

        {scanning && stage && <div className="scan-notice" role="status"><span className="scan-spinner" />{stage}</div>}
        {error && <div className="error-notice" role="alert">{error}</div>}

        <main className="results-panel">
          {currentFolder && <div className="results-heading">
            <div>
              <h2>{isRootView ? "Biggest items" : "Folder contents"}</h2>
              <p>{isRootView
                  ? "Showing direct items of 10 MiB or more. Open a folder to see every item inside."
                  : "Showing all direct items. Folder sizes include their contents."}</p>
            </div>
            <span className="item-count">{sorted.length} {sorted.length === 1 ? "item" : "items"}</span>
          </div>}

          {sorted.length === 0 && !scanning && (
            <div className="empty-state">
              <h3>{currentFolder ? "Nothing to show here" : "Choose a folder to scan"}</h3>
              <p>{currentFolder
                ? isRootView ? "No direct items over 10 MiB were found." : "This folder has no readable direct items."
                : "Find large files and folders, then open folders to inspect their contents."}</p>
            </div>
          )}

          {sorted.length > 0 && (
            <div className="results-list" role="list" aria-label="Folder contents">
              <div className="list-labels" aria-hidden="true"><span>What</span><span>Path</span><span>Size</span><span></span></div>
              {sorted.map((f) => (
                <div className="result-row" role="listitem" key={f.key}>
                  <label className="row-check">
                    <input type="checkbox" checked={selected.has(f.key)} onChange={() => toggle(f.key)}
                      disabled={scanning} aria-label={"Select " + f.label + " for Trash"} />
                  </label>
                  <span className={"item-icon" + (f.isDir ? " directory-icon" : "")}><ItemIcon directory={f.isDir} /></span>
                  <div className="item-info">
                    {f.isDir ? (
                      <button className="item-name item-open" onClick={() => openFolder(f.path)} disabled={scanning}
                        title={"Open " + f.path}>{f.label}<span aria-hidden="true">›</span></button>
                    ) : <span className="item-name">{f.label}</span>}
                    <span className="item-path" title={f.path}>{f.path}</span>
                  </div>
                  <span className="item-size">{formatBytes(f.bytes)}</span>
                  <button className="text-action" onClick={() => revealInFinder(f.path)}
                    disabled={busyAction === "reveal:" + f.path} title={"Reveal " + f.path + " in Finder"}>Reveal</button>
                </div>
              ))}
            </div>
          )}
        </main>
      </div>

      {done && sorted.length > 0 && !scanning && (
        <footer className="action-bar">
          <div className="selection-summary">
            <span className="selection-count">{selected.size} selected</span>
            <span className="selection-bytes">{formatBytes(selectedBytes)} listed size</span>
            {selected.size > 0 && <button className="text-action" onClick={clearSelection}>Clear</button>}
          </div>
          <button className="button button-danger" onClick={moveSelectedToTrash}
            disabled={selected.size === 0 || busyAction === "trash"}>
            {busyAction === "trash" ? "Moving to Trash…" : "Move to Trash"}
          </button>
        </footer>
      )}
      {done && (
        <div className="scan-footnote" role="status">
          {done.skipped > 0
            ? done.skipped + " entries were skipped or unavailable. Shown folder sizes may be incomplete."
            : "Folder sizes are allocated disk space. Nothing moves to Trash without your confirmation."}
        </div>
      )}

      {confirmTrash && (
        <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
          if (event.target === event.currentTarget) setConfirmTrash(null);
        }}>
          <div className="confirm-dialog" role="dialog" aria-modal="true" aria-labelledby="trash-title">
            <div className="eyebrow">REVIEW YOUR SELECTION</div>
            <h2 id="trash-title">Move to Trash?</h2>
            <p>{confirmTrash.paths.length} {confirmTrash.paths.length === 1 ? "item" : "items"} · {formatBytes(confirmTrash.bytes)} listed size. Space is usually freed after you empty Trash.</p>
            <ul className="confirm-list">{confirmTrash.paths.map((path) => <li key={path} title={path}>{path}</li>)}</ul>
            <div className="dialog-actions">
              <button className="button button-secondary" onClick={() => setConfirmTrash(null)}>Cancel</button>
              <button className="button button-danger" onClick={performTrash}>Move to Trash</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
