#!/usr/bin/env node
// Bumps the patch version across package.json, src-tauri/Cargo.toml, and
// src-tauri/tauri.conf.json in lockstep, so every release build ships a
// version number that's never been shipped before.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = dirname(dirname(fileURLToPath(import.meta.url)));

const pkgPath = join(root, "package.json");
const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
const [major, minor, patch] = pkg.version.split(".").map(Number);
const oldVersion = pkg.version;
const newVersion = `${major}.${minor}.${patch + 1}`;

pkg.version = newVersion;
writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");

const cargoPath = join(root, "src-tauri", "Cargo.toml");
const cargo = readFileSync(cargoPath, "utf8");
writeFileSync(
  cargoPath,
  cargo.replace(/^version = "[^"]+"/m, `version = "${newVersion}"`),
);

const confPath = join(root, "src-tauri", "tauri.conf.json");
const conf = JSON.parse(readFileSync(confPath, "utf8"));
conf.version = newVersion;
writeFileSync(confPath, JSON.stringify(conf, null, 2) + "\n");

console.log(`Version bumped: ${oldVersion} -> ${newVersion}`);
