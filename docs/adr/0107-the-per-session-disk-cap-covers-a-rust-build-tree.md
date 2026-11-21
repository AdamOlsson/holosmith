# The per-session disk cap is 20 GB, because a session's normal command set builds a 9.6 GB tree

`session_disk_soft_gb` has defaulted to spec item 14's **10** since ADR-0069, and that number was decided when a
session's build was the exceptional thing: the image had no compiler, so a Rust project's `target/` was a directory an
operator mounted a registry for, or built elsewhere. ADR-0105 put the toolchain in the image and ADR-0106 put the
lock's crates and the web app's packages there, so `cargo fmt`, `cargo clippy --all-targets`, `cargo test` and both
musl release builds are now commands a session runs with no flag, no mount and no network. They are the ordinary work
of a Rust project, and they are what fills the cap.

Measured on this tree, in a container shaped like a session's (`--network none`, the invoking uid, a session's
`--cpus 2 --memory 4096m`), with `du -sk` on the tree's `target/`:

| commands | `target/` |
|---|---|
| `cargo build --locked`, `cargo clippy --all-targets`, `cargo test --no-run` | **9.2 GB** |
| the same, then both musl release builds | **9.6 GB** |

Against a cap of 10 the second row leaves **0.4 GB**, and that is not headroom: it is less than the checkout the same
worktree holds outside `target/` — the source tree and an `npm ci`'s `node_modules/` — so the next configuration or
the next build crosses it. The measurement is of `target/` alone while the cap is over the whole worktree, so the
worktree total was never measured at all. `docs/config.md`'s row said the same thing in ADR-0105's words — **8.9 GB**
after one `cargo test --no-run`, "so a Rust project has little headroom and a second build's `target/` exceeds it" —
and the sentence was honest when a compile was an event. Offline builds make the same command set the routine one, so
the default has to cover a build tree rather than one compile of it.

The decision is that the default is **20**. Nothing else moves: not the `du`, not the tick, not the stop, not the
reason word, not the event, and not the key's `0` off switch.

## D1 — 20, not keeping 10 and accepting the stop

The alternative was to leave the number and let a session that tests and then builds be stopped with `disk_quota`.
Refused, because the cap's own purpose is the one that alternative spends: ADR-0069's watchdog exists so **one
session that hoards disk cannot starve the others**, and a cap whose headroom over the routine command set is smaller
than the checkout that produced it is not catching a hoarder — it is stopping the sessions that build, at a point in
their work that costs the most, with the crossing one configuration beyond the measurement rather than a hoard.

The recovery is the part that makes it unacceptable rather than merely annoying, once the crossing happens.
"Resumable after cleanup" means the operator deletes the thing that filled the disk, which is the build tree, so what
a stop at 10 costs a Rust project is a lost turn **and** a cold rebuild of a tree that had just been built. A cap
0.4 GB above the routine command set is one a session meets while it works rather than a rare event, which makes the
cap an off switch with an `error` event attached. 20 leaves 10.4 GB of headroom over the measured 9.6: enough for a
second configuration or a debug-and-release pair to coexist, and still small enough to catch a session that hoards,
because a checkout is megabytes and a build tree is the only thing in a worktree that reaches gigabytes.

## D2 — the measurement still counts everything under the worktree

Exempting `target/` from the `du` was the other way to make the old number fit, and it is refused for ADR-0069's
reason, which still holds: the cap is a watchdog over the **row's own worktree**, and everything under it counts —
`target/`, `node_modules/`, and a nested mount too — which is why that ADR's `du -sk` deliberately passes no
`--one-file-system`.

The exemption would also be backwards. A session's writes are overwhelmingly its build output; `src/` is megabytes
against a build tree's gigabytes. A cap that measured only what a session checked out would measure the one thing that
never grows, catch nothing, and read as a cap. Making it work would take an exclusion list — `du` has no POSIX option
for one, so it would be a name list in this module that every language a session builds would want a row in, and a
project whose build directory is spelled differently from the list would be measured and stopped again. Raising one
number is one edit; a list of names is a second source of truth about what a build tree is.

## D3 — 20 next to `min_free_gb`'s 20 and `max_live_sessions`' 3

