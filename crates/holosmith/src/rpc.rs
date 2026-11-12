//! The mapping from pi's RPC records to the transcript's frames.
//!
//! Spec item 15 needs the agent's text and thinking to arrive as the operator watches, so a turn's frames
//! cannot be a summary built at the end: every `message_update` pi emits becomes a `message.delta` on the
//! writer as it arrives. This module is that translation and nothing else — one record in, at most one frame
//! out — which is what lets the committed fixture (a real turn, captured once) run through *the same code* a
//! container drives.
//!
//! **The contract it writes is ADR-0018/0019's** (`events`), with two additive keys on `message.end`
//! (`stop_reason`, `tool_calls`) that ADR-0040 records: the story's "a completed message produces
//! `message.end` with usage, tool-call summary and stop reason". A delta is one appended text node, so the
//! *only* frames this module emits for text are deltas — pi's `text_end`/`thinking_end` `content` is not
//! re-emitted, because the only replacing frame in the contract is `message.snapshot` and compaction is its
//! only producer (ADR-0032). A provider whose end content disagreed with its deltas therefore shows the
//! deltas, which is exactly what was on screen live.
//!
//! **The transcript's turn is the operator's prompt, not pi's assistant response.** pi emits `turn_start`/
//! `turn_end` around *one assistant response* (its `docs/json.md`: "A turn is one assistant response plus any
//! tool calls and tool results produced by that response"), and a run can hold several of them once retries,
//! steering or tool calls are involved. The contract's turn is the prompt: the stub emits one `turn.start`/`turn
//! .end` pair per prompt, `turns_before` numbers the log by those rows, and the page counts a turn from
//! `turn.start` and a settled turn from `turn.end`. So [`pump`] writes the pair itself — `turn.start` before the
//! first frame and `turn.end` when pi settles — and pi's own boundaries are records it declines. Mapping them
//! would have made "turn 3" mean "third assistant response" and told the page a turn had settled while the row
//! was still `running`.
//!
//! **Three rules the mapping keeps:**
//!
//! - **pi's messages have no id, so one is minted.** A transcript block is keyed by the message id, and the
//!   log is durable, so two turns must never mint the same one: the id carries the process's start instant
//!   and a process-wide counter (`msg-<nanos>-<n>`), the same shape the stub's ids have (the reason: a
//!   restart that started over at `msg-1` would fold a new turn into a previous turn's block).
//! - **Unknown is counted, never fatal.** A record type or nested event type this build does not map — a
//!   queue update, an extension error, and the provider's `toolcall_*` — answers
//!   [`Mapped::Nothing`] and moves a counter, and [`pump`] logs that counter once per turn. A mapping layer
//!   that died on a frame it did not recognise would take the session with it.
//! - **The turn ends on `agent_settled`, not on `agent_end`.** `agent_end` closes one low-level agent run and
//!   pi can still retry, recover from an overflow, or deliver a queued message after it; `agent_settled` is
//!   pi saying it has no work left (`docs/json.md`).
//!
//! **Two records take more than a frame**. A tool result can carry an *image*, which is stored beside
//! the tool's text and named by `tool.end`'s `images` instead of being carried in a payload; and an extension
//! can ask the client a *question*, which in RPC mode **blocks the run** until an `extension_ui_response`
//! arrives. Neither can be a frame alone: the first is I/O, and the second is a write to pi's stdin. So both
//! answer a variant of [`Mapped`] that [`pump`] finishes — [`Mapped::Tool`] for the output and the images,
//! [`Mapped::Cancel`] for the dialog — and the mapping itself stays a pure function of one record (ADR-0043).
//! A dialog nothing answers is a hung session, which is what V8's third case is about; the answer here is
//! always *cancel*, because nothing in this build can type into a form on a phone, and the cancel is recorded
//! as a `turn.error` frame so the operator sees why the extension behaved as it did.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::agent::{Agent, AgentRecords, Received};
use crate::backend::{BackendError, TurnEnd};
use crate::clock;
use crate::events::{
    EventWriter, MESSAGE_DELTA, MESSAGE_END, NewEvent, TOOL_END, TOOL_START, TURN_END, TURN_ERROR,
    TURN_START,
};
use crate::tool::{self, Store};

/// The prefix of a minted message id. It is the stub's, so a log cannot tell which backend wrote a block.
const MESSAGE_PREFIX: &str = "msg";

/// A process-wide counter for minted ids, because a session runs several turns in one process.
static MINTED: AtomicU64 = AtomicU64::new(1);

/// The instant this process first minted an id, which keeps two runs of the binary from minting the same ones.
///
/// The counter alone cannot be the id: a restarted process would start over at 1 and fold a new turn's deltas
/// into a previous turn's block against a durable log. The value is taken on the first mint, not at startup —
/// uniqueness does not care which, and only the wording of this policy does.
fn minted_instant() -> u128 {
    static INSTANT: OnceLock<u128> = OnceLock::new();
    *INSTANT.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    })
}

/// A message id nobody else in this process — and no earlier run of the binary — will mint.
fn mint() -> String {
    let counted = MINTED.fetch_add(1, Ordering::Relaxed);
    format!("{MESSAGE_PREFIX}-{}-{counted}", minted_instant())
}

/// What one pi record means for the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapped {
    /// No frame: a message starting (which only names the id its deltas will use), a system or user
    /// message, an end record whose content is already on screen, or a record this build does not map.
    Nothing,
    /// One frame to append, in the order it arrived.
    Frame {
        /// The event's `type` column, from [`crate::events`]'s constants.
        kind: &'static str,
        /// Its `payload` column, JSON as text.
        payload: String,
    },
    /// pi has no work left for this run: the turn is over.
    Settled {
        /// `agent_settled.aborted`: the run ended because something aborted it.
        aborted: bool,
    },
    /// A tool call that finished, with the output that belongs in a file rather than in the log.
    ///
    /// The mapping cannot write it — it does no I/O, which is what lets a fixture replay without a data
    /// root — so it answers the whole record and [`pump`] stores it, then asks [`ToolEnd::frame`] for the
    /// payload once it knows whether the write worked.
    Tool(ToolEnd),
    /// A dialog an extension asked the client for, which must be answered or the run blocks.
    ///
    /// Like a tool's output, the mapping cannot write it: the answer is a line on pi's stdin, and the mapping
    /// has no stdin. [`pump`] writes [`Cancel::response`] and then appends [`Cancel::frame`].
    Cancel(Cancel),
}

