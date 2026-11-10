# The smoke run replays a recording as `pi`, and owns every name it makes

HS-077 is spec item 10's second step — "run the candidate with a smoke config (alternate port, temporary data
directory, `container_prefix=smoke_`, `network_prefix=smoke_`) against the real Docker daemon, using a fake pi in the
image that replays the session recorded in phase 1. Create a session, run a turn, delete it" — and the sentence it
serves is short: **a bad release is caught by a robot, not by the operator at midnight.** So the deliverable is not a
procedure but an exit code, and the decisions below are all about which process answers "did it pass" and what it is
allowed to touch while it finds out.

Two limits are recorded here rather than in prose somewhere else. **No provider call is made**: the fake replays
records, so the key proxy is *started and joined* and never exercised — that is V3's by-hand run, which needs a real
key and which no gate may hold. And **the smoke image is not what a promotion ships**: it exists to be a host for one
recording, and `server deploy`'s step 3 still builds the two product images.

## D1 — the verb is `holosmith smoke [--binary PATH] [--root DIR]`, and its exit code is the contract

A **host** verb, for ADR-0080 D1's reason spelled the other way round: the spec says item 10's steps are implemented
by `holosmith upgrade <binary>` and not by a separate script, HS-078 *is* that command, so the smoke test has to be
something HS-078 can run in-process and branch on. A `scripts/smoke.sh` was refused because a script is not the
product and would be a second entry point to the pipeline; a `case_smoke`-only design was refused because a gate case
cannot be a promotion's go/no-go — it runs on a machine that has the tree.

The codes are three, because two of the things that can happen are not verdicts about the candidate:

| Code | Meaning | Who reads it |
| --- | --- | --- |
| 0 | the candidate passed: a session ran a recorded turn and was deleted | HS-078 promotes |
| 1 | a measurement failed — including the smoke **image build** | HS-078 refuses the promotion |
| 2 | the test could not run here: no daemon, no `git`, a `--binary` that is missing, not executable or built for another CPU | HS-078 reports "this machine cannot judge it" |

2 is decided **before** anything is attempted, and 1 is every failure after that. The image build is a 1 rather than a
2 deliberately: the recipe is this build's own, so a daemon that cannot build it has failed the measurement, and
calling it "could not run" would let a broken recipe be reported as a missing prerequisite. The operator-facing
consequence is in `docs/build.md`: a machine with no network for the base is a red smoke test, not a quiet skip.

## D2 — the container's `pi` is the candidate, behind a two-line wrapper

`crates/holosmith/images/smoke.Dockerfile` (embedded with `include_str!`) is `FROM` the agent image's own digest-pinned
node base, `COPY holosmith /usr/local/bin/holosmith`, and writes `/usr/local/bin/pi` as

```sh
#!/bin/sh
exec /usr/local/bin/holosmith container fake-pi "$@"
```

The three properties this buys, in order of how much they cost if lost:

- **The candidate is what is tested.** The image is built from the *bytes `--binary` names* — the same bytes this run
  read, hashed and ELF-checked (`builder::artifact_problem` against `builder::default_target()`, refused as a 2) — so
  HS-078 cannot promote a binary the smoke run never ran.
- **The launch path stays measured.** `agent::version_of` runs `docker run --entrypoint pi <image> --version` and
  `version_check` compares it with `pins/pi.txt`; the wrapper answers the pin, so the check runs through the real path
  instead of being skipped for a smoke run. Skipping it would have been the story's own version of PER-108's P1 — a
  claim nothing reads.
- **A symlink would not do.** An argv[0] of `pi` reaches *this* binary's clap parser with `--mode`, which is not a
  subcommand it has; the wrapper is what turns pi's arguments into `container fake-pi`'s.

A third Dockerfile is a decision rather than a variant: the agent image is what a session runs in production (pinned
pi, git, the toolchains), and this one carries no `apt-get` at all. Node stays because the base has it and because the
boot's network self-test probes `agent_image` as `node -e` — in a smoke run that image is this one. The two recipes
naming one digest is asserted by a test, because "two pinned files drift" is the defect HS-048's P1 and ADR-0080's
`COPY --from` both were.

The image is tagged `holosmith-smoke:<version>` and labelled `org.holosmith.smoke-recipe-sha256` = the SHA-256 of
(recipe ‖ candidate bytes) — `builder::ensure_image`'s read-then-apply, and one label rather than two because a
promotion asks one question. A changed recipe or a different candidate rebuilds; a second run of the same candidate
does not.

