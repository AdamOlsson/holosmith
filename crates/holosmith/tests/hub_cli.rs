//! `holosmith hub host`: the registry's own routes, from this machine.
//!
//! The CLI is a **client**, so the thing worth measuring is the request it sends and the answer it makes of the reply
//! — not a mock of `hosts::register`, which would prove nothing about "the same checks as `POST /api/hosts`". Every
//! test here stands a one-shot listener in and compares the bytes: the four guards' headers, the origin as **text**
//! (ADR-0074's landmine), the `?force=true` the API's own override is spelled with, and the envelope split ADR-0076
//! D6 fixes.
//!
//! What is deliberately *not* here: the rules themselves. They are `hosts::{name_problem, origin}`'s and the route's
//! own unit tests', and the gate's `case_hosts` measures them over the wire. A second copy of the alphabet in this
//! file would be the fork the story exists to prevent.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use holosmith::config::Config;
use holosmith::hosts::client::{Client, Failure};

/// The row a `201` answers, as the route serialises it.
const ROW: &str = r#"{"name":"workshop","url":"https://workshop.tail1234.ts.net","preview":null,"reach":"unreachable","version":null,"last_error":"nothing answered","last_checked_at":7}"#;

/// One connection's request bytes, and the answer the caller wants sent back.
///
/// A real socket rather than a fake `Client`: the head is the measurement, and a client that is asked instead of
/// observed would only agree with itself. The listener answers **one** connection and returns, for `hosts::probe`'s
/// own reason — a loop over `incoming()` outlives the test and hangs the binary.
fn record(answer: &'static str) -> (SocketAddr, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
    let addr = listener.local_addr().expect("the bound address is known");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("one connection arrives");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            // The head ends at the blank line; the body's length is the `Content-Length` the client wrote, so the
            // read ends where the request does rather than at a timeout.
            if let Some(head) = head_of(&request) {
                let length = content_length(&head);
                if request.len() >= head.len() + 4 + length {
                    break;
                }
            }
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => request.extend_from_slice(&buffer[..read]),
                Err(error) => panic!("the request could not be read ({error})"),
            }
        }
        stream
            .write_all(answer.as_bytes())
            .expect("the answer is writable");
        String::from_utf8_lossy(&request).into_owned()
    });
    (addr, handle)
}

/// The head of a request, once its blank line has arrived.
fn head_of(request: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(request);
    text.split_once("\r\n\r\n")
        .map(|(head, _)| head.to_string())
}

/// The `Content-Length` a head names, or `0`.
fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0)
}

/// A config whose listener is `addr` and whose origin is the default one.
fn config(addr: SocketAddr) -> Config {
    Config {
        control_listen: addr,
        ..Config::default()
    }
}

/// One client call, and the request it sent.
fn send<T>(answer: &'static str, call: impl FnOnce(&Client) -> T) -> (T, String) {
    let (addr, handle) = record(answer);
    let client = Client::of(&config(addr)).expect("a bound listener is an address to dial");
    let result = call(&client);
    let request = handle.join().expect("the listener's thread returns");
    (result, request)
}

#[test]
fn the_client_sends_the_four_guards_worth_of_headers_and_the_origins_text() {
    // The four checks, as bytes: `Host` is the allow-list's question and the client sends the origin's authority —
    // what `tailscale serve` hands the listener — `Origin` is the configured origin **verbatim** (the guard compares
    // text, ADR-0074), and the two `bad_client` headers are on the request because a `POST` needs both.
    let (result, request) = send(
        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 161\r\nConnection: close\r\n\r\n",
        |client| client.add("workshop", "https://workshop.tail1234.ts.net"),
    );
    // The `Content-Length` above is the row's, and the answer has no body: this arm is about the **request**.
    assert!(result.is_err(), "an answer with no body is not a row");
    let head = head_of(request.as_bytes()).expect("a complete head");
    assert!(
        head.starts_with("POST /api/hosts HTTP/1.1\r\n"),
        "the request line is not the route's: {head}"
    );
    let origin = Config::default().control_origin;
    assert!(
        head.contains(&format!(
            "\r\nHost: {}\r\n",
            origin.host().expect("the default origin names a host")
        )),
        "the client does not send the origin's authority as `Host`: {head}"
    );
    assert!(
        head.contains(&format!("\r\nOrigin: {}\r\n", origin.as_str())),
        "the client does not send the configured origin verbatim: {head}"
    );
    assert!(
        head.contains("\r\nX-Holosmith-Client: 1\r\n"),
        "the client does not send the client header: {head}"
    );
    assert!(
        head.contains("\r\nContent-Type: application/json\r\n"),
        "the client does not send a JSON content type: {head}"
    );
    assert!(
        request.ends_with(r#"{"name":"workshop","url":"https://workshop.tail1234.ts.net"}"#),
        "the body is not the route's two fields: {request}"
    );
}

#[test]
fn a_year_created_row_is_read_back_as_a_row() {
    let answer = format!(
        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ROW}",
        ROW.len()
    );
    // `Box::leak` keeps the answer a `&'static str` for the recorder's signature without a second helper.
    let leaked: &'static str = Box::leak(answer.into_boxed_str());
    let (result, request) = send(leaked, |client| {
        client.add("workshop", "https://workshop.tail1234.ts.net")
    });
    let row = result.expect("a 201 with a row is a row");
    assert_eq!(row.name, "workshop");
    assert_eq!(row.url.as_str(), "https://workshop.tail1234.ts.net");
    assert_eq!(row.last_checked_at, 7);
    assert!(
        head_of(request.as_bytes()).is_some_and(|head| head.starts_with("POST ")),
        "the row was not read from a POST: {request}"
    );
}

