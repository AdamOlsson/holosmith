# The turn runner, the backend's events, and one code per state a prompt is refused in

HS-015 is the story the product exists for: a prompt becomes a turn, and the turn's output reaches
the screen. Three things had to be decided that the spec does not decide, and each would be
expensive to change once HS-016 (abort), HS-017 (steer), HS-032 (deltas), HS-033 (tools) and HS-034
(usage) have built on it: who moves a session's state when a turn is running, what a backend is
allowed to produce, and what a prompt's refusal looks like.

## The prompt's transition is the handler's; the way out of `running` is the runner's

`POST /api/sessions/{id}/prompt` decides its refusals in a fixed order — body, row, state — then
applies the table's `prompt` row, refetches the row and answers **202 with it**. The row therefore
says `running` before the answer exists, which is what makes a client that acts on the 202 correct:
it can refetch, or open the stream, without racing the transition. This is the provisioner's own
"row first, the event follows" rule, applied to an answer instead of to a report.

The turn itself runs off the request's task, in `crate::turn::Turn`, built like the provisioner:
`spawn` hands it to a task, `run` is what a test awaits. The runner **touches** `last_active_at`,
asks the backend to run the turn, and then ends the turn through the same one function every state
change goes through (ADR-0016):

| Ending | Row of the table | Reason |
| --- | --- | --- |
| the agent finished | `running` + `agent_settled` → `idle` | none |
| anything else | `running` + `unexpected_exit` → `error` | the backend's own error text |

`agent_settled` is the only normal way out of `running`; abort's two rows (`abort_completed`,
`abort_ladder`) are HS-016's and land above it in the same table. The runner never writes a state
itself, and it does not re-apply `prompt`: the handler owns that row, because a second application
would be refused (there is no `running + prompt` row) or would race the very answer it is behind.

## The backend emits a turn's events; the server owns every state change

`SessionBackend::start_turn` gained the event writer:
`start_turn(&self, session, prompt, events: &EventWriter)`. The split is the decision:

- **The backend produces the turn's frames.** They are the backend's own output — its script, or
  pi's RPC — and only it knows what the agent said.
- **The backend cannot move a state.** It has no `Transitioner`, and the source-level guard of
  ADR-0016 forbids a status write outside the one function, so the row is untouchable from behind the
  trait. What it *could* do is forge a `session.status` frame through the writer it is handed, since
  an event's kind and payload are free-form strings. That is the lie ADR-0016's "row first, event
  follows" rule forbids, it would show every client a state the row does not hold, and closing it
  properly means typing the payload so a status frame cannot be built outside `state.rs` — a change
  to the event layer that no story has asked for. It is recorded here as a known hole rather than
  claimed shut: the enforcement today is the row, plus a reviewer's eye on what a backend appends.

The stub's script is the whole of what `serve --backend stub` shows for a turn, and it exists so
both the stream and *every rendering path of the screen* are exercised with no container: a
boundary, thinking and text deltas on one message id, a tool call with its output, the message's end
with its usage, and the closing boundary. Its pace is the stub's existing per-step delay — one
number for "how fast this fake moves", not a second knob — and its message and tool ids are minted
from one process-wide counter (`msg-<n>`, `tool-<n>`): ADR-0018 asks only that a message id be
unique within a session, and one counter in one process gives that with no state per session. The
script is unconditional today; a backend-raised stop signal arrives with HS-016, which is the story
that needs one.

Three payloads are pinned here, because HS-033 and HS-034 will own their full treatment:

| Kind | Payload | Meaning |
| --- | --- | --- |
| `message.end` | `{"message_id": "…", "usage": {"in": 1200, "out": 340}}` | the message is complete, with its token counts |
| `tool.start` | `{"tool_id": "…", "name": "read_file", "arguments": "src/main.ts"}` | a tool call began |
| `tool.end` | `{"tool_id": "…", "name": "read_file", "output": "…"}` | the call returned, with its output |

No `output_ref`: the stub writes no file, and item 15's ref belongs to HS-033's output file. The
screen renders the block and the counts minimally — a `.tool` block with the call's name and its
output, and one `in N · out N` line inside the message block, each appended when its frame arrives
and never re-rendered. The full treatment (truncation, a persisted output file, totals in the
header) is HS-033's and HS-034's, and a rendering path that does not exist cannot be exercised by
the script that exists to exercise it.

## A prompt's refusal is `409 session_<state>`

The spec names two of these codes — `session_stopped` (item 8, for prompt, steer and abort on a
stopped session) and `session_running` (item 12) — and HS-017 names a third, `session_idle`, for
steering what is not running. So the rule is the code family, not the two codes: a request that a
session's state refuses is `409` with `session_` and that state's own machine word, spelled from
`Status::as_str` (`session_starting`, `session_running`, `session_stopped`, `session_error`,
`session_deleting`).

The alternative — one umbrella code such as `session_not_idle` — would force a client to read the
message, or refetch the row, to learn which state it is in before it can decide what to do next,
and the states want different things: `stopped` means "resume and retry" (HS-018), `starting` means
"wait", `running` means "steer or abort instead" (HS-016, HS-017). The body is
`{"prompt": <non-empty string>}` and anything else is `422 bad_body`: one field, no length cap
beyond axum's own 2 MiB, because the spec sets none and a cap would be an invented number (the
create's rule, ADR-0012).

`idle` is the one state a prompt is accepted in, so `ApiError::not_idle` has five codes and no
sixth: a call with `Idle` is a bug in its caller rather than a state a client can reach.

## Consequences

- `crates/holosmith/tests/turns.rs` proves the accepted prompt (202 with the `running` row), a
  second prompt's `409 session_running`, `session_starting` against the slow stub, `stopped` and
  `error` by driving the table directly (their endpoints are HS-018's and HS-019's), `not_found`,
  the five `bad_body` shapes, and — off the wire — the script's kinds in the order the stream
  delivers them, one message id per turn, the tool call's ids matching, the usage on its message,
  `running → idle`, and `last_active_at` strictly increasing across the turn. Every refusal is
  asserted on its status, its `code` and its `content-type`.
- `scripts/verify.sh` grows three cases: `case_prompt_turn` (the 202 and the row, then the stream
  read until the settle, with the six kinds in order), `case_prompt_bad_body` and
  `case_prompt_unknown_session`. 26 cases.
- `web/test/transcript.test.ts` grows three tests: the usage change names its message, tool events
  keep their own order and are never message blocks, and a `tool.end` without its `tool.start`
  creates the block first.
- `last_active_at` moves through one new repository method, `touch`. It succeeds for a row that is
  gone, because the caller is a background task that can lose its session to a delete: an `Err` there
  would turn an ordinary race into a logged failure.
- HS-016 and HS-017 answer with the same `not_idle` family, and HS-017 needs the one code this
  story does not add (`session_idle`) because it is refused for a state this one accepts.
- The turn runner is where HS-016's abort ladder and HS-022's timeout will attach: they end the turn
  early, which is a second way out of `running` and already two rows of the table.
