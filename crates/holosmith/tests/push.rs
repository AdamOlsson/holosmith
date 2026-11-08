//! Web Push (HS-066, spec item 11): the two routes, the trigger table, the three triggers and the stream rule.
//!
//! The story's own rules are what these tests own, and each group is a different kind of measurement:
//!
//! - **Rows and routes** over a real `App` on a real socket: a subscribe is an upsert, a delete names its row
//!   through the query, and every refusal is ADR-0008's code.
//! - **The trigger table** as a table over crafted frames, because the table is a pure function of (frame, arrival,
//!   threshold) — the one place in this build where a policy can be proved exhaustively instead of sampled.
//! - **The three triggers end to end** through a real [`Pusher`] with a recorder `Deliver`: a real stub turn for
//!   `turn_finished`, the transitioner's own `error` row, and the ladder's two reason words. Plus the negative: a
//!   turn under the threshold sends nothing.
//! - **The stream pair**: the real `StreamRegistry` held and dropped, which is the one rule the plan says has to be
//!   read at the moment of sending.
//! - **The signature**: a token signed with a pair a real `openssl ecparam` child generated, verified by a second
//!   `openssl dgst -verify` child — "we signed with the machine's pair" as a measurement.
//! - **The request** the recorder was asked for: `Authorization`, `TTL`, `Content-Encoding`, and a body that carries
//!   none of the plaintext.
//! - **The containers hold nothing**: no `docker run` vector and no environment variable names a subscription, the
//!   pair or the word `push` — the sender is a host process (spec item 11's "the server, not the containers").
//!
//! The encryption itself is `src/push/crypto.rs`'s and `src/push/encrypt.rs`'s, pinned there by RFC 4231, RFC 5869,
//! FIPS 197, NIST's GCM cases and RFC 8291 §5's complete example. This file does not re-derive a primitive.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use holosmith::config::Config;
use holosmith::container::{ContainerSettings, SessionContainer};
use holosmith::events::TURN_START;
use holosmith::http::{self, AppState};
use holosmith::push::send::Disposition;
use holosmith::push::{self, Deliver, Pusher, Watcher};
use holosmith::sessions::Status;
use holosmith::sessions::state::{SESSION_STATUS, SessionStatus, Transitioner, Trigger};
use holosmith::vapid;
use serde_json::Value;
use tokio::time::Instant as TokioInstant;

mod support;
use support::{PATIENCE, Response, Served, TempDir, fast_stub, request, state_in, with_stub};

const GRACE: Duration = Duration::from_millis(100);

/// The control origin the guards accept: `Config::default`'s `control_listen` is loopback, and every other
/// socket-level test quotes the same one.
const ORIGIN: &str = "http://localhost:8080";

/// RFC 8291 §5's application-server pair, used as the machine's own pair: the one P-256 pair this test suite has
/// vectors for, so a signature made with it can be checked against the RFC's own numbers.
const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const AS_PUBLIC: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";

/// The same document's user agent: a subscription this build can really derive a secret with.
const UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH_SECRET: &str = "BTBZMqHH6r4Tts7J_aSIgg";

/// A state whose data root holds a VAPID pair, so a send can be signed; and whose database is its own.
fn subscribed(label: &str) -> (TempDir, AppState) {
    let dir = TempDir::new(label);
    let state = state_in(&dir, config(&dir));
    write_pair(&state);
    (dir, state)
}

/// A config whose two roots are inside `dir`.
///
/// `Config::default` names the *machine's* paths, and a test that used them would write a VAPID pair into
/// `/var/lib/holosmith` where the next test's boot could read it — and one test's removal of the pair is another
/// test's missing pair. The roots are the test's own for the reason every database in this suite is.
fn config(dir: &TempDir) -> Config {
    Config {
        data_root: dir.path().join("data"),
        projects_root: dir.path().join("projects"),
        ..Config::default()
    }
}

/// Write the machine's pair where `server deploy` writes it, with the mode that makes it a secret.
fn write_pair(state: &AppState) {
    let path = vapid::store(&state.config.data_root);
    std::fs::create_dir_all(path.parent().expect("the secrets directory")).expect("the directory");
    let pair = vapid::Pair {
        public_key: AS_PUBLIC.to_string(),
        private_key: AS_PRIVATE.to_string(),
    };
    std::fs::write(&path, vapid::render(&pair)).expect("the pair is writable");
}

