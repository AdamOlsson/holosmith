//! The host registry as a client meets it (HS-068, spec items 4 and 16).
//!
//! Four groups, one per thing the story had to get right:
//!
//! - **The registration's rules** (item 4's first clause) over the wire: the scheme, the host, the port, the name and
//!   the body, each answered with its own code.
//! - **The merge** (item 16): a real `hosts.toml` over a real database, the file winning on a name and an unmentioned
//!   row surviving, with the two malformed-file arms.
//! - **The probe**, through the merge, against **real listeners on loopback**: a healthy answer, a `302` that is
//!   refused *and asked exactly once*, and a payload named something else.
//! - **The delete's refusal** (item 4's last bullet): the local row with a live session is `409 host_busy`, and
//!   `?force=true` removes it.
//!
//! Why the probe is measured through the seed rather than through `POST /api/hosts`: the API's host predicate accepts
//! a MagicDNS name or an address in 100.64.0.0/10, and this machine can produce neither (no `tailscaled`, no way to
//! add a 100.64 address without root). The seed is the operator's own file and is not filtered by that predicate
//! (ADR-0073 D9), so a loopback row is the only way to make the probe reach anything from a test — and the probe is
//! the same code either way. The API's **refusal** of a non-Holosmith answer is measured where its two halves are:
//! `hosts::accept` with a refused outcome below, and `hosts::probe`'s own arms in the crate's unit tests.
//!
//! Every test owns its `TempDir` and its `Database` (ADR-0009's measured same-process "database is locked" trap).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use holosmith::config::Config;
use holosmith::hosts::{self, Reach};
use holosmith::http::error::{
    BAD_BODY, BAD_HOST_NAME, BAD_HOST_URL, HOST_BUSY, NOT_FOUND, UNKNOWN_FIELD,
};
use holosmith::http::{self, AppState};
use holosmith::preflight::Tools;

mod support;
use support::{Response, Served, TempDir, fast_stub, request, state_in, with_stub};

const GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// `Config::default().control_origin`, which is what the default guards expect — and, for one test below, what a row
/// naming *this* machine has to say.
const ORIGIN: &str = "http://localhost:8080";

/// The name a seeded row that names this machine has.
const LOCAL_ROW: &str = "thisbox";

/// A healthy payload, spelled exactly as `/api/health` answers one.
const PAYLOAD: &str = r#"{"name":"holosmith","version":"verify","role":"server","uptime_s":0,"preview_origin":"http://localhost:8081","application_server_key":null}"#;

/// The four headers a mutating request needs (ADR-0010) plus the host.
const HEADERS: &[(&str, &str)] = &[
    ("Host", "127.0.0.1"),
    ("Origin", ORIGIN),
    ("Content-Type", "application/json"),
    ("X-Holosmith-Client", "1"),
];

fn body(name: &str, url: &str) -> String {
    format!(r#"{{"name":"{name}","url":"{url}"}}"#)
}

async fn post(addr: SocketAddr, body: &str) -> Response {
    send(addr, "POST", "/api/hosts", body).await
}

async fn delete(addr: SocketAddr, name: &str, query: &str) -> Response {
    send(addr, "DELETE", &format!("/api/hosts/{name}{query}"), "").await
}

async fn send(addr: SocketAddr, method: &str, path: &str, body: &str) -> Response {
    let mut text = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in HEADERS {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    request(addr, &text).await
}

async fn list(addr: SocketAddr) -> Response {
    request(
        addr,
        "GET /api/hosts HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )
    .await
}

/// The ADR-0008 envelope: the status, JSON, and exactly the code the request earned.
fn assert_refusal(response: &Response, status: u16, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body
    );
    let value: serde_json::Value = serde_json::from_str(&response.body).expect("a refusal is JSON");
    assert_eq!(value["code"], code, "{}", response.body);
}

/// The rows a `GET /api/hosts` answered, as `(name, reach, version, last_error)`.
fn rows(response: &Response) -> Vec<serde_json::Value> {
    assert_eq!(response.status, 200, "{}", response.body);
    let value: serde_json::Value = serde_json::from_str(&response.body).expect("a list is JSON");
    value["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("no `hosts` array: {}", response.body))
        .clone()
}

/// The row named `name`, if the list holds one.
fn row(response: &Response, name: &str) -> Option<serde_json::Value> {
    rows(response)
        .into_iter()
        .find(|row| row["name"] == serde_json::Value::String(name.to_string()))
}

/// A listener that answers up to **two** requests with `answer` and then stops, with the port it bound and a counter
/// of requests that keeps counting.
///
/// Two, not one, because that is what makes "a redirect is not followed" measurable: a probe that followed one would
/// open a second connection to this port, and the counter is what says it did. The listener is **non-blocking with a
/// deadline** rather than a `for stream in listener.incoming()`: a loop with no end leaves the thread parked in
/// `accept()` for the rest of the binary, holding a port, which is exactly the hang `docs/notes.md` records from this
/// story's own unit test.
fn stand_in(answer: Vec<u8>) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::AtomicUsize;
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback listener");
    listener
        .set_nonblocking(true)
        .expect("a loopback listener is made non-blocking");
    let port = listener.local_addr().expect("the bound address").port();
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut served = 0;
        while served < 2 && std::time::Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    // A non-blocking listener accepts a blocking stream; the read is bounded so a peer that connects
                    // and says nothing cannot park this thread either.
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                    let mut request = [0_u8; 1024];
                    if stream.read(&mut request).unwrap_or(0) > 0 {
                        served += 1;
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let _ = stream.write_all(&answer);
                    }
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    });
    (port, asked)
}

