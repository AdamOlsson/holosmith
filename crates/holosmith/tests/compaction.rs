//! Compaction (spec item 15): an old message's deltas become one snapshot.
//!
//! The job is a task rather than an endpoint, so the tests call its one pass — `Compactor::sweep` is public
//! for exactly that reason, as `Sweeper::sweep` and `Deleter::remove` are — and assert the log, the seqs and
//! the rendered transcript.
//!
//! **No test ages a row.** The window is a constructor argument rather than a config key (ADR-0032), and an
//! event's `created_at` is the *producer's*, so a test writes its message with an instant two hours old and
//! "older than the window" is a fact instead of a wait. The shipped artifact cannot do that — it writes
//! `now` — which is why `case_compaction` in `scripts/verify.sh` is the one that ages rows in a database.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use holosmith::clock;
use holosmith::compactor::Compactor;
use holosmith::config::Config;
use holosmith::events::{
    Event, MESSAGE_DELTA, MESSAGE_END, MESSAGE_SNAPSHOT, NewEvent, TURN_END, TURN_START,
};
use holosmith::http::AppState;
use holosmith::sessions::NewSession;
use holosmith::sessions::state::{Transitioner, Trigger};
use serde_json::{Value, json};

mod support;
use support::{PATIENCE, TempDir, state_in};

/// How far in the past a test writes the rows it wants folded.
const OLD: i64 = 2 * 60 * 60;

/// The window every test uses. Shorter than the spec's day so a test can also write a message that is
/// *inside* it, and passed to the constructor because the production window is a constant and not a key.
const WINDOW: Duration = Duration::from_secs(60 * 60);

fn config() -> Config {
    Config {
        max_live_sessions: 8,
        ..Config::default()
    }
}

/// The wiring `serve` builds, with a window of the test's own.
fn compactor(state: &AppState) -> Compactor {
    Compactor::with_window(Arc::clone(&state.database), WINDOW)
}

fn create(state: &AppState) -> String {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .sessions()
        .create(
            &NewSession {
                project: "holosmith".to_string(),
                model: "claude-sonnet-4.5".to_string(),
                thinking: "high".to_string(),
                network: false,
                client_key: None,
                base_commit: None,
            },
            clock::unix_now(),
        )
        .expect("a session is created")
        .id
}

/// A state with one session in `idle` — the state compaction may fold — reached through the one state
/// function rather than written.
async fn idle_session(dir: &TempDir) -> (AppState, String) {
    let state = state_in(dir, config());
    let id = create(&state);
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(&id, Trigger::PiReady, None)
        .await
        .expect("provisioning finished, so the session is idle");
    (state, id)
}

async fn append(state: &AppState, session: &str, kind: &str, payload: Value, at: i64) {
    state
        .events
        .append(NewEvent {
            session_id: session.to_string(),
            kind: kind.to_string(),
            payload: payload.to_string(),
            created_at: at,
        })
        .await
        .expect("the writer is running");
}

/// One message the way a backend writes it: its thinking deltas, its text deltas, then its `message.end`,
/// every row carrying an instant `age` seconds in the past.
async fn message(
    state: &AppState,
    session: &str,
    id: &str,
    text: &[&str],
    thinking: &[&str],
    age: i64,
) {
    let at = clock::unix_now() - age;
    for piece in thinking {
        append(
            state,
            session,
            MESSAGE_DELTA,
            json!({"message_id": id, "kind": "thinking", "text": piece}),
            at,
        )
        .await;
    }
    for piece in text {
        append(
            state,
            session,
            MESSAGE_DELTA,
            json!({"message_id": id, "kind": "text", "text": piece}),
            at,
        )
        .await;
    }
    append(
        state,
        session,
        MESSAGE_END,
        json!({"message_id": id, "usage": {"in": 10, "out": 5}}),
        at,
    )
    .await;
}

