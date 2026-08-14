#!/usr/bin/env bash
#
# mac-disk-scan.sh — read-only disk space detective for macOS
#
# This script NEVER deletes anything. It scans the well-known places where
# macOS (and dev tooling) hides gigabytes, prints a sorted report, and writes
# a cleanup script full of *commented-out* commands for you to review.
#
# Usage:
#   chmod +x mac-disk-scan.sh
#   ./mac-disk-scan.sh                  # normal scan, human-readable
#   ./mac-disk-scan.sh --deep           # also hunt node_modules + huge files (slower)
#   ./mac-disk-scan.sh --json           # emit NDJSON events on stdout, for a GUI
#   ./mac-disk-scan.sh --deep --json    # combine, any order
#
# --json still writes the normal report.txt / cleanup.sh files; it just
# replaces the human-readable stdout with one JSON object per line:
#   {"type":"stage","name":"..."}
#   {"type":"finding","risk":"safe|review","label":"...","path":"...","bytes":N}
#   {"type":"blind","path":"..."}
#   {"type":"volume","total_bytes":N,"used_bytes":N,"free_bytes":N}
#   {"type":"snapshot_warning","count":N}
#   {"type":"done","reclaimable_bytes":N,"blind_count":N,"report":"...","cleanup":"..."}
#
# Tip: give your terminal (or the app wrapping this script) Full Disk Access
# (System Settings > Privacy & Security > Full Disk Access) or some
# directories will silently report 0 B / show up as blind.

set -uo pipefail

DEEP=0
JSON_MODE=0
for arg in "$@"; do
  case "$arg" in
    --deep) DEEP=1 ;;
    --json) JSON_MODE=1 ;;
  esac
done

OUTDIR="$HOME/disk-report-$(date +%Y%m%d-%H%M)"
mkdir -p "$OUTDIR"
REPORT="$OUTDIR/report.txt"
CLEANUP="$OUTDIR/cleanup.sh"

# ---------- pretty output ----------
if [ -t 1 ] && [ "$JSON_MODE" -eq 0 ]; then
  B=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GRN=$'\033[32m'
  YEL=$'\033[33m'; CYA=$'\033[36m'; R=$'\033[0m'
else
  B=""; DIM=""; RED=""; GRN=""; YEL=""; CYA=""; R=""
fi

say() {
  printf '%s\n' "$*" >>"$REPORT"
  [ "$JSON_MODE" -eq 1 ] || printf '%s\n' "$*"
}
hdr() {
  printf '\n=== %s ===\n' "$*" >>"$REPORT"
  if [ "$JSON_MODE" -eq 1 ]; then
    printf '{"type":"stage","name":"%s"}\n' "$(jesc "$*")"
  else
    printf '\n%s\n' "${B}${CYA}=== $* ===${R}"
  fi
}
# out — like `tee -a "$REPORT"`, but in JSON mode writes to the report file
# only (findings are reported as structured events instead, not text rows).
out() {
  if [ "$JSON_MODE" -eq 1 ]; then cat >>"$REPORT"; else tee -a "$REPORT"; fi
}
# jesc — minimal JSON string escaper (backslash, quote, newline). No python3
# dependency on purpose: this has to run on a bare macOS with nothing installed.
jesc() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n' ' '
}

# ---------- helpers ----------
# Size of a path in KB (0 if missing/unreadable). -x = stay on this volume.
size_kb() {
  [ -e "$1" ] || { echo 0; return; }
  local kb
  kb=$(du -skx "$1" 2>/dev/null | awk 'NR==1{print $1+0}')
  echo "${kb:-0}"          # du prints nothing when the path is unreadable
}

# Paths macOS protects behind Full Disk Access. If these are unreadable the
# scan is blind to them, and they are often the biggest things on the disk.
BLIND=()
BLIND_COUNT=0
readable() {
  [ -e "$1" ] || return 0
  if ! ls "$1" >/dev/null 2>&1; then
    BLIND+=("$1")
    BLIND_COUNT=$((BLIND_COUNT + 1))
    [ "$JSON_MODE" -eq 1 ] && printf '{"type":"blind","path":"%s"}\n' "$(jesc "$1")"
    return 1
  fi
  return 0
}