The two disk keys are different guards and stay different, which is the fact a raised cap makes worth writing down.

`min_free_gb` (ADR-0068) is **one measurement of the whole filesystem**: `serve` refuses to boot below it, `doctor`
reports it, and the create and the resume answer `507 disk_floor`. `session_disk_soft_gb` is **per session**, measured
by `du` over that session's worktree, and it stops that session with reason `disk_quota`. The floor is asked before a
session exists; the cap is asked about a session that exists.

So the worst case on one disk is `max_live_sessions` × the cap: **3 × 20 = 60 GB** of worktrees by default, where it
used to be 30. The floor does not bound that sum and never did — it refuses to *start* when free space is below
20 GB, which says nothing about three sessions already running at 20 GB each — which is why the arithmetic an
operator needs is the product, not either number. `docs/config.md`'s first worked example now writes both: the cap at
its default 20, and `min_free_gb = 60` as the floor for a host that lets three Rust projects build at once. A disk
that cannot hold 60 GB is a host that lowers `max_live_sessions`, because lowering the cap brings D1 back for every
Rust session on it.

Nothing enforces the product in code. The floor and the cap are two independent checks whose sentences name their own
request (ADR-0068's D2), `doctor` reports the floor, and the third session's `disk_quota` event is what an operator
sees when the disk is the thing that ran out.

## D4 — a session over the cap is still stopped, and no behaviour changed

The raise is a config default, the golden file that pins it, and the documents that state it. `quota.rs` is untouched
except for its module doc: the same
`watchable` candidate set, the same five-minute `TICK`, the same `du -sk`, the same ladder, the same `stopped` row
with `disk_quota` and the same one `error` event. `0` is still the off switch.

No gate can fail for the number, and that is worth stating rather than discovering: a case's `docker run` carries no
quota, because the cap is a host-side `du` watchdog over a worktree rather than a container limit, and
`case_disk_quota` proves the stop **mechanism** with a stand-in `du` reporting 15 GB against a cap the case writes
itself — the spec's 10, since a case whose cap sat above its own stand-in would measure a session that never crossed
it. The number's own pin is elsewhere: `tests/golden/config-default.toml` is the rendered defaults byte for byte, and
`the_defaults_render_byte_identically_to_the_golden_file` compares `Config::default()` with it, so the default cannot
move without that file moving in the same commit.

## Consequences

- The default is 20 in `Config::default()` and in `tests/golden/config-default.toml`, and `docs/config.md`'s row for
  the key carries 20 with the measurement that decided it (9.2 GB after the build, clippy and `cargo test --no-run`,
  9.6 GB after both release builds) in place of the stale 8.9 GB sentence.
- `docs/config.md`'s first worked example changes shape: the cap is the default 20 rather than 5, the floor is 60
  rather than 10 because D3's product is what a three-session Rust host holds, and the egress allow-list loses
  `crates.io`, `index.crates.io`, `static.crates.io` and `registry.npmjs.org` — ADR-0106 put those sets in the image,
  so pairing a Rust machine's small cap with a registry allow-list would tell an operator that a build still needs
  the network. A project that adds a dependency adds the registries back, which `docs/egress.md` already says.
- `quota.rs`'s module doc keeps item 14's sentence **as a quote**, `(default 10)` and all, and states the shipped
  20 beside it: the quote is the spec's, the number the code reads is the config's, and rewriting a quote to agree
  with a later decision is how a quotation stops being one. GLOSSARY's **Session disk cap** entry carries the same
  two facts in its own words.
- **Amended by this ADR**, so neither reads as still true: ADR-0105's `case_agent_toolchain` consequence, whose
  8.9 GB "little headroom" measurement is superseded by the two above, and ADR-0106's D8, which deferred this raise
  wanting "a number measured on a project that builds" plus a statement about `min_free_gb` and several sessions on
  one disk — the table above is the number and D3 is the statement. ADR-0069's opening quote of the spec sentence is
  left alone for the reason the module doc's is.
- A machine that upgrades keeps whatever its file says: this is a default, so a config that wrote `10` still stops at
  10, and an operator whose Rust sessions were being stopped should raise the key rather than wait for a release.