/// A `200` with `body` behind it.
fn answered(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// A `302` to somewhere else, with no body.
fn redirected() -> Vec<u8> {
    b"HTTP/1.1 302 Found\r\nLocation: http://elsewhere.invalid/api/health\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
}

/// Serve `state`'s router, with the default guards.
async fn served(state: AppState) -> Served {
    Served::start(http::router(state), GRACE).await
}

fn config_with_seed(dir: &TempDir, seed: &str) -> Config {
    Config {
        data_root: dir.path().to_path_buf(),
        hosts_seed: Some(dir.write("hosts.toml", seed)),
        ..Config::default()
    }
}

fn database_of(state: &AppState) -> std::sync::MutexGuard<'_, holosmith::db::Database> {
    state
        .database
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registration_is_refused_with_a_code_of_its_own_for_each_rule() {
    let dir = TempDir::new("hosts-refusals");
    let served = served(state_in(&dir, Config::default())).await;

    // Item 4's first clause, arm by arm. Every one of these is refused before any probe runs, which is why a machine
    // with no tailnet can measure all four.
    for (url, needle) in [
        ("ftp://box.tail1234.ts.net", "http:// or https://"),
        ("https://box.tail1234.ts.net/api", "path"),
        ("https://example.com", "neither a MagicDNS name"),
        ("https://localhost", "neither a MagicDNS name"),
        ("http://8.8.8.8:8080", "100.64.0.0/10"),
        ("https://box.tail1234.ts.net:22", "allowed range"),
        ("http://box.tail1234.ts.net:443", "allowed range"),
    ] {
        let response = post(served.addr, &body("workshop", url)).await;
        assert_refusal(&response, 422, BAD_HOST_URL);
        assert!(
            response.body.contains(needle),
            "{url} answered {}",
            response.body
        );
    }

    // The name is its own field with its own code, including the one name HS-069 reserves.
    for (name, needle) in [
        ("local", "own sessions"),
        ("Workshop", "lowercase"),
        ("work shop", "lowercase"),
        ("", "must not be empty"),
    ] {
        let response = post(served.addr, &body(name, "https://box.tail1234.ts.net")).await;
        assert_refusal(&response, 422, BAD_HOST_NAME);
        assert!(
            response.body.contains(needle),
            "{name:?} answered {}",
            response.body
        );
    }

    // The body itself: a missing field, a non-string field, and a fourth key.
    for malformed in [
        r#"{"name":"workshop"}"#,
        r#"{"name":1,"url":"https://box.tail1234.ts.net"}"#,
        r#"["name"]"#,
        "not json",
    ] {
        let response = post(served.addr, malformed).await;
        assert_refusal(&response, 422, BAD_BODY);
    }
    let response = post(
        served.addr,
        r#"{"name":"workshop","url":"https://box.tail1234.ts.net","role":"server"}"#,
    )
    .await;
    assert_refusal(&response, 422, UNKNOWN_FIELD);

    // Nothing was stored by any of them: a refusal is not a row with a flag.
    assert_eq!(
        rows(&list(served.addr).await),
        Vec::<serde_json::Value>::new()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_that_does_not_answer_is_a_row_marked_unreachable_with_its_sentence() {
    let dir = TempDir::new("hosts-unreachable");
    let served = served(state_in(&dir, Config::default())).await;

    // A valid MagicDNS name that resolves nowhere: the registration is *stored*, because a machine that is asleep is
    // a registry row rather than a client error (item 16's "not removed"). No `.ts.net` name resolves from this
    // machine, so the sentence is the probe's own.
    let response = post(
        served.addr,
        &body("workshop", "https://workshop.tail1234.ts.net"),
    )
    .await;
    assert_eq!(response.status, 201, "{}", response.body);
    let stored: serde_json::Value = serde_json::from_str(&response.body).expect("the row is JSON");
    assert_eq!(stored["name"], "workshop");
    assert_eq!(stored["url"], "https://workshop.tail1234.ts.net");
    assert_eq!(stored["reach"], "unreachable");
    assert_eq!(stored["version"], serde_json::Value::Null);
    assert!(
        stored["last_error"]
            .as_str()
            .is_some_and(|sentence| !sentence.is_empty()),
        "an unreachable row carries the sentence: {}",
        response.body
    );
    assert!(stored["last_checked_at"].as_i64().is_some_and(|at| at > 0));
    // The list is the same row, by name.
    let listed = list(served.addr).await;
    assert_eq!(row(&listed, "workshop"), Some(stored));

    // A second registration of one name is that machine registered again: one row, not two.
    let again = post(
        served.addr,
        &body("workshop", "https://attic.tail1234.ts.net"),
    )
    .await;
    assert_eq!(again.status, 201, "{}", again.body);
    let listed = rows(&list(served.addr).await);
    assert_eq!(listed.len(), 1, "one name is one row: {listed:?}");
    assert_eq!(listed[0]["url"], "https://attic.tail1234.ts.net");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_probe_reads_a_real_answer_and_refuses_anything_that_is_not_a_health_reply() {
    let dir = TempDir::new("hosts-probe");
    let (healthy, asked) = stand_in(answered(PAYLOAD));
    let (redirect, redirected_asked) = stand_in(redirected());
    let (foreign, _) = stand_in(answered(
        &PAYLOAD.replace(r#""name":"holosmith""#, r#""name":"something-else""#),
    ));
    let config = config_with_seed(
        &dir,
        &format!(
            "[[hosts]]\nname = \"healthy\"\nurl = \"http://127.0.0.1:{healthy}\"\n\n\
             [[hosts]]\nname = \"redirect\"\nurl = \"http://127.0.0.1:{redirect}\"\n\n\
             [[hosts]]\nname = \"foreign\"\nurl = \"http://127.0.0.1:{foreign}\"\n\n\
             [[hosts]]\nname = \"{LOCAL_ROW}\"\nurl = \"{ORIGIN}\"\n"
        ),
    );
    let state = state_in(&dir, config.clone());
    {
        let database = database_of(&state);
        let merged = hosts::merge(&database, &config, &Tools::default()).expect("the seed merges");
        assert_eq!(merged.from_file, 4);
        assert_eq!(merged.stored, 4);
        assert_eq!(merged.probed, 4);
        assert_eq!(merged.unreachable, 2, "{merged:?}");
    }
    let served = served(state).await;
    let listed = list(served.addr).await;

    // The healthy answer: the row is reachable and carries the version **the host reported**, not this build's.
    let healthy_row = row(&listed, "healthy").expect("the healthy row");
    assert_eq!(healthy_row["reach"], "reachable", "{}", listed.body);
    assert_eq!(healthy_row["version"], "verify");
    assert_eq!(healthy_row["last_error"], serde_json::Value::Null);
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);

    // The redirect: refused (not followed), marked unreachable with the `302` in its sentence, and **asked once** —
    // which is the whole of "redirects are not followed" that a client can see.
    let redirect_row = row(&listed, "redirect").expect("the redirect row");
    assert_eq!(redirect_row["reach"], "unreachable");
    let sentence = redirect_row["last_error"].as_str().expect("a sentence");
    assert!(sentence.contains("302"), "{sentence}");
    assert_eq!(
        redirected_asked.load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    // A payload named something else is the same answer as a redirect: something answered, and not with health.
    let foreign_row = row(&listed, "foreign").expect("the foreign row");
    assert_eq!(foreign_row["reach"], "unreachable");
    assert!(
        foreign_row["last_error"]
            .as_str()
            .is_some_and(|sentence| sentence.contains("something-else")),
        "{}",
        listed.body
    );

    // A row naming this machine is not probed at all: it is `reachable` with this build's own version.
    let local_row = row(&listed, LOCAL_ROW).expect("the local row");
    assert_eq!(local_row["reach"], "reachable");
    assert_eq!(
        local_row["version"],
        env!("CARGO_PKG_VERSION"),
        "the local row reports this build's version"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_file_wins_on_a_name_and_a_row_it_does_not_mention_survives() {
    let dir = TempDir::new("hosts-merge");
    let (first, _) = stand_in(answered(PAYLOAD));
    let config = config_with_seed(
        &dir,
        &format!("[[hosts]]\nname = \"workshop\"\nurl = \"http://127.0.0.1:{first}\"\n"),
    );
    let state = state_in(&dir, config.clone());
    {
        let database = database_of(&state);
        hosts::merge(&database, &config, &Tools::default()).expect("the first merge");
        // A row the file does not mention, written the way a request writes one.
        database
            .hosts()
            .upsert(&hosts::Host {
                name: "api-made".to_string(),
                url: holosmith::config::Origin::parse("https://api.tail1234.ts.net")
                    .expect("an origin"),
                reach: Reach::Unreachable,
                version: None,
                last_error: Some("unreachable earlier".to_string()),
                last_checked_at: 1,
            })
            .expect("the API's row");
    }
    // The second boot's file disagrees about `workshop` and says nothing about `api-made`.
    let (second, _) = stand_in(answered(PAYLOAD));
    dir.write(
        "hosts.toml",
        &format!("[[hosts]]\nname = \"workshop\"\nurl = \"http://127.0.0.1:{second}\"\n"),
    );
    {
        let database = database_of(&state);
        hosts::merge(&database, &config, &Tools::default()).expect("the second merge");
    }
    let served = served(state).await;
    let listed = list(served.addr).await;
    assert_eq!(
        row(&listed, "workshop").expect("the file's row")["url"],
        format!("http://127.0.0.1:{second}"),
        "the file wins on a name: {}",
        listed.body
    );
    assert_eq!(
        row(&listed, "api-made").expect("the API's row")["url"],
        "https://api.tail1234.ts.net",
        "a row the file does not mention survives: {}",
        listed.body
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seed_that_does_not_parse_merges_nothing_and_leaves_the_table_alone() {
    let dir = TempDir::new("hosts-bad-seed");
    let config = config_with_seed(&dir, "[[hosts]]\nname = \"workshop\"\n");
    let state = state_in(&dir, config.clone());
    {
        let database = database_of(&state);
        database
            .hosts()
            .upsert(&hosts::Host {
                name: "kept".to_string(),
                url: holosmith::config::Origin::parse("https://kept.tail1234.ts.net")
                    .expect("an origin"),
                reach: Reach::Unreachable,
                version: None,
                last_error: Some("earlier".to_string()),
                last_checked_at: 1,
            })
            .expect("a row the merge must not touch");
        let error = hosts::merge(&database, &config, &Tools::default())
            .expect_err("a table with no url is not a seed");
        assert!(error.contains("has no `url`"), "{error}");
        assert!(
            error.contains("hosts.toml"),
            "the sentence names the file: {error}"
        );
    }
    let served = served(state).await;
    let listed = list(served.addr).await;
    assert_eq!(rows(&listed).len(), 1, "{}", listed.body);
    assert!(row(&listed, "kept").is_some());

    // No seed key at all is no seed and no failure: item 16's file is optional — and every row the table holds is
    // still probed, because reachability is a fact of this moment rather than of the row's creation (ADR-0073 D8).
    let quiet = TempDir::new("hosts-no-seed");
    let config = Config {
        data_root: quiet.path().to_path_buf(),
        ..Config::default()
    };
    let state = state_in(&quiet, config.clone());
    {
        let database = database_of(&state);
        database
            .hosts()
            .upsert(&hosts::Host {
                name: "unasked".to_string(),
                url: holosmith::config::Origin::parse("https://unasked.tail1234.ts.net")
                    .expect("an origin"),
                reach: Reach::Reachable,
                version: Some("an older answer".to_string()),
                last_error: None,
                last_checked_at: 1,
            })
            .expect("a row with a stale verdict");
        let merged =
            hosts::merge(&database, &config, &Tools::default()).expect("no key is not a failure");
        assert_eq!(merged.from_file, 0);
        assert_eq!(merged.path, None);
        assert_eq!(
            merged.probed, 1,
            "a machine with no seed still re-asks its rows"
        );
        let row = database
            .hosts()
            .one("unasked")
            .expect("the row reads")
            .expect("the row is still there");
        assert_eq!(
            row.reach,
            Reach::Unreachable,
            "the stale `reachable` was replaced by this boot's own answer"
        );
        assert!(row.last_checked_at > 1, "the clock moved with the probe");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_local_row_is_refused_while_this_machine_has_live_sessions_unless_forced() {
    let dir = TempDir::new("hosts-busy");
    let config = config_with_seed(
        &dir,
        &format!(
            "[[hosts]]\nname = \"{LOCAL_ROW}\"\nurl = \"{ORIGIN}\"\n\n\
             [[hosts]]\nname = \"faraway\"\nurl = \"https://faraway.tail1234.ts.net\"\n"
        ),
    );
    let state = with_stub(state_in(&dir, config.clone()), fast_stub());
    {
        let database = database_of(&state);
        hosts::merge(&database, &config, &Tools::default()).expect("the seed merges");
    }
    let served = served(state.clone()).await;

    // A live session on this machine: created through the API, and `starting` is live (it is the cap's own set).
    let created = request(
        served.addr,
        &format!(
            "POST /api/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
             Content-Type: application/json\r\nX-Holosmith-Client: 1\r\n\
             Idempotency-Key: verify-hosts-1\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#.len(),
            r#"{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}"#
        ),
    )
    .await;
    assert_eq!(created.status, 202, "{}", created.body);
    let id =
        serde_json::from_str::<serde_json::Value>(&created.body).expect("the row is JSON")["id"]
            .as_str()
            .expect("an id")
            .to_string();
    // `starting` and `idle` are both live — the fast stub may already have finished the four steps — and both are the
    // cap's own set, which is the set this refusal counts.
    let status = database_of(&state)
        .sessions()
        .get(&id)
        .expect("the row reads")
        .expect("the row is there")
        .status
        .to_string();
    assert!(
        ["starting", "idle", "running"].contains(&status.as_str()),
        "the session is live, not {status}"
    );

    // The local row: refused, with the count and the override in the sentence.
    let refused = delete(served.addr, LOCAL_ROW, "").await;
    assert_refusal(&refused, 409, HOST_BUSY);
    assert!(refused.body.contains("live session"), "{}", refused.body);
    assert!(refused.body.contains("force=true"), "{}", refused.body);
    // Nothing was removed by the refusal.
    assert!(row(&list(served.addr).await, LOCAL_ROW).is_some());

    // A row that does not name this machine has no sessions this machine can count, so it goes without a force.
    let faraway = delete(served.addr, "faraway", "").await;
    assert_eq!(faraway.status, 204, "{}", faraway.body);

    // The forced removal of the local row: the row goes, the session stays.
    let forced = delete(served.addr, LOCAL_ROW, "?force=true").await;
    assert_eq!(forced.status, 204, "{}", forced.body);
    assert!(row(&list(served.addr).await, LOCAL_ROW).is_none());
    assert!(
        database_of(&state)
            .sessions()
            .get(&id)
            .expect("the row reads")
            .is_some(),
        "the session outlived the row that named it"
    );

    // And a name the table does not hold is `404`, both before and after.
    let missing = delete(served.addr, "nobody", "").await;
    assert_refusal(&missing, 404, NOT_FOUND);
}

/// The API's own refusal of a non-Holosmith answer, where its two halves meet.
///
/// The probe's bytes are `hosts::probe`'s tests; the mapping is this. One function, so a probe that answered a
/// redirect and a `register` that stored it cannot disagree.
#[test]
fn a_refused_answer_is_the_registrations_own_code_and_a_merge_marks_the_row() {
    // The composition the API cannot reach: a **real** probe whose answer is a foreign payload, through the one
    // mapping that turns a refused outcome into `422 not_holosmith`. A literal `Refused` would only test the match.
    let (foreign, _) = stand_in(answered(
        &PAYLOAD.replace(r#""name":"holosmith""#, r#""name":"something-else""#),
    ));
    let origin = holosmith::config::Origin::parse(&format!("http://127.0.0.1:{foreign}"))
        .expect("an origin");
    let outcome = hosts::probe::ask(&origin, &Tools::default());
    assert!(
        matches!(outcome, hosts::probe::Outcome::Refused(_)),
        "a payload named something else is refused: {outcome:?}"
    );
    let refusal = hosts::accept(outcome, "workshop", &origin)
        .expect_err("a foreign payload is not a health reply");
    assert!(
        matches!(refusal, hosts::Refusal::NotHolosmith(ref sentence) if sentence.contains("something-else")),
        "{refusal:?}"
    );
    // The seed's mapping answers a row instead, because a file has no client to refuse.
    let row = hosts::merge_outcome(
        hosts::probe::Outcome::Refused("answered HTTP 302".to_string()),
        "workshop",
        &origin,
    );
    assert_eq!(row.reach, Reach::Unreachable);
    assert_eq!(row.last_error.as_deref(), Some("answered HTTP 302"));
    assert_eq!(row.version, None);
}
