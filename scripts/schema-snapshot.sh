#!/usr/bin/env bash
# Write docs/schema/previous.sql: the schema the **previous release** shipped (HS-075, ADR-0047).
#
# A release step, not a story step. Run it against a database built by the release you are *leaving* — the
# simplest way is to start that release's binary once with an empty `--data-dir`, which migrates it — and commit
# the result. `tests/schema_previous.rs` then refuses any migration that drops or renames what the file lists.
#
#   scripts/schema-snapshot.sh <path to app.db> [output]
#
# python3 is used for `sqlite3`, which this machine has and the shell does not: `sqlite3` the CLI is not installed
# and nothing in the gate may depend on it (docs/build.md, prerequisites).
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
database="${1:-}"
target="${2:-$repo_root/docs/schema/previous.sql}"

if [ -z "$database" ] || [ ! -f "$database" ]; then
  printf 'usage: scripts/schema-snapshot.sh <path to app.db> [output]\n' >&2
  printf 'the path must be an app.db a release has already migrated\n' >&2
  exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
  printf 'schema-snapshot: python3 is needed for its sqlite3 module (docs/build.md, prerequisites)\n' >&2
  exit 2
fi

version="$(python3 - "$database" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
print(db.execute("SELECT coalesce(max(version), 0) FROM schema_version").fetchone()[0])
PY
)"
if [ "$version" = "0" ]; then
  printf 'schema-snapshot: %s has no schema_version rows, so it is not a migrated database\n' "$database" >&2
  exit 1
fi
artifact_version="$("$repo_root/dist/x86_64-unknown-linux-musl/holosmith" version --json 2>/dev/null |
  sed -n 's/.*"version":"\([^"]*\)".*/\1/p' || true)"

{
  printf '%s\n' \
    '-- The schema the **previous** release shipped, as a fact a test can compare against (HS-075, ADR-0047).' \
    '--' \
    '-- This file is NOT a migration: it does not live in `crates/holosmith/migrations` (that directory is embedded' \
    '-- in the binary and byte-grepped by scripts/check-static.sh), and nothing applies it. `tests/schema_previous.rs`' \
    '-- executes it into a throwaway database and asserts that every table and column it names still exists in this' \
    '-- build'"'"'s schema -- so a migration that drops or renames any of them fails the gate, and a migration that adds' \
    '-- is invisible to it.' \
    '--' \
    '-- **Refresh it at release time**, from the release you are leaving, with' \
    '-- `scripts/schema-snapshot.sh <data root>/app.db`. A contract migration (`NNNN_contract_*.sql`) changes this file' \
    '-- in the same commit: that diff is the record of what was given up, and of the release that gave it up.' \
    '--' \
    "-- Snapshot of the schema this build leaves behind: schema $version" \
    "-- (regenerate rather than edit; docs/build.md, \"The database and its migrations\")."
  printf '\n'
  if [ -n "$artifact_version" ]; then
    printf -- '-- Written by scripts/schema-snapshot.sh from a database of holosmith %s, schema %s.\n\n' \
      "$artifact_version" "$version"
  fi
  python3 - "$database" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
rows = db.execute(
    "SELECT sql FROM sqlite_master"
    " WHERE sql IS NOT NULL AND type IN ('table', 'index') AND name NOT LIKE 'sqlite_%'"
    " ORDER BY type DESC, name"
).fetchall()
for (sql,) in rows:
    print(sql.strip() + ";")
    print()
PY
} >"$target.tmp"

mv "$target.tmp" "$target"
printf 'wrote %s (%s lines, schema %s)\n' "$target" "$(wc -l <"$target")" "$version"