fmt_kb() {
  awk -v k="${1:-0}" 'BEGIN{
    if (k >= 1048576) printf "%.1f GB", k/1048576;
    else if (k >= 1024) printf "%.0f MB", k/1024;
    else printf "%d KB", k;
  }'
}

# Collected findings: "<kb>|<tag>|<label>|<path>"
FINDINGS=()
TOTAL_SAFE_KB=0

# add <tag> <label> <path>
#   tag SAFE   = regenerated automatically, deleting is low risk
#   tag REVIEW = look inside before deleting, may contain data you want
add() {
  local tag="$1" label="$2" path="$3" kb
  readable "$path" || return
  kb=$(size_kb "$path")
  [ "${kb:-0}" -lt 10240 ] && return   # skip anything under ~10 MB
  FINDINGS+=("$kb|$tag|$label|$path")
  [ "$tag" = "SAFE" ] && TOTAL_SAFE_KB=$((TOTAL_SAFE_KB + kb))
  if [ "$JSON_MODE" -eq 1 ]; then
    printf '{"type":"finding","risk":"%s","label":"%s","path":"%s","bytes":%s}\n' \
      "$([ "$tag" = "SAFE" ] && echo safe || echo review)" \
      "$(jesc "$label")" "$(jesc "$path")" "$((kb * 1024))"
  fi
}

: >"$REPORT"
say "macOS disk scan — $(date)"
say "Host: $(hostname 2>/dev/null)  User: ${USER:-$(whoami 2>/dev/null)}"

# ---------- 1. overall volume state ----------
hdr "Volume overview"
# On modern macOS "/" is the sealed, read-only system snapshot (~12 GB and
# never changes). Everything you actually own lives on the Data volume.
say "Data volume (this is the number that matters):"
df -h /System/Volumes/Data 2>/dev/null | out
say ""
say "System volume (read-only, ignore):"
df -h / 2>/dev/null | out

# APFS reports "purgeable" space that df hides. This is the honest number.
if command -v diskutil >/dev/null 2>&1; then
  diskutil info / 2>/dev/null \
    | grep -E "Container Total Space|Container Free Space|Volume Used Space" \
    | out
fi

if [ "$JSON_MODE" -eq 1 ]; then
  # -P for portable single-line output, -k for KB so the math is exact
  read -r _ TOTAL_KB USED_KB FREE_KB _ < <(df -Pk /System/Volumes/Data 2>/dev/null | awk 'NR==2')
  printf '{"type":"volume","total_bytes":%s,"used_bytes":%s,"free_bytes":%s}\n' \
    "$(( ${TOTAL_KB:-0} * 1024 ))" "$(( ${USED_KB:-0} * 1024 ))" "$(( ${FREE_KB:-0} * 1024 ))"
fi

# ---------- 2. Time Machine local snapshots ----------
# These are the #1 cause of "my Mac says the disk is full but I deleted things".
hdr "Time Machine local snapshots"
SNAPS=""
if command -v tmutil >/dev/null 2>&1; then
  SNAPS=$(tmutil listlocalsnapshots / 2>/dev/null | grep -E '^com\.apple\.TimeMachine' || true)
fi
if [ -n "$SNAPS" ]; then
  SNAP_COUNT=$(printf '%s\n' "$SNAPS" | wc -l | tr -d ' ')
  say "${RED}Found $SNAP_COUNT local snapshot(s).${R} Each one pins deleted files on disk."
  printf '%s\n' "$SNAPS" | out
  say "These can hold tens of GB and are the usual reason free space doesn't come back."
  [ "$JSON_MODE" -eq 1 ] && printf '{"type":"snapshot_warning","count":%s}\n' "$SNAP_COUNT"
else
  say "None found (good)."
fi

# ---------- 3. known junk locations ----------
hdr "Scanning known cache & junk locations (this takes a minute)"

# System / user level caches
add SAFE   "User caches"               "$HOME/Library/Caches"
add SAFE   "System caches"             "/Library/Caches"
add SAFE   "User logs"                 "$HOME/Library/Logs"
add SAFE   "Saved application state"   "$HOME/Library/Saved Application State"
add REVIEW "Trash"                     "$HOME/.Trash"
add REVIEW "Downloads folder"          "$HOME/Downloads"