## D3 — the recording is a committed file with a header, and the header is checked

`crates/holosmith/fixtures/smoke-turn.jsonl` is one header line and then the **verbatim** 69 records of HS-032's
`tests/fixtures/pi-turn-tool.jsonl`: deltas, one tool round trip, non-zero usage, closing `agent_settled`.

```json
{"type":"fixture","version":1,"schema":7,"recorded":"HS-032","turn":"tool"}
```

- **It is embedded, not read.** The container runs the *candidate*, and the candidate is what must hold the recording —
  `include_str!`, ADR-0080's rule for recipes for the same reason (a release has no `fixtures/` beside it).
- **A header line rather than a `const` beside the file.** The acceptance asks that the fixture be "versioned with the
  schema it was recorded against"; a header makes that a property of the *file* the binary carries, and a `const` would
  make it a property of the source code next to it.
- **Its claims are read.** A test compares `schema` with `schema::SCHEMA_VERSION` and fails naming both numbers and the
  two honest fixes (re-record, or say why the mapping is unaffected); a second test asserts the body is byte-identical
  to HS-032's file, so a hand edit cannot pass as a recording. A copy of a recording is only honest while something
  keeps it equal.
- **At run time a newer recording is refused and an older one replays** with one printed note. Not symmetry for its own
  sake: the recording lives in the binary, so a header from the future is a mis-built artifact and replaying records a
  build does not map would produce a turn with quietly missing frames, while an older header is the ordinary case one
  release after a schema bump — `rpc::Mapping` counts a record it does not know rather than dying (ADR-0040).

The mapping is not changed by this story. `tests/rpc.rs` already replays these records through it; what HS-077 adds is
the **writer half** — a program that speaks pi's side of the protocol.

## D4 — `container fake-pi` answers **every** command, with the id it was sent

A command in, a `response` out, and the incoming id echoed: `agent::request` waits for the id it sent, so the
recording's own `req-1` is unusable and is dropped along with the header. The recording follows the `prompt` answer,
and EOF exits 0 — closing stdin is what pi treats as an orderly shutdown, and a non-zero code would be classified as a
failed agent on a delete.

"Every command" includes `abort`, `steer`, `clear_queue` and anything a later pi adds: a command answered with nothing
is a `COMMAND_TIMEOUT` inside a *different* story's path (the delete's abort, the ladder's rungs), which is a hang in
the wrong place. pi's own flags (`--mode rpc`, `--no-session`, `--approve`, `--model`, `--thinking`) are accepted and
ignored; `container fake-pi --version` is the one argument that is read.

## D5 — the smoke config is a file, and "cannot touch real state" is four settings plus the names they compose

