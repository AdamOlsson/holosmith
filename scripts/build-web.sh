#!/usr/bin/env bash
# Build the app shell into web/dist, which build.rs embeds in the binary (ADR-0007).
#
# Usage: scripts/build-web.sh
# Needs node and npm on PATH. Fetches nothing but the dependencies declared in
# web/package-lock.json (`npm ci`), and runs no package install script. scripts/build.sh
# calls this before cargo, and scripts/check-static.sh checks the artifact for the
# placeholder marker a build without web/dist carries.

set -euo pipefail

# Prints this script's header comment, whatever its line count.
usage() {
  sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root/web"

case "${1:-}" in
  '') ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    echo "build-web.sh: unknown argument: $1" >&2
    exit 2
    ;;
esac

for tool in node npm; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "build-web.sh: $tool not found on PATH." >&2
    echo "  The app shell needs node and npm; see docs/build.md." >&2
    echo "  A binary built without it serves a placeholder page (ADR-0007)." >&2
    exit 1
  fi
done

# Reinstall only when there are no modules or the lock file moved. `npm ci` deletes and
# reinstalls node_modules from the lock, so it is never the fast path.
if [ ! -d node_modules ] || [ package-lock.json -nt node_modules ]; then
  echo "installing web dependencies (npm ci)"
  npm ci --no-audit --no-fund
fi

# `npm test` runs the transcript's rules before the build: they are folded from synthetic frames,
# not from a browser (ADR-0018), and a failing rule should name the rule rather than surface as a
# bundler error halfway through the app.
npm test

npm run build

if [ ! -f dist/index.html ]; then
  echo "build-web.sh: the build left no web/dist/index.html" >&2
  exit 1
fi

files="$(find dist -type f | wc -l)"
printf 'web: %s files, %s KiB in web/dist\n' "$files" "$(( $(du -sk dist | cut -f1) ))"