# Xcode / iOS — usually the single biggest offender on a dev machine
add SAFE   "Xcode DerivedData"         "$HOME/Library/Developer/Xcode/DerivedData"
add SAFE   "Xcode module cache"        "$HOME/Library/Developer/Xcode/UserData/IB Support"
add REVIEW "Xcode archives"            "$HOME/Library/Developer/Xcode/Archives"
add SAFE   "iOS DeviceSupport symbols" "$HOME/Library/Developer/Xcode/iOS DeviceSupport"
add SAFE   "watchOS DeviceSupport"     "$HOME/Library/Developer/Xcode/watchOS DeviceSupport"
add SAFE   "CoreSimulator devices"     "$HOME/Library/Developer/CoreSimulator/Devices"
add SAFE   "CoreSimulator caches"      "$HOME/Library/Developer/CoreSimulator/Caches"
add SAFE   "Xcode iOS Support"         "$HOME/Library/Developer/Xcode/iOS Support"

# JS / package manager caches
add SAFE   "npm cache"                 "$HOME/.npm/_cacache"
add SAFE   "yarn cache"                "$HOME/Library/Caches/Yarn"
add SAFE   "yarn berry cache"          "$HOME/.yarn/berry/cache"
add SAFE   "pnpm store"                "$HOME/Library/pnpm/store"
add SAFE   "pnpm cache"                "$HOME/.pnpm-store"
add SAFE   "bun install cache"         "$HOME/.bun/install/cache"
add SAFE   "Next.js telemetry/cache"   "$HOME/.next"
add SAFE   "Playwright browsers"       "$HOME/Library/Caches/ms-playwright"
add SAFE   "Puppeteer browsers"        "$HOME/.cache/puppeteer"
add SAFE   "Cypress binaries"          "$HOME/Library/Caches/Cypress"
add SAFE   "Electron/electron-builder" "$HOME/Library/Caches/electron"

# Other language / tool caches
add SAFE   "Homebrew cache"            "$HOME/Library/Caches/Homebrew"
add SAFE   "pip cache"                 "$HOME/Library/Caches/pip"
add SAFE   "Go module cache"           "$HOME/go/pkg/mod"
add SAFE   "Cargo registry"            "$HOME/.cargo/registry"
add SAFE   "Gradle caches"             "$HOME/.gradle/caches"
add SAFE   "CocoaPods cache"           "$HOME/Library/Caches/CocoaPods"

# Docker — Docker.raw grows and never shrinks by itself
add REVIEW "Docker disk image"         "$HOME/Library/Containers/com.docker.docker/Data/vms"
add REVIEW "Docker (newer layout)"     "$HOME/Library/Containers/com.docker.docker/Data/vms/0"
add REVIEW "OrbStack data"             "$HOME/.orbstack"

# Apps that hoard
add SAFE   "Chrome cache"              "$HOME/Library/Caches/Google/Chrome"
add SAFE   "Safari cache"              "$HOME/Library/Caches/com.apple.Safari"
add SAFE   "Slack cache"               "$HOME/Library/Application Support/Slack/Cache"
add SAFE   "Slack service worker"      "$HOME/Library/Application Support/Slack/Service Worker"
add SAFE   "Discord cache"             "$HOME/Library/Application Support/discord/Cache"
add SAFE   "VS Code cached data"       "$HOME/Library/Application Support/Code/CachedData"
add SAFE   "VS Code workspace storage" "$HOME/Library/Application Support/Code/User/workspaceStorage"
add REVIEW "iOS device backups"        "$HOME/Library/Application Support/MobileSync/Backup"
add REVIEW "Claude desktop VM image"   "$HOME/Library/Application Support/Claude/vm_bundles"
add REVIEW "Cursor global storage"     "$HOME/Library/Application Support/Cursor/User/globalStorage"
add SAFE   "Chrome on-device AI model" "$HOME/Library/Application Support/Google/Chrome/OptGuideOnDeviceModel"
add SAFE   "Google updater cache"      "$HOME/Library/Application Support/Google/GoogleUpdater/crx_cache"
add SAFE   "nvm node versions"         "$HOME/.nvm/versions"
add SAFE   "npx cache"                 "$HOME/.npm/_npx"
add REVIEW "Parallels VMs"             "$HOME/Parallels"
add REVIEW "UTM VMs"                   "$HOME/Library/Containers/com.utmapp.UTM"
add REVIEW "Mail data"                 "$HOME/Library/Mail"
add REVIEW "Messages attachments"      "$HOME/Library/Messages"
add REVIEW "GarageBand/Logic sounds"   "$HOME/Library/Application Support/GarageBand"

