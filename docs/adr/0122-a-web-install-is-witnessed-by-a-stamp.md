# A web install is witnessed by a stamp, not by a directory that exists

A gate run stopped at the `build` phase with this, on a checkout where no file had a wrong mode:

```text
> tsc --noEmit && vite build && vite build --config vite.sw.config.ts

sh: 1: tsc: Permission denied
FAIL build/failed
     scripts/build.sh exited non-zero
```

Nothing was unreadable. `web/node_modules` held only `.bin/` — the remains of an interrupted `npm ci` — so `.bin/tsc`
was a symlink to `../typescript/bin/tsc` whose target was gone, and `sh` reports a failed `exec` on a dangling symlink
as `Permission denied` (bash says `No such file or directory`). The recovery is one command, `npm ci`, and the guard
that knew how to run it was the thing that declined.

## D1 — The guard asks whether an install *finished*, and the stamp says so

`scripts/build-web.sh`'s guard was `[ ! -d node_modules ] || [ package-lock.json -nt node_modules ]`. A directory that
exists and is incomplete satisfies both tests, so the reinstall never ran; `npm test` then passed anyway, because
node strips the TypeScript itself and the app's rules do not need the packages — and the failure surfaced one step
later, naming a permission instead of a missing package.

The guard is now a **stamp**, `node_modules/.holosmith-install`, written after `npm ci` returns 0, and the test is
`[ ! -f "$install_stamp" ] || [ package-lock.json -nt "$install_stamp" ]`.

**Inside `node_modules` is the whole argument.** `npm ci` deletes and recreates that directory, so the stamp cannot
survive an install it did not witness: an interrupted one takes the stamp with it, and only a `npm ci` that returned 0
can put one back. That ordering is what makes the stamp mean "a completed install of this lock" rather than "an
install happened once". The `-nt` test is the existing witness of a moved lock, kept unchanged and pointed at the file
that now answers the other half.

**What it costs.** A tree that installed before the stamp existed pays one `npm ci` — and `set -e` means a failed
install writes no stamp, so the same tree pays it again rather than being trusted. That is the direction to err in:
`npm ci` is a reinstall, never a mystery, and every alternative that could be wrong in the other direction is one the
operator has to debug with a `Permission denied` that names nothing.

**Rejected: a list of the build's own binaries** (`tsc`, `vite`, anything under `node_modules/.bin`). It would put a
third copy of `package.json`'s scripts in the tree, and every new script the app's build gains would be a second place
to remember. It also answers a narrower question than the guard needs: a tree missing one package the build does not
call today would pass it and fail later.

**Rejected: `node_modules/.package-lock.json`,** which npm writes at the end of an install and which would need no file
of ours at all. It is npm's own record in npm's own shape; a guard that read it would be depending on a package
manager's internal state and would break, silently, on the release that reorganises it. The tree writes a stamp it
names and can explain instead.

## What is measured, and what is not

`tests/build_script.rs` runs `scripts/build-web.sh` three times in a fixture tree, with an `npm` shim that records its
argv and agrees with everything: a `node_modules` holding only `.bin/tsc` as a dangling symlink is reinstalled (the log
has `ci`), one carrying a stamp newer than the lock is not (the log has no `ci`, and the run reaches `run build`), and
one carrying a stamp older than the lock is reinstalled too — which is the run that fails if the `-nt` test goes.
The mtime the test needs a side of is moved a minute back rather than left to the clock, because two files written in
the same instant are not an ordering any filesystem promises.

What the test does **not** pin is the message: `tsc: Permission denied` is `sh`'s sentence about a dangling symlink, not
this tree's, and asserting it would be pinning a shell's error text. The decision is the fact worth holding.
