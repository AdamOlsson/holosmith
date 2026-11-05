# A tool call's output is a file, and its frame says where

HS-033 is the story that makes a tool call legible without making the log heavy. HS-032 turned pi's records
into the transcript's frames for text and thinking; the records a tool leaves behind — its arguments, its
result, how long it took, whether it failed — were *declined*, and the story that owns them has to answer two
questions the contract did not: what a `tool.*` frame carries, and where a result that is two megabytes long
lives when the log is replayed to a phone on every reconnect.

Spec item 15's line is "the full output of a tool call, inspectable, without bloating the database"; the
acceptance names the four pieces: name, arguments summary, exit status and duration in the frames; the whole
output in `sessions/<id>/tool/<tool_id>.txt` capped at 5 MB with a truncation marker; `tool.end` carrying a
truncated `output` plus `output_ref`; and `GET /api/sessions/:id/tool-output/:tool_id` serving it with no way to
traverse out. And one negative: `session.log` holds stderr only.

## Which record is the call, since pi prints two

pi emits both:

| Record | What it carries |
| --- | --- |
| `toolcall_start`/`_delta`/`_end` | the *provider's* stream of the call being generated: `contentIndex`, and the call's arguments arriving as deltas |
| `tool_execution_start`/`_update`/`_end` | the call's **run**: `toolCallId`, `toolName`, the complete `args`, the `result`, `isError`, `durationMs` |

The two share an id (`toolCall.id` is the execution's `toolCallId` — verified in the committed fixture), so
mapping both would open **two blocks for one call**. The pair that has the arguments *and* the result is
`tool_execution_*`, so those two are mapped and the provider's three stay declined (a documented record with no
frame: one `debug` per turn, not the `warn` an unknown type gets).

`tool_execution_update` — the tool's `partialResult` — is declined too. It is not in the contract, the page has
no rule for a replacing tool frame, and inventing one would be a second producer of "this block's whole state"
after ADR-0032 made compaction the only one.

## The frames, additively

ADR-0019's table pinned `tool.start` as `{"tool_id", "name", "arguments"}` and `tool.end` as
`{"tool_id", "name", "output"}`. This story **amends both by addition**, the pattern ADR-0040 used for
`message.end`:

```json
{"tool_id": "call_00_…", "name": "bash", "arguments": "{\"command\":\"ls\"}"}
{"tool_id": "call_00_…", "name": "bash", "output": "<preview>", "status": "ok",
 "duration_ms": 412, "output_ref": "/api/sessions/<id>/tool-output/<tool_id>"}
```

- **`arguments` is a summary**: the compact JSON of pi's `args`, capped at 2000 characters (`tool::ARGUMENTS_MAX`)
  with an ellipsis. A `write` of a large file must not put the file in a payload, and the story asks for a
  *summary* by that name. **The rest is retained nowhere** — `message.end`'s `tool_calls` carries the call's
  name only, and the stored file holds the result, not the arguments — so a `write` whose body exceeds the cap
  is only in the agent's own message, which this build does not keep. That is recorded here rather than
  glossed: a reader who needs the whole call has to decide where it should live, and that is a new story.
- **`status` is `ok` or `error`**, from pi's `isError`. pi reports **no exit code** in the record's own fields:
  a `bash` tool's status code sits inside `result` (`structuredContent.exit_code` in the captured turn), which
  is tool-specific, and **the file does not keep it** — the file holds the result's *text* blocks, and
  `details`/`structuredContent` are dropped. So the frame carries the tool's own verdict, which is this story's
  reading of the acceptance's "exit status", and the ADR says plainly what is not retained rather than pointing
  at a file that cannot answer. Absent `isError` is `ok`: pi omits it when the tool did not run, and a frame
  that said `error` for that would report a failure nobody observed. Keeping the whole `result` (or a `details`
  blob, or an exit code as a key of its own) is a new decision with its own story: it is a second payload shape
  for one field, and HS-035 will be the story that touches a result's non-text parts anyway.
- **`duration_ms`** is pi's `durationMs`, optional because pi measured none when the tool did not run.
- **`output` is a preview** (`tool::PREVIEW_MAX` = 2000 characters, ellipsis-marked): the transcript's own
  rendering limit, so a 5 MB output draws as a paragraph and the log's rows stay small. This is the one
  *semantic* change to ADR-0019's table, which is why ADR-0019 now carries an "Amended by ADR-0041" note: the
  key keeps its name and its position, and its meaning narrows from "the output" to "the beginning of it".
- **`output_ref` is present only when the file was written.** A store that refused (an id that cannot be a
  filename, a full disk) still leaves a frame, because the tool ran; a frame that claimed a reference to
  nothing would be a dead link the operator could not tell from a slow one.

