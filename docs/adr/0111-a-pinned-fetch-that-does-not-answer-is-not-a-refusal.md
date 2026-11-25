# A pinned fetch that does not answer is a gate that could not verify, not a refusal

`case_server_images` failed on this host with

```text
ERROR: failed to build: failed to solve: process "/bin/sh -c curl -fsSL -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz …"
```

and musl.cc answered `200` again minutes later; the retry rebuilt both product images and the case passed. Nothing in the
tree was wrong. Both recipes fetch their two musl cross toolchains **at build time** from that host
(`crates/holosmith/images/agent.Dockerfile`, `builder.Dockerfile`), so every image build is only as available as the one
server.

**Why that is a defect rather than a slow retry.** The gate is the acknowledgement for a promotion the browser starts
(ADR-0110 D2), so a gate run that exits 1 for a network fetch is recorded as `pass::Verdict::Refused` →
`State::Refused`: the report a caller reads, and the app's **Service** screen, say the gate **refused the tree** about a
candidate nobody refused. A warm Docker cache hides it, which is exactly why it bites the one case that untags both
product images before it starts. Four paths rebuild an image and therefore reach that host: `server deploy`'s step 3 and
`case_server_images` (which rebuilds on purpose), `holosmith update`'s pass (whose gate is the candidate tree's
`scripts/verify.sh`, and that script contains the case), `holosmith build`'s first run (`holosmith-builder:<version>`),
and any `upgrade` whose image labels say the recipe changed.

Both `curl`s are already pinned by SHA-256, so a vendored copy or a mirror is *technically* safe to substitute. What was
missing is the other two halves: nothing retried the fetch, and nothing told a network failure apart from a tarball that
did not match its digest.

## Three answers, and the two refused

**Refused: vendor the tarballs into the build context.** It is 230 MB of someone else's binaries in two tarballs, and
the context is an archive this binary builds in memory: vendoring means either committing them to git, or inventing a
cache input that the release build — which runs on a machine with the artifact and nothing else — cannot produce. The
pin does not need the bytes to be local; the digest is what makes a fetched tarball the pinned one.

**Refused: point the recipes at a pinned mirror.** A mirror is a second host with exactly the same failure mode, plus a
second URL and a second digest to rotate, and the recipe's availability would then be the *worse* of two hosts. It also
answers the wrong question: the defect is not that musl.cc is the chosen host, it is that a fetch which did not answer
was reported as a verdict about a tree.

**Taken: classify the ending, and bound the retry inside the recipe.** The fetch stays where it is, the pin stays
exactly as it is, and two things are added: a retry the recipe performs by itself, and a classification the binary and
the gate share, so the four paths above can say *this did not answer* rather than *this tree failed*.

## D1 — The retry is bounded, and `--retry` alone is not enough

Both `RUN` blocks carry `--retry 4 --retry-delay 2 --retry-all-errors`, and the `| sha256sum -c -` check and the two
digests are untouched.

Measured in this base's own `curl` (7.88.1, the package `apt-get` installs in `node:24-bookworm-slim`), against a
connection that is refused:

| flags | attempts | wall clock |
|---|---|---|
| none | 1 | 0.3 s |
| `--retry 4 --retry-delay 2` | **1** | 0.3 s |
| `--retry 4 --retry-delay 2 --retry-connrefused` | 5 | 8.3 s |
| `--retry 4 --retry-delay 2 --retry-all-errors` | 5 | 8.3 s |

So `--retry` alone would have been a comment rather than a fix: it retries a timeout and an HTTP 5xx and **not** a
refused connection. `--retry-all-errors` is the flag that makes an outage look like an outage, and it is a superset of
`--retry-connrefused`, so the recipes name one flag rather than two. A name that does not resolve was already transient
in 7.88.1 (5 attempts, 8.3 s with plain `--retry` as well); the refused connection is the case this flag exists for.

