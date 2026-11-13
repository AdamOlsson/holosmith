//! `holosmith hub host`: the registry's own routes, called from this machine.
//!
//! The spec's Commands table asks `hub host add|list|remove` to "perform the same checks as `POST /api/hosts`", and
//! the only way for a second caller to perform the *same* checks is to be a client of the route that owns them. So
//! this module is a small hand-written HTTP client — `crate::http::health::ask`'s shape with a body and a status
//! line — and the registry's rules, their sentences and their codes stay exactly where they are.
//!
//! # Which address, and why not the origin
//!
//! The client dials `control_listen`, not `control_origin`: it runs **on** the machine, so the hop `tailscale serve`
//! would make is one it can skip, and an `https` origin would mean an `s_client` child and a trust seam for a
//! request that never leaves the box.
//!
//! It still sends the origin's own **host** in `Host` and its exact text in `Origin`, which is what the served
//! request looks like after `tailscale serve` has proxied it: the guard compares `Origin` as text against
//! `control_origin` or a loopback alias of it (ADR-0074's landmine), and `Host` is the allow-list's question. A
//! caller on this machine arrives from `127.0.0.1`, which is ADR-0075's **front door** and asks no whois. The other
//! two guards' headers — `Content-Type: application/json` even with no body, and `X-Holosmith-Client: 1` — are sent
//! on every non-`GET`, because they are what `bad_client` reads.
//!
//! # What a refusal is
//!
//! ADR-0008's envelope, split exactly where the Computers screen splits it (`hostFailure`, ADR-0076 D6): the CLI
//! prints the envelope's `message` **verbatim**, and keeps the `code` in front only for the codes that are not about
//! something the operator just typed. One sentence per refusal, two callers — the fork this story exists to prevent.
//!
//! Nothing is invented when nothing answers: a machine whose `serve` is not running has no registry to add to, and
//! that is one sentence naming the address and the deploy step that waits for `/api/health`.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Config;
use crate::hosts::Host;
use crate::http::error::{BAD_HOST_NAME, BAD_HOST_URL, NOT_HOLOSMITH};

/// How long one call may take, connect and answer.
///
/// `add` is the reason it is one number rather than two: the route runs a probe of the host being registered, which
/// is bounded at `hosts::probe::PROBE_TIMEOUT` itself but is preceded by name resolution. Ten seconds is the route's
/// own bound plus room, and `list`/`remove` answer in milliseconds under it.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The largest answer this client reads before it gives up on the request being an answer at all.
const MAX_ANSWER: usize = 256 * 1024;

/// This machine's own control listener, as a client of its own registry.
#[derive(Debug, Clone)]
pub struct Client {
    /// The address dialled: the listener's, with an unspecified address folded to loopback.
    target: SocketAddr,
    /// `control_origin` verbatim — the `Origin` header the guard compares as text.
    origin: String,
    /// The origin's host, without its port, which is the `Host` a served request carries. The guard reduces `Host` to
    /// a port-less name itself (`guard::host_of`), so writing it that way removes the question rather than relying on
    /// the reduction agreeing with this field.
    host: String,
}

/// Why one call did not do what it was asked.
///
/// Two arms and the split is ADR-0008's own: a **refusal** is the API answering about this request (a code and the
/// sentence to show), while **unreachable** is this machine's problem (nothing answered, or something answered that
/// is not the API) — which no code can describe, because no route produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The registry refused this request, with the envelope's two fields.
    Refused { code: String, message: String },
    /// Nothing answered, or what answered was not this API.
    Unreachable(String),
}

impl Failure {
    /// The one sentence an operator reads, which is `hostFailure`'s rule (ADR-0076 D6).
    ///
    /// The three registration codes are about values the operator typed into the same command, so the sentence alone
    /// is the answer; every other code names something the operator cannot see from here, so it stays in front. A
    /// `host_busy` therefore reads `host_busy: "thisbox" is this machine, and it has 1 live session; …`, exactly as
    /// the screen shows it.
    pub fn sentence(&self) -> String {
        match self {
            Self::Refused { code, message } => match code.as_str() {
                BAD_HOST_NAME | BAD_HOST_URL | NOT_HOLOSMITH => message.clone(),
                _ => format!("{code}: {message}"),
            },
            Self::Unreachable(sentence) => sentence.clone(),
        }
    }
}