/// One finished tool call: what the log's frame says, plus the output the file keeps.
///
/// The two are deliberately separate. `output` is the whole of what the tool printed and never becomes a
/// payload — a `cat` of a large file in the log would be replayed on every reconnect and folded by
/// compaction — while `preview` (built when the frame is) is what the transcript draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEnd {
    /// pi's `toolCallId`, which is the id its `tool_execution_start` carried and the name the file has.
    pub tool_id: String,
    /// pi's `toolName`.
    pub name: String,
    /// Everything the tool printed: the text of its result's content blocks.
    pub output: String,
    /// The images its result carried, in the order pi listed them.
    ///
    /// Stored beside the output by the pump, because the mapping does no I/O, and named by
    /// [`ToolEnd::frame`] as `images` once they are on disk.
    pub images: Vec<tool::Image>,
    /// `"ok"` or `"error"` — pi's `isError`.
    ///
    /// pi reports no exit code: a `bash` tool's status lives inside `result.details`, which the stored file
    /// holds verbatim. So the frame carries the tool's own verdict, which is the honest spelling of the
    /// story's "exit status" (ADR-0041).
    pub status: &'static str,
    /// pi's `durationMs`, absent when it measured none (a tool that did not run).
    pub duration_ms: Option<u64>,
}

/// One dialog an extension asked for, and this build's answer: cancelled.
///
/// pi's dialog methods (`select`, `confirm`, `input`, `editor`) emit an `extension_ui_request` and **block**
/// until an `extension_ui_response` with the request's id arrives; their fire-and-forget siblings (`notify`,
/// `setStatus`, `setWidget`, `setTitle`, `set_editor_text`) never wait for anything and stay declined. A
/// session has no way to answer a form — the operator is on a phone, and the run is unattended — so the answer
/// is always `cancelled: true`, which the extension receives as `undefined` (or `false` for a `confirm`), and
/// the frame says so rather than leaving an extension's odd behaviour unexplained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancel {
    /// The request's own `id`, which the response must repeat.
    pub id: String,
    /// pi's `method`, which the frame names so the operator can see what was asked.
    pub method: String,
}

impl Cancel {
    /// The line pi is waiting for: the response that dismisses the dialog.
    pub fn response(&self) -> Value {
        json!({"type": "extension_ui_response", "id": self.id, "cancelled": true})
    }

    /// The one frame the cancel leaves: `turn.error`, with the reason and the method that asked.
    pub fn frame(&self) -> Mapped {
        frame(
            TURN_ERROR,
            json!({"reason": cancelled_reason(&self.method), "method": self.method}),
        )
    }
}

/// What a `turn.error` says about a dialog nothing could answer.
///
/// One sentence, built here and nowhere else, so the frame a test asserts and the frame a phone draws cannot
/// drift: it names the method, says the thing that could not happen, and says what was done instead.
fn cancelled_reason(method: &str) -> String {
    format!(
        "an extension asked for {method}, and a session cannot answer a dialog, so it was cancelled"
    )
}

impl Mapped {
    /// This as one frame, when it is one: a tool's result answers its payload with the **preview** and no
    /// `output_ref` because nothing was stored.
    ///
    /// The served path does not use this — [`pump`] stores first and then builds the frame — it is for a
    /// replay with no data root (a committed fixture, a unit test), where "what frame would this be" is the
    /// whole question. A cancelled dialog answers **its** frame here, because that frame needs nothing stored:
    /// it is the record of a write, not of a file.
    pub fn into_frame(self) -> Option<(&'static str, String)> {
        match self {
            Mapped::Frame { kind, payload } => Some((kind, payload)),
            Mapped::Tool(result) => match result.frame(None, &[]) {
                Mapped::Frame { kind, payload } => Some((kind, payload)),
                _ => None,
            },
            Mapped::Cancel(cancel) => match cancel.frame() {
                Mapped::Frame { kind, payload } => Some((kind, payload)),
                _ => None,
            },
            Mapped::Nothing | Mapped::Settled { .. } => None,
        }
    }
}

impl ToolEnd {
    /// The `tool.end` frame, given what the store made of the output and the images (`None`/empty when
    /// nothing could be written).
    ///
    /// The one place the payload's shape is spelled: the store supplies the preview and the references, and a
    /// failed write still answers a frame — with a preview and no `output_ref` — because the transcript must
    /// show that something ran. `images` is present **only when an image was stored**, so a call whose result
    /// held none answers exactly the payload ADR-0041 pinned, and a row written before this key still draws.
    pub fn frame(&self, stored: Option<&tool::Stored>, images: &[tool::StoredImage]) -> Mapped {
        let mut payload = serde_json::Map::new();
        payload.insert("tool_id".to_string(), Value::String(self.tool_id.clone()));
        payload.insert("name".to_string(), Value::String(self.name.clone()));
        let output = match stored {
            Some(stored) => stored.preview.clone(),
            None => tool::preview(&self.output),
        };
        payload.insert("output".to_string(), Value::String(output));
        payload.insert("status".to_string(), Value::String(self.status.to_string()));
        if let Some(duration) = self.duration_ms {
            payload.insert("duration_ms".to_string(), Value::from(duration));
        }
        if let Some(stored) = stored {
            payload.insert(
                "output_ref".to_string(),
                Value::String(stored.reference.clone()),
            );
        }
        if !images.is_empty() {
            payload.insert(
                "images".to_string(),
                Value::Array(
                    images
                        .iter()
                        .map(|image| {
                            json!({
                                "index": image.index,
                                "mime": image.mime,
                                "output_ref": image.reference,
                            })
                        })
                        .collect(),
                ),
            );
        }
        frame(TOOL_END, Value::Object(payload))
    }
}