**The bound is four retries, two seconds apart**: at most 8 s added to a build whose bound is forty minutes
(`images::BUILD_TIMEOUT`), and no unbounded wait. The recipe is the right place rather than the binary because the
retry has to survive a `docker build` run by anything — a deploy, a promotion, the gate, a hand-run build — and the
binary cannot retry a step the daemon performs.

`--retry-all-errors` also retries an HTTP 4xx, which is deliberate: a `404` from that host is worth three more attempts,
and it still fails afterwards. The digest check has not moved, so a retried attempt that returns the wrong bytes is the
failure it always was.

**Measured end to end, by hand, with the real BuildKit.** The exact `RUN` line was built on `--network none`, on the
agent image — the same Debian bookworm `curl` 7.88.1 the recipes install — and it parsed, made five attempts 2 s apart
and failed in 9 s with

```text
 > [2/2] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz …:
0.166 curl: (6) Could not resolve host: musl.cc
…
ERROR: failed to build: failed to solve: process "/bin/sh -c curl …" did not complete successfully: exit code: 6
```

which is the log D2 is written against, and the log the gate's stand-in prints. What this ADR does **not** claim to have
measured is a real outage *during* a real gate run: that is `scripts/verify.sh`'s own `case_server_images`, and it is the
parent's run.

## D2 — One classifier, in the binary, over the build log

`images::fetch_did_not_answer(text) -> bool` answers the question, and it is asked in the one place a failed build
becomes an error: `images::ensure` wraps its sentence and that bit in `images::BuildError`, which `deploy`'s step 3 and
`promote`'s image step both already go through.

Two facts have to be in the log at once:

- the host's name — `musl.cc` — because either fact alone is a false positive: a compile error can quote a URL, and a
  `curl` failing inside some other image is that image's question and not this build's;
- the **failing child's own exit status**, read from the *last* `did not complete successfully: exit code: N` line: one of
  curl's transfer-never-arrived codes, `6` the name, `7` the connection, `28` the wait, `35` the TLS handshake, `52` an
  empty reply, `56` a broken one — or `22`, curl's `-f`, and only when the log also carries `returned error: 5`, an HTTP
  5xx, which is the host declining. A proxy refusal (`5`) is deliberately not in the set: neither recipe sets a proxy,
  so that ending cannot occur and a fixture for it would invent a log shape.