The harness writes `<root>/config.toml` **before** it asks the daemon anything, so a refusal leaves an operator a config
to read and the gate can assert it on a machine with no daemon. Role `server`, backend `docker`, `control_listen =
"127.0.0.1:0"` (the port is the kernel's, and `serve --port 0` rewrites `control_origin` from the bound address),
a `preview_listen` picked by a bind-and-drop (the two listeners may not be the same address), `data_root` and
`projects_root` under the root, `agent_image` = the smoke image, `container_prefix` and `network_prefix` = `smoke_`,
`min_free_gb` and `session_disk_soft_gb` 0 (a full disk and a per-session cap are facts about a machine, not about a
candidate), `idle_stop_min` 0 (the sweeper must not take the session mid-turn), and `max_live_sessions` 1.

**The root defaults to `<data_root>/smoke`**, because the data root is the one directory the config already guarantees
the daemon can see — it is a container mount source — and because HS-078 has to run this with no operator input. A
`/tmp` root mounts as an empty root-owned directory and fails with no error anywhere (ADR-0035's measurement); `--root`
is for anything else, and the gate and the demo pass `target/smoke-$$`.

The root's ending is three rules, and the middle one is why a refusal's sentence can point at a file:

- **A passing run removes it**, and a root that cannot be removed after a pass is a **failure** (exit 1): a second
  run would inherit a database, a worktree and a log that answer about the previous candidate.
- **A failing run keeps it** — it is the evidence an operator reads, and the smoke config in it is what every refusal
  in this command names. It is *not* removed on the way out, which is the one place this harness differs from a test's
  scratch directory. `HOLOSMITH_KEEP_SCRATCH=1` keeps a passing one too.
- **A directory that is not a smoke run's is refused, never deleted.** `--root` is an operator's argument, so the
  harness recognises its own root by the first line of `config.toml` ([`smoke::CONFIG_MARKER`]) and refuses a
  non-empty stranger by name. Deleting a path on a guess is not a failure a sentence can make good.

**The prefix reaches five kinds of name, through one composer per kind, and every reader uses it** (config's
`container_prefix`/`network_prefix`, HS-077's other half):

| Name | Composed | Why the prefix is load-bearing |
| --- | --- | --- |
| session container | `smoke_s_<id>` | a real session's `s_<id>` must not collide, and the reconcile's sweep must not see it |
| session network | `smoke_agents_<id>` | the same, and `docker network rm` on a real one would take a live session's isolation with it |
| egress proxy | `smoke_hs_egress` | a boot `rm -f`s and re-runs it; unprefixed, the machine's live proxy would be replaced |
| key proxy | `smoke_hs_key` | worse: the replacement mounts the *smoke* config's key store, which is empty, so every live session's model calls fail at that address until the next real boot |
| self-test network | `smoke_holosmith-selftest` | the boot's self-test joins the proxies to it; unprefixed, two boots would fight over one network and attach each other's proxies |

Deliberately **not** prefixed: the machine's `egress` network (created idempotently, used read-only) and the
per-network aliases `egress-proxy`/`key-proxy` (aliases are per network, so they cannot collide). The empty default
reproduces `s_<id>`, `agents_<id>`, `hs_egress`, `hs_key` and `holosmith-selftest` byte for byte, and both directions
are asserted — a prefix that reached only some of the five is the defect the tests would otherwise be blind to.

## D6 — the turn's assertions are a pure function of the log

`smoke::assert_frames` reads the session's `GET /api/sessions/{id}/events/history` page and demands four facts, each
with its own sentence: a `turn.start`/`turn.end` pair (the transcript's turn is the prompt, ADR-0040), at least one
`message.delta`, a `tool.start`/`tool.end` pair with the **same** id, and a `message.end` whose `usage` is non-zero (a
zero report is not a report, ADR-0042). Each arm is unit-tested by removing exactly one frame, so a smashed assertion —
the shape PER-108's review named — fails here rather than at a promotion.

The turn is waited for by `turn.end`, not by the row's `idle`: the row and the log are written by different paths
(ADR-0019), so an `idle` row does not mean the frames are committed. The delete is waited for by `404`, and the
container and network by a bounded `docker ps`/`network ls`.

## D7 — the gate's `case_smoke` has three arms and no `SKIP`

1. **always, no daemon** — the artifact's own `container fake-pi` over two pipes: `--version` equals `pins/pi.txt`, the
   handshake and the prompt echo their ids, the recording's `req-1` and its header never appear, a tool call is there,
   usage is non-zero and the last line is `agent_settled`. Deleting the subcommand, the fixture or the header fails
   here on any machine.
2. **always, no daemon** — a `docker` stand-in that answers `version` and refuses `build`: exit **1**, the daemon's own
   words in the sentence, and the smoke config on disk with `container_prefix`, `network_prefix`, the ephemeral port
   and the run's own data root.
3. **real** — the full run against the daemon, asserting its report, the row's absence, no `smoke_` container or
   network left, and the machine's two proxies by **id** (`docker inspect --format '{{.Id}}'`), because a replaced
   container keeps its name — which is exactly the damage D5's prefixing exists to prevent. The case **makes** those
   two containers (`docker create --name hs_egress hs_key`) rather than hoping an earlier case left them, because two
   empty strings compare equal to themselves and a vacuous gate assertion cited by an ADR is this repository's
   recurring finding. No daemon prints one `note`, never a `SKIP`, and the same holds for the one machine with no
   local image to make the sentinels from.

Arm 3 asserts the proxies are **running**, which is the only measurement the prefixing works; it cannot assert a
provider call, and does not claim to.

## Consequences

- `holosmith smoke` is a host verb the spec's Commands table does not list, exactly as `holosmith build` is
  (ADR-0080 D1); the Commands table's `upgrade <binary>` still takes a binary, and this verb is what judges it.
- The two name prefixes are config keys with empty defaults (`KNOWN_KEYS` 34 → 36), so a smoke run is a config, not a
  mode, and a machine may legitimately name all its containers with a prefix of its own.
- The failure arm a **fixture** cannot demonstrate is recorded here: with the recording embedded, "a fixture with a
  record removed" is a `tests/` fixture, not a demo, and the demo's failure cases are the refusing stand-in (exit 1)
  and a machine with no daemon (exit 2).