/// One turn's mapping: which of pi's messages is current, and what it did with the records it could not map.
///
/// One value per turn, because the id it mints is per message and the count is per turn. It holds no clock,
/// no database and no agent — a record in, a frame out — so a fixture can be replayed through it without a
/// container (which is what `tests/rpc.rs` does).
#[derive(Debug)]
pub struct Mapping {
    session_id: String,
    /// The assistant message pi is currently streaming, if any.
    current: Option<String>,
    /// Records of a type this build knows and has no frame for: pi's `agent_start`/`agent_end`, its queue and
    /// level bookkeeping, the retry and compaction records, the provider's `toolcall_*` blocks and a tool's
    /// partial results. Declining them is the design, so they are not worth a line.
    declined: u64,
    /// Records this build does **not** know: a type (or a nested event type) a later pi release invented, or a
    /// shape that is not the one the documentation describes. Counted separately, because that is the one worth
    /// a `warn`: it is how a mapping layer learns that pi has grown a frame it should map.
    unknown: u64,
}

impl Mapping {
    /// A mapping for one turn of `session_id`.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            current: None,
            declined: 0,
            unknown: 0,
        }
    }

    /// How many records this turn produced no frame for at all, of either kind.
    pub fn skipped(&self) -> u64 {
        self.declined + self.unknown
    }

    /// How many of those are records this build does not know — the number worth a `warn`.
    pub fn unknown(&self) -> u64 {
        self.unknown
    }

    /// Map one record pi printed.
    ///
    /// The record is pi's own JSONL, already parsed: the reader rejects what is not JSON (`agent.rs`), so
    /// everything that reaches here is an object of some kind and the question is only what it means.
    pub fn map(&mut self, record: &Value) -> Mapped {
        match record.get("type").and_then(Value::as_str) {
            // pi's own response boundaries: the transcript's turn is the prompt, so [`pump`] writes that pair and
            // these are declined (see the module doc). `agent_start`/`agent_end` are one low-level run's edges,
            // and a run can retry, recover and continue after them.
            Some("agent_start" | "agent_end" | "turn_start" | "turn_end") => self.note_declined(),
            Some("agent_settled") => Mapped::Settled {
                aborted: record
                    .get("aborted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            Some("message_start") => {
                // A user or system message is not a transcript block: the page draws the operator's own
                // prompt from the composer and nothing from pi's system prompt. Only an assistant message
                // opens a block, and the id it opens is the id its deltas carry.
                if role(record) == Some("assistant") {
                    self.current = Some(mint());
                }
                Mapped::Nothing
            }
            Some("message_update") => self.update(record),
            Some("message_end") => self.end(record),
            // The tool's own execution: the pair. The *provider's* `toolcall_*` above are declined and
            // these two are mapped, because they carry the complete `args` and the result, and pi gives them
            // the same id — mapping both would open two blocks for one call (ADR-0041).
            Some("tool_execution_start") => self.tool_start(record),
            // A partial result. It is not in the contract and the page has no rule for a replacing tool
            // frame, so it is a documented record with no frame rather than an unknown one.
            Some("tool_execution_update") => self.note_declined(),
            Some("tool_execution_end") => self.tool_end(record),
            // A `response` is the reader's to route to the command that asked; the mapping meets one only
            // when a fixture is replayed verbatim, and it is a documented record either way.
            Some("response") => self.note_declined(),
            // An extension asking the client for something. The dialog methods block the run until they
            // are answered and the fire-and-forget ones never wait, so which method it is decides what happens
            // — see `ui_request`.
            Some("extension_ui_request") => self.ui_request(record),
            // Bookkeeping pi documents and this contract has no frame for: the queue and level updates, the
            // compaction and retry records, and an extension that failed to load (`extension_error`, which is
            // about an extension rather than about the turn).
            Some(
                "queue_update"
                | "session_info_changed"
                | "thinking_level_changed"
                | "entry_appended"
                | "compaction_start"
                | "compaction_end"
                | "auto_retry_start"
                | "auto_retry_end"
                | "summarization_retry_scheduled"
                | "summarization_retry_attempt_start"
                | "summarization_retry_finished"
                | "bash_execution_update"
                | "extension_error",
            ) => self.note_declined(),
            // A record type this build has never heard of: a later pi release's, and the one case worth a line.
            _ => self.note_unknown(),
        }
    }

    /// One `message_update`: its nested event decides whether it is a delta.
    fn update(&mut self, record: &Value) -> Mapped {
        let Some(event) = record
            .get("assistantMessageEvent")
            .filter(|event| event.is_object())
        else {
            return self.note_unknown();
        };
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return self.note_unknown();
        };
        let kind = match kind {
            "text_delta" => "text",
            "thinking_delta" => "thinking",
            // The ends of a block, the start of the provider stream and its `done`: the content is already on
            // screen as deltas (ADR-0040).
            "text_start" | "text_end" | "thinking_start" | "thinking_end" | "start" | "done" => {
                return self.note_declined();
            }
            // A provider stream that failed is worth a line, and it is the only diagnostic anywhere: the
            // assistant message's `stopReason` says `error`, and pi's reason is the sentence an operator needs.
            "error" => {
                let reason = event
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("none");
                let detail = event.get("error").and_then(Value::as_str).unwrap_or("none");
                tracing::warn!(
                    target: "holosmith::rpc",
                    session = %self.session_id,
                    reason,
                    detail,
                    "pi's provider stream ended with an error"
                );
                return self.note_declined();
            }
            // `toolcall_start`/`_delta`/`_end` are the provider's streaming of the call's arguments. They are
            // declined because `tool_execution_start` carries the same call's complete `args` and
            // `tool_execution_end` its result, so mapping both would open two blocks for one call (ADR-0041).
            "toolcall_start" | "toolcall_delta" | "toolcall_end" => return self.note_declined(),
            _ => return self.note_unknown(),
        };
        let Some(text) = event.get("delta").and_then(Value::as_str) else {
            return self.note_unknown();
        };
        if text.is_empty() {
            // Nothing to append. A frame with no text would add a row and a text node that say nothing.
            return Mapped::Nothing;
        }
        // A delta with no current message means this build missed the `message_start` (a lagged subscriber, or
        // a provider that skipped it). Minting one keeps the text rather than dropping it, and the debug line
        // is what makes such a turn diagnosable.
        let message_id = self.streaming_id("a delta arrived with no message started");
        frame(
            MESSAGE_DELTA,
            json!({"message_id": message_id, "kind": kind, "text": text}),
        )
    }

    /// One `message_end`: the assistant message's whole result, which is where usage comes from.
    fn end(&mut self, record: &Value) -> Mapped {
        let Some(message) = record.get("message").filter(|message| message.is_object()) else {
            return self.note_unknown();
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            // The user's own prompt, pi's system message, and a tool result (whose text is `tool.end`'s, from
            // the execution record, and whose images are that story's). None of them is a transcript block.
            return Mapped::Nothing;
        }
        let message_id = self.ended_id("a message ended with no message started");
        let mut payload = serde_json::Map::new();
        payload.insert("message_id".to_string(), Value::String(message_id));
        if let Some(usage) = usage(message) {
            payload.insert("usage".to_string(), usage);
        }
        if let Some(reason) = message.get("stopReason").and_then(Value::as_str) {
            payload.insert("stop_reason".to_string(), Value::String(reason.to_string()));
        }
        payload.insert("tool_calls".to_string(), Value::Array(tool_calls(message)));
        frame(MESSAGE_END, Value::Object(payload))
    }

    /// One `tool_execution_start`: the call's name and a summary of its arguments.
    ///
    /// The block the page draws is keyed by the tool id, so an id pi did not give is a call that cannot be
    /// drawn: it is declined rather than rendered under a blank heading, the same rule a message follows.
    fn tool_start(&mut self, record: &Value) -> Mapped {
        let Some(tool_id) = record.get("toolCallId").and_then(Value::as_str) else {
            return self.note_unknown();
        };
        if tool_id.is_empty() {
            return self.note_declined();
        }
        let name = record
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // The arguments are a *summary*: a `write` of a large file must not put the file in a payload, and the
        // story asks for a summary by that name. Nothing retains the rest — `message.end`'s `tool_calls` carries
        // the call's name only, and the stored file holds the result, not the arguments — which ADR-0041
        // records rather than pretends otherwise.
        let arguments = record
            .get("args")
            .map(|args| tool::truncate(&args.to_string(), tool::ARGUMENTS_MAX))
            .unwrap_or_default();
        frame(
            TOOL_START,
            json!({"tool_id": tool_id, "name": name, "arguments": arguments}),
        )
    }

    /// One `tool_execution_end`: the result, which the pump stores and the frame previews.
    fn tool_end(&mut self, record: &Value) -> Mapped {
        let Some(tool_id) = record.get("toolCallId").and_then(Value::as_str) else {
            return self.note_unknown();
        };
        if tool_id.is_empty() {
            return self.note_declined();
        }
        let name = record
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let status = match record.get("isError").and_then(Value::as_bool) {
            Some(true) => "error",
            // Absent is not an error: pi omits the field when a tool did not run, and a frame that said
            // `error` for that would report a failure nobody observed.
            Some(false) | None => "ok",
        };
        let result = record.get("result");
        Mapped::Tool(ToolEnd {
            tool_id: tool_id.to_string(),
            name,
            output: result_text(result),
            images: result_images(result),
            status,
            duration_ms: record.get("durationMs").and_then(Value::as_u64),
        })
    }

    /// One `extension_ui_request`: a dialog to cancel, or a record with no frame at all.
    ///
    /// The two categories are pi's own (`docs/rpc-extension-ui.md`), and they are the whole decision: a dialog
    /// method **blocks the run** until it is answered, so it is cancelled; a fire-and-forget method never waits
    /// for anything, so answering one would be an invented exchange. A method this build has never seen is
    /// *unknown* — the `warn` — because a later pi that grew a blocking method is exactly the thing that must
    /// not hang a session in silence. A dialog with no id cannot be answered at all, so it is unknown too.
    fn ui_request(&mut self, record: &Value) -> Mapped {
        let Some(method) = record.get("method").and_then(Value::as_str) else {
            return self.note_unknown();
        };
        match method {
            "select" | "confirm" | "input" | "editor" => {
                let id = record
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty());
                let Some(id) = id else {
                    // The response has to repeat the id, so a request without one is a dialog nothing can
                    // dismiss: worth the `warn`, because the run may well be waiting on it.
                    return self.note_unknown();
                };
                Mapped::Cancel(Cancel {
                    id: id.to_string(),
                    method: method.to_string(),
                })
            }
            "notify" | "setStatus" | "setWidget" | "setTitle" | "set_editor_text" => {
                self.note_declined()
            }
            _ => self.note_unknown(),
        }
    }

    /// The id of the message being streamed, minting and keeping one when no start opened it.
    ///
    /// Kept rather than taken: every delta of one message carries the same id, and only the message's end
    /// releases it. A delta that arrives with no `message_start` behind it (a lagged subscriber, a provider
    /// that skipped one) *opens* the message instead of minting an id per delta, which would scatter one
    /// message across a frame per token.
    fn streaming_id(&mut self, why: &str) -> String {
        match self.current.clone() {
            Some(id) => id,
            None => {
                tracing::debug!(
                    target: "holosmith::rpc",
                    session = %self.session_id,
                    why,
                    "the mapping minted a message id no start had opened"
                );
                let id = mint();
                self.current = Some(id.clone());
                id
            }
        }
    }

    /// The id the ending message had, releasing it so the next message mints its own.
    fn ended_id(&mut self, why: &str) -> String {
        let id = self.streaming_id(why);
        self.current = None;
        id
    }

    /// Count a record of a kind this build knows and has no frame for, and say there is none.
    fn note_declined(&mut self) -> Mapped {
        self.declined += 1;
        Mapped::Nothing
    }

    /// Count a record this build does not know, and say there is none.
    fn note_unknown(&mut self) -> Mapped {
        self.unknown += 1;
        Mapped::Nothing
    }
}