/// A session's whole log, in seq order.
fn log(state: &AppState, session: &str) -> Vec<Event> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .events()
        .read_since(session, 0, 10_000)
        .expect("the log reads")
}

/// Wait until the log satisfies `done`: the writer commits on its own tick, and **coalescing means the
/// number of rows a message becomes is not the number of events a test appended** (ADR-0031) — so a test
/// waits for the row it means, never for a count. Waiting on a count is how its own live-path test
/// passed for the wrong reason.
async fn wait_for_log(
    state: &AppState,
    session: &str,
    what: &str,
    mut done: impl FnMut(&[Event]) -> bool,
) {
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        let log = log(state, session);
        if done(&log) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not reach the log: {log:#?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The `message_id` an event's payload names, if it names one.
fn payload_id(event: &Event) -> Option<String> {
    serde_json::from_str::<Value>(&event.payload)
        .ok()?
        .get("message_id")?
        .as_str()
        .map(str::to_string)
}

/// Wait until every one of `ids` has its `message.end` row — the last event a message produces, so its
/// presence is "the message is committed".
async fn wait_for_ends(state: &AppState, session: &str, ids: &[&str]) {
    let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    wait_for_log(state, session, "the messages' ends", |log| {
        ids.iter().all(|id| {
            log.iter()
                .any(|event| event.kind == MESSAGE_END && payload_id(event).as_deref() == Some(id))
        })
    })
    .await;
}

fn snapshot_of<'a>(log: &'a [Event], id: &str) -> Option<&'a Event> {
    log.iter()
        .find(|event| event.kind == MESSAGE_SNAPSHOT && payload_id(event).as_deref() == Some(id))
}

