# The tree fetches its own cross toolchain, from the recipe's own pin

`scripts/build.sh` needed `x86_64-linux-musl-gcc` (and, before ADR-0119, its aarch64 twin) because
`.cargo/config.toml` names it, and it found them by sourcing `~/.pi/toolchains/env.sh` when `command -v` missed. That
is a contract with one machine's layout: a file outside the tree, whose contents are a `PATH`, `RUSTUP_HOME` and
`CARGO_HOME`, and whose absence left a reader with a linker-not-found that reads like a code error. ADR-0118 already
had to say so out loud — "the one file it does source is the machine-local toolchain env" — and a script that cannot
say where a toolchain comes from is a script two machines have to configure before it works.

## D1 — Three places, in order, and nothing outside the tree

The cross linker a target needs is resolved in this order, and the first hit wins:

1. **`PATH`.** A session's container carries the cross linker already and has no network, so the fetch is never
   reached there. This is also the order that keeps a host's own toolchain authoritative, which is what the old
   sourcing was really for.
2. **`toolchains/<arch>-linux-musl-cross/bin`** in this tree. `.gitignore` holds `/toolchains/` beside `/target/` and
   `/dist/`: it is build output, fetched once and reused.
3. **The fetch**, into `toolchains/`, from the URL the recipe pins.

Nothing outside the tree is ever searched: no `~/.pi`, no `env.sh`, no `rustup`. A machine with a toolchain elsewhere
puts it on `PATH`, which is the same sentence `AGENTS.md` now gives.

## D2 — The URL and the digest are read out of the recipe, so the digest has one copy

`crates/holosmith/images/agent.Dockerfile` fetches the same tarball at image-build time and pins it by SHA-256 in the
same `RUN` (ADR-0111, ADR-0116). The script reads that `RUN` with `sed` — the range from the URL line to the
`sha256sum -c -` line, then the 64 hex characters out of the `echo "…"` it holds — keyed by the requested triple's
architecture, and the way `scripts/check-static.sh` reads `PLACEHOLDER_MARKER` out of `build_support/web_embed.rs`.

A literal digest in `scripts/build.sh` would be the second copy of a value only the recipe's author knows to move, and
a bump of the pin is already one edit that both the image and this tree follow. An empty read is a refusal naming the
recipe, not a fetch nobody checked; `crates/holosmith/tests/build_script.rs` scans the script for a 64-hex literal and
`tests/images.rs` counts the fetches per recipe, so the two halves cannot drift silently.

The fetch is `curl` — with the recipe's `--retry 4 --retry-delay 2 --retry-all-errors`, because musl.cc away is what
a retry is for — or `wget` when curl is not on `PATH`. The bytes are checked with `sha256sum -c` **before** anything is
extracted: a mismatch removes the tarball and refuses, so no later run can mistake a half-written archive for a
toolchain. What the script does not do is ADR-0116's certificate fallback: that decision is the *image* build's, whose
reader replays the warning to an operator, and a host fetch that cannot verify musl.cc's certificate refuses and says
so. A host that has the toolchain on `PATH` — every machine this gate runs on — never reaches either.

## D3 — One script, two kinds of build, and a flag for each thing it cannot know

`scripts/build.sh` grew the flags the removed `holosmith build` shape needed, and nothing else:

- `--profile dev|release` (default `release`). `dev` is cargo's dev profile, and the binary is placed at
  `target/<triple>/dev/holosmith` — through a `.tmp` and `mv -f`, as `dist/` is — because cargo names that profile's
  output directory `debug` and a path that says `debug` beside one that says `release` is the place a debug binary
  gets picked up by mistake. `dist/` is the layout a promotion reads, so `dev` never writes it.
- `--target <triple>...`, still repeatable, defaulting to the one release target (ADR-0119).
- `--no-check`, which skips `scripts/check-static.sh`, and is implied by `dev` — that script reads `dist/`.
- `--require-clean-tree`, which refuses a tag ending in `-dirty` **before** the compiler check and the web build, so a
  dirty tree costs a millisecond. The tag is the artifact's own claim about itself, and a promotion plants the binary
  under that tag. The gate's own build passes no flag and stays exempt: it may build from an uncommitted worktree.

The order is usage and target validation, the tag, the compiler, the linker, the web app, cargo, the artifact's
placement, then the check. The compiler check now asks `rustc` as well as `cargo`: the driver and the compiler are two
answers, and an artifact is the pin's only when both are.

## What is not measured

- **A real fetch from musl.cc.** A session has no network, so the gate's only measurement of the fetch path is
  `crates/holosmith/tests/build_script.rs` with a shim `curl` that writes the wrong bytes: the URL is asserted from the
  shim's argv, and the digest refusal from the sentence and the absent `toolchains/`. The happy path — a real tarball,
  extracted, and a build that uses it — is proven by the machine that has neither a linker on `PATH` nor a `toolchains/`
  directory, and no run here is one.
- **The certificate.** Whether musl.cc presents a valid chain today is a property of that host. This decision's fetch is
  the verified one, and a host that cannot verify it gets a refusal (`AGENTS.md`'s prerequisite bullet still says the
  toolchain is wanted on `PATH`).
- **`--require-clean-tree` on a real dirty tree.** The test sets `HOLOSMITH_GIT_TAG` instead of editing a file, because
  a test that dirties the worktree is a test that changes what the next test measures.

## Consequences

- A clone with no toolchain directory and no network can build once it is given `toolchains/`; a clone with neither
  fetches the pinned tarball and checks it.
- Nothing this repository's scripts do depends on `~/.pi/toolchains/env.sh` any more, and nothing sources it: `AGENTS.md`
  named rustc 1.99.1 on `PATH` as the host's own prerequisite at the time of this decision, and the compiler check
  is what refuses without it.
- `--target` and `--no-check` are still `scripts/build.sh`'s flags, and the tree's own callers pass neither: the gate
  runs the script bare in both its arms (`scripts/verify.sh`'s `build` phase, and `case_agent_offline_build`'s
  container), and only `crates/holosmith/tests/build_script.rs` passes either. The `holosmith-builder` entry script that
  passed both went with the verb (ADR-0121).