/// Drive one turn: map pi's records onto `events` until the agent settles.
///
/// Called by [`crate::backend::DockerBackend::start_turn`] after it has written the prompt, with a subscription
/// taken *before* that write so no record can be lost between the two. The caller's subscription is the only
/// ordering the transcript relies on: records arrive in the order pi printed them, each frame is appended in
/// that order, and the writer's seqs are that order too. The channel has a bound and the reader *waits* when it
/// fills, so a slow commit slows pi rather than dropping text — "every delta reaches the writer" is a property
/// of that, not a hope (ADR-0040).
///
/// **The turn's boundary pair is written here**, not mapped from pi's `turn_start`/`turn_end`: the contract's
/// turn is the operator's prompt (see the module doc), so `turn.start` goes out before the first frame and
/// `turn.end` when pi says it settled. A turn the agent never settles — its process died mid-run — writes no
/// `turn.end`, because the turn did not end: `crate::exit` records what became of the row.
///
/// **A tool's output is written here too**: the mapping answers [`Mapped::Tool`] with everything the
/// tool printed, and this stores it through [`crate::tool::Store`] before the frame is built. That is the one
/// piece of I/O in a turn's mapping path, and it is here rather than in the mapping because a mapping that
/// wrote files could not be replayed from a fixture. Its images are stored the same way, and the
/// frame names them rather than carrying them.
///
/// **A dialog is answered here**: `agent` is the process whose stdin the `extension_ui_response` goes
/// to, and the write happens **before** the `turn.error` frame is appended, because pi is blocked until the
/// answer arrives — a frame first would be a log that says the cancel happened while the run is still
/// waiting for it. A failed write is a `warn` and the turn carries on: the dialog's own `timeout`, when the
/// extension gave one, is what releases the run then, and the frame still records that something asked.
pub async fn pump(
    records: &mut AgentRecords,
    mapping: &mut Mapping,
    events: &EventWriter,
    store: &Store,
    agent: &Agent,
) -> Result<TurnEnd, BackendError> {
    append(events, mapping, TURN_START, "{}").await;
    loop {
        match records.recv().await {
            Received::Record(record) => {
                // One record, one mapping: mapping it twice would mint two message ids for one message.
                let mapped = mapping.map(&record);
                let (kind, payload) = match mapped {
                    Mapped::Settled { aborted } => {
                        log_declined(mapping);
                        append(events, mapping, TURN_END, "{}").await;
                        return Ok(if aborted {
                            TurnEnd::Aborted
                        } else {
                            TurnEnd::Settled
                        });
                    }
                    // The whole output goes to its file, its images to theirs; the frame carries a preview and
                    // the references. A store that refused (an id that cannot be a filename, a full disk) still
                    // leaves a frame: the tool ran, and the transcript says so.
                    Mapped::Tool(result) => {
                        let stored =
                            store.store(&mapping.session_id, &result.tool_id, &result.output);
                        let images = if result.images.is_empty() {
                            Vec::new()
                        } else {
                            store.store_images(&mapping.session_id, &result.tool_id, &result.images)
                        };
                        match result.frame(stored.as_ref(), &images) {
                            Mapped::Frame { kind, payload } => (kind, payload),
                            // `ToolEnd::frame` always answers a frame; nothing else is a frame.
                            _ => continue,
                        }
                    }
                    // The answer first — pi is blocked on it — and then the frame that says it happened.
                    Mapped::Cancel(cancel) => {
                        if let Err(error) = agent.respond(cancel.response()).await {
                            tracing::warn!(
                                target: "holosmith::rpc",
                                session = %mapping.session_id,
                                method = %cancel.method,
                                %error,
                                "an extension's dialog could not be cancelled"
                            );
                        }
                        match cancel.frame() {
                            Mapped::Frame { kind, payload } => (kind, payload),
                            _ => continue,
                        }
                    }
                    Mapped::Frame { kind, payload } => (kind, payload),
                    Mapped::Nothing => continue,
                };
                append(events, mapping, kind, &payload).await;
            }
            Received::Closed => {
                log_declined(mapping);
                return Err(BackendError::failed(
                    "the turn: the agent's output ended before it settled".to_string(),
                ));
            }
        }
    }
}

