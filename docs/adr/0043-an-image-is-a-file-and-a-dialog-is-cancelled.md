# An image is a file, and a dialog is cancelled

HS-035 is V8's last two cases, and they are the two records the mapping could not finish with a frame. HS-032
made `Mapping::map` one record in, at most one frame out, with no clock, no database and no agent, because that
is what lets a committed fixture replay through the code a container drives. Both of these records need
something that is not a frame:

- `tool_execution_end.result.content` can hold `{"type": "image", "mimeType": "image/png", "data": "<base64>"}`.
  A payload must not carry it — the log is replayed to a phone on every reconnect and folded by compaction
  (ADR-0032) — and the transcript today says `[image]` in the text, which is a trace rather than a picture.
- `extension_ui_request` with a **dialog** method (`select`, `confirm`, `input`, `editor`) **blocks the run**
  until the client answers with an `extension_ui_response` (`docs/rpc-extension-ui.md`). A session that ignores
  it does not fail; it hangs, which is worse, because nothing in the log says why.

So `Mapped` grows a second and third variant that the pump finishes, the store gains an image half, `Agent`
gains one write, and the contract gains one kind.

## The image

**An image is a file beside the tool's text**, under the same session directory and the same id rule:

```text
<data>/sessions/<id>/tool/<tool_id>.txt          the tool's text (HS-033)
<data>/sessions/<id>/tool/<tool_id>-<index>.png   one image block of its result (HS-035)
```

- **`index` is the block's position in `result.content`**, not a counter of images, so a frame's entry says
  where in the result the picture came from and two blocks cannot be confused for one another.
- **The extension is the store's, from a four-row table** (`image/png` → `png`, `image/jpeg` → `jpg`,
  `image/gif` → `gif`, `image/webp` → `webp`). A type this build cannot name is stored anyway, under `.bin`, and
  served as `application/octet-stream`: keeping the bytes is honest, naming a type this build cannot check is a
  guess about data it never looked at, and the **frame still carries pi's own `mime`**, which is what a client
  shows.
- **The bytes are decoded in the store, not in the mapping.** `tool::decode_base64` is written out — forty
  lines, standard alphabet, optional padding — because the alternative is a dependency for one input. The cap
  (`IMAGE_CAP`, the spec's 5 MB) is applied to the **decoded** length, and it is computed from the encoded
  length **before the decode allocates**: the body is agent-controlled text, and counting it as whole
  four-character groups would let `IMAGE_CAP + 2` bytes through on an unpadded one, so the trailing group
  counts too. What this does **not** bound is the *record*: `agent::read_records` reads one line with no
  length limit (HS-032's reader), so a gigabyte of base64 is a gigabyte of memory before this function is
  reached. That is recorded here rather than papered over — bounding a record is that reader's decision and
  its own story. An invalid or oversized body is a `warn` and no entry, so the frame's `images` names exactly
  the files that exist and a dead reference cannot be produced.
- **A second result for one tool id replaces the first — the whole set, not just the same index.** The read
  side tries the extensions in a fixed order and answers an index nobody names, so a stale `.png` from an
  earlier result would answer for a later `.gif`, and an index the later result does not carry would answer at
  all although no frame names it. So the files are decoded first and then every `<tool_id>-<digits>.<known
  extension>` that the new set does not name is removed — one sweep, whose digit requirement is what keeps it
  from touching another tool whose id merely begins with this one. HS-033's text file keeps the same rule for
  its one name.
- **`tool.end` gains `images: [{index, mime, output_ref}]` additively, and only when one was stored.** A call
  whose result held none answers exactly ADR-0041's payload, and a row written before this key still draws.

**The route is `GET /api/sessions/{id}/tool-image/{tool_id}/{index}`**, read-only, with the row read before any
directory is (ADR-0010/0017, ADR-0041). The **content type comes from the file, never from the request**: a
client asks for the pair the frame gave it, so a URL cannot talk an image into being something else. The tool id
is checked against the allow-list before anything is joined and the index is parsed as a number, so a traversal
is impossible by construction rather than by cleaning up — the same rule and the same reasoning as
`tool-output`. Four ways to have no image (an unknown session, a name that cannot be a filename, an index that
is not a number, a file that is not there) are one `404 not_found`.

## The dialog

**A dialog is cancelled, and the pump writes the cancel.** The categories are pi's own, and they are the whole
decision:

| Method | What this build does |
| --- | --- |
| `select`, `confirm`, `input`, `editor` | `Mapped::Cancel` → the pump writes `{"type":"extension_ui_response","id":…,"cancelled":true}` and appends the frame |
| `notify`, `setStatus`, `setWidget`, `setTitle`, `set_editor_text` | declined: they never wait for an answer, so answering one would be an invented exchange |
| anything else, or a dialog with no `id` | *unknown* — the `warn`, because a later pi that grew a blocking method is exactly what must not hang a session silently |

The extension receives `undefined` for `select`/`input`/`editor` and `false` for `confirm`, which is pi's
documented meaning of a cancellation. The write happens **before** the frame is appended: pi is blocked on the
response, so a frame first would be a log claiming a cancel that had not happened while the run waited for it. A
failed write is a `warn` and the turn carries on — the dialog's own `timeout`, when the extension gave one, is
what releases the run then.

`Agent::respond(&self, line)` is the seam: one JSONL line on the child's stdin under `RESPOND_TIMEOUT` (10 s),
with no waiter registered because there is no reply to correlate. A caller-supplied bound was rejected for it —
a bound somebody has to remember to pass is a bound somebody will forget, and what it protects against (a
process that stopped reading its stdin, pinning the mutex for the session's life) has nothing to do with what
the command is.

## The frame: `turn.error`

The acceptance asks for "an `error` event with a clear reason" and the contract had no error kind. ADR-0018's
table of kinds gains one, and ADR-0019's gains the `images` key:

```json
{"reason": "an extension asked for select, and a session cannot answer a dialog, so it was cancelled",
 "method": "select"}
```

- **Two keys, no message id**, exactly a steer's shape (ADR-0021): one frame, one block, nothing continues it,
  and it belongs where in the turn it happened. On screen it is a block of its own, drawn with
  `createElement`/`textContent` like everything else.
- **No `error` frame without a cancel.** The record and the refusal are one event: an extension error with no
  dialog (`extension_error`) is still declined, and inventing a frame for it would be a second producer of "why
  something went wrong" before there is a story that owns the first.
- **The sentence is built in one function** (`cancelled_reason`), so the frame a test asserts and the frame a
  phone draws cannot drift.

## What this story does not decide

- **Answering a dialog.** A phone would have to type into it, and nothing in this build presents the question.
  The cancel is the honest answer until a story owns a form; every dialog method it cancels is recorded, so the
  log says what was asked.
- **Drawing the image.** The frame names a URL and a mime, and the transcript draws **one link per picture**
  (`image 1 · image/png`), which is this story's "rendered **or linked**". An inline `<img>`, a thumbnail or a
  lightbox is a screen decision with a size cost on a phone and a second fetch policy, and no story owns it
  yet.
- **A second replacing frame.** `message.snapshot` still has exactly one producer (ADR-0032).
- **What a delete keeps.** Image files are a session's own directory, so HS-043's keep-rules own their fate.
- **Deleting images or bounding their number.** One tool result can carry several blocks; each is stored, and
  the cap per image is the only bound. A total-per-session budget is a machine property, and would be a config
  key rather than a constant.