/// The page's transcript model, reduced to what a replay can differ in: each message's text and thinking,
/// in the order the blocks were created.
///
/// It mirrors `web/src/transcript.ts`, which is the thing the acceptance means by "an identical
/// transcript": a delta **appends** to its kind's node, a snapshot **replaces** both nodes of its block,
/// and every other kind is not the transcript's to draw. A second rendering rule here would be a test of
/// itself rather than of the log.
fn transcript(log: &[Event]) -> Vec<(String, String, String)> {
    let mut order: Vec<String> = Vec::new();
    let mut blocks: HashMap<String, (String, String)> = HashMap::new();
    for event in log {
        let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
            continue;
        };
        let Some(id) = payload["message_id"].as_str() else {
            continue;
        };
        if !order.iter().any(|seen| seen == id) {
            order.push(id.to_string());
        }
        let block = blocks.entry(id.to_string()).or_default();
        match event.kind.as_str() {
            MESSAGE_DELTA => match payload["kind"].as_str() {
                Some("text") => block
                    .0
                    .push_str(payload["text"].as_str().unwrap_or_default()),
                Some("thinking") => block
                    .1
                    .push_str(payload["text"].as_str().unwrap_or_default()),
                _ => {}
            },
            MESSAGE_SNAPSHOT => {
                *block = (
                    payload["text"].as_str().unwrap_or_default().to_string(),
                    payload["thinking"].as_str().unwrap_or_default().to_string(),
                );
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .map(|id| {
            let (text, thinking) = blocks.remove(&id).unwrap_or_default();
            (id, text, thinking)
        })
        .collect()
}

#[tokio::test]
async fn an_old_message_becomes_one_snapshot_holding_the_whole_text() {
    let dir = TempDir::new("compaction-old");
    let (state, id) = idle_session(&dir).await;
    message(
        &state,
        &id,
        "m1",
        &["Hello, ", "world"],
        &["think", "ing"],
        OLD,
    )
    .await;
    wait_for_ends(&state, &id, &["m1"]).await;

    compactor(&state).sweep().await;

    let after = log(&state, &id);
    let snapshot = snapshot_of(&after, "m1").expect("the message became a snapshot");
    // The pinned payload, key order included: a snapshot is the frame ADR-0018 fixed, and the renderer
    // reads it by these three names.
    assert_eq!(
        snapshot.payload, r#"{"message_id":"m1","text":"Hello, world","thinking":"thinking"}"#,
        "the snapshot is not the contract's shape"
    );
    assert!(
        !after.iter().any(|event| event.kind == MESSAGE_DELTA),
        "a delta row survived compaction: {after:#?}"
    );
    assert_eq!(
        after
            .iter()
            .filter(|event| event.kind == MESSAGE_END)
            .count(),
        1,
        "compaction removed a frame that is not a delta: {after:#?}"
    );
}

#[tokio::test]
async fn the_snapshot_takes_the_highest_removed_seq_and_the_counter_does_not_rewind() {
    let dir = TempDir::new("compaction-seq");
    let (state, id) = idle_session(&dir).await;
    // Two kinds, because coalescing merges consecutive deltas of one kind into one row (ADR-0031): a
    // message of text alone can be a single delta row, and "the highest removed seq" over one row is a
    // claim that cannot fail.
    message(&state, &id, "m1", &["a", "b"], &["t"], OLD).await;
    wait_for_ends(&state, &id, &["m1"]).await;
    let before = log(&state, &id);
    let removed: Vec<i64> = before
        .iter()
        .filter(|event| event.kind == MESSAGE_DELTA)
        .map(|event| event.seq)
        .collect();
    assert!(
        removed.len() >= 2,
        "the seq rule cannot be checked over one removed row: {removed:?}"
    );
    let highest = *removed.last().expect("the message has deltas");
    let newest = before.last().expect("the log is not empty").seq;

    compactor(&state).sweep().await;

    let after = log(&state, &id);
    let snapshot = snapshot_of(&after, "m1").expect("the message became a snapshot");
    assert_eq!(
        snapshot.seq, highest,
        "the snapshot did not take the highest seq of the rows it removed"
    );
    for seq in &removed[..removed.len() - 1] {
        assert!(
            !after.iter().any(|event| event.seq == *seq),
            "seq {seq} should be a gap and is still in the log: {after:#?}"
        );
    }
    // The counter, behaviourally: `sessions.last_seq` is not exposed, and what matters is that the next
    // event is numbered after everything the log ever held — not after the seq a snapshot reused.
    append(&state, &id, TURN_END, json!({}), clock::unix_now()).await;
    wait_for_log(&state, &id, "the turn's end", |log| {
        log.iter().any(|event| event.kind == TURN_END)
    })
    .await;
    let next = log(&state, &id).last().expect("the new event").seq;
    assert_eq!(
        next,
        newest + 1,
        "seq assignment rewound past a seq compaction had reused"
    );
}

#[tokio::test]
async fn a_replay_through_the_compaction_boundary_renders_the_same_transcript() {
    let dir = TempDir::new("compaction-transcript");
    let (state, id) = idle_session(&dir).await;
    let now = clock::unix_now();
    append(&state, &id, TURN_START, json!({}), now - OLD).await;
    // The shape a backend produces: one message at a time, its deltas consecutive.
    message(
        &state,
        &id,
        "m1",
        &["first ", "message"],
        &["one", " thought"],
        OLD,
    )
    .await;
    message(&state, &id, "m2", &["second"], &[], OLD).await;
    append(&state, &id, TURN_END, json!({}), now - OLD).await;
    wait_for_log(&state, &id, "the turn's end", |log| {
        log.iter().any(|event| event.kind == TURN_END)
    })
    .await;

    let before = transcript(&log(&state, &id));
    let rows_before = log(&state, &id).len();

    compactor(&state).sweep().await;

    let after_log = log(&state, &id);
    assert_eq!(
        transcript(&after_log),
        before,
        "compaction changed what the page draws"
    );
    // Without this the equality above would hold for an unchanged log, which is the vacuous pass this
    // kind of test is most likely to produce.
    assert!(
        after_log.len() < rows_before,
        "nothing was compacted, so the transcripts were compared over the same log"
    );
    assert_eq!(
        before
            .iter()
            .map(|(id, text, thinking)| (id.as_str(), text.as_str(), thinking.as_str()))
            .collect::<Vec<_>>(),
        [("m1", "first message", "one thought"), ("m2", "second", ""),]
    );
}

#[tokio::test]
async fn a_message_inside_the_window_and_one_with_no_end_are_untouched() {
    let dir = TempDir::new("compaction-window");
    let (state, id) = idle_session(&dir).await;
    // Fresh: completed now, so the window has not passed.
    message(&state, &id, "fresh", &["just now"], &[], 0).await;
    // Open: deltas but no `message.end`, so it is not a completed message however old it is.
    append(
        &state,
        &id,
        MESSAGE_DELTA,
        json!({"message_id": "open", "kind": "text", "text": "still arriving"}),
        clock::unix_now() - OLD,
    )
    .await;
    wait_for_ends(&state, &id, &["fresh"]).await;
    let before = log(&state, &id);

    compactor(&state).sweep().await;

    assert_eq!(
        log(&state, &id),
        before,
        "a message the window does not cover was folded"
    );
}

#[tokio::test]
async fn a_session_with_a_turn_running_is_never_folded() {
    let dir = TempDir::new("compaction-running");
    let (state, id) = idle_session(&dir).await;
    message(&state, &id, "m1", &["a turn's text"], &[], OLD).await;
    wait_for_ends(&state, &id, &["m1"]).await;
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(&id, Trigger::Prompt, None)
        .await
        .expect("an idle session may be prompted");
    let before = log(&state, &id);

    compactor(&state).sweep().await;

    assert_eq!(
        log(&state, &id),
        before,
        "compaction folded a session whose turn is running"
    );
}

#[tokio::test]
async fn the_state_is_read_again_inside_the_lock_and_is_the_authority() {
    // `sweep` filters candidates before it takes the lock, so that filter is a guess about a moment that
    // has already passed. The per-session step re-reads the row inside the lock, and that re-read is what
    // the story's "never while a turn is running" rests on — `Transitioner::apply` takes the same lock, so
    // a prompt cannot move the row while the fold is deciding.
    let dir = TempDir::new("compaction-authority");
    let (state, id) = idle_session(&dir).await;
    message(&state, &id, "m1", &["a turn's text"], &[], OLD).await;
    wait_for_ends(&state, &id, &["m1"]).await;
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(&id, Trigger::Prompt, None)
        .await
        .expect("an idle session may be prompted");
    let before = log(&state, &id);

    let compaction = compactor(&state)
        .fold(&id)
        .expect("the fold answers rather than failing");

    assert_eq!(
        compaction,
        Default::default(),
        "the fold wrote to a session whose state it had not re-read"
    );
    assert_eq!(log(&state, &id), before, "the log changed");
}

#[tokio::test]
async fn a_delta_the_contract_does_not_describe_stops_the_whole_session() {
    // A fourth key is a payload ADR-0018 does not have, and it names no message the fold can attribute it
    // to — so folding the rows that *can* be attributed would delete text this pass cannot account for.
    let dir = TempDir::new("compaction-unreadable");
    let (state, id) = idle_session(&dir).await;
    // The bad row sits **before** the message's `message.end`, which is where a delta that could belong to a
    // message being folded is: a message's deltas all precede its own end, and the scan is bounded by the
    // newest candidate's end. A malformed delta above that bound belongs to a message still inside the
    // window, and a later pass will see it.
    append(
        &state,
        &id,
        MESSAGE_DELTA,
        json!({"message_id": "m1", "kind": "text", "text": "x", "extra": 1}),
        clock::unix_now() - OLD,
    )
    .await;
    message(&state, &id, "m1", &["readable"], &[], OLD).await;
    wait_for_ends(&state, &id, &["m1"]).await;
    let before = log(&state, &id);

    let compaction = compactor(&state).fold(&id).expect("the fold answers");

    assert_eq!(compaction.unreadable, 1, "{compaction:?}");
    assert_eq!(compaction.messages, 0, "{compaction:?}");
    assert_eq!(
        log(&state, &id),
        before,
        "a session with an unreadable delta was compacted anyway"
    );
}

#[tokio::test]
async fn a_second_pass_finds_nothing_to_fold() {
    let dir = TempDir::new("compaction-idempotent");
    let (state, id) = idle_session(&dir).await;
    message(&state, &id, "m1", &["a", "b"], &[], OLD).await;
    wait_for_ends(&state, &id, &["m1"]).await;

    compactor(&state).sweep().await;
    let after_first = log(&state, &id);
    compactor(&state).sweep().await;

    assert_eq!(
        log(&state, &id),
        after_first,
        "a second pass rewrote a message it had already folded"
    );
    assert_eq!(
        after_first
            .iter()
            .filter(|event| event.kind == MESSAGE_SNAPSHOT)
            .count(),
        1,
        "a second snapshot was written for one message: {after_first:#?}"
    );
}

#[tokio::test]
async fn one_pass_folds_at_most_the_limit() {
    let dir = TempDir::new("compaction-limit");
    let (state, id) = idle_session(&dir).await;
    for message_id in ["m1", "m2", "m3"] {
        message(&state, &id, message_id, &["text"], &[], OLD).await;
    }
    wait_for_ends(&state, &id, &["m1", "m2", "m3"]).await;
    let cutoff = clock::unix_now();

    let first = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .events()
            .compact(&id, cutoff, 2)
            .expect("the pass answers")
    };

    assert_eq!(first.messages, 2, "{first:?}");
    let after_first = log(&state, &id);
    assert_eq!(
        after_first
            .iter()
            .filter(|event| event.kind == MESSAGE_SNAPSHOT)
            .count(),
        2,
        "the limit did not bound the pass"
    );

    // The limit is a *window*, not a filter: the next pass must fold what this one left, which is the only
    // thing that makes a bounded pass converge. It holds because an already-folded message is recognised
    // by its snapshot — without that the second pass would re-read the same two oldest messages for ever and
    // the third would never be folded.
    let second = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .events()
            .compact(&id, cutoff, 2)
            .expect("the pass answers")
    };
    assert_eq!(second.messages, 1, "{second:?}");
    let after_second = log(&state, &id);
    for message_id in ["m1", "m2", "m3"] {
        assert!(
            snapshot_of(&after_second, message_id).is_some(),
            "{message_id} was never folded: {after_second:#?}"
        );
    }
}

#[tokio::test]
async fn another_sessions_log_is_not_touched() {
    // Per session, not per machine: one foldable session is folded while a session whose turn is running
    // is not — so the sweep neither reaches across sessions nor stops at the first one it must skip.
    let dir = TempDir::new("compaction-sessions");
    let (state, folded) = idle_session(&dir).await;
    let kept = create(&state);
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(&kept, Trigger::PiReady, None)
        .await
        .expect("the second session is idle too");
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(&kept, Trigger::Prompt, None)
        .await
        .expect("an idle session may be prompted");
    message(&state, &folded, "m1", &["fold me"], &[], OLD).await;
    message(&state, &kept, "m1", &["leave me"], &[], OLD).await;
    wait_for_ends(&state, &folded, &["m1"]).await;
    wait_for_ends(&state, &kept, &["m1"]).await;
    let before = log(&state, &kept);

    compactor(&state).sweep().await;

    assert_eq!(
        log(&state, &kept),
        before,
        "compaction reached into another session's log"
    );
    assert!(
        snapshot_of(&log(&state, &folded), "m1").is_some(),
        "the eligible session was not folded, so the assertion above is vacuous"
    );
}
