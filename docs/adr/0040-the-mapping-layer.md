# The mapping layer: pi's records become the transcript's frames

HS-032 is the story that makes a turn real. HS-031 put pi's RPC stdio in the server's hands and stopped at
the ready handshake; this story reads what pi says next and turns it into the contract ADR-0018/0019 pinned,
so the operator watches text and thinking arrive instead of waiting for a summary. Spec item 15 is the event
stream, V8 the awkward frames the layer must survive, and the story's acceptance names the four things that
must be true of every record: it is tagged with a message id and a kind, it reaches the writer in order, a
finished message says how it finished, and nothing unknown is ever fatal.

## One pure module, so the fixture is the production path

`src/rpc.rs` holds the whole translation: `Mapping::map(&mut self, record: &Value) -> Mapped`, one record in
and at most one frame out, plus the two facts a message needs (which id is being streamed, how many records
this turn declined). It reads no clock, no database and no agent.

That is what makes the story's last acceptance line worth anything. The fixture is a **real turn** — the pi
release `pins/pi.txt` pins, inside `pi-agent:local`, with one prompt — committed verbatim under
`crates/holosmith/tests/fixtures/`. The replay tests hand its records to the same `Mapping` a container drives,
so they assert what the product does with records nobody wrote by hand, including the ones it must *decline*
(pi's system prompt, the operator's own user message, `agent_start`/`agent_end`, the compaction and retry
records).

The second half is `rpc::pump`, the loop `DockerBackend::start_turn` runs: subscribe, write the prompt, append
every frame through the `EventWriter` until `agent_settled`. `tests/rpc.rs` drives it with a fake agent process
(a shell script that prints the fixture) and reads the frames back out of a real event log, which is where the
"every delta reaches the writer, in order" is actually asserted.

## pi's messages have no id, so one is minted, and it carries the process instant

A transcript block is keyed by a message id and the log is durable, so two turns must never mint the same one.
pi's `message_start`/`message_end` carry a message with `content`, `usage` and `stopReason` and **no id**, so
the mapping mints `msg-<process instant>-<counter>`: the process's start instant supplies the range a restart
would otherwise reuse, and a process-wide counter supplies the rest. It is the stub's shape (`msg-<nanos>-<n>`)
for the stub's reason — a restart that began again at `msg-1` would fold a new turn's deltas into a previous
turn's block against a durable log (HS-011).

A delta with no `message_start` behind it (a lagged subscriber, or a provider that skipped one) **opens** the
message rather than minting an id per delta, and logs one `debug` line: the text is kept, and one message stays
one block.

## What is not emitted: `text_end`, `thinking_end`, and any replacing frame

pi's `text_end` and `thinking_end` carry the block's authoritative `content`, and the upstream documentation
advises a client to replace its reconstruction with it. This build does not: the deltas already produced the
text, and the contract's only replacing frame is `message.snapshot`, whose **only** producer is compaction
(ADR-0032). Emitting the end content as deltas would duplicate the block; emitting it as a snapshot would
invent a second producer for a frame the renderer treats as "this message's whole state, last one wins", and a
snapshot in the middle of a message would wipe the thinking that preceded it.

The price, recorded rather than hidden: a provider whose end content disagreed with the deltas it streamed
shows the deltas — which is exactly what was on screen live. The alternative (a snapshot per message end) would
have made the transcript right at the end and wrong while it mattered.

## `message.end` gains two keys, additively

The acceptance asks a completed message to carry "usage, tool-call summary and stop reason". ADR-0018 pinned
`message.end` as `{"message_id", "usage": {"in", "out"}}`, so this story **amends** it by addition:

```json
{"message_id":"msg-…","usage":{"in":991,"out":4},"stop_reason":"stop","tool_calls":["bash","read"]}
```

- `stop_reason` is pi's `stopReason` (`stop`, `length`, `toolUse`, `error`, `aborted`, …) verbatim; the words
  are pi's, and paraphrasing them would be this build inventing a taxonomy.
- `tool_calls` is the message's tool-call **names**, in order. A summary, not a record of the calls: their
  arguments, output and status are `tool.start`/`tool.end`, which are HS-033's, and duplicating them here would
  put a tool's whole output in the log twice.
- `usage` stays optional: emitted when pi reports both counts, omitted when it does not (HS-023's rule for a
  backend that cannot report usage). `in` is pi's `input` and `out` its `output`; the cache counters are
  deliberately **not** folded in — HS-034 owns "real token counts" and with it any decision about how a cached
  prompt is counted, and inventing the arithmetic here would make that story's numbers disagree with this one's.