**Amended by ADR-0043:** `tool.end` also carries `images: [{index, mime, output_ref}]`, additively, and only
when a picture was stored — one file per image block beside this file, under `<tool_id>-<block index>.<ext>`,
served by its own route with the type the *file* has. So a call's result is still one block, a frame whose
result held no image answers exactly the payload above, and this story's "an image block is not retained" is
now the opposite: HS-035 retains it, and `details`/`structuredContent` are what remain unkept.

## The file, and why the mapping is not the writer

The mapping is one record in, at most one frame out, with no clock, no database and no I/O — which is what lets
a committed fixture replay through the code a container drives (ADR-0040). Writing files there would end that.
So `Mapping::map` answers a fourth variant, `Mapped::Tool(ToolEnd)` — the whole output, plus what the frame
needs — and **`rpc::pump`** does the write: it already owns the session id and the writer, and it is the one
place a turn's frames are appended in order. `ToolEnd::frame(stored)` is the single spelling of the payload, so
the served path and a store-less replay agree.

`src/tool.rs` is the store: `Store::new(data_root)`, `store(session, tool_id, output) -> Option<Stored>`,
`read`, `path`, `preview`, `valid_id`.

- **The path** is `<data>/sessions/<id>/tool/<tool_id>.txt`, beside `session.log` and `pi-home`, because it is
  one session's own file about one of its own turns.
- **The cap is 5 MiB of output** (`tool::CAP`), a constant and not a config key, for the reason ADR-0032's
  window and HS-021's tick are: it is a rule of the contract, not a property of a machine. It is *defensive* in
  practice: pi's own `bash` tool already truncates what it hands back (its description, in the committed
  fixture, promises the last 2000 lines or 50 KB), so the six-megabyte cases in `tests/rpc.rs` are synthetic and
  the two-megabyte one in the gate is planted. A tool from a project-local extension need not truncate, which is
  the case the cap exists for. The body is cut on a
  **character boundary**, so a file is always valid UTF-8, and one marker line follows it:
  `[truncated: stored 5242880 of 6291450 bytes]`. The marker does not count against the cap — the cap is on the
  output, and a marker that ate into it would misreport where the output stopped.
- **The second write to one id replaces the first.** A tool id is repeated only if the agent repeats it, and then
  the later output is the one worth having; appending two runs under one name would make the file a lie about
  what the tool printed.
- **The id rule is an allow-list**: 1–128 characters of `[A-Za-z0-9_-]`, reusing `exit::is_path_segment`. pi's
  ids are the provider's (`call_<base32>`), so "32 hex" — the shape the plan first reached for — would refuse
  every real one. The allow-list is what makes a traversal **impossible by construction** rather than by
  cleaning up: `.`, `/`, `\` and every control character are simply not characters an id has, so `..` cannot be
  spelled. A refused id is a name no file here can hold, and both the write and the route answer "there is no
  such output" instead of a second reason an operator has to learn.
- **What the id rule defends against, and what it does not.** The threat is a *name the agent chose*: a tool id
  arrives from pi's records, so it is agent-controlled text, and joining it unchecked would be a traversal. It
  is not a defence against something that can already write inside `<data>/sessions/<id>/tool/`: a symlink
  planted there and named `<tool_id>.txt` is followed by `std::fs::read`. Nothing in this build can create one —
  the container's mounts are the worktree and the agent's own directory, not a session's data directory — so the
  gap is recorded rather than papered over, and a story that gives an agent a writable path under `<data>` has
  to come back to this line.

## The route

`GET /api/sessions/{id}/tool-output/{tool_id}`, read-only (so every guard applies and the manifest records
`ReadOnly` with nothing new — ADR-0010, ADR-0017), `text/plain; charset=utf-8`, because what is stored is the
tool's own text and a link that opens in a browser should read as text.

**The row is checked before any directory is read.** A session the table does not hold is `404 not_found` with
`unknown_session`'s sentence, whatever is left on disk — a delete's leftovers (HS-043 owns what survives it) are
not reachable through a session nobody can see. An id that is not a filename and a file that is not there are
one answer with one sentence, because to a caller they are one fact: there is nothing to fetch. The code is
ADR-0008's `not_found` for all three; the sentences differ because the requests do.

## What this story does not decide

- **Images in a tool result** are HS-035's. The text blocks of `result.content` are what the file holds; a block
  that is not text is written as `[<type>]`, so a result carrying an image leaves a trace in the text instead of
  being silently dropped, and HS-035 adds storage and a rendered frame on top.
- **Deleting tool files.** A delete removes the row and its event log (HS-019); what happens to a session's
  directory is HS-043's keep-rules.
- **Cache counters in a tool's usage** — HS-034.
- **A cap or a preview length as a config key.** Both are the contract's numbers here; a machine cannot tune
  what a client can render.
- **A UI affordance for anything but the link**: the tool block draws the name, the status word, the duration,
  the preview and one anchor. A larger tool presentation (collapsible output, syntax highlighting) is a screen
  story with no owner yet.