/// A subscription's two key fields, base64url as a browser sends them.
fn keys() -> (String, String) {
    (UA_PUBLIC.to_string(), AUTH_SECRET.to_string())
}

fn endpoint(name: &str) -> String {
    format!("https://push.example/{name}")
}

/// The subscribe body for `endpoint` and a pair of keys.
fn body(endpoint: &str, p256dh: &str, auth: &str) -> String {
    format!(r#"{{"endpoint":"{endpoint}","p256dh":"{p256dh}","auth":"{auth}"}}"#)
}

/// One mutating request: the four headers ADR-0010's guards ask for.
async fn mutate(addr: std::net::SocketAddr, method: &str, path: &str, payload: &str) -> Response {
    let text = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    request(addr, &text).await
}

/// A create, which is the one mutating request here that also carries an `Idempotency-Key` (HS-008).
async fn create(addr: std::net::SocketAddr, key: &str) -> Response {
    let payload = r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#;
    let text = format!(
        "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
         Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nIdempotency-Key: {key}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    request(addr, &text).await
}

/// The stored rows, as `(endpoint, p256dh, auth, created_at)`.
fn rows(state: &AppState) -> Vec<(String, String, String, i64)> {
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .push()
        .all()
        .expect("the rows read")
        .into_iter()
        .map(|stored| {
            (
                stored.endpoint,
                stored.p256dh,
                stored.auth,
                stored.created_at,
            )
        })
        .collect()
}

/// Insert one subscription the way the route would, for a test whose subject is what happens *after* it.
fn store(state: &AppState, endpoint: &str) {
    let (p256dh, auth) = keys();
    let database = state.database.lock().expect("the lock is not poisoned");
    database
        .push()
        .upsert(endpoint, &p256dh, &auth, 1_760_000_000)
        .expect("the row is stored");
}

/// Wait for `condition`, or fail with `what`.
async fn until(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = TokioInstant::now() + PATIENCE;
    while TokioInstant::now() < deadline {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{what} did not happen within {PATIENCE:?}");
}

/// The session row's status, and its reason.
fn status_of(state: &AppState, id: &str) -> (Status, Option<String>) {
    let database = state.database.lock().expect("the lock is not poisoned");
    let session = database
        .sessions()
        .get(id)
        .expect("the row reads")
        .expect("the row exists");
    (session.status, session.status_reason)
}

// --- 1. rows and routes -----------------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_is_an_upsert_and_delete_names_its_row_through_the_query() {
    let dir = TempDir::new("push-routes");
    let state = state_in(&dir, config(&dir));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let (p256dh, auth) = keys();
    let endpoint = endpoint("one");

    // The first subscribe: 204, one row, the keys as sent.
    let answer = mutate(
        served.addr,
        "POST",
        "/api/push/subscribe",
        &body(&endpoint, &p256dh, &auth),
    )
    .await;
    assert_eq!(answer.status, 204, "{}", answer.body);
    assert_eq!(
        rows(&state),
        vec![(
            endpoint.clone(),
            p256dh.clone(),
            auth.clone(),
            rows(&state)[0].3
        )],
        "the row is not what was sent"
    );
    let created_at = rows(&state)[0].3;

    // The browser re-subscribing: the same endpoint with new keys is one row, and `created_at` keeps the first
    // value. The keys move; the row's age does not.
    let rotated = vapid::base64url(&[0x22; 16]);
    let answer = mutate(
        served.addr,
        "POST",
        "/api/push/subscribe",
        &body(&endpoint, &p256dh, &rotated),
    )
    .await;
    assert_eq!(answer.status, 204, "{}", answer.body);
    let stored = rows(&state);
    assert_eq!(stored.len(), 1, "the re-subscribe made a second row");
    assert_eq!(stored[0].2, rotated, "the keys did not move");
    assert_eq!(stored[0].3, created_at, "the row's age moved with its keys");

    // The delete names the endpoint through the query, percent-decoded, and 204 means the row is gone.
    let answer = mutate(
        served.addr,
        "DELETE",
        &format!("/api/push/subscribe?endpoint={}", encode(&endpoint)),
        "",
    )
    .await;
    assert_eq!(answer.status, 204, "{}", answer.body);
    assert!(rows(&state).is_empty(), "the row survived its delete");

    // A second delete is `404 not_found` with its own sentence, not a second 204.
    let answer = mutate(
        served.addr,
        "DELETE",
        &format!("/api/push/subscribe?endpoint={}", encode(&endpoint)),
        "",
    )
    .await;
    assert_eq!(answer.status, 404, "{}", answer.body);
    let body: Value = serde_json::from_str(&answer.body).expect("an envelope");
    assert_eq!(body["code"], "not_found");
    assert_eq!(
        body["message"],
        "there is no subscription with that endpoint"
    );

    // An endpoint whose own text has a `&` and a `?` in it: the query is split on `&`, so a client that sent the
    // value raw would have it truncated and be told `404` about a row that exists. Percent-encoding is the
    // client's contract (RFC 3986) and this is the arm that says the route honours it — `%26` and `%3F` decode
    // into the endpoint the row is keyed by.
    let awkward = "https://push.example/a?b=1&c=2/sentinel";
    let answer = mutate(
        served.addr,
        "POST",
        "/api/push/subscribe",
        &store_body(awkward, &keys().0, &keys().1),
    )
    .await;
    assert_eq!(answer.status, 204, "{}", answer.body);
    assert!(
        rows(&state).iter().any(|row| row.0 == awkward),
        "the awkward endpoint was not stored: {:?}",
        rows(&state)
    );
    let answer = mutate(
        served.addr,
        "DELETE",
        &format!("/api/push/subscribe?endpoint={}", encode(awkward)),
        "",
    )
    .await;
    assert_eq!(
        answer.status, 204,
        "an escaped `&` did not decode back into the endpoint: {}",
        answer.body
    );
    assert!(
        !rows(&state).iter().any(|row| row.0 == awkward),
        "the escaped `&` left the row behind"
    );
    served.finish().await.expect("a clean shutdown");
}

/// The subscribe body, for a test that already has a local `body` of its own.
fn store_body(endpoint: &str, p256dh: &str, auth: &str) -> String {
    body(endpoint, p256dh, auth)
}

/// Percent-encode a value the way a browser's `URLSearchParams` does for the characters an endpoint has.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte))
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[tokio::test]
async fn every_refusal_is_the_sentence_its_fix_deserves() {
    let dir = TempDir::new("push-refusals");
    let state = state_in(&dir, config(&dir));
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let (p256dh, auth) = keys();

    let cases: [(&str, String, &str); 8] = [
        // Not https, and a URL with no host: one code, two next steps.
        (
            "not https",
            body("http://push.example/one", &p256dh, &auth),
            "bad_endpoint",
        ),
        (
            "no host",
            body("https:///one", &p256dh, &auth),
            "bad_endpoint",
        ),
        // `p256dh` is 64 octets, and `auth` 15: one code, two next steps.
        (
            "short point",
            body(&endpoint("a"), &vapid::base64url(&[0x04; 64]), &auth),
            "bad_subscription_key",
        ),
        (
            "short secret",
            body(&endpoint("b"), &p256dh, &vapid::base64url(&[0; 15])),
            "bad_subscription_key",
        ),
        // A point that is not uncompressed is a key that is not one.
        (
            "compressed point",
            body(&endpoint("c"), &vapid::base64url(&[0x02; 65]), &auth),
            "bad_subscription_key",
        ),
        // The body's shape: a fourth key, a missing field, and a body that is not an object.
        (
            "fourth key",
            format!(
                r#"{{"endpoint":"{}","p256dh":"{p256dh}","auth":"{auth}","keys":{{}}}}"#,
                endpoint("d")
            ),
            "unknown_field",
        ),
        (
            "missing field",
            format!(r#"{{"endpoint":"{}","p256dh":"{p256dh}"}}"#, endpoint("e")),
            "bad_body",
        ),
        ("not an object", "[]".to_string(), "bad_body"),
    ];
    for (label, payload, code) in cases {
        let answer = mutate(served.addr, "POST", "/api/push/subscribe", &payload).await;
        assert_eq!(answer.status, 422, "{label}: {}", answer.body);
        let envelope: Value = serde_json::from_str(&answer.body).expect("an envelope");
        assert_eq!(envelope["code"], code, "{label}: {}", answer.body);
    }
    // Nothing above stored a row.
    assert!(rows(&state).is_empty(), "a refusal stored a subscription");

    // A `DELETE` with no endpoint is `bad_query`, which is not the same mistake as a missing row.
    for path in ["/api/push/subscribe", "/api/push/subscribe?endpoint="] {
        let answer = mutate(served.addr, "DELETE", path, "").await;
        assert_eq!(answer.status, 422, "{path}: {}", answer.body);
        let envelope: Value = serde_json::from_str(&answer.body).expect("an envelope");
        assert_eq!(envelope["code"], "bad_query", "{path}");
    }
    served.finish().await.expect("a clean shutdown");
}

#[test]
fn both_routes_are_in_the_manifest_and_mutating() {
    let dir = TempDir::new("push-manifest");
    let state = state_in(&dir, config(&dir));
    let app = http::App::new(
        http::router(state.clone()),
        http::Guards::from_config(&state.config),
    );
    let mut found: Vec<(String, String)> = app
        .manifest()
        .iter()
        .filter(|entry| entry.path == "/api/push/subscribe")
        .map(|entry| (entry.method.to_string(), format!("{:?}", entry.kind)))
        .collect();
    found.sort();
    assert_eq!(
        found,
        [
            ("DELETE".to_string(), "Mutating".to_string()),
            ("POST".to_string(), "Mutating".to_string())
        ]
    );
}

// --- 2. the trigger table ---------------------------------------------------------------------------------

/// A frame, as the writer would hand it to a subscriber.
fn frame(kind: &str, payload: &str) -> holosmith::events::Event {
    holosmith::events::Event {
        session_id: "0123456789abcdef".to_string(),
        seq: 1,
        kind: kind.to_string(),
        payload: payload.to_string(),
        created_at: 1_760_000_000,
    }
}

/// A `session.status` frame with the transitioner's own payload.
fn status(from: Status, to: Status, reason: Option<&str>) -> holosmith::events::Event {
    frame(
        SESSION_STATUS,
        &SessionStatus::transitioned(from, to, reason.map(str::to_string)).to_json(),
    )
}

#[test]
fn the_trigger_table_fires_on_four_shapes_and_nothing_else() {
    let base = TokioInstant::now();
    let at = |millis: u64| base + Duration::from_millis(millis);
    let threshold = || Watcher::new().with_threshold(Duration::from_millis(50));

    // The four firing shapes.
    let mut watcher = threshold();
    assert_eq!(
        watcher.observe(&status(Status::Running, Status::Error, None), at(0)),
        Some(push::ERROR)
    );
    let mut watcher = threshold();
    assert_eq!(
        watcher.observe(
            &status(Status::Running, Status::Stopped, Some(push::TURN_TIMEOUT)),
            at(0)
        ),
        Some(push::TURN_TIMEOUT)
    );
    let mut watcher = threshold();
    assert_eq!(
        watcher.observe(
            &status(Status::Running, Status::Idle, Some(push::BUDGET_EXCEEDED)),
            at(0)
        ),
        Some(push::BUDGET_EXCEEDED)
    );
    let mut watcher = threshold();
    assert_eq!(watcher.observe(&frame(TURN_START, "{}"), at(0)), None);
    assert_eq!(
        watcher.observe(&status(Status::Running, Status::Idle, None), at(50)),
        Some(push::TURN_FINISHED)
    );

    // The negatives: a stop reason item 11 does not name, a settle under the threshold, a settle with no
    // recorded `turn.start`, a `stopped` with no reason, and a frame that is not a status at all.
    for reason in [
        "forced_abort",
        "disk_quota",
        "evict",
        "idle_stop",
        "pi_exit",
    ] {
        let mut watcher = threshold();
        watcher.observe(&frame(TURN_START, "{}"), at(0));
        assert_eq!(
            watcher.observe(
                &status(Status::Running, Status::Idle, Some(reason)),
                at(60_000)
            ),
            None,
            "{reason}"
        );
    }
    let mut watcher = threshold();
    watcher.observe(&frame(TURN_START, "{}"), at(0));
    assert_eq!(
        watcher.observe(&status(Status::Running, Status::Idle, None), at(49)),
        None,
        "a settle under the threshold is not worth a notification"
    );
    let mut watcher = threshold();
    assert_eq!(
        watcher.observe(&status(Status::Running, Status::Idle, None), at(60_000)),
        None,
        "a turn this task did not see start has no duration"
    );
    let mut watcher = threshold();
    watcher.observe(&frame(TURN_START, "{}"), at(0));
    assert_eq!(
        watcher.observe(&status(Status::Running, Status::Stopped, None), at(60_000)),
        None
    );
    assert_eq!(watcher.observe(&frame("message.end", "{}"), at(0)), None);
    assert_eq!(
        push::LONG_TURN,
        Duration::from_secs(20),
        "item 11's twenty seconds"
    );
}

// --- 3. and 7. the triggers end to end, and the request they produced ---------------------------------------

/// What a delivery was asked for, and what it answered.
#[derive(Default)]
struct Recorder {
    sent: Mutex<Vec<(String, Vec<u8>)>>,
    answer: Mutex<u16>,
}

impl Recorder {
    fn with_answer(answer: u16) -> Arc<Self> {
        let recorder = Arc::new(Self::default());
        *recorder.answer.lock().expect("the lock") = answer;
        recorder
    }

    fn sent(&self) -> Vec<(String, Vec<u8>)> {
        self.sent.lock().expect("the lock").clone()
    }

    /// The requests whose body mentions `needle`'s session — the only way to tell two sessions' sends apart.
    fn count(&self) -> usize {
        self.sent.lock().expect("the lock").len()
    }
}

impl Deliver for Recorder {
    fn deliver(&self, endpoint: &str, request: &[u8], _dir: &Path) -> Result<Disposition, String> {
        self.sent
            .lock()
            .expect("the lock")
            .push((endpoint.to_string(), request.to_vec()));
        let answer = *self.answer.lock().expect("the lock");
        Ok(Disposition::of(answer))
    }
}

/// A pusher over `state` with a recorder, spawned on the runtime's own task.
fn pusher(state: &AppState, recorder: Arc<Recorder>) -> Pusher {
    state
        .pusher()
        .with_threshold(Duration::from_millis(50))
        .with_deliver(recorder)
}

/// Wait until `recorder` has been asked to deliver `count` times, and answer the last request.
async fn wait_for_sends(recorder: &Recorder, count: usize) -> (String, Vec<u8>) {
    until(
        || recorder.count() >= count,
        &format!("send number {count}"),
    )
    .await;
    recorder.sent().pop().expect("there is a send")
}

/// The three triggers end to end, and the negative.
///
/// The recorder sees the **sealed** request, so what it can assert is the request's shape, its number and its
/// endpoint — the payload's `kind` is unreadable here by construction, which is the whole point of the encryption.
/// That word is proved where it is decided: `Watcher`'s table (above), and `encrypt.rs`'s unit test that seals the
/// `{host, session_id, kind}` struct and reads the three keys back.
///
/// A real stub turn earns `turn_finished` (its script outlives the fifty-millisecond threshold); the transitioner's
/// own `unexpected_exit` row earns `error`; and the abort ladder's rung 2 records each reason word on the row, which
/// is the frame a timed-out or over-budget turn writes.
#[tokio::test]
async fn three_triggers_and_the_negative_reach_the_recorder() {
    let (dir, state) = subscribed("push-triggers");
    // The stub's pace is the turn's: one frame per step, so a turn of the script's dozen frames outlives the
    // fifty-millisecond threshold while the four provisioning steps stay quick enough to wait for. `abort_grace` is
    // short because the ladder's two reasons wait it out before rung 2 writes.
    let state = with_stub(
        state,
        holosmith::backend::StubBackend::new(Duration::from_millis(20)),
    );
    let recorder = Recorder::with_answer(201);
    let served = Served::start(http::router(state.clone()), GRACE).await;
    let task = pusher(&state, Arc::clone(&recorder)).spawn();
    store(&state, &endpoint("triggers"));

    // A create and a prompt: a real turn, whose `turn.start` and `idle` the pusher sees through the writer's
    // broadcast.
    let created = create(served.addr, "push-turns").await;
    assert_eq!(created.status, 202, "{}", created.body);
    let body: Value = serde_json::from_str(&created.body).expect("a create answers JSON");
    let id = body["id"].as_str().expect("the id").to_string();
    until(
        || status_of(&state, &id).0 == Status::Idle,
        "the session provisions",
    )
    .await;
    let prompt = mutate(
        served.addr,
        "POST",
        &format!("/api/sessions/{id}/prompt"),
        r#"{"prompt":"go"}"#,
    )
    .await;
    assert_eq!(prompt.status, 202, "{}", prompt.body);

    // 1. `turn_finished`: one send, and the request is the shape a push service reads.
    let (host, request) = wait_for_sends(&recorder, 1).await;
    assert_eq!(host, endpoint("triggers"), "the sender named the wrong row");
    let head = String::from_utf8_lossy(&request).to_string();
    let (head, sealed) = head.split_once("\r\n\r\n").expect("a request has a head");
    let head = head.to_string();
    for wanted in [
        "TTL: 3600",
        "Content-Encoding: aes128gcm",
        "Content-Type: application/octet-stream",
        "Connection: close",
        &format!("k={AS_PUBLIC}"),
    ] {
        assert!(head.contains(wanted), "the head lacks {wanted:?}: {head}");
    }
    assert!(head.contains("Authorization: vapid t="), "{head}");
    // The body is sealed: the payload is in it nowhere, and it is a whole record (a salt, an `rs`, an idlen, the
    // application server's key and a tag).
    assert!(sealed.len() > 100, "the body is {} octets", sealed.len());
    assert!(
        !sealed.contains(&id),
        "the sealed body carries the session id"
    );
    assert!(!sealed.contains("turn_finished"));
    assert!(!sealed.contains("local"));

    // The token is the machine's signature over its own claims: a second `openssl` child says so.
    let token = head
        .split("Authorization: vapid t=")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .expect("the head carries a token")
        .to_string();
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "{token}");
    let tmp = push::jwt::Temp::new(state.config.data_root.as_path()).expect("a temp directory");
    push::jwt::verify(
        AS_PUBLIC,
        &format!("{}.{}", parts[0], parts[1]),
        parts[2],
        "openssl",
        &tmp,
    )
    .expect("the token is signed by the machine's pair");
    // The claims' audience is the endpoint's origin: the one field a push service compares.
    let claims: Value =
        serde_json::from_slice(&vapid::unbase64url(parts[1]).expect("base64url")).expect("JSON");
    assert_eq!(claims["aud"], "https://push.example");

    // 2. The `error` trigger, through the transitioner's own row.
    Transitioner::new(Arc::clone(&state.database), state.events.clone())
        .apply(&id, Trigger::UnexpectedExit, Some("the container exited"))
        .await
        .expect("idle + unexpected_exit is the table's row");
    wait_for_sends(&recorder, 2).await;
    assert_eq!(status_of(&state, &id).0, Status::Error);

    // 3. The two reason words, each on a session of its own: a `running` row, then the ladder's rung 2, which is
    //    the frame a timed-out or over-budget turn writes.
    let mut sends = recorder.count();
    for (index, reason) in [push::TURN_TIMEOUT, push::BUDGET_EXCEEDED]
        .into_iter()
        .enumerate()
    {
        let created = create(served.addr, &format!("push-reason-{index}")).await;
        assert_eq!(created.status, 202, "{}", created.body);
        let body: Value = serde_json::from_str(&created.body).expect("a create answers JSON");
        let other = body["id"].as_str().expect("the id").to_string();
        until(
            || status_of(&state, &other).0 == Status::Idle,
            "the session provisions",
        )
        .await;
        Transitioner::new(Arc::clone(&state.database), state.events.clone())
            .apply(&other, Trigger::Prompt, None)
            .await
            .expect("idle + prompt is the table's row");
        let session = {
            let database = state.database.lock().expect("the lock is not poisoned");
            database
                .sessions()
                .get(&other)
                .expect("the row reads")
                .expect("the row exists")
        };
        state.ladder().end_turn(&session, reason).await;
        sends += 1;
        wait_for_sends(&recorder, sends).await;
        let (_, request) = recorder.sent().pop().expect("a send");
        assert!(
            !String::from_utf8_lossy(&request).contains(reason),
            "the sealed body carries {reason:?}"
        );
        assert_eq!(
            status_of(&state, &other).1.as_deref(),
            Some(reason),
            "the ladder did not record {reason:?} on the row"
        );
    }

    task.abort();
    served.finish().await.expect("a clean shutdown");
    let _ = dir;
}

#[tokio::test]
async fn an_open_stream_suppresses_the_send_and_a_machine_with_no_pair_sends_nothing() {
    let (dir, state) = subscribed("push-streams");
    let state = with_stub(state, fast_stub());
    let recorder = Recorder::with_answer(201);
    let pusher = pusher(&state, Arc::clone(&recorder));
    store(&state, &endpoint("streams"));

    // The stream pair, over the state's **real** registry: held, nothing is sent; dropped, one is. This is the rule
    // spec item 11 words as "unless an SSE stream for that session is open", read at the moment of sending.
    let guard = state.streams.open("0123456789abcdef");
    pusher.announce("0123456789abcdef", push::ERROR).await;
    assert_eq!(
        recorder.count(),
        0,
        "a notification was sent while a stream was open"
    );
    drop(guard);
    pusher.announce("0123456789abcdef", push::ERROR).await;
    assert_eq!(recorder.count(), 1, "the send after the stream closed");

    // A machine that never deployed has no pair: one warn and no send, which is not an error per notification.
    std::fs::remove_file(vapid::store(&state.config.data_root)).expect("the pair is removable");
    pusher.announce("0123456789abcdef", push::ERROR).await;
    assert_eq!(recorder.count(), 1, "a machine with no pair sent a message");
    let _ = dir;
}

#[tokio::test]
async fn a_gone_endpoint_loses_its_row_and_a_key_problem_keeps_it() {
    // The disposition table over the real announce path: the row's survival is the assertion, because a `401`/`403`
    // is a key problem and must never delete a subscription whose browser is still subscribed.
    for (status, kept) in [
        (201, true),
        (200, true),
        (202, true),
        (404, false),
        (410, false),
        (401, true),
        (403, true),
        (429, true),
        (500, true),
    ] {
        let (dir, state) = subscribed(&format!("push-disposition-{status}"));
        let recorder = Recorder::with_answer(status);
        let pusher = pusher(&state, Arc::clone(&recorder));
        store(&state, &endpoint("dead"));
        pusher.announce("0123456789abcdef", push::ERROR).await;
        assert_eq!(recorder.count(), 1, "{status}: nothing was delivered");
        assert_eq!(
            !rows(&state).is_empty(),
            kept,
            "{status}: the row's survival is the table's answer"
        );
        let _ = dir;
    }
}

// --- 6. the signature -------------------------------------------------------------------------------------

#[tokio::test]
async fn a_pair_a_real_ecparam_child_generated_signs_a_token_a_second_child_verifies() {
    if std::process::Command::new("openssl")
        .arg("version")
        .output()
        .is_err()
    {
        eprintln!(
            "SKIP: this machine has no openssl, so a VAPID token cannot be signed or verified"
        );
        return;
    }
    let dir = TempDir::new("push-signature");
    let generated = std::process::Command::new("openssl")
        .args([
            "ecparam",
            "-name",
            "prime256v1",
            "-genkey",
            "-noout",
            "-outform",
            "DER",
        ])
        .output()
        .expect("openssl runs");
    assert!(generated.status.success(), "openssl refused: {generated:?}");
    let pair = vapid::from_der(&generated.stdout).expect("a fresh key is a SEC1 key");
    let tmp = push::jwt::Temp::new(dir.path()).expect("a temp directory");
    let token = push::jwt::token(
        &pair,
        "https://push.example",
        "mailto:me@example",
        1_760_000_000,
        "openssl",
        &tmp,
    )
    .expect("the machine can sign");
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "{token}");
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    // The claims are RFC 8292's three, and `exp` is twelve hours past `now`.
    let claims: Value =
        serde_json::from_slice(&vapid::unbase64url(parts[1]).expect("the claims are base64url"))
            .expect("the claims are JSON");
    assert_eq!(claims["aud"], "https://push.example");
    assert_eq!(
        claims["exp"].as_i64().expect("a number"),
        1_760_000_000 + push::jwt::EXP_SECONDS
    );
    assert_eq!(claims["sub"], "mailto:me@example");
    push::jwt::verify(&pair.public_key, &signing_input, parts[2], "openssl", &tmp)
        .expect("the pair's own signature verifies");
    // A tampered signing input is refused, which is what makes the verification a measurement rather than a claim.
    assert!(
        push::jwt::verify(
            &pair.public_key,
            &format!("{signing_input}tampered"),
            parts[2],
            "openssl",
            &tmp
        )
        .is_err(),
        "a tampered signing input verified"
    );
}

#[test]
fn the_disposition_table_is_one_removal_and_everything_else_kept() {
    for status in [200, 201, 202] {
        assert_eq!(Disposition::of(status), Disposition::Delivered, "{status}");
    }
    for status in [404, 410] {
        assert_eq!(Disposition::of(status), Disposition::Gone, "{status}");
    }
    // The rule the task runs is `Gone` or not, so the second half of each arm is that one comparison.
    for status in [204, 401, 403, 429, 500, 503] {
        assert_eq!(Disposition::of(status), Disposition::Kept, "{status}");
        assert!(
            !matches!(Disposition::of(status), Disposition::Gone),
            "{status}"
        );
    }
}

// --- 8. the containers hold nothing -----------------------------------------------------------------------

#[test]
fn no_session_container_carries_a_subscription_the_pair_or_the_word_push() {
    // The label deliberately avoids the word under test: this suite's scratch directories are named after the
    // test, and a `TempDir::new("push-containers")` would put "push" into a container's own home path.
    let dir = TempDir::new("hs066-containers");
    let state = state_in(&dir, config(&dir));
    let session = {
        let database = state.database.lock().expect("the lock is not poisoned");
        database
            .sessions()
            .create(
                &holosmith::sessions::NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: None,
                    base_commit: None,
                },
                1_760_000_000,
            )
            .expect("the row is created")
    };
    // The row a subscription would have to reach the container through: written, so the test measures a machine
    // that really has one.
    store(&state, &endpoint("containers"));
    let settings = ContainerSettings::at(
        state.config.projects_root.clone(),
        state.config.data_root.clone(),
    );
    let arguments = SessionContainer::of(&settings, &session).run_args();
    let endpoint_value = endpoint("containers");
    for argument in &arguments {
        for forbidden in ["push", "vapid", "subscribe", &endpoint_value] {
            assert!(
                !argument.contains(forbidden),
                "a session's container is given {forbidden:?}: {argument}"
            );
        }
    }
    // And the environment: no variable names one either. `--env KEY=VALUE` is the only shape a variable takes.
    let environment: Vec<&String> = arguments
        .iter()
        .enumerate()
        .filter(|(index, _)| index > &0 && arguments[index - 1] == "--env")
        .map(|(_, argument)| argument)
        .collect();
    assert!(!environment.is_empty(), "the container has no environment");
    for variable in environment {
        let name = variable
            .split('=')
            .next()
            .unwrap_or_default()
            .to_lowercase();
        for forbidden in ["push", "vapid", "subscribe"] {
            assert!(
                !name.contains(forbidden),
                "a session's environment names {forbidden:?}: {variable}"
            );
        }
    }
}