/// Queue one frame for the turn's session.
///
/// A failed append means the writer has stopped, which in a served process means the process is going down. The
/// loss is logged and the turn carries on, exactly as the stub's own turn does: the turn *ran*, so failing it
/// here would write a session that did its work into `error` because the process was leaving.
async fn append(events: &EventWriter, mapping: &Mapping, kind: &str, payload: &str) {
    let event = NewEvent {
        session_id: mapping.session_id.clone(),
        kind: kind.to_string(),
        payload: payload.to_string(),
        created_at: clock::unix_now(),
    };
    if let Err(error) = events.append(event).await {
        tracing::error!(
            target: "holosmith::rpc",
            session = %mapping.session_id,
            kind,
            %error,
            "a turn frame was not queued"
        );
    }
}

/// One line per turn about what the mapping did with the records it had no frame for.
///
/// Two levels, because the two facts are different: declining a documented record is the design and is a `debug`
/// note, while a record type this build does not know is the one thing an operator should be able to find in the
/// log — it is how a pi release that grew a frame gets noticed. At most one line per turn either way.
fn log_declined(mapping: &Mapping) {
    if mapping.declined > 0 {
        tracing::debug!(
            target: "holosmith::rpc",
            session = %mapping.session_id,
            declined = mapping.declined,
            "records with no frame in this contract were declined"
        );
    }
    if mapping.unknown > 0 {
        tracing::warn!(
            target: "holosmith::rpc",
            session = %mapping.session_id,
            unknown = mapping.unknown,
            "records of a type this build does not map were dropped; pi may have grown a frame"
        );
    }
}

/// One frame, serialised once.
fn frame(kind: &'static str, payload: Value) -> Mapped {
    Mapped::Frame {
        kind,
        payload: payload.to_string(),
    }
}

/// A tool's result as text: the `text` blocks of `result.content`, in order.
///
/// A result is `{"content": [...], "details": {...}}` (`docs/json.md`). The content blocks are what the tool
/// printed; a block that is not text is named in brackets rather than dropped, so a result carrying an image
/// leaves a trace in the transcript's text (`[image]`) beside the stored picture names in `images`. The
/// brackets are the *type*, which is why an image reads as `[image]` whether one was stored or not: the text
/// is a description of the blocks, and the frames are where the detail lives. `details` and pi's own
/// `structuredContent` are **not** kept — a `bash` tool's exit code lives there, and ADR-0041 says so, so
/// that `tool.end`'s `status` is read as the tool's verdict rather than as a status code that was thrown away
/// by accident. A result that is absent or `null` is the empty string: a tool that printed nothing, not the
/// text `null`.
fn result_text(result: Option<&Value>) -> String {
    let Some(result) = result else {
        return String::new();
    };
    let Some(blocks) = result.get("content").and_then(Value::as_array) else {
        // No `content` at all: the whole value's JSON, which is at least true — but `null` is nothing, not the
        // word "null".
        return match result {
            Value::Null => String::new(),
            other => other.to_string(),
        };
    };
    let mut text = String::new();
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            other => text.push_str(&format!("[{other}]")),
        }
    }
    text
}

