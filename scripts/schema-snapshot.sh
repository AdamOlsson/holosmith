#!/usr/bin/env bash
# Write docs/schema/previous.sql: the schema the **previous release** shipped (HS-075, ADR-0047).
#
# A release step, not a story step. Run it against a database the *previous* release migrated — the simplest way is
# to start that release's binary once with an empty `--data-dir` — and commit the result.
# `tests/schema_previous.rs` then refuses any migration that drops or renames what the file lists.
#
#   scripts/schema-snapshot.sh <path to app.db> [output]
#
# The first file of a project's life is necessarily this build's own schema: nothing has been released before it,
# and the header below says which release the dump came from so a later reader is never guessing.
#
# python3 is used for its `sqlite3` module: the `sqlite3` CLI is not installed here and nothing in the gate may
# depend on it (docs/build.md, prerequisites). The connection is **read-only**, because this step reads a release
# and has no business changing one.
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

# Read-only, and a database without the table is "not migrated" rather than a traceback: `set -e` turns a Python
# exception into an exit code with no sentence, and this script's whole job is to be runnable by hand at 3am.
version="$(python3 - "$database" <<'SQL'
import sqlite3, sys
uri = f"file:{sys.argv[1]}?mode=ro"
try:
    db = sqlite3.connect(uri, uri=True)
    print(db.execute("SELECT coalesce(max(version), 0) FROM schema_version").fetchone()[0])
except sqlite3.Error:
    print(0)
SQL
)"
if [ "$version" = "0" ]; then
  printf 'schema-snapshot: %s has no applied migrations, so it is not a migrated database\n' "$database" >&2
  exit 1
fi

# The binary's own version, for the provenance line. Derived from `uname -m` the way `verify.sh` does it, because
# this repository ships two targets and a hard-coded triple would silently drop the line on the other one.
case "$(uname -m)" in
  x86_64) host_target=x86_64-unknown-linux-musl ;;
  aarch64) host_target=aarch64-unknown-linux-musl ;;
  *) host_target="" ;;
esac
artifact_version=""
if [ -n "$host_target" ] && [ -x "$repo_root/dist/$host_target/holosmith" ]; then
  artifact_version="$("$repo_root/dist/$host_target/holosmith" version --json 2>/dev/null |
    sed -n 's/.*"version":"\([^"]*\)".*/\1/p')"
fi

tmp="$target.tmp"
trap 'rm -f "$tmp"' EXIT

{
  printf '%s\n' \
    '-- The schema the **previous** release shipped, as a fact a test can compare against (HS-075, ADR-0047).' \
    '--' \
    '-- This file is NOT a migration: it does not live in `crates/holosmith/migrations` (that directory is embedded' \
    '-- in the binary and byte-grepped by scripts/check-static.sh), and nothing applies it. `tests/schema_previous.rs`' \
    '-- runs it into a throwaway database and asserts that every table and column it names still exists in this' \
    '-- build'"'"'s schema -- so a migration that drops or renames any of them fails the gate. A migration that *adds*' \
    '-- is invisible to it, because adding is the rule.' \
    '--' \
    '-- **Refresh it at release time**, from the release you are leaving, with' \
    '-- `scripts/schema-snapshot.sh <data root>/app.db`, and commit it with the change that needed it. A contract' \
    '-- migration (`NNNN_contract_*.sql`) changes this file in the same commit: that diff is the record of what was' \
    '-- given up, and of the release that gave it up.' \
    '--' \
    '-- The very first file of a project'"'"'s life is that build'"'"'s own schema, because there is no earlier release to' \
    '-- dump: that is this file'"'"'s bootstrap case, and the provenance line below is what says which release it came' \
    '-- from.'
  printf '\n'
  if [ -n "$artifact_version" ]; then
    printf -- '-- Snapshot from a database of holosmith %s, schema %s (scripts/schema-snapshot.sh).\n\n' \
      "$artifact_version" "$version"
  else
    printf -- '-- Snapshot from a database at schema %s (scripts/schema-snapshot.sh).\n\n' "$version"
  fi
  python3 - "$database" <<'SQL'
import sqlite3, sys
db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
rows = db.execute(
    "SELECT sql FROM sqlite_master"
    " WHERE sql IS NOT NULL AND type IN ('table', 'index') AND name NOT LIKE 'sqlite_%'"
    " ORDER BY type DESC, name"
).fetchall()
for (sql,) in rows:
    print(sql.strip() + ";")
    print()
SQL
} >"$tmp"

mv "$tmp" "$target"
trap - EXIT
printf 'wrote %s (%s lines, schema %s)\n' "$target" "$(wc -l <"$target")" "$version"
