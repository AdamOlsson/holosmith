//! The mapping from pi's RPC records to the transcript's frames (HS-032).
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
//! **Three rules the mapping keeps:**
//!
//! - **pi's messages have no id, so one is minted.** A transcript block is keyed by the message id, and the
//!   log is durable, so two turns must never mint the same one: the id carries the process's start instant
//!   and a process-wide counter (`msg-<nanos>-<n>`), the same shape the stub's ids have (HS-011's reason: a
//!   restart that started over at `msg-1` would fold a new turn into a previous turn's block).
//! - **Unknown is counted, never fatal.** A record type or nested event type this build does not map — a
//!   queue update, an extension error, and every `toolcall_*` until HS-033 — answers
//!   [`Mapped::Nothing`] and moves a counter, and [`pump`] logs that counter once per turn. A mapping layer
//!   that died on a frame it did not recognise would take the session with it.
//! - **The turn ends on `agent_settled`, not on `agent_end`.** `agent_end` closes one low-level agent run and
//!   pi can still retry, recover from an overflow, or deliver a queued message after it; `agent_settled` is
//!   pi saying it has no work left (`docs/json.md`).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::agent::{AgentRecords, Received};
use crate::backend::{BackendError, TurnEnd};
use crate::clock;
use crate::events::{EventWriter, MESSAGE_DELTA, MESSAGE_END, NewEvent, TURN_END, TURN_START};

/// The prefix of a minted message id. It is the stub's, so a log cannot tell which backend wrote a block.
const MESSAGE_PREFIX: &str = "msg";

/// A process-wide counter for minted ids, because a session runs several turns in one process.
static MINTED: AtomicU64 = AtomicU64::new(1);

/// The instant this process started, which keeps two runs of the binary from minting the same ids.
///
/// The counter alone cannot be the id: a restarted process would start over at 1 and fold a new turn's
/// deltas into a previous turn's block against a durable log.
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
}

/// One turn's mapping: which of pi's messages is current, and how many records it declined.
///
/// One value per turn, because the id it mints is per message and the count is per turn. It holds no clock,
/// no database and no agent — a record in, a frame out — so a fixture can be replayed through it without a
/// container (which is what `tests/rpc.rs` does).
#[derive(Debug)]
pub struct Mapping {
    session_id: String,
    /// The assistant message pi is currently streaming, if any.
    current: Option<String>,
    /// How many records this turn produced no frame for.
    skipped: u64,
}

impl Mapping {
    /// A mapping for one turn of `session_id`.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            current: None,
            skipped: 0,
        }
    }

    /// How many records this turn produced no frame for: the number [`pump`] logs once at the end.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Map one record pi printed.
    ///
    /// The record is pi's own JSONL, already parsed: the reader rejects what is not JSON (`agent.rs`), so
    /// everything that reaches here is an object of some kind and the question is only what it means.
    pub fn map(&mut self, record: &Value) -> Mapped {
        match record.get("type").and_then(Value::as_str) {
            Some("turn_start") => frame(TURN_START, json!({})),
            Some("turn_end") => frame(TURN_END, json!({})),
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
            // Everything else pi emits — `agent_start`, `agent_end`, `queue_update`,
            // `thinking_level_changed`, `extension_ui_request` (HS-035), `extension_error`, the compaction
            // and retry records — has no frame in this contract yet.
            _ => self.skip(),
        }
    }

    /// One `message_update`: its nested event decides whether it is a delta.
    fn update(&mut self, record: &Value) -> Mapped {
        let Some(event) = record.get("assistantMessageEvent") else {
            return self.skip();
        };
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return self.skip();
        };
        let kind = match kind {
            "text_delta" => "text",
            "thinking_delta" => "thinking",
            // The ends of a block, the start of the provider stream and its `done`/`error`: the content is
            // already on screen as deltas (ADR-0040's D3), and `toolcall_*` is HS-033's.
            "text_start" | "text_end" | "thinking_start" | "thinking_end" | "start" | "done"
            | "error" => return Mapped::Nothing,
            _ => return self.skip(),
        };
        let Some(text) = event.get("delta").and_then(Value::as_str) else {
            return self.skip();
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
            return self.skip();
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            // The user's own prompt, pi's system message, and (until HS-033) a tool result. None of them is
            // a transcript block.
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

    /// Count a record this build has no frame for, and answer that there is none.
    fn skip(&mut self) -> Mapped {
        self.skipped += 1;
        Mapped::Nothing
    }
}