# ---------- 4. biggest folders in home ----------
hdr "Biggest top-level folders in your home directory"
# The glob must include dotfolders — ~/.nvm, ~/.npm, ~/.cargo and friends
# routinely hold several GB and a bare * misses every one of them.
for d in "$HOME"/*/ "$HOME"/.*/; do
  [ -d "$d" ] || continue
  case "$d" in
    "$HOME"/./|"$HOME"/../) continue ;;
    *"/Library/CloudStorage/"*) continue ;;
  esac
  kb=$(size_kb "$d")
  printf '%s|%s\n' "$kb" "$d"
done | sort -t'|' -k1,1nr | head -20 | while IFS='|' read -r kb path; do
  printf '  %10s  %s\n' "$(fmt_kb "$kb")" "$path" | out
done

# ---------- 4b. space outside the home directory ----------
# If your home total is far below the volume's used space, the difference is
# hiding here (or behind Full Disk Access).
hdr "Space outside your home directory"
HOME_KB=$(size_kb "$HOME")
say "Your home directory totals $(fmt_kb "$HOME_KB")"
say ""
for d in /Applications /Library /Users/Shared /opt /usr/local /private/var; do
  [ -d "$d" ] || continue
  kb=$(size_kb "$d")
  [ "${kb:-0}" -lt 102400 ] && continue
  printf '  %10s  %s\n' "$(fmt_kb "$kb")" "$d" | out
done
say ""
say "${DIM}Run with sudo to see inside protected system folders.${R}"

# Other mounted volumes, and other user accounts
if [ -d /Users ]; then
  OTHERS=$(find /Users -maxdepth 1 -type d ! -name Shared ! -name "$(basename "$HOME")" ! -name Users 2>/dev/null | tail -n +2)
  [ -n "$OTHERS" ] && { say "Other user accounts on this Mac:"; printf '%s\n' "$OTHERS" | out; }
fi

# ---------- 5. deep scan (optional) ----------
if [ "$DEEP" -eq 1 ]; then
  hdr "node_modules / build artifact folders"
  find "$HOME" -maxdepth 7 \
       \( -path "$HOME/Library/CloudStorage" -o -path "$HOME/Library/Caches" \) -prune -o \
       -type d \( -name node_modules -o -name .next -o -name .turbo -o -name dist -o -name build \) -prune -print \
       2>/dev/null | while read -r d; do
    kb=$(size_kb "$d")
    [ "$kb" -ge 51200 ] && printf '%s|%s\n' "$kb" "$d"
  done | sort -t'|' -k1,1nr | head -30 | while IFS='|' read -r kb path; do
    printf '  %10s  %s\n' "$(fmt_kb "$kb")" "$path" | out
  done

  hdr "Individual files larger than 500 MB"
  find "$HOME" -type f -size +500M \
       -not -path "*/Library/CloudStorage/*" \
       2>/dev/null | while read -r f; do
    kb=$(du -kx "$f" 2>/dev/null | awk 'NR==1{print $1+0}')
    printf '%s|%s\n' "$kb" "$f"
  done | sort -t'|' -k1,1nr | head -25 | while IFS='|' read -r kb path; do
    printf '  %10s  %s\n' "$(fmt_kb "$kb")" "$path" | out
  done
fi

# ---------- 6. the report ----------
hdr "Findings, largest first"
printf '  %-10s %-7s %-28s %s\n' "SIZE" "RISK" "WHAT" "PATH" | out
printf '  %s\n' "------------------------------------------------------------------------" | out

printf '%s\n' "${FINDINGS[@]:-}" | sort -t'|' -k1,1nr | while IFS='|' read -r kb tag label path; do
  [ -z "${kb:-}" ] && continue
  if [ "$tag" = "SAFE" ]; then color="$GRN"; else color="$YEL"; fi
  if [ "$JSON_MODE" -eq 0 ]; then
    printf '  %-10s %s%-7s%s %-28s %s\n' "$(fmt_kb "$kb")" "$color" "$tag" "$R" "$label" "$path"
  fi
  printf '  %-10s %-7s %-28s %s\n' "$(fmt_kb "$kb")" "$tag" "$label" "$path" >>"$REPORT"
