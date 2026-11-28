# The two cases that built this tree in its own image are deleted, and what each proved is named

`case_agent_toolchain` and `case_agent_offline_build` are deleted whole from `scripts/verify.sh`, and no case replaces what
they covered. ADR-0128 replaced their *subject* — a build in a session's shape, `case_session_build` — and closed by
handing away the consequence: "What happens to that coverage when they are deleted is **not this decision's**: the
decisions that delete them own it, and this record states the gap rather than closing it." This is the deleting
decision, and this file is the statement ADR-0128 asked for.

## The decision, in a line

Both cases go, no case is added, and every claim they held is named here — the ones something else measures and the ones
nothing does.

## What each case proved

`case_agent_toolchain` compiled **this repository's tree in this repository's image**: `hs-holosmith:<digest>` over
`docker/Dockerfile`, the image a session here runs, in a session's shape — `--network none`, the invoking uid,
`--cpus 2 --memory 4096m`, the tracked tree copied with `git ls-files` and mounted at `/src` — and asked the image for
`cargo --version` and `rustc --version` against `pins/rust.txt`'s version and for `x86_64-linux-musl-gcc --version`,
compared `/opt/cargo-vendor/.holosmith-cargo-lock` with the copy's `Cargo.lock`, ran the tracked `.cargo/config.toml`'s
shadowing rule, and then ran the five commands a worker runs: `cargo build --locked`, `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`, and `npm ci`. `--network none` was the fact: the crates and
the npm packages had to be the image's own, and the case passed no `--offline` and mounted nothing over `CARGO_HOME`.
That is 552 s of the gate's run.

`case_agent_offline_build` ran the **release build** in the same shape: `scripts/build-web.sh` first — its own `npm ci`,
the app's own rules, the two vite builds — then `scripts/build.sh`, `cargo build --release --locked` with
`build.sh`'s last step, `scripts/check-static.sh`, still inside it; and then `file -b` on `dist/<triple>/holosmith`
**inside the image**, asserting `statically linked`. That is the fetch-nothing proof: the release artifact built with no
network to fetch anything from. That is 370 s.

## Why they went

This tree's own build is **this project's** claim and not the product's. A project owns its recipe and the toolchain that
recipe installs (ADR-0123); this repository is a project like any other, and its image is the one `docker/Dockerfile`
builds from the repository root (ADR-0126); a vendored crate set is the project's own check, because Holosmith reads no
project's lock and no project's crate set (ADR-0127). A gate case that compiled this tree in that image was therefore a
project measuring itself inside the product's gate, and it was the gate's most expensive kind of case — a full compile of
this tree inside a container, twice.

The half that is the product's is the **shape** a session's build runs in, and ADR-0128 gave that a case of its own
(`case_session_build`, about 3 s). The half that is this project's is claimed on the host already: the gate's `build`
phase runs `scripts/build.sh` over the release target, whose last step `scripts/check-static.sh` asserts the artifact is
statically linked. So "this tree builds, and the artifact it ships is static" is measured on every gate run with no
container. What the deleted pair added over that was the **image**, and the next section is what that was worth.

## What owns each claim now

- **The tree compiled in this project's image.** `scripts/verify-in-container.sh` (ADR-0109) runs the whole gate in
  `hs-holosmith:<digest>`, as the invoking uid. It is the one thing left that puts this tree's source inside this
  project's image and builds it there.
- **The crate-set marker's two halves.** `scripts/check-crate-set.sh` is the reader of the marker's *content*: it
  compares the digest with this tree's `Cargo.lock` and answers 3 with one sentence when they differ, and on a host it
  finds no marker and therefore compares nothing. `crates/holosmith/tests/images.rs` is the assertion that the recipe
  *writes* it — that the vendor instruction contains the marker's path beside the crates it wrote the digest for.
- **The session shape** a build runs in — the invoking uid, a cache that uid reads and cannot write, no network, one fast
  sentence for a dependency the image does not hold. `case_session_build`, whose subject is a fixture project and whose
  cache is recorded npm bytes (ADR-0128).
- **The release artifact's build and its static linkage**, on the host, in the gate's `build` phase
  (`scripts/build.sh`, `scripts/check-static.sh`).

## What owns nothing

- **The network's absence, for this tree's own build.** `scripts/verify-in-container.sh` runs on **the host's network**,
  because `--network none` cannot reach the four sites in that gate that dial a container's own address — so the gate in
  the image is not a network-less run, and no case measures that this tree builds with nothing to reach. What stands in
  its place is the image's own configuration, `[net] offline = true` at `/.cargo/config.toml` and
  `npm_config_offline=true` in its environment, which is a setting and not a measurement of a container that has nothing
  to reach. The two deleted cases were the only ones that ran this tree's build in a container that could reach nothing.