/// Drive one turn: map pi's records onto `events` until the agent settles.
///
/// Called by [`crate::backend::DockerBackend::start_turn`] after it has written the prompt, with a
/// subscription taken *before* that write so no record can be lost between the two. The caller's
/// subscription is the only ordering the transcript relies on: records arrive in the order pi printed them,
/// and each frame is appended in that order, so the writer's seqs are that order too.
///
/// **Nothing here is fatal except the agent going quiet.** A lagged subscriber (the buffer filled) loses
/// whole records — text the operator will not see — and says so once; a record this build does not map is
/// counted. Only a closed stream ends the turn without an answer, because a turn with no `agent_settled` is a
/// process that died mid-turn, and the row's own exit classification (`crate::exit`) is what records that.
pub async fn pump(
    records: &mut AgentRecords,
    mapping: &mut Mapping,
    events: &EventWriter,
) -> Result<TurnEnd, BackendError> {
    loop {
        match records.recv().await {
            Received::Record(record) => {
                // One record, one mapping: mapping it twice would mint two message ids for one message.
                let mapped = mapping.map(&record);
                let (kind, payload) = match mapped {
                    Mapped::Settled { aborted } => {
                        log_skipped(mapping);
                        return Ok(if aborted {
                            TurnEnd::Aborted
                        } else {
                            TurnEnd::Settled
                        });
                    }
                    Mapped::Frame { kind, payload } => (kind, payload),
                    Mapped::Nothing => continue,
                };
                let event = NewEvent {
                    session_id: mapping.session_id.clone(),
                    kind: kind.to_string(),
                    payload,
                    created_at: clock::unix_now(),
                };
                // A failed append means the writer has stopped, which in a served process means the process
                // is going down. The loss is logged and the turn carries on, exactly as the stub's own turn
                // does: the turn *ran*, so failing it here would write a session that did its work into
                // `error` because the process was leaving.
                if let Err(error) = events.append(event).await {
                    tracing::error!(
                        target: "holosmith::rpc",
                        session = %mapping.session_id,
                        %error,
                        "a turn frame was not queued"
                    );
                }
            }
            Received::Lagged(missed) => {
                tracing::warn!(
                    target: "holosmith::rpc",
                    session = %mapping.session_id,
                    missed,
                    "the agent's records fell behind; those frames are lost, and the turn continues"
                );
            }
            Received::Closed => {
                log_skipped(mapping);
                return Err(BackendError::failed(
                    "the turn: the agent's output ended before it settled".to_string(),
                ));
            }
        }
    }
}

/// One line per turn when the mapping declined something, so a new record type is findable.
fn log_skipped(mapping: &Mapping) {
    if mapping.skipped == 0 {
        return;
    }
    tracing::debug!(
        target: "holosmith::rpc",
        session = %mapping.session_id,
        skipped = mapping.skipped,
        "records this build has no frame for were dropped"
    );
}

/// One frame, serialised once.
fn frame(kind: &'static str, payload: Value) -> Mapped {
    Mapped::Frame {
        kind,
        payload: payload.to_string(),
    }
}

/// A record's `message.role`, when it names one.
fn role(record: &Value) -> Option<&str> {
    record.get("message")?.get("role")?.as_str()
}

/// An assistant message's usage as the contract's two counts, or `None` when pi reported neither.
///
/// `in` is pi's `input` and `out` its `output` — the two counts a provider bills for the message itself. The
/// cache counters are separate fields and are deliberately not folded in: HS-034 owns "real token counts" and
/// with it any decision about how a cached prompt is counted, and inventing the arithmetic here would make
/// that story's numbers disagree with this one's.
fn usage(message: &Value) -> Option<Value> {
    let counts = message.get("usage")?;
    let input = counts.get("input")?.as_u64()?;
    let output = counts.get("output")?.as_u64()?;
    Some(json!({ "in": input, "out": output }))
}

/// The names of the message's tool calls, in the order the provider emitted them.
///
/// A *summary*: the acceptance asks `message.end` to carry one, and the arguments and the output belong to
/// `tool.start`/`tool.end` (HS-033) rather than to the message's own record. A block with no name is skipped
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
                Mapped::Frame { kind, payload } => frames.push((
                    kind.to_string(),
                    serde_json::from_str(&payload).expect("a frame's payload is JSON"),
                )),
                Mapped::Settled { aborted } => settled.push(aborted),
                Mapped::Nothing => {}
            }
        }
        (frames, settled)
    }

    fn mapping() -> Mapping {
        Mapping::new("0123456789abcdef")
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
        assert_eq!(payload["usage"], json!({"in": 991, "out": 4}));
        assert_eq!(payload["stop_reason"], "toolUse");
        assert_eq!(payload["tool_calls"], json!(["bash", "read"]));
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
    fn the_turn_boundaries_are_empty_frames_and_the_settle_ends_the_turn() {
        let mut mapping = mapping();
        let (frames, settled) = run(
            &mut mapping,
            &[
                json!({"type": "turn_start"}),
                json!({"type": "turn_end", "message": {}, "toolResults": []}),
                json!({"type": "agent_settled", "aborted": true}),
            ],
        );
        assert_eq!(frames[0].0, TURN_START);
        assert_eq!(frames[0].1, json!({}));
        assert_eq!(frames[1].0, TURN_END);
        assert_eq!(frames[1].1, json!({}));
        assert_eq!(settled, vec![true]);
    }

    #[test]
    fn an_unknown_record_and_an_unknown_nested_event_are_counted_and_harmless() {
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
        // `queue_update`, `agent_start`, `agent_end`, `session_info_changed`, the toolcall delta (HS-033's),
        // the invented event and the extension error: seven records with no frame. `assistant_start` is
        // mapped, not skipped.
        assert_eq!(mapping.skipped(), 7);
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
        assert_eq!(mapping.skipped(), 7);
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