/// HS-067's one server-side claim: the browser can learn this machine's **application server key** from the
/// endpoint the page already fetches, and the value there is the pair's own public half — byte for byte, not a
/// re-derivation — which is what makes "each machine's keys are independent" a fact rather than a hope.
///
/// The second half is the negative a page needs: a machine with no pair answers `null`, so the settings screen
/// can say why the Subscribe button is not offered instead of subscribing with half a key.
#[tokio::test]
async fn the_health_answer_carries_this_machines_application_server_key() {
    let (_dir, state) = subscribed("health-key");
    let served = Served::start(http::router(state.clone()), PATIENCE).await;
    let response = request(
        served.addr,
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let payload: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    assert_eq!(
        payload["application_server_key"], AS_PUBLIC,
        "the health answer does not carry the pair's public half: {}",
        response.body
    );
    assert_eq!(
        payload["application_server_key"].as_str().map(str::len),
        Some(87),
        "a P-256 uncompressed point in base64url is 87 characters"
    );
    // The private half never appears anywhere in the answer: it is the one string a browser must not be given.
    assert!(
        !response.body.contains(AS_PRIVATE),
        "the health answer leaks the private key: {}",
        response.body
    );

    // The pair goes away while the server keeps serving — a machine whose secrets were removed is asked again —
    // and the answer is the page's other arm.
    std::fs::remove_file(vapid::store(&state.config.data_root)).expect("the pair is removable");
    let response = request(
        served.addr,
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )
    .await;
    let payload: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    assert_eq!(
        payload["application_server_key"],
        Value::Null,
        "a machine with no pair must answer null: {}",
        response.body
    );
    served.finish().await.expect("a clean shutdown");
}