/// A record's `message.role`, when it names one.
fn role(record: &Value) -> Option<&str> {
    record.get("message")?.get("role")?.as_str()
}

/// The image blocks of a tool's result, in order, as the store will take them.
///
/// pi's block is `{"type": "image", "mimeType": "image/png", "data": "<base64>"}`. The block's **index in
/// `result.content`** is what the reference names, so two images of one result cannot be confused and a frame's
/// entry says where in the result it came from. A block that is an image but carries no `data`, or no
/// `mimeType`, is skipped: there is nothing to store, and the text already says `[image]`.
fn result_images(result: Option<&Value>) -> Vec<tool::Image> {
    let Some(blocks) = result
        .and_then(|result| result.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            if block.get("type").and_then(Value::as_str) != Some("image") {
                return None;
            }
            let mime = block.get("mimeType").and_then(Value::as_str)?;
            let data = block.get("data").and_then(Value::as_str)?;
            if mime.is_empty() || data.is_empty() {
                return None;
            }
            Some(tool::Image {
                index,
                mime: mime.to_string(),
                data: data.to_string(),
            })
        })
        .collect()
}

/// An assistant message's usage as the contract's two counts, or `None` when the agent reported nothing.
///
/// **A prompt's cached part is part of what it cost** (ADR-0042). pi reports `input`, `output`,
/// `cacheRead`, `cacheWrite`, `reasoning` and `totalTokens`, and a conversation with a warm prompt is mostly
/// cache reads: counting only `input` would make the budget a fraction of the bill. So
/// `in = input + cacheRead + cacheWrite` and `out = output`. `reasoning` is **not** added: the fixtures show it
/// already inside `output` (the thinking turn reports 1492 in, 10 out and `reasoning: 4`, with
/// `totalTokens: 1502`). The arithmetic is not this comment's to assert — `tests/rpc.rs` checks
/// `in + out == totalTokens` on every record of every committed fixture, which is what makes a provider that
/// changes its mind visible.
///
/// **A report of nothing is not a report.** A provider that answers zeroes would otherwise leave the screen
/// unable to say "no usage reported" and the budget counting a zero for ever, so a sum of zero answers `None`
/// — the same answer as a message with no `usage` at all. Spec item 13 has one condition, not two.
fn usage(message: &Value) -> Option<Value> {
    let counts = message.get("usage")?;
    let part = |field: &str| counts.get(field).and_then(Value::as_u64).unwrap_or(0);
    let input = part("input") + part("cacheRead") + part("cacheWrite");
    let output = part("output");
    if input == 0 && output == 0 {
        return None;
    }
    Some(json!({ "in": input, "out": output }))
}

