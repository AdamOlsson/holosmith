//! The writer thread: the one place an event's seq is assigned and its row committed (HS-009).
//!
//! A `std::thread`, not a tokio task, because rusqlite is blocking: a 25 ms synchronous
//! transaction on the async runtime would stall every handler sharing it. It receives over a
//! bounded `tokio::sync::mpsc` with `blocking_recv` and broadcasts over `tokio::sync::broadcast`,
//! whose `send` is synchronous and callable from any thread (ADR-0013).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{broadcast, mpsc};

use super::{Event, EventStream, EventWriter, NewEvent, RepoError, WriterOptions, coalesce};
use crate::db::Database;

/// How many appends may queue before a producer waits: backpressure on the producer, never on
/// the writer.
const PRODUCER_QUEUE: usize = 1024;

/// How many committed events a subscriber may fall behind before it is told it lagged. `send`
/// never waits for a receiver, so a slow one cannot delay a commit (HS-010 re-fetches).
const SUBSCRIBER_BUFFER: usize = 1024;

const THREAD_NAME: &str = "holosmith-events";

/// Start the writer thread and return its two handles: the producers' and the subscribers'.
///
/// The writer handle can make more subscribers of its own (`EventWriter::subscribe`), which is
/// how a caller that only holds the `EventWriter` — `AppState` — reaches the broadcast.
pub fn spawn(database: Arc<Mutex<Database>>, options: WriterOptions) -> (EventWriter, EventStream) {
    let (sender, receiver) = mpsc::channel(PRODUCER_QUEUE);
    let (events, subscribers) = broadcast::channel(SUBSCRIBER_BUFFER);
    let broadcasting = events.clone();
    std::thread::Builder::new()
        .name(THREAD_NAME.to_string())
        .spawn(move || run(database, options, receiver, broadcasting))
        .expect("the event writer thread is spawnable");
    (
        EventWriter { sender, events },
        EventStream {
            receiver: subscribers,
        },
    )
}

/// Block for one event, commit the batch it belongs to, then broadcast it.
///
/// The loop ends when every producer handle has been dropped, which is how a process stops the
/// writer without a shutdown flag.
fn run(
    database: Arc<Mutex<Database>>,
    options: WriterOptions,
    mut incoming: mpsc::Receiver<NewEvent>,
    events: broadcast::Sender<Event>,
) {
    while let Some(first) = incoming.blocking_recv() {
        // One pass over the batch before it becomes rows: the consecutive deltas of one message and kind
        // are one row with one seq (HS-059, spec item 15). The batch is the unit the story names — "inside
        // each 25 ms commit" — and it is the only place the events are still values rather than rows.
        let batch = coalesce(collect(first, &mut incoming, options.commit_interval));
        let assigned = match commit(&database, &batch) {
            Ok(assigned) => assigned,
            Err(error) => {
                // One transaction for the batch, so nothing from it committed: no client saw a
                // seq and no seq was used. Dropping the batch is the honest outcome; a retry is
                // the producer's to make.
                tracing::error!(
                    target: "holosmith::events",
                    %error,
                    count = batch.len(),
                    "a batch of events was not committed"
                );
                continue;
            }
        };
        // Strictly after `commit` returned: a subscriber that reads the row back finds it, which
        // is the whole "no client sees a seq a crash could undo" rule (ADR-0013).
        for event in assigned {
            // `Err` means no subscriber is connected, which is normal for a fresh process and is
            // not a failure.
            let _ = events.send(event);
        }
    }
}

/// `first` and everything that arrives before `interval` has passed.
///
/// Waiting out the window is what makes "commit every 25 ms" true when events arrive one at a
/// time; a batch of one is still one transaction, so the interval is a latency ceiling and not a
/// batching rule.
fn collect(
    first: NewEvent,
    incoming: &mut mpsc::Receiver<NewEvent>,
    interval: Duration,
) -> Vec<NewEvent> {
    let mut batch = vec![first];
    let deadline = Instant::now() + interval;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match incoming.try_recv() {
            Ok(event) => batch.push(event),
            // Nothing else is queued: wait out the window rather than commit early, so a burst
            // that is still arriving joins this batch.
            Err(TryRecvError::Empty) => std::thread::sleep(remaining),
            Err(TryRecvError::Disconnected) => return batch,
        }
    }
    // Whatever arrived while the last sleep ran belongs to this batch too: the window is a
    // deadline for starting the commit, not a limit on what the commit contains.
    while let Ok(event) = incoming.try_recv() {
        batch.push(event);
    }
    batch
}

/// One transaction for the whole batch, with the database's lock held only for it.
///
/// The guard is dropped when this returns, before the caller broadcasts, so a subscriber's
/// read-back cannot deadlock against the writer.
fn commit(database: &Mutex<Database>, batch: &[NewEvent]) -> Result<Vec<Event>, RepoError> {
    let database = database.lock().unwrap_or_else(PoisonError::into_inner);
    database.events().insert_batch(batch)
}
