# A deploy plants the binary it is running, so the path it was started from stops mattering

`server deploy`'s step 5 rendered the unit's `ExecStart` from `std::env::current_exe()` whenever the release tree had
no live release. That is a fact about the *download*, not about the machine: `deploy --ssh` runs a copy under
`~/.cache/holosmith` (ADR-0078), a local build runs `dist/<triple>/holosmith`, and the unit named whichever of those
was started. `docs/requirements.md` had to carry the consequence as advice — "keep the binary somewhere permanent" —
and advice in a requirements page is a defect in the product: the next `cargo clean` or cache sweep was a unit that no
longer boots, and nothing said so until the machine was restarted.

The tree the layout already exists for fixes it (ADR-0082 D4, ADR-0084): `releases/<tag>/` plus the `current` symlink
is what systemd is supposed to run, and a promotion already seeds it out of the binary the unit names
(`promote::previous_release`). So step 5 does the same thing one step earlier — it **plants the binary this run is**
— and both `server deploy`/`hub deploy` and `install` get it, because an install is the deploy plus the start
(ADR-0091).

## D1 — no live release: the running binary becomes a release, and the unit names `current/holosmith`

Three facts are read from the process itself, and none of them spawns a child:

- **the bytes** are a read of `std::env::current_exe()`;
- **the tag** is `version::VersionInfo::current().git_tag`, which the build script put in the binary;
- **the digest** is `images::sha256`/`hex` over those bytes — the two functions `promote` seeds with, so a release's
  `.sha256` has one spelling whichever verb wrote it.

`Release::write(tag, &bytes, &digest)` then writes the two files and `Release::switch(tag)` points `current` at the
directory, relatively, by the staged-link-and-rename ADR-0082 D4 already fixed. The unit is rendered through
`root.current().join(release::BINARY)` — the same path `promote::write_unit` renders — so a fresh machine and a promoted
one are one shape and a promotion has nothing left to correct.

**`promote::ask_version`'s spawn is deliberately not copied.** A promotion runs `<candidate> version --json` because it
is reading a *foreign* file it has not installed; a deploy asking its own binary the same question would be a second
process to learn a value it is holding.

## D2 — the tree, not the path, because the path is the download

A unit naming the file the command was started from is a unit naming a temporary. The three ways that bites are all
recorded in the product already: `deploy --ssh` copies to `~/.cache/holosmith` (ADR-0078), `scripts/build.sh` writes
`dist/<triple>/`, and `holosmith build --out` writes wherever the operator pointed it. Every one of them is a directory
something else may clean up, and none of them is inside the install tree the unit's own `--root` describes.

The tree is also the only shape a **rollback** can work in: a `current` that a deploy pointed at a download has no
`releases/` behind it, so the first promotion would have planted a release from a cache path and the second would have
had nothing to go back to. Planting on the first deploy makes the rollback target exist from the beginning
(ADR-0083).

## D3 — a live release is never reseeded

The read is `Release::live` first, and when it resolves the tree is left **byte-identical**: the unit is rendered from
the live path and a second deploy says `satisfied`. That is the property that keeps a deploy from undoing a promotion —
ADR-0082 D5's rule, and `tests/deploy.rs`'s `a_release_tree_makes_the_unit_run_through_current` has asserted it since
the promotion landed.

The one thing a deploy does not do is replace a live release, and the `satisfied` line says so in one clause when the
running binary's digest differs from the live release's: `holosmith upgrade` is the command that promotes. Both
renderings print that clause — the terse one on the indented line under the mark (ADR-0085) — so the operator who ran
`server deploy` after a `cargo build` is told, on the line they actually read, that the unit kept naming the release
that was live and what would replace it. A deploy that silently overwrote the live release would be a promotion
without the review gate, the smoke run, the drain or the backup (ADR-0082 D1, D6–D9), which is exactly what the flag
exists to prevent.

## D4 — an unusable tree falls back to the running path, and never refuses

Four things can stop a seed, and each costs the seed and not the install:

- **the tag is not one path segment** (`release::tag_problem`): a value with a space, a `/` or a leading `-` cannot
  name a directory. A build outside git is **not** one of these — `git describe` has no answer there, so `build.rs`
  writes the literal `unknown`, and `unknown` is a legal release name: such a binary seeds `releases/unknown/`, points
  `current` at it, and the *second*, different no-git build is the one refused, by `placement` (the third bullet).
  ADR-0082 D3 refuses a *promotion* over an unusable tag because a promotion has an operator and a candidate in hand; a
  deploy has neither and is not a promotion.
- **the running path or its bytes cannot be read**: `current_exe` failing is already today's failure — there is no path
  to name — but an unreadable *copy* is not, and the unit names the running path as it always did.
- **`Release::placement` refuses**: `releases/<tag>/holosmith` already holds a *different* binary under this tag. The
  tree's rule is one binary per tag (ADR-0082 D4), and the refusal is right; a deploy that overwrote it would be the
  ambiguity a rollback cannot resolve.
- **one of the tree's two writes fails** (`write`, `switch`): the read answered, and the apply could not carry it out —
  a `current` that is a directory or a regular file instead of a symlink is the state ADR-0084's table names, and
  `rename(2)` onto a directory fails.

The first three stop the **read**, so the unit is rendered from the running path before anything is written. The fourth
can only be discovered by writing, and it falls back once: the release plan is emptied, the unit is rendered again from
`std::env::current_exe()` with the same `no release was planted` clause carrying the write's own sentence, and only
that unit is applied. A machine whose `current` is a directory therefore ends exactly where it ended before this
change, with a unit naming the binary that is running — an install must never be worse than it was before it seeded
anything, because a machine with a unit that names a cache path boots and a machine with no unit does not.

