#!/usr/bin/env node
import { readFileSync, writeFileSync, mkdirSync, copyFileSync, existsSync, rmSync } from "node:fs";
import { resolve, join } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(import.meta.dirname, "..");
const tauriDir = join(root, "src-tauri");
const generated = join(tauriDir, ".appstore");
const identifier = JSON.parse(readFileSync(join(tauriDir, "tauri.conf.json"), "utf8")).identifier;
const {
  APPLE_TEAM_ID: team,
  APPLE_SIGNING_IDENTITY: appIdentity,
  APPLE_INSTALLER_SIGNING_IDENTITY: installerIdentity,
  APP_STORE_PROVISION_PROFILE: profile,
} = process.env;

function fail(message) {
  console.error(message);
  process.exit(1);
}
function run(command, args, env = process.env) {
  const result = spawnSync(command, args, { cwd: root, env, stdio: "inherit" });
  if (result.status !== 0) fail(`${command} failed`);
}
function plist(path) {
  const result = spawnSync("plutil", ["-convert", "json", "-o", "-", path], { encoding: "utf8" });
  if (result.status !== 0) fail(`Cannot inspect ${path}: ${result.stderr}`);
  return JSON.parse(result.stdout);
}
if (!/^[A-Z0-9]{10}$/.test(team ?? "")) fail("Set APPLE_TEAM_ID to your 10-character Apple Team ID.");
if (!/^[a-zA-Z0-9.-]+$/.test(identifier) || !identifier.includes(".")) fail("Set a valid bundle identifier in tauri.conf.json.");
if (!appIdentity || !installerIdentity) fail("Set APPLE_SIGNING_IDENTITY and APPLE_INSTALLER_SIGNING_IDENTITY.");
if (!profile || !existsSync(profile)) fail("Set APP_STORE_PROVISION_PROFILE to your Mac App Store Connect provisioning profile.");
run("xcodebuild", ["-version"]);
mkdirSync(generated, { recursive: true });
copyFileSync(profile, join(generated, "embedded.provisionprofile"));
writeFileSync(join(generated, "Entitlements.plist"), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.app-sandbox</key><true/>
  <key>com.apple.security.files.user-selected.read-write</key><true/>
  <key>com.apple.application-identifier</key><string>${team}.${identifier}</string>
  <key>com.apple.developer.team-identifier</key><string>${team}</string>
</dict></plist>
`);
writeFileSync(join(generated, "tauri.conf.json"), JSON.stringify({
  app: {
    security: {
      csp: {
        "default-src": "'self'",
        "connect-src": "ipc: http://ipc.localhost",
        "img-src": "'self' data:",
        "style-src": "'self' 'unsafe-inline'",
      },
    },
  },
  bundle: {
    targets: ["app"],
    resources: [],
    macOS: {
      entitlements: ".appstore/Entitlements.plist",
      files: { "embedded.provisionprofile": ".appstore/embedded.provisionprofile" },
    },
  },
}, null, 2) + "\n");
run("plutil", ["-lint", join(generated, "Entitlements.plist")]);
run("npm", ["run", "tauri", "build", "--", "--bundles", "app", "--config", "src-tauri/.appstore/tauri.conf.json"]);
const app = join(tauriDir, "target/release/bundle/macos/Disk Detective.app");
const pkg = join(tauriDir, "target/release/bundle/macos/Disk Detective.pkg");
run("codesign", ["--verify", "--deep", "--strict", app]);
const signedEntitlements = join(generated, "signed-entitlements.plist");
rmSync(signedEntitlements, { force: true });
run("codesign", ["-d", "--xml", "--entitlements", signedEntitlements, app]);
const entitlements = plist(signedEntitlements);
if (entitlements["com.apple.security.app-sandbox"] !== true ||
    entitlements["com.apple.security.files.user-selected.read-write"] !== true ||
    entitlements["com.apple.application-identifier"] !== `${team}.${identifier}`) {
  fail("Signed app is missing the required sandbox or file access entitlements.");
}
if (plist(join(app, "Contents", "Info.plist")).CFBundleIdentifier !== identifier) {
  fail("Signed app bundle identifier does not match tauri.conf.json.");
}
run("xcrun", ["productbuild", "--sign", installerIdentity, "--component", app, "/Applications", pkg]);
run("pkgutil", ["--check-signature", pkg]);
console.log(`Created ${pkg}`);