#[test]
fn a_list_is_the_routes_own_wrapper() {
    let body = format!("{{\"hosts\":[{ROW}]}}");
    let answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let leaked: &'static str = Box::leak(answer.into_boxed_str());
    let (result, request) = send(leaked, Client::list);
    let rows = result.expect("a 200 with the list is a list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "workshop");
    let head = head_of(request.as_bytes()).expect("a complete head");
    assert!(
        head.starts_with("GET /api/hosts HTTP/1.1\r\n"),
        "the list is not the route's: {head}"
    );
    // A `GET` needs only `Host`, and the client sends the origin and the client header anyway — one head for three
    // calls is one shape to read, and the guard ignores both on a `GET`. What a `GET` must **not** carry is the
    // content type: `bad_client` reads it as the claim "this request has a body", and there is none.
    assert!(
        head.contains("\r\nOrigin: "),
        "the client's one head lost its origin: {head}"
    );
    assert!(!head.contains("Content-Type:"), "{head}");
}

#[test]
fn force_is_the_apis_own_query_flag_and_a_delete_carries_the_json_content_type() {
    for (force, path) in [
        (false, "DELETE /api/hosts/thisbox HTTP/1.1"),
        (true, "DELETE /api/hosts/thisbox?force=true HTTP/1.1"),
    ] {
        let (result, request) = send(
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n",
            |client| client.remove("thisbox", force),
        );
        result.expect("a 204 is a removal");
        let head = head_of(request.as_bytes()).expect("a complete head");
        assert_eq!(
            head.lines().next().unwrap_or_default(),
            path,
            "--force is not the API's override"
        );
        // A `DELETE` is a non-`GET`, and `bad_client` reads the content type even with no body.
        assert!(
            head.contains("\r\nContent-Type: application/json\r\n"),
            "a DELETE without a JSON content type answers 403 bad_client: {head}"
        );
        assert!(
            head.contains("\r\nContent-Length: 0\r\n"),
            "a DELETE with a body promised and not sent would hang the route: {head}"
        );
    }
}

#[test]
fn a_name_that_is_not_the_alphabets_still_reaches_the_route_so_the_route_can_refuse_it() {
    let (result, request) = send(
        "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 42\r\nConnection: close\r\n\r\n{\"code\":\"not_found\",\"message\":\"nope\"}",
        |client| client.remove("a b", false),
    );
    assert_eq!(
        result,
        Err(Failure::Refused {
            code: "not_found".to_string(),
            message: "nope".to_string(),
        })
    );
    let head = head_of(request.as_bytes()).expect("a complete head");
    assert!(
        head.starts_with("DELETE /api/hosts/a%20b HTTP/1.1"),
        "a name with a space broke the request line instead of being encoded: {head}"
    );
}

#[test]
fn each_refusal_is_the_apis_code_and_message_and_prints_as_the_screen_prints_it() {
    let body = r#"{"code":"bad_host_name","message":"`name` must not be \"local\""}"#;
    let answer = format!(
        "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let leaked: &'static str = Box::leak(answer.into_boxed_str());
    let (result, _) = send(leaked, |client| {
        client.add("local", "https://box.tail1.ts.net")
    });
    let failure = result.expect_err("a 422 is a refusal");
    assert_eq!(
        failure.sentence(),
        "`name` must not be \"local\"",
        "the CLI did not print the API's sentence verbatim"
    );

    // A code that names something the operator cannot see from the command keeps its code in front, exactly as the
    // Computers screen does — ADR-0076 D6, one caller further out.
    let body = r#"{"code":"host_busy","message":"\"thisbox\" is this machine, and it has 1 live session"}"#;
    let answer = format!(
        "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let leaked: &'static str = Box::leak(answer.into_boxed_str());
    let (result, _) = send(leaked, |client| client.remove("thisbox", false));
    let failure = result.expect_err("a 409 is a refusal");
    assert!(
        failure.sentence().starts_with("host_busy: "),
        "a busy row lost the code the screen keeps: {}",
        failure.sentence()
    );
    assert!(
        failure.sentence().contains("1 live session"),
        "the API's sentence was not printed verbatim: {}",
        failure.sentence()
    );
}

#[test]
fn nothing_answering_is_a_sentence_naming_the_address_and_faking_nothing() {
    // A port the kernel gave and then closed: the connect is refused, and the sentence has to say where and what
    // would answer — a CLI that printed a `404` here would be inventing an answer it never received.
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
    let addr = listener.local_addr().expect("the bound address is known");
    drop(listener);
    let client = Client::of(&config(addr)).expect("a bound-then-closed port is still an address");
    let failure = client.list().expect_err("nothing is serving there");
    let sentence = failure.sentence();
    assert!(
        sentence.contains(&addr.to_string()),
        "the sentence does not name the address: {sentence}"
    );
    assert!(
        sentence.contains("serve"),
        "the sentence does not name what would answer: {sentence}"
    );
}

#[test]
fn a_body_that_is_not_the_envelope_is_not_a_refusal() {
    // A `403` from a proxy, an HTML error page, anything that is not the API: there is no code to report, so it is
    // the machine's problem rather than this request's.
    let (result, _) = send(
        "HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\nConnection: close\r\n\r\nforbidden",
        Client::list,
    );
    let failure = result.expect_err("a 403 without an envelope is not a refusal");
    assert!(
        matches!(failure, Failure::Unreachable(_)),
        "an answer that is not the API was reported as a refusal: {failure:?}"
    );
    assert!(
        failure.sentence().contains("envelope"),
        "the sentence does not say what was missing: {}",
        failure.sentence()
    );
}