**A `current` this tree does not understand is taken over rather than refused, and the line says what it replaced.**
`Release::tag_of` answers `None` for a link naming something that is not `releases/<tag>`, so the deploy has no live
release, seeds one and re-points the link; the clause names the target it replaced, because a link that moved with no
word is the one thing a `(2 actions)` cannot show. `promote::previous_release` refuses that state instead ("point the
link at a release, or remove it and let this command seed one"), and the difference is what each verb is for: a deploy
puts a machine back into a state it can boot from, a promotion needs a release to go back to before it switches
anything, and a deploy that refused would leave the quick start's own command needing a hand repair first.

## D5 — an unusable tree is not a silent one, and a plan says what it would plant

`--dry-run` writes nothing, so the seed's read has to answer the same question the write would: `Release::placement`
is `write`'s own three arms asked without writing (`Fresh`, `Current`, or the refusal), and it is what the plan and the
apply both go through. The step's plan therefore names the release — `releases/<tag>/holosmith` and its `.sha256` —
`current -> releases/<tag>`, and the unit file, in the plan vocabulary ADR-0085 already fixed for every other action.

The two release actions are `Action` values like the rest and are applied by `crate::release` rather than by an
`install` child: a release's binary is staged beside its name and renamed into place, and a switch is one syscall, which
is a rule that module owns and a plan line cannot honestly spell as an argv. A release the tree already holds —
`Written::Current` **and** a digest file that reads back as this digest — is no action at all: `write` would rewrite one
file with the content it has, and a plan's count is what a reader checks the run against.

## What this does not change, and what it keeps

- **A promotion still owns the candidate.** `upgrade` writes `releases/<candidate>/`, switches and restarts; the deploy
  never touches a release that is live, and never writes a release whose tag is not its own.
- **A rollback still owns the switch back** (ADR-0083), and still writes back the link target it read, verbatim.
- **`doctor`'s `release` finding is unchanged** (ADR-0084 D4): it still asks `Release::live` and `release::unit_executable`
  together. What changed is how often it can fail — a freshly deployed machine resolves, where before it resolved only
  after a promotion.
- **Step 1's `LINK_NOT_LIVE` exemption stays, with a truer clause.** A broken `current` is still a state step 5 repairs,
  and now the repair is the seed: the running binary becomes a release and the link is re-pointed at it, rather than a
  unit written on a plain path over a tree that stays broken.
- **`promote::previous_release`'s first-release arm stays.** A machine deployed by a release that did not seed — an
  older one, or one whose seed fell back — still has no `current`, and the promotion still plants the binary its unit
  names. Seeds do not accumulate: the tree's rule is one directory per tag.
- **A stale build tag still occupies its release name.** `build.rs` opts out of cargo's default rerun, so a tree that
  changed without a fresh `git describe` keeps the previous one. A deploy of such a binary plants
  `releases/<stale-tag>/` with bytes that tag does not name, and `Release::placement` then refuses the *honest* build of
  that tag (ADR-0082 D4's one-binary-per-tag rule) and falls back. Promotion already had that trap; the deploy is a
  second, cheaper way to trip it.

## How it is measured

- **`tests/deploy.rs`**: `a_fresh_tree_is_seeded_with_the_binary_this_run_is` — a tree with no `releases/` gets
  `releases/<this binary's tag>/holosmith` with the running bytes and their digest, a **relative** `current` link, and a
  unit naming `current/holosmith`; the second run is `satisfied` and leaves the planted binary's mtime, the unit's bytes
  and the release count untouched. Its last phase is D5's count: with `current` removed and the release intact, the step
  plans **one** action — the switch, not a release write that would rewrite a file with the content it has — and then
  D4's takeover: a `current` the tree cannot explain is re-pointed, with the target it replaced named in the line.
  `a_release_tree_makes_the_unit_run_through_current`'s broken-`current` arm is the
  repair: the tree is re-pointed at a release holding the running binary, and the unit follows the link.
- **`tests/deploy.rs`, the three arms D3 and D4 turn on**:
  `a_release_holding_another_binary_falls_back_to_this_run` is the `placement` refusal — the step is `changed` with
  **one** action, the unit's `ExecStart` and `ExecStartPre` name `std::env::current_exe()`, the refused release keeps
  its bytes, no `current` is made, and the sentence carries `no release was planted` and the refusal's own reason.
  `a_live_release_that_is_not_this_run_is_named_on_the_line` is D3's clause — a live release holding different bytes
  leaves a second deploy `satisfied` with this run's digest and the live one's in the sentence, `holosmith upgrade`
  named, and the same clause present in `Report::render`'s default rendering.
  `a_current_that_is_a_directory_still_leaves_a_unit` is D4's fourth arm, the one no read can find: `current` as a
  plain directory stops the switch, and the step is `changed` with the unit alone — naming
  `std::env::current_exe()`, carrying the switch's own reason, and leaving the operator's entry where it was.
- **`tests/release.rs`**: `placement` answers `write`'s three arms, and asking does not create the tree.
- **`scripts/verify.sh`**: `case_install` asserts the shipped artifact's unit names `<tree>/current/holosmith`, that
  `current` is the relative link to `releases/<the artifact's own git tag>`, and that the planted binary and its digest
  are the artifact's own; `case_install_terse`'s byte-for-byte report carries the seed's actions and the clause a
  `satisfied` preflight line earns about the filesystem it measured (ADR-0085); `case_server_service`
  asserts the `ExecStartPre` line through `current`. `tests/install.rs`, `tests/upgrade.rs` and `tests/doctor.rs` needed
  no change: none of them asserts what a fresh deploy's unit names, and the ones that render a unit do it through
  `deploy::unit_text`, which did not move.
