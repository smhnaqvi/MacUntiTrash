# SpaceGlance

![SpaceGlance icon](src-tauri/icons/128x128.png)

SpaceGlance is a macOS app that shows which items occupy space inside a folder you choose. It scans locally and lets you review and move selected items to Trash.

> [!IMPORTANT]
> Mac App Store preparation is in progress. A signed, sandboxed build must be tested before submission.

## Features

- Choose a folder with the native macOS picker; only that folder is scanned.
- See the allocated size of each top-level item larger than 10 MB. Every finding is marked **REVIEW**.
- Reveal an item in Finder or move it to Trash after confirmation. Space is usually freed when Trash is emptied.
- Stop a running scan and see a count of items that could not be read.

The app does not upload scan results or create cleanup scripts. It uses React, TypeScript, Tailwind, Tauri, and Rust.

## Develop

Install Node.js, Rust, and Xcode Command Line Tools on macOS:

```bash
npm ci
npm run tauri dev
```

Check changes with `npm run build` and `cargo test --manifest-path src-tauri/Cargo.toml --locked`.

## Mac App Store build

Register `com.deviatech.spaceglance` in Apple Developer, create a **Mac App Store Connect** provisioning profile, and install Apple Distribution and Mac Installer Distribution certificates. A full Xcode installation is required.

Set `APPLE_TEAM_ID`, `APPLE_SIGNING_IDENTITY`, `APPLE_INSTALLER_SIGNING_IDENTITY`, and `APP_STORE_PROVISION_PROFILE` in your build environment, then run:

```bash
npm run release:app-store
```

This creates a sandboxed `.app` and signed `.pkg` under `src-tauri/target/release/bundle/macos/`. Confirm the bundle ID, signing, sandbox permissions, and scan/Trash behavior on a clean Mac before uploading the package to App Store Connect. `npm run release:local` is only for local, non-store builds.
