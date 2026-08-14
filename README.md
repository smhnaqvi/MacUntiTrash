# Disk Detective

A small GUI over `mac-disk-scan.sh`: React + Tailwind frontend, Tauri v2 /
Rust backend. The backend's only job is to run the script with `--json` and
stream its output to the UI — all detection logic stays in the shell script.

**Honesty note:** this was written and the bash/JSON half was fully tested,
but the Rust/Tauri half was written against the Tauri v2 API from
documentation, not compiled — there's no macOS or Rust toolchain available
in the environment that produced it. It should be close, but budget 15–30
minutes for `cargo`/`tauri` to point out something version-specific,
especially in `tauri.conf.json`'s bundle/icon config, which shifts between
Tauri releases.

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

`src-tauri/icons/` already has a real, valid icon set (32×32, 128×128,
128×128@2x, and a verified `.icns`) — a plain placeholder glyph, generated
programmatically, not the empty file that broke the first build. The app
will compile as-is. Swap it for real branding whenever you want:

```bash
npx tauri icon /path/to/your-square-image.png
```

## Building a shippable .app

```bash
npm run tauri build
```

Output lands in `src-tauri/target/release/bundle/macos/` (and `dmg/` for a
disk image). It's unsigned, so first launch needs right-click → Open, same
as the plain `.app` wrapper from earlier — Gatekeeper, not a bug.

## Full Disk Access

Grant it to **this specific app**, not Terminal — a packaged `.app` is a new
bundle identity as far as TCC is concerned, so the earlier Terminal grant
doesn't carry over.

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
src-tauri/src/main.rs     spawns it, forwards each line as a
  run_scan()               `scan-line` Tauri event — no parsing here
        │
        ▼
src/App.tsx               listens for `scan-line`, JSON.parse's each one,
                           builds the findings table live as it streams in
```

Three more Rust commands back the UI: `trash_paths` (moves selected items to
Trash via Finder/AppleScript — reversible, not `rm -rf`), `reveal_in_finder`,
and `open_fda_settings`.

## Updating the scanner

`src-tauri/bin/mac-disk-scan.sh` is a bundled resource, not a symlink. If you
improve the standalone script later, copy it back in here:

```bash
cp /path/to/updated/mac-disk-scan.sh src-tauri/bin/mac-disk-scan.sh
```

## Things deliberately left out of v1

- **Root-level cleanup** (thinning Time Machine snapshots) needs `sudo` and
  isn't wired into the delete button — the UI just surfaces a warning with
  the count. Doing this safely from a GUI means a signed privileged helper
  (`SMAppService`), which is a real chunk of work on its own.
- **Auto re-grant detection for FDA** — the app doesn't poll TCC state. If a
  scan comes back with a shrinking-but-nonzero blind list after you thought
  you'd granted access, it usually means the grant didn't fully take —
  quit and relaunch again rather than assuming the app is broken.