Nothing else changed: the coalescer parses only `message.delta`, the repository reads `usage`/`message_id` by
name, and the page drops a key it does not render. A fourth key on a delta would have been a different matter —
that payload is pinned exactly (ADR-0031).

## The turn ends on `agent_settled`, and never on a record this build does not know

`agent_settled{aborted}` is pi saying it has no further work; `agent_end` is not, because pi can still retry,
recover from an overflow, or deliver a queued message after it. So `agent_settled` is the only record that ends
a turn, `aborted: true` answers `TurnEnd::Aborted` (which the runner writes as `abort_completed`), and
everything else answers `TurnEnd::Settled`.

Unknown record types, unknown nested `assistantMessageEvent` types, and malformed shapes are counted and
dropped — one `debug` line per turn with the count. `toolcall_*` is counted this way on purpose: HS-033 owns it,
and a mapping that guessed would have to be rewritten there. A record that is not JSON at all never reaches the
mapping: the reader (`agent.rs`) has always rejected it.

Records cross from the reader to a turn on a **bounded broadcast**, subscribed *before* the prompt is written so
no record can be lost between the write and the subscription. A subscriber that falls behind is told how many
it missed and carries on: the alternative — failing the turn — would kill a session because the log was busy.

## The other four turn operations, because nothing else owns them

`steer`, `clear_queue` and `abort` are pi's own commands (`docs/rpc-commands.md`) and stopped refusing here
too: no story between this one and HS-046 owns them, and HS-046 is the gate that runs every phase-1 flow
against a real Docker host. Each is one `request` bounded by its own timeout, and the ladder still bounds rung
one by what is left of `abort_grace` (ADR-0020). The `turn.steer` **frame** stays the prompt route's
(`http::sessions::record_steer`, ADR-0021): a backend that also wrote it would put two blocks in the transcript
for one instruction.

## A removal the server provokes is never news — including the container step's

The resume flow exposed a real bug that HS-031's generation had only half closed. `start_container` begins with
`docker rm -f s_<id>` (spec item 6), which ends the attached `docker run` of the session's *previous* agent. That
exit arrives **before** the new launch registers its generation, so it is still "current" when it is classified:
the row becomes `error` with "the container exited 137 and printed nothing", and the provisioner stops at the
next step boundary before the new container is up. Measured on this story's own demo — resume after a refused
prompt — which lost that race about one run in two, while the gate case won it, which is what flakiness looks
like.

The fix is ADR-0039's rule applied to every removal the server performs: `remove_container` **supersedes** the
session's launch before it removes anything (a bumped generation and the receiver dropped), so the exit answers
`ProcessEnd::NotCurrent`. `supersede_agent` rather than `forget_agent`, because the generation is derived from
the registry entry: a forgotten entry restarts the count at 1, and a watcher from the previous launch would
compare equal to the new one. The delete keeps `forget_agent`, which is right — nothing follows it.

## The fixture is captured by hand, and refreshed by a release step

There is no provider key in a session's container (HS-039 builds the key proxy) and the session's network is
`--internal` (HS-028), so a prompt on a real session is *refused by pi* before a stream exists. That is what the
gate's `case_docker_turn` asserts, and it is a different observable from a hang: the row leaves `running` for
`error` carrying pi's own sentence, the container is still alive, and resume-and-prompt works a second time.

The streaming half is therefore the fixture, captured with the same image, pi and command on the default bridge
with the operator's own provider credentials mounted into a scratch agent directory under `target/`:

```bash
# one handshake, one prompt, every record until agent_settled
docker run --rm -i --init --network bridge --entrypoint pi \
  -v <scratch>/work:/work -v <scratch>/pi-home:<scratch>/pi-home -w /work \
  -e PI_CODING_AGENT_DIR=<scratch>/pi-home -e PI_OFFLINE=1 \
  -e PI_SKIP_VERSION_CHECK=1 -e PI_TELEMETRY=0 \
  pi-agent:local pi --mode rpc --no-session --approve --model <provider/model> --thinking high
```

Two files are committed: one turn that answered with text, and one that thought first (the thinking arm is
otherwise only unit-tested). Refreshing them is a release step, not a story step, and
`tests/rpc.rs`'s `a_fixture_whose_frames_drifted_fails_this_guard` is what makes a silent refresh visible.

## What this story does not decide

- Tool calls, their arguments, their output and `output_ref` are HS-033's.
- The missing-usage warning a session screen shows is HS-034's; this story only stops inventing zeroes.
- Images in a tool result and extension UI requests are HS-035's, and the record subscription is the seam they
  will use.
- Fold cache counters into `usage` — HS-034.