**The status, and never a diagnostic anywhere in the log — that is the rule's whole shape.** The log is *every* layer's
(`Docker::run_captured` captures the build's whole stdout and stderr into one text), and D1's `--retry-all-errors` makes
a fetch that retried and then **succeeded** leave `curl: (7) …` and `Warning: Transient problem …` behind. A build whose
fetch retried successfully and whose later layer then failed for a reason the tree owns — a compile error, a `COPY` of a
file the context does not carry, a crate the vendored set does not hold — matches a text search on both halves, and
would be excused as an outage: a reproducible tree defect would go unreported while the host is flaky, which is the one
outcome worse than the defect this ADR exists for. A log whose final status is `exit code: 1`, or one that names no
status at all (a `COPY` failure), is the tree's, whatever a retried attempt printed earlier.

**A digest mismatch is not a fetch that did not answer, and that exclusion is the point.** The pin exists to catch
exactly that log, so `did NOT match` wins over a retried attempt's connection error in the same log: the bytes arrived
and were not the pinned ones. `curl: (22) … returned error: 404` is excluded for the same reason — the recipe naming a
tarball that is not there is this tree's defect. So are a compile error, a `COPY` of a file the context does not carry,
and a daemon that would not build: those are exit 1's, and a caller that could not tell them apart would excuse a broken
recipe as an outage.

The sentence is `images::failure`'s, which was already the one sentencer for a failed image build: when the log
classifies, it says the download did not answer, names the host, and says this is "a machine that could not reach that
host and not a recipe or a tree that is wrong" — with the child's last line and the tag that was not created, as before.
`holosmith build` prints that sentence too, because it goes through the same function.

## D3 — Exit 4, and only for that failure

`deploy::FETCH_UNANSWERED` is **4**: 0 is success, 1 is a run that failed, 2 is a gate invoked wrongly, and 3 is
`scripts/check-crate-set.sh`'s stale image. `deploy::Report::exit_code()` is the one place a report becomes a code, and
it answers 4 **only** when a step failed and `Report::unanswered` is set — which step 3 sets from `BuildError` and
nothing else can. Every other failure, including every other failure of step 3, stays 1.

`commands::deploy` returns that code instead of a flat `ExitCode::FAILURE`. **The ssh path needed no change**: it already
propagates the child's own status (`u8::try_from(code)`, `map_or(FAILURE, ExitCode::from)`), so a `--ssh` deploy whose
target could not fetch answers 4 here as it answered it there.

**`holosmith build` keeps exit 1, and says the same sentence.** Its builder image reaches the same fetch, and
`builder::ensure_image` already answers with `images::failure`'s sentence, so the operator reads the fetch and not the
recipe; the code is left alone because the acceptance is about the gate, no caller reads `holosmith build`'s status as a
verdict about a tree, and giving the release path a second exit-code vocabulary would be a decision this ticket did not
ask for.

**`install` keeps exit 1 as well.** Its run is the deploy's six steps plus its own three, and its exit code is its own
command's verdict over its own lines; the gate's install cases (`case_install`, `case_install_terse`) deploy a **hub**,
where step 3 is `skipped`, so no gate path reads an image-build code through that verb. `server deploy` is the verb the
gate and a promotion run, and the one this classification is for.

## D4 — The gate carries it at column 0, never as a `SKIP`

`scripts/verify.sh` gains `unreachable <case> [detail]`, next to `fail`: it prints one line — `UNREACHABLE <case>` at
column 0, one marker token this script writes — and exits **4**.

- **Not `fail`.** Exit 1 is "the tree is not proven", and `holosmith update` records it as a refusal.
- **Not `SKIP`.** A skipped case leaves the gate green, and here the product images were never built: a green run over a
  build that did not happen is the one shape this gate may not have.

`case_server_images` uses it when the deploy exits 4 rather than failing on `$name/deploy/exit`, and the code's own
meaning is the same one `server deploy` answers. `scripts/verify-in-container.sh` repeats no status — it `exec`s
`docker run` — so 4 reaches a caller untouched; that was confirmed by reading it, not by a change.

**The helper's own contract is measured, not assumed**, and inside `case_image_fetch` so it costs nothing: the case
calls it in a command substitution, asserts the status is 4 and that the line begins `UNREACHABLE <case>` at column 0,
and the case's `ok` line is preceded by one of its own that says which of the two answers this is — the fetch is still
there and its ending is classified, not the build that stopped fetching. The `exit` inside a case is also what keeps
`phase_serve`'s `|| fail` from turning it into a `FAIL`: measured in a shell with the same shape, the `||` does not see
an `exit`.

**The measurement is a case of its own, with no daemon and no network.** `case_image_fetch` puts a `docker` stand-in on
`PATH` whose `build` prints the musl.cc failure shape (the same lines a real BuildKit prints, quoted in D1 above) and
exits 1, runs the shipped artifact's `server deploy --verbose --root …` against a `_ping` socket and that stand-in, and
asserts three facts, each one an assertion of its own: the exit code is 4; step 3's line names the host, says the
download did not answer, says this is not a tree that is wrong, and says no new image was tagged; and exactly one build
was asked for — the proxy, which step 4 would have needed — with nothing after it, so the tags are the absent state
`case_server_images` wants. A machine with no python3 prints one `SKIP`; nothing else can stop it, because no daemon
answers and nothing is fetched.

The two directions are pinned below the gate as well, where they cost nothing: `tests/images.rs` feeds the classifier a
connection musl.cc never opened, a name that did not resolve, an HTTP 5xx and a 404, a missing package, a missing
context file, a tarball that failed its digest, a `curl` failure against another host, and — the shape the rule is
anchored for — a fetch that retried and got its bytes in front of a compile error and in front of a `COPY` of a file
that is not there; `tests/deploy.rs` drives a deploy whose build fails each way and asserts exit 4 with the fetch's
sentence and exit 1 with the copy's and with the retried-then-compile one; and both recipes are asserted to carry the
three retry flags, their URL and the digest check, so a later edit cannot drop one silently.

## D5 — The report says `could not verify`, and never `refused`

`pass::RunFailure` gains `Fetch`, and `verdict(4)` answers `Verdict::Unverified(RunFailure::Fetch)` — which `pass()`
already routes to `Outcome::CouldNotVerify`, so the report's `state` is `could-not-verify` with no new arm anywhere. The
word is the one this tree already uses for a gate that reached no verdict; `GLOSSARY.md` now defines it.

The sentence names the fetch, says the product images were never built, and says this is a host that did not answer and
not a tree that failed — and the word "refused" appears nowhere in it, which
`tests/update.rs`'s `every_gate_exit_code_keeps_the_meaning_its_wrappers_header_gives_it` asserts over the whole
report, not only the detail. That assertion is the acceptance in one line: the report a caller polls, and the Service
screen that renders it, cannot tell an operator the gate refused a candidate when a host was away.

## What is not measured

- **A real outage during a real gate run.** `case_image_fetch`'s build log is the shape a real BuildKit printed (D1) but
  it is printed by a stand-in, and the real `case_server_images` build is the parent's gate run, on a machine whose
  access to musl.cc nobody controls. The classification of the real log's shape was measured by hand against a real
  `docker build` under `--network none`.
- **A retry that succeeds after one failure.** The recipes' flags would make it transparent; measuring it needs a server
  that answers the first N requests and then does not, which is a test double at the network layer this build does not
  have.
- **`holosmith update` end to end over an exit 4.** `tests/update.rs`'s fixture gate exits the code, so the translation,
  the report and the assertion that nothing says `refused` are facts about the command; the run that would produce a
  real 4 is the gate run above.
- **The `exit`-inside-a-case discipline D4 leans on.** The only path that carries a 4 out of the gate is `unreachable`'s
  `exit` from inside `case_server_images`, which `phase_serve`'s per-case `|| fail` and `run_phase`'s do not see.
  `case_image_fetch` measures the helper's status and its marker line, but the *escape* was asserted by hand in a shell
  with the same shape, not by a case: it takes a build that cannot fetch to reach it, and no case can be made to fail
  that way on demand.
- **Every other way an image build can fail to reach a host.** The classification covers curl-level failures of the two
  musl `RUN`s and nothing else: a build that ran past `images::BUILD_TIMEOUT`, one that could not pull either base image,
  and one that could not reach cargo's or npm's registry all still answer 1 and are recorded as a refusal. That is why the
  boundary is curl's own exit codes rather than "the network was involved": the timeout's own sentence already says it is
  the machine rather than the recipe (`images.rs`, `BUILD_TIMEOUT`), and widening the code to every network failure would
  move the very refusal this ADR exists to keep.

## Consequences

- `server deploy` answers 4 for step 3's unanswered fetch; `holosmith build` and `install` keep 1 and print the same
  sentence. `scripts/verify.sh` answers 4 with an `UNREACHABLE` line, and `scripts/verify-in-container.sh` and `--ssh`
  propagate it without knowing about it.
- `holosmith update`'s report and the app's Service screen say `could not verify` for a gate that could not fetch,
  where they used to say `refused`.
- `AGENTS.md`'s gate paragraph and `GLOSSARY.md`'s **Could not verify** entry are where a reader learns the code and
  the word; the two recipes' comments say why the fetch is at build time and what a host that stays away means, which
  is what the next reader will be looking for when the build fails.
- The gate's own `case_server_images` no longer needs a warm cache to be honest: a fetch that does not answer is a run
  that says it measured nothing, rather than one that looks like a red tree.