impl Client {
    /// The client a config's listener answers, or the sentence saying there is no address to dial.
    ///
    /// A `:0` control listener is refused rather than guessed at: a port the kernel gives is one this process cannot
    /// know, and a client that asked for it would connect to nothing. An unspecified address is folded to loopback
    /// (`health::ask`'s fold): a machine's own registry is reached on this machine.
    pub fn of(config: &Config) -> Result<Self, String> {
        let listen = config.control_listen;
        if listen.port() == 0 {
            return Err(format!(
                "the control listener is {listen}, which asks the kernel for a port, so there is no address here \
                 to reach this machine's own registry on: point `control_listen` at the port `serve` binds"
            ));
        }
        let host = config.control_origin.host().ok_or_else(|| {
            format!(
                "`control_origin` ({}) names no host, so there is no `Host` to send and the guard would refuse the \
                 request as `bad_host`",
                config.control_origin.as_str()
            )
        })?;
        Ok(Self {
            target: SocketAddr::new(
                if listen.ip().is_unspecified() {
                    IpAddr::V4(Ipv4Addr::LOCALHOST)
                } else {
                    listen.ip()
                },
                listen.port(),
            ),
            origin: config.control_origin.as_str().to_string(),
            host: host.to_string(),
        })
    }

    /// Every row, by name (the route's own order).
    pub fn list(&self) -> Result<Vec<Host>, Failure> {
        let body = self.request("GET", "/api/hosts", None)?;
        serde_json::from_str::<crate::http::hosts::HostList>(&body)
            .map(|list| list.hosts)
            .map_err(|error| {
                Failure::Unreachable(format!(
                    "{} answered a body that is not a host list ({error}): something else is on that port",
                    self.target
                ))
            })
    }

    /// Register one machine, and answer the row the API stored.
    pub fn add(&self, name: &str, url: &str) -> Result<Host, Failure> {
        let body = serde_json::json!({ "name": name, "url": url }).to_string();
        let answer = self.request("POST", "/api/hosts", Some(&body))?;
        serde_json::from_str::<Host>(&answer).map_err(|error| {
            Failure::Unreachable(format!(
                "{} answered a body that is not a host row ({error}): the registration was stored, and this is not \
                 the answer this client can read it back from",
                self.target
            ))
        })
    }

    /// Forget one row, and `?force=true` is the API's own override for a row this machine still has sessions for.
    pub fn remove(&self, name: &str, force: bool) -> Result<(), Failure> {
        let path = if force {
            format!("/api/hosts/{}?force=true", segment(name))
        } else {
            format!("/api/hosts/{}", segment(name))
        };
        self.request("DELETE", &path, None).map(drop)
    }

    /// One request, one answer's body, or the failure it is.
    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<String, Failure> {
        let mut stream = TcpStream::connect_timeout(&self.target, TIMEOUT).map_err(|error| {
            Failure::Unreachable(format!(
                "nothing answered at {} ({error}): this machine's `serve` is not running, and the registry it holds \
                 is only reachable while it is — `sudo systemctl start holosmith` on a deployed machine, or \
                 `holosmith serve` by hand",
                self.target
            ))
        })?;
        let _ = stream.set_read_timeout(Some(TIMEOUT));
        let _ = stream.set_write_timeout(Some(TIMEOUT));
        let payload = body.unwrap_or_default();
        // The head is the four guards' worth of headers, in the shape `tailscale serve` would hand the listener:
        // a `Host` this machine answers on, and the configured origin as text.
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nX-Holosmith-Client: 1\r\n",
            self.host, self.origin
        );
        if method != "GET" {
            head.push_str("Content-Type: application/json\r\n");
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        ));
        let mut request = head.into_bytes();
        request.extend_from_slice(payload.as_bytes());
        stream.write_all(&request).map_err(|error| {
            Failure::Unreachable(format!(
                "{method} {path} on {} could not be sent ({error})",
                self.target
            ))
        })?;
        let mut answer = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut capped = false;
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    answer.extend_from_slice(&buffer[..read]);
                    if answer.len() >= MAX_ANSWER {
                        capped = true;
                        break;
                    }
                }
                Err(error) => {
                    return Err(Failure::Unreachable(format!(
                        "{method} {path} on {} was not answered ({error})",
                        self.target
                    )));
                }
            }
        }
        self.read(method, path, &answer, capped)
    }

    /// One answer's status line and body, as the API means them.
    ///
    /// `capped` is the read's own fact and it is spent before the parse: a body this client stopped reading is not a
    /// body that is wrong, and reporting it as "something else is on that port" would blame the answer for the
    /// reader's own cap.
    fn read(
        &self,
        method: &str,
        path: &str,
        answer: &[u8],
        capped: bool,
    ) -> Result<String, Failure> {
        if capped {
            return Err(Failure::Unreachable(format!(
                "{method} {path} on {} answered more than this client reads ({MAX_ANSWER} bytes) in one answer",
                self.target
            )));
        }
        let text = String::from_utf8_lossy(answer);
        let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| {
            Failure::Unreachable(format!(
                "{method} {path} on {} answered no complete header block",
                self.target
            ))
        })?;
        let status = head.lines().next().unwrap_or_default();
        let code = status
            .split_whitespace()
            .nth(1)
            .and_then(|digits| digits.parse::<u16>().ok());
        let Some(code) = code else {
            return Err(Failure::Unreachable(format!(
                "{method} {path} on {} answered {status:?}, which is not a status line",
                self.target
            )));
        };
        if (200..300).contains(&code) {
            return Ok(body.to_string());
        }
        // The envelope, or a sentence about something that is not this API: a `403 bad_host` from the guard, a
        // fallback's `404`, anything else on the port.
        match serde_json::from_str::<Envelope>(body) {
            Ok(error) => Err(Failure::Refused {
                code: error.code,
                message: error.message,
            }),
            Err(_) => Err(Failure::Unreachable(format!(
                "{method} {path} on {} answered {code} with a body that is not the error envelope",
                self.target
            ))),
        }
    }
}

