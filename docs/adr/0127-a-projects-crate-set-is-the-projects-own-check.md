# A project's vendored crate set is the project's own check, and Holosmith reads no project's lock

ADR-0123 D2 gave a project's image a content-addressed tag and named what the tag cannot see: a `COPY Cargo.lock …` over
a lock that has moved leaves the tag where it was, so the image a session resolves is stale and no read says so. That ADR
left the closing to "the change that closes it". ADR-0106 D7 had already closed the same hole for the image every
session ran then: this tree's recipe writes the digest of the `Cargo.lock` it vendored with to
`/opt/cargo-vendor/.holosmith-cargo-lock` **in the instruction that ran `cargo vendor --locked`**, and
`scripts/check-crate-set.sh` compares that with the tree's lock, answering **3** with one sentence when they differ.
ADR-0126 then deleted the agent image, made this repository's recipe a **project's** — `docker/Dockerfile`, built from the
project root — and left the marker and the script where they were: both of this tree's gates run the check before their
first phase, over this project's own image.

So the question is not whether the check works; it is **whose it is**, and what a project that vendors content is told.
Three shapes were on the table: **a documented pattern with no mechanism in the binary**, **one convention a project sets**
— a `docker/.lock-digest` file or an `org.holosmith.lock-digest` label — that a session's start would compare with the
tree's lock and warn about, and **nothing, stated**. The decision is the first, carrying the third's honest sentence, and
the two not taken are rejected below for their own reasons.

## D1 — the marker is derived, and that is the property a convention cannot have

`sha256sum Cargo.lock | cut -d' ' -f1 > /opt/cargo-vendor/.holosmith-cargo-lock` is a clause of the `RUN` whose
neighbouring clause is `cargo vendor --locked`: the file the digest is taken over is the file the directory beside it was
built from, unpacked by the same instruction that wrote the digest. It therefore cannot disagree with the crates on its
own — there is no state in which the marker is updated and the vendor directory is not, because one instruction did both.

**Rejected: one convention — a `docker/.lock-digest` file, or an `org.holosmith.lock-digest` label, that a project
chooses to set.** It is a hand-maintained second source for a fact the recipe already holds, and it is stale on every
*legitimate* lock bump: a host edits the lock, rebuilds nothing, and the next session is warned from inside a container
that cannot rebuild the image it runs in. A warning nobody in that container can act on is worse than the honest cargo
error. It is also the same shape ADR-0123 D2 refused for the tag — a config key, or a label Holosmith would have to
write and read for a project's image — arriving as a second spelling of the currency question this milestone already
answered with one existence check.

## D2 — `scripts/check-crate-set.sh` is this project's CI, not a product mechanism

This repository is a project like any other (ADR-0126), and this is that project's check: it runs in this tree's
`scripts/gate-quick.sh` and `scripts/verify.sh`, before their first phase, over this tree's image and this tree's
`Cargo.lock`. The exit **3** that `pass.rs` and a promotion map to "stale crate set" is therefore this project's gate
answering about a candidate of this project — a candidate's tree whose lock its image has not vendored — and not a
Holosmith feature another project inherits.

