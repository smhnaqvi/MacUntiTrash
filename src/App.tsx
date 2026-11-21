import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Scan events from the Rust backend
type Risk = "review";

interface Finding {
  key: string; // path, used as the row/selection key
  label: string;
  path: string;
  bytes: number;
  risk: Risk;
}

type ScanEvent =
  | { type: "stage"; name: string }
  | { type: "finding"; risk: Risk; label: string; path: string; bytes: number }
  | { type: "done"; skipped: number };

function formatBytes(n: number): string {
  if (!n || n <= 0) return "0 MB";
  const gb = n / 1024 ** 3;
  if (gb >= 1) return `${gb.toFixed(1)} GB`;
  const mb = n / 1024 ** 2;
  return `${mb.toFixed(0)} MB`;
}

export default function App() {
  const [scanning, setScanning] = useState(false);
  const [picking, setPicking] = useState(false);
  const [stage, setStage] = useState<string | null>(null);
  const [findings, setFindings] = useState<Finding[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [folder, setFolder] = useState<string | null>(null);
  const [done, setDone] = useState<{ skipped: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null); // "trash" | "reveal:<path>" | null
  const [confirmTrash, setConfirmTrash] = useState<{ paths: string[]; bytes: number } | null>(null);

  const unlistenRefs = useRef<Array<() => void>>([]);

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
            { key: evt.path, label: evt.label, path: evt.path, bytes: evt.bytes, risk: evt.risk },
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
    }).then((un) => {
      unlistenRefs.current.push(un);
    });
    listen<string>("scan-failed", (e) => {
      setError(e.payload);
      setScanning(false);
      setStage(null);
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
    setDone(null);
    try {
      const selectedFolder = await invoke<string | null>("run_scan");
      setPicking(false);
      if (selectedFolder) setFolder(selectedFolder);
      else { setScanning(false); setStage(null); }
    } catch (err) {
      setPicking(false);
      setError(String(err));
      setScanning(false);
      setStage(null);
    }
  }, []);

  const stopScan = useCallback(async () => {
    try {
      await invoke("stop_scan");
    } catch (err) {
      setError(String(err));
    }
  }, []);

  const sorted = useMemo(() => [...findings].sort((a, b) => b.bytes - a.bytes), [findings]);
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
    <div className="h-screen flex flex-col overflow-hidden">
      {/* Header */}
      <header className="px-6 py-4 border-b border-neutral-800 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold tracking-tight">Disk Detective</h1>
          <p className="text-xs text-neutral-500">Scan a folder you choose. Review items before moving them to Trash.</p>
        </div>
        <div className="flex items-center gap-3">
          {scanning && !picking ? (
            <button
              onClick={stopScan}
              className="px-4 py-2 rounded-lg bg-neutral-800 hover:bg-red-600 text-neutral-200 hover:text-white text-sm font-medium transition-colors"
            >
              Stop scan
            </button>
          ) : (
            <button
              onClick={runScan}
              disabled={picking}
              className="px-4 py-2 rounded-lg bg-emerald-600 hover:bg-emerald-500 text-sm font-medium transition-colors"
            >
              {picking ? "Choosing folder…" : "Choose folder to scan"}
            </button>
          )}
        </div>
      </header>

      {folder && <div className="px-6 py-2 text-xs text-neutral-400 truncate" title={folder}>Scanning: {folder}</div>}

      {/* Scanning status */}
      {scanning && stage && (
        <div className="px-6 py-2 text-xs text-neutral-400 flex items-center gap-2 bg-neutral-900/50">
          <span className="w-1.5 h-1.5 rounded-full bg-emerald-500 animate-pulse" />
          {stage}
        </div>
      )}

      {error && (
        <div className="mx-6 mt-3 px-3 py-2 rounded-md bg-red-950/50 border border-red-900 text-red-300 text-sm">
          {error}
        </div>
      )}

      {/* Findings table */}
      <main className="flex-1 overflow-auto px-6 py-4">
        {sorted.length === 0 && !scanning && (
          <div className="h-full flex items-center justify-center text-neutral-600 text-sm">
            Choose a folder to see which items use space.
          </div>
        )}

        {sorted.length > 0 && (
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-xs text-neutral-500">
                <th className="sticky top-0 z-10 bg-neutral-950 border-b border-neutral-800 py-2 pr-2 w-8"></th>
                <th className="sticky top-0 z-10 bg-neutral-950 border-b border-neutral-800 py-2 pr-4 w-20">
                  Risk
                </th>
                <th className="sticky top-0 z-10 bg-neutral-950 border-b border-neutral-800 py-2 pr-4">
                  What
                </th>
                <th className="sticky top-0 z-10 bg-neutral-950 border-b border-neutral-800 py-2 pr-4">
                  Path
                </th>
                <th className="sticky top-0 z-10 bg-neutral-950 border-b border-neutral-800 py-2 pr-4 text-right w-24">
                  Size
                </th>
                <th className="sticky top-0 z-10 bg-neutral-950 border-b border-neutral-800 py-2 w-16"></th>
              </tr>
            </thead>
            <tbody>
              {sorted.map((f) => (
                <tr key={f.key} className="border-b border-neutral-900 hover:bg-neutral-900/40">
                  <td className="py-2 pr-2">
                    <input
                      type="checkbox"
                      className="accent-emerald-500 h-5 w-5 cursor-pointer"
                      checked={selected.has(f.key)}
                      onChange={() => toggle(f.key)}
                    />
                  </td>
                  <td className="py-2 pr-4">
                    <span
                      className="text-[11px] px-1.5 py-0.5 rounded font-medium bg-amber-950 text-amber-400"
                    >
                      REVIEW
                    </span>
                  </td>
                  <td className="py-2 pr-4 text-neutral-200">{f.label}</td>
                  <td className="py-2 pr-4 text-neutral-500 font-mono text-xs max-w-xs">
                    <button
                      onClick={() => revealInFinder(f.path)}
                      disabled={busyAction === `reveal:${f.path}`}
                      title={`Reveal ${f.path} in Finder`}
                      className="block truncate max-w-xs text-left hover:text-neutral-200 hover:underline disabled:opacity-50"
                    >
                      {f.path}
                    </button>
                  </td>
                  <td className="py-2 pr-4 text-right text-neutral-300 tabular-nums">{formatBytes(f.bytes)}</td>
                  <td className="py-2 text-right">
                    <button
                      onClick={() => revealInFinder(f.path)}
                      disabled={busyAction === `reveal:${f.path}`}
                      className="text-xs text-neutral-500 hover:text-neutral-200"
                    >
                      Reveal
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </main>

      {/* Footer / action bar */}
      {sorted.length > 0 && !scanning && (
        <footer className="px-6 py-3 border-t border-neutral-800 flex items-center justify-between bg-neutral-950">
          <div className="flex items-center gap-3 text-sm">
            <button onClick={clearSelection} className="text-neutral-400 hover:text-neutral-100 text-xs">
              Clear
            </button>
            <span className="text-neutral-500 ml-2">
              {selected.size} selected — {formatBytes(selectedBytes)}
            </span>
          </div>
          <button
            onClick={moveSelectedToTrash}
            disabled={selected.size === 0 || busyAction === "trash"}
            className="px-4 py-2 rounded-lg bg-red-600 hover:bg-red-500 disabled:bg-neutral-800 disabled:text-neutral-500 text-sm font-medium transition-colors"
          >
            {busyAction === "trash" ? "Moving to Trash…" : `Move ${selected.size || ""} to Trash`}
          </button>
        </footer>
      )}

      {done && (
        <div className="px-6 py-2 text-xs text-neutral-500 border-t border-neutral-900">
          Scan complete.{done.skipped > 0 ? ` ${done.skipped} item(s) could not be read.` : ""}
        </div>
      )}

      {confirmTrash && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 px-6">
          <div className="w-full max-w-sm rounded-xl border border-neutral-800 bg-neutral-950 p-5 shadow-xl">
            <h2 className="text-sm font-semibold text-neutral-100">Move to Trash?</h2>
            <p className="mt-2 text-sm text-neutral-400">
              Move {confirmTrash.paths.length} item(s) ({formatBytes(confirmTrash.bytes)}) to Trash.
              Disk space is usually freed after you empty the Trash. Files can be restored until then.
            </p>
            <div className="mt-5 flex justify-end gap-3">
              <button
                onClick={() => setConfirmTrash(null)}
                className="px-3 py-1.5 rounded-md text-sm text-neutral-400 hover:text-neutral-100"
              >
                Cancel
              </button>
              <button
                onClick={performTrash}
                className="px-3 py-1.5 rounded-md bg-red-600 hover:bg-red-500 text-sm font-medium"
              >
                Move to Trash
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
