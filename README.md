# Disk Detective

A small macOS GUI over [`mac-disk-scan.sh`](src-tauri/bin/mac-disk-scan.sh):
React + Tailwind frontend, Tauri v2 / Rust backend. The backend's only job
is to run the script with `--json` and stream its output to the UI — all
detection logic stays in the shell script.

Scans your disk for reclaimable space (caches, old build artifacts,
`node_modules`, oversized files, etc.), shows what it found with a
safe/review risk label, and lets you move selected items to Trash —
never a hard delete.

## Features

- **Live streaming scan** with a stage indicator, and a **Stop** button
  that actually cancels the in-flight scan (kills the whole scanner
  process tree, not just the top-level script).
- **Findings table** sorted by size, each row tagged `SAFE` or `REVIEW`,
  with the path itself clickable to reveal the item in Finder.
- **Move to Trash**, gated behind an in-app confirmation modal — items go
  to Trash via Finder/AppleScript (reversible, not `rm -rf`) until you
  empty it yourself.
- **System Storage panel**, pinned to the top of the window: total, used,
  free, and reclaimable space once a scan completes.
- **Blind spot and Time Machine snapshot warnings**, with a one-click
  deep link to the Full Disk Access settings pane.

## Setup

```bash
# 1. Rust toolchain (skip if already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Node deps
npm install

# 3. Run it
npm run tauri dev
```

`npm run tauri dev` opens a live window; edit `src/App.tsx` and it hot-reloads.

### About the icon

`src-tauri/icons/` holds the macOS icon set (32×32, 64×64, 128×128,
128×128@2x, and `.icns`). Swap it for your own branding with:

```bash
npx tauri icon /path/to/your-square-image.png
```

This also generates Windows/iOS/Android variants under `src-tauri/icons/`;
since this app only ships a macOS bundle, those aren't checked in — delete
them after regenerating, or keep them if you plan to target other platforms.

## Building a shippable .app

```bash
npm run release
```

This bumps the patch version in `package.json`, `src-tauri/Cargo.toml`,
and `src-tauri/tauri.conf.json` together, then runs `tauri build` — so
every release build ships a version number that's never shipped before.
Output lands in `src-tauri/target/release/bundle/macos/` (and `dmg/` for
a disk image you can hand to someone else to try). It's unsigned, so
first launch needs right-click → Open, same as any unsigned `.app` —
Gatekeeper, not a bug.

To build without bumping the version, run `npx tauri build` directly.

## Full Disk Access

Grant it to **this specific app**, not Terminal — a packaged `.app` is a
new bundle identity as far as TCC is concerned, so an earlier Terminal
grant doesn't carry over, and a fresh rebuild in a new location needs its
own grant too.

System Settings → Privacy & Security → Full Disk Access → add
`Disk Detective.app` → fully quit and reopen it. The app has a button that
deep-links to this exact pane once you've run a scan and it detects blind
spots — but the first grant has to happen manually since the app can't
read anything until it has the permission.

## How the pieces fit

```
mac-disk-scan.sh --json  →  stdout, one JSON object per line
        │
        ▼
src-tauri/src/main.rs     spawns it in its own process group, forwards
  run_scan() / stop_scan() each line as a `scan-line` Tauri event (no
                           parsing here), and can kill the whole group
                           on demand — no parsing here
        │
        ▼
src/App.tsx               listens for `scan-line` / `scan-stopped`,
                           JSON.parse's each line, builds the findings
                           table live as it streams in
```

Three more Rust commands back the UI: `trash_paths` (moves selected items
to Trash via Finder/AppleScript), `reveal_in_finder`, and
`open_fda_settings`.

## Things deliberately left out

- **Root-level cleanup** (thinning Time Machine snapshots) needs `sudo`
  and isn't wired into the delete button — the UI just surfaces a warning
  with the count. Doing this safely from a GUI means a signed privileged
  helper (`SMAppService`), which is a real chunk of work on its own.
- **Auto re-grant detection for FDA** — the app doesn't poll TCC state.
  If a scan comes back with a shrinking-but-nonzero blind list after you
  thought you'd granted access, it usually means the grant didn't fully
  take — quit and relaunch again rather than assuming the app is broken.

## Contributing

Issues and pull requests are welcome. For anything beyond a small fix,
please open an issue first to discuss the approach — this is a small,
focused tool and changes to the detection logic in
`mac-disk-scan.sh` in particular should stay conservative about what
counts as "safe" to suggest deleting.

## License

[MIT](LICENSE)
