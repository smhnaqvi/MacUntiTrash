import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---- types mirroring the NDJSON events emitted by mac-disk-scan.sh --json ----
type Risk = "safe" | "review";

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
  | { type: "blind"; path: string }
  | { type: "volume"; total_bytes: number; used_bytes: number; free_bytes: number }
  | { type: "snapshot_warning"; count: number }
  | { type: "done"; reclaimable_bytes: number; blind_count: number; report: string; cleanup: string };

function formatBytes(n: number): string {
  if (!n || n <= 0) return "0 MB";
  const gb = n / 1024 ** 3;
  if (gb >= 1) return `${gb.toFixed(1)} GB`;
  const mb = n / 1024 ** 2;
  return `${mb.toFixed(0)} MB`;
}

export default function App() {
  const [scanning, setScanning] = useState(false);
  const [deep, setDeep] = useState(true);
  const [stage, setStage] = useState<string | null>(null);
  const [volume, setVolume] = useState<{ total: number; used: number; free: number } | null>(null);
  const [findings, setFindings] = useState<Finding[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [blind, setBlind] = useState<string[]>([]);
  const [snapshotCount, setSnapshotCount] = useState<number | null>(null);
  const [done, setDone] = useState<{ reclaimable: number; report: string; cleanup: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null); // "trash" | "reveal:<path>" | null

  const unlistenRef = useRef<null | (() => void)>(null);

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
        case "volume":
          setVolume({ total: evt.total_bytes, used: evt.used_bytes, free: evt.free_bytes });
          break;
        case "finding":
          setFindings((prev) => [
            ...prev,
            { key: evt.path, label: evt.label, path: evt.path, bytes: evt.bytes, risk: evt.risk },
          ]);
          break;
        case "blind":
          setBlind((prev) => (prev.includes(evt.path) ? prev : [...prev, evt.path]));
          break;
        case "snapshot_warning":
          setSnapshotCount(evt.count);
          break;
        case "done":
          setDone({ reclaimable: evt.reclaimable_bytes, report: evt.report, cleanup: evt.cleanup });
          setStage(null);
          setScanning(false);
          break;
      }
    }).then((un) => {
      unlistenRef.current = un;
    });
    return () => unlistenRef.current?.();
  }, []);

  const runScan = useCallback(async (deepMode: boolean) => {
    setScanning(true);
    setError(null);
    setStage("Starting…");
    setFindings([]);
    setSelected(new Set());
    setBlind([]);
    setSnapshotCount(null);
    setDone(null);
    try {
      await invoke("run_scan", { deep: deepMode });
    } catch (err) {
      setError(String(err));
      setScanning(false);
      setStage(null);
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

  function selectAllSafe() {
    setSelected(new Set(sorted.filter((f) => f.risk === "safe").map((f) => f.key)));
  }

  function clearSelection() {
    setSelected(new Set());
  }

  async function moveSelectedToTrash() {
    const paths = sorted.filter((f) => selected.has(f.key)).map((f) => f.path);
    if (paths.length === 0) return;
    const confirmed = window.confirm(
      `Move ${paths.length} item(s) to Trash, freeing about ${formatBytes(selectedBytes)}?\n\n` +
        `Files go to Trash and can be restored until you empty it.`
    );
    if (!confirmed) return;

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

  async function openFullDiskAccessSettings() {
    try {
      await invoke("open_fda_settings");
    } catch (err) {
      setError(String(err));
    }
  }

  const usedPct = volume ? Math.min(100, (volume.used / volume.total) * 100) : 0;

  return (
    <div className="min-h-screen flex flex-col">
      {/* Header */}
      <header className="px-6 py-4 border-b border-neutral-800 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold tracking-tight">Disk Detective</h1>
          <p className="text-xs text-neutral-500">Finds reclaimable space. Never deletes without asking.</p>
        </div>
        <div className="flex items-center gap-3">
          <label className="flex items-center gap-2 text-sm text-neutral-400 select-none">
            <input
              type="checkbox"
              className="accent-emerald-500"
              checked={deep}
              disabled={scanning}
              onChange={(e) => setDeep(e.target.checked)}
            />
            Deep scan (node_modules, files &gt; 500 MB)
          </label>
          <button
            onClick={() => runScan(deep)}
            disabled={scanning}
            className="px-4 py-2 rounded-lg bg-emerald-600 hover:bg-emerald-500 disabled:bg-neutral-800 disabled:text-neutral-500 text-sm font-medium transition-colors"
          >
            {scanning ? "Scanning…" : "Scan"}
          </button>
        </div>
      </header>

      {/* Volume bar */}
      {volume && (
        <div className="px-6 py-3 border-b border-neutral-800">
          <div className="flex justify-between text-xs text-neutral-400 mb-1.5">
            <span>
              {formatBytes(volume.used)} used of {formatBytes(volume.total)}
            </span>
            <span className={volume.free < 10 * 1024 ** 3 ? "text-red-400 font-medium" : ""}>
              {formatBytes(volume.free)} free
            </span>
          </div>
          <div className="h-1.5 rounded-full bg-neutral-800 overflow-hidden">
            <div
              className={`h-full rounded-full transition-all ${
                usedPct > 90 ? "bg-red-500" : usedPct > 75 ? "bg-amber-500" : "bg-emerald-500"
              }`}
              style={{ width: `${usedPct}%` }}
            />
          </div>
        </div>
      )}

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

      {/* Snapshot warning */}
      {snapshotCount !== null && snapshotCount > 0 && (
        <div className="mx-6 mt-3 px-3 py-2 rounded-md bg-amber-950/40 border border-amber-900/60 text-amber-300 text-sm">
          Found {snapshotCount} local Time Machine snapshot(s) — these can pin tens of GB of deleted
          files on disk. Thin them with <code className="text-amber-200">tmutil</code> in Terminal; not
          automated here since it needs sudo.
        </div>
      )}

      {/* Blind spots warning */}
      {blind.length > 0 && (
        <div className="mx-6 mt-3 px-3 py-2 rounded-md bg-amber-950/40 border border-amber-900/60 text-amber-300 text-sm flex items-center justify-between gap-4">
          <span>
            {blind.length} location(s) couldn't be read — likely hiding the largest items. Grant Full
            Disk Access, fully quit and reopen this app, then scan again.
          </span>
          <button
            onClick={openFullDiskAccessSettings}
            className="shrink-0 px-3 py-1.5 rounded-md bg-amber-900/60 hover:bg-amber-900 text-xs font-medium whitespace-nowrap"
          >
            Open Settings
          </button>
        </div>
      )}

      {/* Findings table */}
      <main className="flex-1 overflow-auto px-6 py-4">
        {sorted.length === 0 && !scanning && (
          <div className="h-full flex items-center justify-center text-neutral-600 text-sm">
            Run a scan to see what's using your disk.
          </div>
        )}

        {sorted.length > 0 && (
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-xs text-neutral-500 border-b border-neutral-800">
                <th className="py-2 pr-2 w-8"></th>
                <th className="py-2 pr-4 w-20">Risk</th>
                <th className="py-2 pr-4">What</th>
                <th className="py-2 pr-4">Path</th>
                <th className="py-2 pr-4 text-right w-24">Size</th>
                <th className="py-2 w-16"></th>
              </tr>
            </thead>
            <tbody>
              {sorted.map((f) => (
                <tr key={f.key} className="border-b border-neutral-900 hover:bg-neutral-900/40">
                  <td className="py-2 pr-2">
                    <input
                      type="checkbox"
                      className="accent-emerald-500"
                      checked={selected.has(f.key)}
                      onChange={() => toggle(f.key)}
                    />
                  </td>
                  <td className="py-2 pr-4">
                    <span
                      className={`text-[11px] px-1.5 py-0.5 rounded font-medium ${
                        f.risk === "safe"
                          ? "bg-emerald-950 text-emerald-400"
                          : "bg-amber-950 text-amber-400"
                      }`}
                    >
                      {f.risk === "safe" ? "SAFE" : "REVIEW"}
                    </span>
                  </td>
                  <td className="py-2 pr-4 text-neutral-200">{f.label}</td>
                  <td className="py-2 pr-4 text-neutral-500 font-mono text-xs truncate max-w-xs" title={f.path}>
                    {f.path}
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
      {sorted.length > 0 && (
        <footer className="px-6 py-3 border-t border-neutral-800 flex items-center justify-between bg-neutral-950">
          <div className="flex items-center gap-3 text-sm">
            <button onClick={selectAllSafe} className="text-neutral-400 hover:text-neutral-100 text-xs">
              Select all SAFE
            </button>
            <span className="text-neutral-700">·</span>
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
        <div className="px-6 py-2 text-xs text-neutral-500 border-t border-neutral-900 flex items-center justify-between">
          <span>
            Scan complete. Full report and a commented-out cleanup script were also written to disk.
          </span>
          <div className="flex gap-3">
            <button onClick={() => revealInFinder(done.report)} className="hover:text-neutral-200">
              Reveal report
            </button>
            <button onClick={() => revealInFinder(done.cleanup)} className="hover:text-neutral-200">
              Reveal cleanup.sh
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
