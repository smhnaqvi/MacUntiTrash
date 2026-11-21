# Disk Detective

![Disk Detective icon](src-tauri/icons/128x128.png)

Disk Detective is a macOS utility for finding large caches, developer artifacts, and other places that may be using disk space. It shows each finding's size and path before you decide what to move to Trash.

> [!IMPORTANT]
> This project is an MVP in preparation for the **Mac App Store**. The current build is not an App Store release. Sandbox access, distribution signing, provisioning, and testing of the packaged app are still required.

## What it does

- Streams scan results with a progress stage and a Stop button.
- Labels findings **SAFE** or **REVIEW**. Review every path before removing it; these labels are guidance, not a guarantee.
- Reveals findings in Finder and moves selected items to Trash after confirmation. Disk space is usually reclaimed only after Trash is emptied.
- Shows disk usage and warns when protected locations could not be read or Time Machine snapshots are present.

Scans run locally. The scanner writes a text report and a commented-out cleanup script to `~/disk-report-YYYYMMDD-HHMM/`. The app does not upload scan results.

## Run locally

You need macOS, Xcode Command Line Tools, Rust, and Node.js with npm.

```bash
npm ci
npm run tauri dev
```

The app uses React, TypeScript, and Tailwind for the interface, Tauri and Rust for the desktop process, and [mac-disk-scan.sh](src-tauri/bin/mac-disk-scan.sh) for the read-only scan. Rust handles scan lifecycle and Finder actions.

## Verify changes

```bash
npm run build
cargo check --manifest-path src-tauri/Cargo.toml --locked
bash -n src-tauri/bin/mac-disk-scan.sh
```

`npm run release` currently builds a local `.app` and `.dmg`; it does **not** produce a signed Mac App Store submission. Do not submit that output to App Store Connect.