- **`cargo fmt --check`, `cargo clippy --all-targets` and `cargo test` in that image.** They are the three commands a
  worker runs on the host, and they belong to a worker's turn rather than to the gate; nothing runs them in the image a
  session here builds in.
- **The compiler, its version and the cross linker, as the image holds them.** `pins/rust.txt` is what the recipe reads
  and `scripts/build.sh` refuses a host compiler that is not its version, so a recipe that installed another one fails a
  build rather than a case. `case_server_images` measures what a built image answers — `pi`'s version, `git`, the two
  packages the gate's own container needs, and the image's uid — and nothing in it asks the image for `cargo`.

Each of those is stated rather than closed: this record's job is to say where the loss is, not to buy it back with a case
whose subject is this repository.

## The two facts about `.cargo/config.toml` that stayed

The deleted case read two things about the tracked `.cargo/config.toml`, and both of them are now
`crates/holosmith/tests/images.rs`'s:

- **The file's own bytes carry no table header but its `[target…]` ones and no dotted top-level `source…` key**, so it
  shadows none of the source replacement the recipe writes at `/.cargo/config.toml`. The test is
  `the_tracked_cargo_config_shadows_none_of_the_images_source_replacement`, and it reads the tracked file and asserts the
  list of shadowing lines is empty.
- **The rule catches both TOML spellings of the table**, so a rule that answered "nothing" to everything fails there
  rather than in a build nobody runs locally. The test is
  `a_source_table_or_a_dotted_source_key_is_what_the_shadowing_rule_catches`, an exact-equality negative sample over a
  `[source.crates-io]` header and a `source.crates-io.replace-with = "vendored-sources"` line.

The reason the path matters is worth keeping where the test is: cargo reads a `.cargo/config.toml` from the working
directory upward, so a session's worktree at `/work` gives it `/work/.cargo/config.toml`, nearer than the image's own at
the filesystem root — a `[source…]` table there shadows the replacement, and the symptom is an empty vendor directory
rather than a config conflict (ADR-0106).

Both are **the tree file's, read with no container**: `project_root().join(".cargo/config.toml")` is the file the recipe
never writes and the file the deleted case's `git ls-files` copy carried, so a dirty worktree makes these tests stricter
than the copy was, an absent file is a failure rather than a pass, and a daemon-free test is where a claim about a
tracked file belongs.

## What supersedes what

**Nothing here supersedes ADR-0128.** This record is the one ADR-0128 asked the deleting decision to write, and it keeps
that record's two load-bearing points whole: the replacement case's subject image is a fixture project that carries node
and no compiler, and its cache is recorded bytes rather than a case-time fetch. Nothing in its D2, D3, D4 or D5 is
reopened, and `case_session_build` keeps the shape D4 lists.

ADR-0127 D2 counts the marker's path as a literal with **three** readers: that script, `scripts/verify.sh`'s
`case_agent_toolchain` arm, and `crates/holosmith/tests/images.rs`. One of the three was this deletion, so the count is
two, and `scripts/check-crate-set.sh`'s header now says what each of the two does — one reader of the marker's content,
one test that the recipe writes it. That is a count and not a decision, which is why ADR-0127 itself is left as it
stands.

`case_session_build`'s own comment in `scripts/verify.sh` is where this file is named for a case's reader, and it says
the same thing in the place a reader deciding whether to widen that case will look.

## What is not measured

- **This record's own numbers.** 552 s and 370 s are the costs ADR-0128 D5 measured for these two cases, not a
  re-measurement of a run made for this decision.
- **Whether the gap above ever bites.** Nothing fails today when a recipe change breaks a bare `cargo build --locked` in
  this project's image under no network, because nothing runs it. The measurement that would catch it is a case this
  record does not add.
- **A project's own equivalent.** Another project that wants this measured in its own gate has ADR-0128's pattern and
  this record's list of what to keep; nothing here generalises it into the product.

## Consequences

- The gate loses two of its three most expensive cases, and `serve_cases` reads `case_base_image_lazy`,
  `case_session_build`, `case_smoke`. Their scratch trees, cost notes, `$WORK` output and
  the assertion that read a container run's `SKIP` lines go with them; `scripts/gate-cleanup.sh` sweeps `verify-*-*` by
  shape, so nothing had to be taught about the loss.
- "This tree compiles in this project's image" is a claim the whole gate in that image (`scripts/verify-in-container.sh`)
  and the host `build` phase answer, and the no-network half of it is a claim nothing answers — the gap this record
  states.
- The tracked `.cargo/config.toml`'s non-shadowing is a daemon-free test now, so it fails in `cargo test` rather than
  minutes into a case that needed a container to read a file.
- Nothing in the binary, the config or the wire changes: this is a deletion of two cases and the places that named them.
