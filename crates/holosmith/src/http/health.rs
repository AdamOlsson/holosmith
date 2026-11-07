//! The health endpoint: the keys of the story, and the one endpoint no role check applies to
//! (ADR-0007).

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config::{Config, Role};
use crate::runtime::Runtime;

/// The name the health payload reports.
pub const NAME: &str = "holosmith";

/// How long a client waits for the answer, connect included: this is a loopback request to a program on this
/// machine, so the bound is the difference between "not serving" and "wedged", never a network's latency.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(3);

/// The largest answer a probe reads before it gives up on the request being a health answer.
const MAX_ANSWER: usize = 8 * 1024;

/// Exactly five keys, in this order. `target` and the rest of the build metadata stay on the
/// startup line and in `version --json`: the host registration of item 4 reads name and
/// version, and a key nobody reads is a promise about a shape.
///
/// `preview_origin` is the fifth (HS-041): the session screen links to `/<host>/<id>/` on it, and this is
/// where the page already looks — it fetches health before either screen draws, so the link costs no request
/// and no second route. The alternative, injecting the value into the app shell, would break
/// `case_app_shell`'s byte-for-byte comparison and would be a dynamic value in a static asset (ADR-0051).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Health {
    pub name: &'static str,
    pub version: String,
    pub role: Role,
    pub uptime_s: u64,
    pub preview_origin: String,
}

impl Health {
    /// What `GET /api/health` reports for this config and this run.
    pub fn of(config: &Config, runtime: &Runtime) -> Self {
        Self {
            name: NAME,
            version: env!("CARGO_PKG_VERSION").to_string(),
            role: config.role,
            uptime_s: runtime.uptime_s(),
            preview_origin: config.preview_origin.as_str().to_string(),
        }
    }
}

/// The handler's body: the payload as JSON.
pub fn respond(health: &Health) -> Json<Health> {
    Json(health.clone())
}

/// The three facts of the payload a **client** reads back: what `holosmith status` prints and what
/// [`crate::doctor`]'s port check asks about.
///
/// Three fields, not five, because `name` is a constant and `preview_origin` is the app shell's business; a
/// client struct that carried them would be a second reading of a shape it does not use.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Answer {
    pub version: String,
    pub role: Role,
    pub uptime_s: u64,
}

/// Ask `listen` for `GET /api/health` and read the answer back, or say why there is none.
///
/// One reader, two callers, and that is the point (HS-050): `holosmith status` prints the `uptime_s` and
/// `doctor`'s port check asks whether anything answers at all, so "this machine is serving" has one meaning in
/// the binary rather than two hand-written requests that can drift.
///
/// Hand-written like [`crate::preflight`]'s `_ping`, and for the same reason: one request, no client library.
/// `/api/health` is guard-exempt, so no header but a `Host` is needed, and `Connection: close` is what makes the
/// read end at the body rather than at a timeout.
pub fn ask(listen: SocketAddr) -> Result<Answer, String> {
    if listen.port() == 0 {
        return Err(format!(
            "{listen} asks the kernel for a port, so there is no address to ask for this machine's health"
        ));
    }
    // An unspecified address is not dialable: the machine's own listener is on loopback.
    let host = if listen.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        listen.ip()
    };
    let target = SocketAddr::new(host, listen.port());
    let mut stream = TcpStream::connect_timeout(&target, ASK_TIMEOUT)
        .map_err(|error| format!("GET /api/health on {target} could not be asked ({error})"))?;
    let _ = stream.set_read_timeout(Some(ASK_TIMEOUT));
    let _ = stream.set_write_timeout(Some(ASK_TIMEOUT));
    let request = b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    stream
        .write_all(request)
        .map_err(|error| format!("GET /api/health on {target} could not be sent ({error})"))?;
    let mut answer = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                answer.extend_from_slice(&buffer[..read]);
                if answer.len() >= MAX_ANSWER {
                    break;
                }
            }
            Err(error) => {
                return Err(format!(
                    "GET /api/health on {target} was not answered ({error})"
                ));
            }
        }
    }
    read_answer(&target, &answer)
}

/// The status line and the body of one answer, as [`ask`] reads them.
fn read_answer(target: &SocketAddr, answer: &[u8]) -> Result<Answer, String> {
    let text = String::from_utf8_lossy(answer);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("GET /api/health on {target} answered no complete header block"))?;
    let status = head.lines().next().unwrap_or_default();
    if !status.contains(" 200") {
        return Err(format!(
            "GET /api/health on {target} answered {status:?} rather than 200"
        ));
    }
    serde_json::from_str::<Answer>(body).map_err(|error| {
        format!(
            "GET /api/health on {target} answered a body that is not the health payload ({error})"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Overrides;
    use crate::runtime::Backend;

    fn health(role: Role) -> Health {
        let mut config = Config {
            role,
            ..Config::default()
        };
        config.apply_overrides(&Overrides::default());
        Health::of(&config, &Runtime::started(Backend::Stub))
    }

    #[test]
    fn the_payload_is_exactly_five_keys_in_order() {
        assert_eq!(
            serde_json::to_string(&health(Role::Hub)).expect("five fields always serialise"),
            format!(
                r#"{{"name":"holosmith","version":"{}","role":"hub","uptime_s":0,"preview_origin":"http://localhost:8081"}}"#,
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn every_role_has_its_spec_word() {
        for (role, word) in [
            (Role::Hub, "hub"),
            (Role::Server, "server"),
            (Role::Both, "both"),
        ] {
            let json = serde_json::to_string(&health(role)).expect("serialisable");
            assert!(
                json.contains(&format!(r#""role":"{word}""#)),
                "{role:?} did not render as {word}: {json}"
            );
        }
    }

    #[test]
    fn uptime_never_goes_backwards() {
        let runtime = Runtime::started(Backend::Docker);
        let first = runtime.uptime_s();
        let second = runtime.uptime_s();
        assert!(second >= first, "{second} < {first}");
    }
}