Nothing about the check's own facts moves, and each is asserted where it can fail: the marker's path is a literal with
three readers (this script, `scripts/verify.sh`'s `case_agent_toolchain` arm and `crates/holosmith/tests/images.rs`) and
one writer (the recipe's `sha256sum` line); `HOLOSMITH_CRATE_SET` moves the path and never the sentence, which is what
makes the mismatch measurable with no root and no image; absent is a pass, because a host fetches its crates and carries
no marker; and nothing goes to stdout.

## D3 — Holosmith never reads a project's crate set

No code in this binary knows the marker's path, runs `scripts/check-crate-set.sh`, writes a label for a project's image,
or asks a session's start to compare anything. A session on a stale image starts.

**Rejected: nothing, stated — as the whole answer.** Stating only that the gap is a project's would have been cheap and
wrong twice over: the pattern is one clause in a recipe the project already has and one script beside it, and this tree
has both, so there is nothing to discover; and the sentence a project author needs — *you check this, we do not* — is
owed whichever shape wins. The chosen shape is that sentence plus the reference implementation behind it.

So a project that vendors a lock file into its image writes its own two halves, and copies them from here:
`docker/Dockerfile`'s vendor instruction with its `sha256sum` clause, and a script of its own that compares that digest
with its tree's `Cargo.lock` and fails its own CI. "Copy `scripts/check-crate-set.sh`" is not a mechanism either — it is
a project's file, in a project's repository, and this repository's is the one the pattern is written down in.

## D4 — a project without a check still gets an honest failure

The half-closed hole is survivable because the image is already offline: `/.cargo/config.toml` sets `net.offline = true`
beside the source replacement, so a build that needs a crate the vendored set does not hold is refused by cargo itself
in its offline voice. Measured in this project's image — the tag the gate builds from this tree — under `--network none`,
as the image's own user, with `rand = "0.8"` in a scratch crate's manifest under `/`, where the image's
`/.cargo/config.toml` replacement and `net.offline = true` are what cargo reads:

```text
error: no matching package named `rand` found
location searched: directory source `/opt/cargo-vendor` (which is replacing registry `crates-io`)
required by package `t v0.1.0 (/tmp/t)`
```

The first line names the crate the build wanted; the `location searched` line names the **directory source** the image
put in crates-io's place, so where cargo looked is the image and not a registry; and the `required by package` line names
what asked. That is a stale crate set's own sentence, and not a hunt for a broken registry. The tree records one other
sentence for the same subject — ``error: the crate `rand` could not be found in registry index.`` — and that one is
`cargo add`'s, measured in the same image under `--network none` (ADR-0106 D7): `cargo add` consults a registry index and
a build consults the directory the image replaced it with, so the two are not two accounts of one failure. What a check
buys a project is not a different failure; it is that failure arriving before a build, in a sentence naming the rebuild.

## What supersedes what

This decision **supersedes ADR-0106 D7's account of the mechanism's owner**. D7 described the marker and the script as
the *agent image's* — the subject of that whole ADR — and that image no longer exists (ADR-0126): the recipe is this
project's, the marker is written by it, and the two gates that run the script are this project's. D7's measurements, its
exit code, its `HOLOSMITH_CRATE_SET` rule and its list of readers stand unchanged; only "the agent image's mechanism"
becomes "this project's own check".

This decision **amends ADR-0123 D2's "What the digest does not cover" sentence**, which said "the change that closes it
owns it". This is that change, and the closing half is the project's rather than Holosmith's: the tag still cannot see a
`COPY`'d lock, and what is now decided is who is told and by what. ADR-0126's currency gap is untouched for the same
reason — `pins/rust.txt` arrives as a build argument, so `--force` is still the repair for it, and no project check makes
that a detection.

**ADR-0113's opening named the wrong command, and so did three comments.** ADR-0113 says a session's `cargo build`
fails with `the crate X could not be found in registry index`, and until this change the headers of
`scripts/verify.sh`, `scripts/check-crate-set.sh` and `scripts/gate-quick.sh` called that a build's sentence too: it is
`cargo add`'s (D4). The three scripts are corrected here, and ADR-0113's record stands with this note rather than being
rewritten.

Nothing else in ADR-0106, ADR-0123 or ADR-0126 changes. The rest of D7's shape — a precondition rather than a phase, a
code of its own, one sentence on stderr — is what runs today and is what this decision leaves alone.

## What is not measured

- **A second project with a recipe of its own and a crate-set script of its own.** There is one such project in this
  tree's measurements, and the pattern's other half is a script another repository would have to write; a case would be a
  case about a project this gate does not have.
- **A stale image through any product path.** It is not refused there by design: nothing in the binary reads the marker,
  so the only captures are this tree's two gates (`case_gate_quick`'s arms and `case_agent_toolchain`'s `vendor/lock`
  assertion) and `tests/images.rs`'s assertion that the vendor step writes the name.
- **The exact sentence a stale image produces inside a session.** D4's text is cargo's, measured in this project's
  image rather than inside a session's container; a session's own build is what a project's check exists to keep from
  happening, so no case runs one.

## Consequences

- `docker/Dockerfile` is the reference implementation of one half and `scripts/check-crate-set.sh` of the other, each
  saying so in one sentence: the next project copies both, and Holosmith reads neither.
- A project with a recipe and no such check loses a build to cargo's offline sentence rather than to a warning it cannot
  act on — which is a worse moment and a better failure.
- `AGENTS.md` and `docs/config.md` say where a project author's check goes, and `GLOSSARY.md`'s **Project image** entry
  says the repair for a `COPY`'d lock is `--force` and the detection is the project's own.
- The binary's surface is unchanged: no config key, no label, no `doctor` finding, no session-start comparison. A
  project's freshness is its own CI's question, in the same way its recipe is (ADR-0123).
