# One release target, and no ARM64 artifact until a second one is asked for

The tree built `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, and the second of them was carried in six
places at once: a musl.cc tarball `RUN` in `crates/holosmith/images/agent.Dockerfile` (323 MB of a 3.54 GB image), a
second `rustup target add`, the `[target.aarch64-unknown-linux-musl]` stanza in `.cargo/config.toml`,
`scripts/build.sh`'s default target list, `scripts/check-static.sh`'s emulator fork, and `qemu-aarch64-static` as a
machine-local prerequisite. No machine in this gate has that emulator, so the second artifact's execution was a `SKIP`
on every run: a per-run line that measured nothing, and a build that spent five minutes producing an artifact nothing
here could run.

Nothing asked for it. A release is promoted by `upgrade`/`rollback` on one machine at a time, `server deploy` hands a
machine the binary it is running, and no story, config key or deploy step names an architecture the host does not
already have. So the tree builds what it can prove.

## D1 — One supported triple, spelled once

`scripts/build.sh` holds the set — `x86_64-unknown-linux-musl` — as one array, and every `--target` value is checked
against it **before any toolchain work**: a triple outside it exits 2 with one sentence naming the triple and saying it
is not a supported release target, rather than failing a minute later as a missing linker that reads like a code error.
`--target` stays repeatable and stays a flag, because that check is the seam a second architecture returns through; a
repeated triple is deduplicated rather than built twice, since the list is a set.

The check-static fork goes with it: every target is the host's own, so `label` is `<triple> native`, there is no
`runner` array, no `HOLOSMITH_QEMU_AARCH64`/`HOLOSMITH_QEMU_X86_64`, and no `SKIP <target> execution`. The empty-root
image is `FROM scratch` plus one copied file, which is now the whole of what it always was.

## D2 — The builder image keeps both, until `holosmith build` goes

`crates/holosmith/images/builder.Dockerfile`, `builder-entry.sh`, `crates/holosmith/src/builder.rs` and the `build`
verb are untouched, and the builder therefore still carries both cross toolchains and still passes `--target "$target"`
to `scripts/build.sh`. That means its aarch64 arm now fails **inside the container** on this decision's own sentence,
which is the honest place for it: the verb's target list is the verb's removal to make, and this decision is about what
the tree ships. A machine that runs `holosmith build --target aarch64-unknown-linux-musl` today gets the container's
refusal rather than an artifact.

## D3 — The boundary, on the record rather than discovered at deploy time

**No ARM64 release artifact ships.** A machine whose `uname -m` is not `x86_64` can be neither built on nor deployed to,
and it is told so with one sentence before it spends anything. `scripts/build.sh` is the first: it reads `uname -m`
before the compiler check and before the web build, and refuses with the machine's own answer, this tree's one target,
and the fact that the artifact could not be run here until the supported set carries that machine's target again. The
ending it replaces is a full cross-build whose last step is `scripts/check-static.sh` running the artifact it just made
and dying on `cannot execute binary file` — minutes spent to learn what one word said first. Its fixture measures that
refusal with a shim `uname`, no network and no daemon. `scripts/verify.sh`'s serve phase is the second: its `*)` arm
refuses a host it has no shipped artifact for in one sentence naming the one target this build makes, so a gate run on
such a machine stops there rather than naming a build script that cannot produce the artifact. `serve`'s artifact is
`dist/x86_64-unknown-linux-musl/holosmith`. Deploying to an ARM64 machine is not planned while this decision stands;
returning aarch64 to the set is D1's flag plus the recipe's `RUN` and the config stanza, in that order.

**`remote.rs` and `server deploy --ssh` are unchanged, and they keep naming both machines.** They judge the bytes they
are handed: `Machine::of_elf` reads `e_machine` from the ELF header and `Machine::parse` reads `uname -m`, and both
still know `aarch64`, because a hand-built aarch64 artifact is still something `--binary` may be given and a target
that answers `arm64` is still a target this binary can describe. Only the doc comment claiming what *this build*
produces moved.

That claim is still measured. `scripts/verify.sh`'s `case_deploy_ssh` used to take its "another machine" binary from
the second `dist/` artifact; it now builds one by copying this artifact and rewriting its ELF header's `e_machine` —
two bytes at offset 18, 0x3e to 0xb7 — which is exactly the file `--binary` is documented to describe, since
`elf_machine` reads the first twenty bytes and nothing else. So the refusal, the resume command and "nothing was
copied" stay live assertions rather than a permanent `SKIP`.

## What is not measured

- **A real aarch64 host.** No case can run one here, which is why the boundary above is written down rather than
  asserted of a real host. What the gate does measure is `scripts/build.sh`'s refusal under a shim `uname`, the refusal
  a host with no shipped artifact gets, and that the deploy's aarch64 path still describes a file it is handed.
- **The image's saved bytes.** The layer cache makes a second tarball's absence invisible to a warm build; the recipe's
  text and `tests/images.rs`'s fetch count (one in the agent recipe, two in the builder's) are what say it is gone.
- **`holosmith build --target aarch64…`'s new failure.** `tests/build.rs` drives the verb through a `docker` stand-in
  that never runs `scripts/build.sh`, so the refusal inside the container is not that test's to measure. What is
  measured is the script's own refusal, by `crates/holosmith/tests/build_script.rs`, in a copied tree with no network
  and no daemon.

## Consequences

- One artifact per build, one tarball in the agent image, one linker stanza, one build loop with two profiles, and no
  per-run `SKIP` naming an emulator.
- `scripts/build.sh --target aarch64-unknown-linux-musl` exits 2 and fetches nothing, which is a fact
  `crates/holosmith/tests/build_script.rs` measures with no network and no daemon.
- "no script in this repo downloads anything" left `AGENTS.md` with the `qemu-aarch64-static` bullet it belonged to.
  That sentence is no longer true of `scripts/build.sh`, which fetches its own cross toolchain into `toolchains/`
  (ADR-0120), so the claim going is the accurate half of this decision rather than a casualty of it.
- `.cargo/config.toml` names one target, and its warning about `RUSTFLAGS` stays: `relocation-model=static` is per
  target and the trap is one container out.