done

say ""
say "${B}Reclaimable from SAFE items alone: $(fmt_kb "$TOTAL_SAFE_KB")${R}"
say "${DIM}(nested paths can overlap — a child cache is counted inside its parent)${R}"

if [ "$BLIND_COUNT" -gt 0 ]; then
  say ""
  say "${RED}${B}WARNING: $BLIND_COUNT location(s) could not be read.${R}"
  say "${RED}These are invisible to this scan and may be the largest things on your disk.${R}"
  for p in "${BLIND[@]:-}"; do
    say "  - $p"
  done
  say ""
  say "Fix: System Settings > Privacy & Security > Full Disk Access,"
  say "add your terminal app, quit it fully, reopen, and run this again."
fi
say "${DIM}SAFE   = regenerated automatically by the tool that made it."
say "REVIEW = may contain data you actually want. Look inside first.${R}"

# ---------- 7. write cleanup script (everything commented out) ----------
{
  echo '#!/usr/bin/env bash'
  echo '#'
  echo "# Cleanup suggestions generated $(date)"
  echo '#'
  echo '# EVERY COMMAND BELOW IS COMMENTED OUT ON PURPOSE.'
  echo '# Read each one, uncomment only what you understand, then run it.'
  echo '# Work top to bottom: the first few usually free the most space.'
  echo ''
  echo 'set -euo pipefail'
  echo ''
  echo '### 1. Time Machine local snapshots (often 10-100 GB) ###'
  echo '# tmutil listlocalsnapshots /'
  echo '# sudo tmutil thinlocalsnapshots / 10000000000 4     # ask macOS to thin ~10 GB'
  echo '# for s in $(tmutil listlocalsnapshots / | sed "s/com.apple.TimeMachine.//;s/.local//"); do'
  echo '#   sudo tmutil deletelocalsnapshots "$s"'
  echo '# done'
  echo ''
  echo '### 2. Empty the Trash ###'
  echo '# rm -rf ~/.Trash/*'
  echo ''
  echo '### 3. Developer caches (safe, rebuilt on demand) ###'
  echo '# rm -rf ~/Library/Developer/Xcode/DerivedData/*'
  echo '# rm -rf ~/Library/Developer/Xcode/iOS\ DeviceSupport/*'
  echo '# xcrun simctl delete unavailable      # remove dead simulators'
  echo '# npm cache clean --force'
  echo '# yarn cache clean'
  echo '# pnpm store prune'
  echo '# brew cleanup -s'
  echo '# go clean -modcache'
  echo '# docker system prune -a --volumes     # WARNING: removes unused images/volumes'
  echo ''
  echo '### 4. Application caches ###'
  echo '# rm -rf ~/Library/Caches/*            # apps rebuild these; may log you out of some'
  echo '# rm -rf ~/Library/Logs/*'
  echo '# rm -rf ~/Library/Saved\ Application\ State/*'
  echo ''
  echo '### 5. Stale node_modules across old projects ###'
  echo '# find ~/code -name node_modules -type d -prune -mtime +90 -exec rm -rf {} +'
  echo '#   (adjust ~/code to wherever your projects live; -mtime +90 = untouched 90+ days)'
  echo ''
  echo '### 6. Review by hand before deleting ###'
  echo '# open ~/Downloads'
  echo '# open ~/Library/Application\ Support/MobileSync/Backup   # old iPhone backups'
  echo ''
} >"$CLEANUP"
chmod +x "$CLEANUP"

if [ "$JSON_MODE" -eq 1 ]; then
  printf '{"type":"done","reclaimable_bytes":%s,"blind_count":%s,"report":"%s","cleanup":"%s"}\n' \
    "$((TOTAL_SAFE_KB * 1024))" "$BLIND_COUNT" "$(jesc "$REPORT")" "$(jesc "$CLEANUP")"
fi

hdr "Done"
say "Full report:        $REPORT"
say "Cleanup suggestions: $CLEANUP  ${DIM}(all commands commented out)${R}"
say ""
say "Next: open the cleanup script, uncomment what you're comfortable with, run it."