/// The names of the message's tool calls, in the order the provider emitted them.
///
/// A *summary*: the acceptance asks `message.end` to carry one, and the arguments and the output belong to
/// `tool.start`/`tool.end` rather than to the message's own record. A block with no name is skipped
/// rather than rendered empty, the same rule the page applies to a frame it cannot draw.
fn tool_calls(message: &Value) -> Vec<Value> {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
                .filter_map(|block| block.get("name").and_then(Value::as_str))
                .map(|name| Value::String(name.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frames `records` produced, with the settlements they carried.
    fn run(mapping: &mut Mapping, records: &[Value]) -> (Vec<(String, Value)>, Vec<bool>) {
        let mut frames = Vec::new();
        let mut settled = Vec::new();
        for record in records {
            match mapping.map(record) {
                Mapped::Settled { aborted } => settled.push(aborted),
                mapped => {
                    if let Some((kind, payload)) = mapped.into_frame() {
                        frames.push((
                            kind.to_string(),
                            serde_json::from_str(&payload).expect("a frame's payload is JSON"),
                        ));
                    }
                }
            }
        }
        (frames, settled)
    }

    /// The session the unit tests' mapping is for. A constant so a test that needs a second one (a cancel's
    /// response, say) does not have to shadow the `mapping()` helper to build it.
    const SESSION: &str = "0123456789abcdef";

    fn mapping() -> Mapping {
        Mapping::new(SESSION)
    }

    fn update(event: Value) -> Value {
        json!({"type": "message_update", "usage": {}, "assistantMessageEvent": event})
    }

    fn assistant_start() -> Value {
        json!({"type": "message_start", "message": {"role": "assistant", "content": [], "stopReason": "pending"}})
    }

    #[test]
    fn a_text_delta_becomes_one_frame_with_the_message_id_and_the_kind() {
        let mut mapping = mapping();
        assert_eq!(mapping.map(&assistant_start()), Mapped::Nothing);
        let (frames, _) = run(
            &mut mapping,
            &[update(
                json!({"type": "text_delta", "contentIndex": 0, "delta": "hello "}),
            )],
        );
        assert_eq!(frames.len(), 1);
        let (kind, payload) = &frames[0];
        assert_eq!(kind, MESSAGE_DELTA);
        assert_eq!(payload["kind"], "text");
        assert_eq!(payload["text"], "hello ");
        let id = payload["message_id"].as_str().expect("an id");
        assert!(!id.is_empty());
        assert_eq!(payload.as_object().expect("an object").len(), 3);
    }

    #[test]
    fn a_thinking_delta_carries_the_thinking_kind() {
        let mut mapping = mapping();
        run(&mut mapping, &[assistant_start()]);
        let (frames, _) = run(
            &mut mapping,
            &[update(
                json!({"type": "thinking_delta", "contentIndex": 0, "delta": "hmm"}),
            )],
        );
        assert_eq!(frames[0].0, MESSAGE_DELTA);
        assert_eq!(frames[0].1["kind"], "thinking");
        assert_eq!(frames[0].1["text"], "hmm");
    }

    #[test]
    fn every_delta_of_one_message_keeps_its_id_and_the_next_message_gets_another() {
        let mut mapping = mapping();
        let (frames, _) = run(
            &mut mapping,
            &[
                assistant_start(),
                update(json!({"type": "text_delta", "delta": "one"})),
                update(json!({"type": "thinking_delta", "delta": "two"})),
                assistant_start(),
                update(json!({"type": "text_delta", "delta": "three"})),
            ],
        );
        let first = frames[0].1["message_id"].as_str().expect("an id");
        assert_eq!(frames[1].1["message_id"], first);
        assert_ne!(frames[2].1["message_id"], first);
    }

    #[test]
    fn a_completed_assistant_message_ends_with_usage_stop_reason_and_tool_calls() {
        let mut mapping = mapping();
        let (frames, _) = run(
            &mut mapping,
            &[
                assistant_start(),
                update(json!({"type": "text_delta", "delta": "let me look"})),
                json!({"type": "message_end", "message": {
                    "role": "assistant",
                    "stopReason": "toolUse",
                    "usage": {"input": 991, "output": 4, "cacheRead": 12, "totalTokens": 1007},
                    "content": [
                        {"type": "text", "text": "let me look"},
                        {"type": "toolCall", "id": "call_1", "name": "bash", "arguments": {"command": "ls"}},
                        {"type": "toolCall", "id": "call_2", "name": "read", "arguments": {}},
                    ],
                }}),
            ],
        );
        let (kind, payload) = frames.last().expect("the message ended");
        assert_eq!(kind, MESSAGE_END);
        assert_eq!(payload["message_id"], frames[0].1["message_id"]);
        assert_eq!(
            payload["usage"],
            json!({"in": 1003, "out": 4}),
            "991 prompt tokens plus the 12 the message reports as cache reads"
        );
        assert_eq!(payload["stop_reason"], "toolUse");
        assert_eq!(payload["tool_calls"], json!(["bash", "read"]));
    }

    #[test]
    fn a_usage_of_nothing_is_no_usage_at_all() {
        // Spec item 13's condition is one: "if pi does not report usage, the token budget is disabled". A
        // provider that answers zeroes is reporting nothing, and a `usage` key of zeroes would leave the screen
        // unable to say so and the budget counting for ever.
        let mut mapping = mapping();
        let (frames, _) = run(
            &mut mapping,
            &[json!({"type": "message_end", "message": {
                "role": "assistant",
                "stopReason": "stop",
                "content": [],
                "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0},
            }})],
        );
        assert_eq!(frames[0].0, MESSAGE_END);
        assert!(
            frames[0].1.get("usage").is_none(),
            "a zero report became a usage key: {}",
            frames[0].1
        );
        // And a message whose prompt was all cache still reports something: the cache is what it cost.
        let (frames, _) = run(
            &mut mapping,
            &[json!({"type": "message_end", "message": {
                "role": "assistant", "stopReason": "stop", "content": [],
                "usage": {"input": 0, "output": 0, "cacheRead": 512, "cacheWrite": 0, "totalTokens": 512},
            }})],
        );
        assert_eq!(frames[0].1["usage"], json!({"in": 512, "out": 0}));
        // And the third counter, which no committed fixture exercises: without this arm, deleting
        // `cacheWrite` from the sum would leave the whole suite green.
        let (frames, _) = run(
            &mut mapping,
            &[json!({"type": "message_end", "message": {
                "role": "assistant", "stopReason": "stop", "content": [],
                "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 512, "totalTokens": 512},
            }})],
        );
        assert_eq!(
            frames[0].1["usage"],
            json!({"in": 512, "out": 0}),
            "a prompt the provider had to write to its cache still cost something"
        );
    }

    #[test]
    fn a_message_with_no_usage_ends_without_the_key() {
        let mut mapping = mapping();
        let (frames, _) = run(
            &mut mapping,
            &[json!({"type": "message_end", "message": {
                "role": "assistant", "stopReason": "stop", "content": [],
            }})],
        );
        assert_eq!(frames[0].0, MESSAGE_END);
        assert!(frames[0].1.get("usage").is_none(), "{}", frames[0].1);
        assert_eq!(frames[0].1["stop_reason"], "stop");
        assert_eq!(frames[0].1["tool_calls"], json!([]));
    }

    #[test]
    fn a_users_message_and_the_system_prompt_are_not_blocks() {
        let mut mapping = mapping();
        let (frames, _) = run(
            &mut mapping,
            &[
                json!({"type": "message_start", "message": {"role": "system", "content": ""}}),
                json!({"type": "message_end", "message": {"role": "system", "content": ""}}),
                json!({"type": "message_start", "message": {"role": "user", "content": "hi"}}),
                json!({"type": "message_end", "message": {"role": "user", "content": "hi"}}),
            ],
        );
        assert!(frames.is_empty(), "{frames:?}");
        assert_eq!(
            mapping.skipped(),
            0,
            "a system message is mapped, not skipped"
        );
    }

    #[test]
    fn pis_own_turn_boundaries_are_declined_and_the_settle_ends_the_turn() {
        // pi's `turn_start`/`turn_end` bracket one *assistant response*, and a run can hold several of them. The
        // contract's turn is the prompt, whose pair `pump` writes, so these two must produce nothing here.
        let mut mapping = mapping();
        let (frames, settled) = run(
            &mut mapping,
            &[
                json!({"type": "turn_start"}),
                json!({"type": "turn_end", "message": {}, "toolResults": []}),
                json!({"type": "agent_settled", "aborted": true}),
            ],
        );
        assert!(frames.is_empty(), "{frames:?}");
        assert_eq!(settled, vec![true]);
        assert_eq!((mapping.skipped(), mapping.unknown()), (2, 0));
    }

    #[test]
    fn a_record_this_build_knows_is_declined_and_one_it_does_not_is_unknown() {
        let mut mapping = mapping();
        let (frames, settled) = run(
            &mut mapping,
            &[
                json!({"type": "queue_update", "steering": [], "followUp": []}),
                json!({"type": "agent_start"}),
                json!({"type": "agent_end", "messages": [], "willRetry": false}),
                json!({"type": "session_info_changed", "name": null}),
                assistant_start(),
                update(json!({"type": "toolcall_delta", "contentIndex": 1, "delta": "{\"comm"})),
                update(json!({"type": "invented_by_a_later_release"})),
                json!({"type": "extension_error", "extensionPath": "/x.ts", "error": "boom"}),
                json!({"type": "agent_settled", "aborted": false}),
            ],
        );
        assert!(frames.is_empty(), "{frames:?}");
        assert_eq!(settled, vec![false]);
        // Declined, because pi documents them and this contract has no frame for them: the queue update,
        // `agent_start`, `agent_end`, the session-info change, the toolcall delta (the provider's, declined so
        // that one call is one block) and the extension error. Unknown, and therefore worth a `warn`: the invented nested event type. `assistant_start` is
        // mapped, not declined.
        assert_eq!((mapping.skipped(), mapping.unknown()), (7, 1));
    }

    #[test]
    fn a_malformed_update_ends_and_starts_are_skipped_rather_than_fatal() {
        let mut mapping = mapping();
        let (frames, _) = run(
            &mut mapping,
            &[
                // No nested event at all.
                json!({"type": "message_update", "usage": {}}),
                // A nested value that is not an object.
                json!({"type": "message_update", "assistantMessageEvent": "text_delta"}),
                // An event with no type.
                json!({"type": "message_update", "assistantMessageEvent": {"delta": "x"}}),
                // A delta with no text.
                update(json!({"type": "text_delta", "contentIndex": 0})),
                // An end with no message, and one whose message is not an object.
                json!({"type": "message_end"}),
                json!({"type": "message_end", "message": "assistant"}),
                // A record with no type at all.
                json!({"id": "x"}),
            ],
        );
        assert!(frames.is_empty(), "{frames:?}");
        assert_eq!(
            (mapping.skipped(), mapping.unknown()),
            (7, 7),
            "a shape the documentation does not describe is a type this build does not know"
        );
    }

    #[test]
    fn a_delta_with_no_message_start_keeps_its_text_under_a_minted_id() {
        let mut mapping = mapping();
        let (frames, _) = run(
            &mut mapping,
            &[update(
                json!({"type": "text_delta", "contentIndex": 0, "delta": "orphan"}),
            )],
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].1["text"], "orphan");
        assert!(
            frames[0].1["message_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
    }

    #[test]
    fn an_empty_delta_adds_no_row() {
        let mut mapping = mapping();
        run(&mut mapping, &[assistant_start()]);
        let (frames, _) = run(
            &mut mapping,
            &[update(
                json!({"type": "text_delta", "contentIndex": 0, "delta": ""}),
            )],
        );
        assert!(frames.is_empty());
        assert_eq!(mapping.skipped(), 0, "an empty delta is mapped to nothing");
    }

    #[test]
    fn a_dialog_is_answered_cancelled_and_a_fire_and_forget_request_is_not_answered_at_all() {
        // pi's own two categories (`docs/rpc-extension-ui.md`). The first blocks the run until a response with
        // the request's id arrives, so it is cancelled; the second never waits for anything, so an answer would
        // be an invented exchange between this build and an extension.
        let mut mapping = mapping();
        let (frames, settled) = run(
            &mut mapping,
            &[
                json!({"type": "extension_ui_request", "id": "uuid-1", "method": "select",
                       "title": "Allow?", "options": ["Allow", "Block"]}),
                json!({"type": "extension_ui_request", "id": "uuid-2", "method": "notify",
                       "message": "hello", "notifyType": "info"}),
                json!({"type": "agent_settled", "aborted": false}),
            ],
        );
        assert!(settled == vec![false]);
        assert_eq!(
            mapping.skipped(),
            1,
            "the notify is the only documented record with no frame"
        );
        assert_eq!(mapping.unknown(), 0);

        // The frame the replay can build: `into_frame` answers it, because a cancel needs nothing stored.
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, TURN_ERROR);
        assert_eq!(frames[0].1["method"], "select");
        assert_eq!(
            frames[0].1["reason"],
            json!(
                "an extension asked for select, and a session cannot answer a dialog, so it was cancelled"
            )
        );

        // The line pi is waiting for, which is the pump's to write.
        let record = json!({"type": "extension_ui_request", "id": "uuid-1", "method": "confirm"});
        let Mapped::Cancel(cancel) = Mapping::new(SESSION).map(&record) else {
            panic!("a dialog is a cancel");
        };
        assert_eq!(
            cancel.response(),
            json!({"type": "extension_ui_response", "id": "uuid-1", "cancelled": true})
        );
        assert_eq!(cancel.method, "confirm");
    }

    #[test]
    fn a_request_with_no_id_or_an_unknown_method_is_unknown_and_worth_a_warning() {
        // A dialog with no id cannot be dismissed — the response has to repeat it — and a method this build has
        // never seen may well be one that blocks. Both are the `warn` count, because a session that hangs on
        // either would do so with nothing in the log.
        let mut mapping = mapping();
        let (frames, _) = run(
            &mut mapping,
            &[
                json!({"type": "extension_ui_request", "method": "select", "title": "no id"}),
                json!({"type": "extension_ui_request", "id": "", "method": "input"}),
                json!({"type": "extension_ui_request", "id": "uuid-3", "method": "invented_method"}),
                json!({"type": "extension_ui_request", "id": "uuid-4"}),
            ],
        );
        assert!(frames.is_empty(), "{frames:?}");
        assert_eq!((mapping.skipped(), mapping.unknown()), (4, 4));
    }

    #[test]
    fn minted_ids_are_unique_across_turns_in_one_process() {
        let mut first = mapping();
        let mut second = mapping();
        let (frames, _) = run(
            &mut first,
            &[
                assistant_start(),
                update(json!({"type": "text_delta", "delta": "a"})),
            ],
        );
        let (next, _) = run(
            &mut second,
            &[
                assistant_start(),
                update(json!({"type": "text_delta", "delta": "b"})),
            ],
        );
        assert_ne!(frames[0].1["message_id"], next[0].1["message_id"]);
    }
}