/// ADR-0008's envelope as a **client** reads it: two owned strings.
///
/// Not `http::error::ErrorBody`, and not a second spelling of it either: the server's type holds `&'static str`
/// because its codes are literals (and its `message` a `Cow` for the one refusal that carries a measurement), which
/// is not a shape any body can be deserialised into. This is the wire's two fields and nothing else — `conflicts`
/// is skipped on the wire for every error but a merge's, and serde ignores what a struct does not name.
#[derive(Debug, serde::Deserialize)]
struct Envelope {
    code: String,
    message: String,
}

/// One path segment, percent-encoded so a name that is not the registry's alphabet still reaches the route.
///
/// The alphabet is `hosts::name_problem`'s and this is **not** a second copy of it: a name the route refuses is
/// supposed to be refused by the route, with the route's sentence. What this prevents is a name that cannot be sent
/// at all — a space or a `?` in a request line is a malformed request, and the answer would be a connection error
/// instead of `422 bad_host_name`.
fn segment(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_segment_is_encoded_so_the_route_can_refuse_it() {
        assert_eq!(segment("workshop"), "workshop");
        assert_eq!(segment("work-shop_2.x~y"), "work-shop_2.x~y");
        // The names the route's own alphabet refuses, which still have to be *sendable* to earn that refusal.
        assert_eq!(segment("a b"), "a%20b");
        assert_eq!(segment("a/b"), "a%2Fb");
        assert_eq!(segment("a?b"), "a%3Fb");
        assert_eq!(segment("Workshop"), "Workshop");
        assert_eq!(segment("ü"), "%C3%BC");
    }

    #[test]
    fn the_three_registration_codes_print_their_sentence_alone() {
        for code in [BAD_HOST_NAME, BAD_HOST_URL, NOT_HOLOSMITH] {
            let failure = Failure::Refused {
                code: code.to_string(),
                message: "a sentence".to_string(),
            };
            assert_eq!(failure.sentence(), "a sentence", "{code}");
        }
        // Everything else keeps its code: it names something the operator cannot see from the command.
        let busy = Failure::Refused {
            code: "host_busy".to_string(),
            message: "it has 1 live session".to_string(),
        };
        assert_eq!(busy.sentence(), "host_busy: it has 1 live session");
        assert_eq!(
            Failure::Unreachable("nothing answered".to_string()).sentence(),
            "nothing answered"
        );
    }

    #[test]
    fn a_control_listener_the_kernel_would_choose_is_not_an_address() {
        let config = Config {
            control_listen: "127.0.0.1:0".parse().expect("a literal address"),
            ..Config::default()
        };
        let reason = Client::of(&config).expect_err("a `:0` listener has no address to dial");
        assert!(reason.contains("asks the kernel for a port"), "{reason}");
    }

    #[test]
    fn the_client_dials_loopback_for_an_unspecified_listener_and_sends_the_origins_authority() {
        let config = Config {
            control_listen: "0.0.0.0:8080".parse().expect("a literal address"),
            ..Config::default()
        };
        let client =
            Client::of(&config).expect("a listener on every address is dialable on loopback");
        assert_eq!(
            client.target,
            "127.0.0.1:8080".parse::<SocketAddr>().expect("a literal")
        );
        assert_eq!(client.origin, config.control_origin.as_str());
        assert_eq!(
            client.host,
            config
                .control_origin
                .host()
                .expect("the default origin names a host")
        );
    }
}
