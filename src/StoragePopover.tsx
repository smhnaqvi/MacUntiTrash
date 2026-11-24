import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Volume = { total: number; used: number; available: number };
type ScanStatus = { active: boolean; path: string | null };

function size(bytes: number) {
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
}

export default function StoragePopover() {
  const [volume, setVolume] = useState<Volume | null>(null);
  const [error, setError] = useState(false);
  const [scan, setScan] = useState<ScanStatus>({ active: false, path: null });

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    const update = () => invoke<Volume>("system_volume_stats")
      .then((stats) => { if (active) { setVolume(stats); setError(false); } })
      .catch(() => { if (active) setError(true); });
    update();
    listen<Volume>("storage-updated", ({ payload }) => {
      setVolume(payload);
      setError(false);
    }).then((remove) => { unlisten = remove; });
    return () => { active = false; unlisten?.(); };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    listen<ScanStatus>("scan-status", ({ payload }) => setScan(payload)).then((remove) => {
      unlisten = remove;
      invoke<ScanStatus>("scan_status").then((status) => { if (active) setScan(status); });
    });
    return () => { active = false; unlisten?.(); };
  }, []);

  const other = volume ? Math.max(0, volume.total - volume.used - volume.available) : 0;
  const usedPercent = volume && volume.total > 0 ? Math.round(volume.used / volume.total * 100) : 0;
  const open = (scanMyFiles: boolean) => invoke("open_main_window", { scanMyFiles });

  return <div className="storage-popover">
    <div className="storage-topline"><span className="storage-mark">◈</span><strong>SpaceGlance</strong><span>DISK</span></div>
    <div className="storage-headline">
      <div><span>Available on startup disk</span><strong>{volume ? size(volume.available) : error ? "Unavailable" : "Checking…"}</strong></div>
      <span className="storage-glyph" aria-hidden="true">▣</span>
    </div>
    {volume && <>
      <div className="storage-progress" role="img" aria-label={`${size(volume.used)} used, ${size(volume.available)} available, ${size(other)} other or unavailable`}>
        <span className="storage-progress-used" style={{ width: `${volume.used / volume.total * 100}%` }} />
        <span className="storage-progress-available" style={{ width: `${volume.available / volume.total * 100}%` }} />
      </div>
      <div className="storage-metrics">
        <div><span>Used</span><strong>{size(volume.used)}</strong></div>
        <div><span>Available</span><strong>{size(volume.available)}</strong></div>
        <div><span>Capacity</span><strong>{size(volume.total)}</strong></div>
      </div>
      <p className="storage-caption">{usedPercent}% used · {size(other)} reserved or unavailable</p>
    </>}
    {scan.active && <div className="storage-scan-status" role="status">
      <span className="scan-spinner" /><div><strong>Scanning storage</strong><small title={scan.path || undefined}>{scan.path || "Preparing scan…"}</small></div>
    </div>}
    <div className="storage-actions">
      <button className="button button-primary" onClick={() => open(true)} disabled={scan.active}>
        {scan.active ? "Scanning…" : "Scan Storage"}
      </button>
      <button className="button button-secondary" onClick={() => open(false)}>Open app</button>
    </div>
  </div>;
}
