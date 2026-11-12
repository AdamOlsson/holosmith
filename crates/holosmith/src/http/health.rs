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

/// Exactly seven keys, in this order. `target` and the rest of the build metadata stay on the
/// startup line and in `version --json`: the host registration of item 4 reads name and
/// version, and a key nobody reads is a promise about a shape.
///
/// `preview_origin` is the fifth: the session screen links to `/<host>/<id>/` on it, and this is
/// where the page already looks — it fetches health before either screen draws, so the link costs no request
/// and no second route. The alternative, injecting the value into the app shell, would break
/// `case_app_shell`'s byte-for-byte comparison and would be a dynamic value in a static asset (ADR-0051).
///
/// `application_server_key` is the sixth (ADR-0072): the Web Push pair's **public** half, which is what
/// `pushManager.subscribe` needs and what nothing served before. `null` is a machine with no pair, and it is
/// read at the moment of asking — a `server deploy` between two page loads is answered truthfully by the
/// second. The same argument as `preview_origin`'s: the page already asks this question, and a browser needs
/// the value before it can subscribe.
///
/// `machine_name` is the seventh (ADR-0079): what this machine calls itself in a notification's payload,
/// so a page can label its own row with the name the phone will show and a hub can read a peer's claim through the
/// hop. It is the config key verbatim (`local` by default), not a derived name, because the tap's path and the
/// registry's row name are the same string one hop apart and only the operator can keep them equal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Health {
    pub name: &'static str,
    pub version: String,
    pub role: Role,
    pub uptime_s: u64,
    pub preview_origin: String,
    pub application_server_key: Option<String>,
    pub machine_name: String,
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
            application_server_key: crate::vapid::public_key(&config.data_root),
            machine_name: config.machine_name.clone(),
        }
    }
}

/// The handler's body: the payload as JSON.
pub fn respond(health: &Health) -> Json<Health> {
    Json(health.clone())
}

/// The four facts of the payload a **client** reads back, plus the one a **hub** needs: what
/// `holosmith status` prints, what [`crate::doctor`]'s port check asks about, what the host registry's probe
/// registers a host by, and where that host's preview listener answers.
///
/// Four fields originally, not six, because `application_server_key` is the app shell's business and `role` is this
/// machine's config where a client reads it as a fact about the other machine. `name` joined because a
/// *third* caller does use it: `POST /api/hosts` refuses an answer whose payload is not a Holosmith's, and item 4
/// spells that as "the reply must carry the Holosmith name and version fields". `preview_origin` joins
/// for the same reason one step further out: the registry's row needs the peer's **preview** origin to forward
/// `/<host>/<id>/*` to it (ADR-0074), and this payload is where a machine says what that is.
///
/// **Optional, so a payload without it still parses.** The key answers for this build's own second listener; a peer
/// that names none is a host whose previews cannot be forwarded (one sentence at the attempt), never a host that is
/// not a Holosmith.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Answer {
    pub name: String,
    pub version: String,
    pub role: Role,
    pub uptime_s: u64,
    #[serde(default)]
    pub preview_origin: Option<String>,
}

/// One payload's body as an [`Answer`], or the reason it is not one.
///
/// The one place that turns bytes into this shape, so the loopback client below and the host registry's probe read
/// one payload one way: `ask` has already checked the status line, and the probe checks it itself before it gets
/// here, but neither may invent a second spelling of "what a health answer is".
pub fn parse_answer(body: &str) -> Result<Answer, serde_json::Error> {
    serde_json::from_str::<Answer>(body)
}

/// Ask `listen` for `GET /api/health` and read the answer back, or say why there is none.
///
/// One reader, two callers, and that is the point: `holosmith status` prints the `uptime_s` and
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
    parse_answer(body).map_err(|error| {
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
        let dir =
            std::env::temp_dir().join(format!("holosmith-health-{role:?}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The data root is the test's own: `Config::default` names the machine's, and this machine may have
        // deployed, which would make the sixth key a fact about the host rather than about this test.
        let mut config = Config {
            role,
            data_root: dir,
            ..Config::default()
        };
        config.apply_overrides(&Overrides::default());
        Health::of(&config, &Runtime::started(Backend::Stub))
    }

    #[test]
    fn the_payload_is_exactly_seven_keys_in_order() {
        assert_eq!(
            serde_json::to_string(&health(Role::Hub)).expect("seven fields always serialise"),
            format!(
                r#"{{"name":"holosmith","version":"{}","role":"hub","uptime_s":0,"preview_origin":"http://localhost:8081","application_server_key":null,"machine_name":"local"}}"#,
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn the_application_server_key_is_the_machines_public_half_or_null() {
        let dir =
            std::env::temp_dir().join(format!("holosmith-health-vapid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = Config {
            data_root: dir.clone(),
            ..Config::default()
        };
        config.apply_overrides(&Overrides::default());
        // No pair: `null`, which is a JSON null rather than an absent key (a client reads one shape).
        let without = Health::of(&config, &Runtime::started(Backend::Stub));
        assert_eq!(without.application_server_key, None);
        assert!(
            serde_json::to_string(&without)
                .expect("serialisable")
                .contains(r#""application_server_key":null,"#),
            "a machine with no pair answers an explicit null"
        );

        let path = crate::vapid::store(&dir);
        std::fs::create_dir_all(path.parent().expect("the secrets directory"))
            .expect("the directory is creatable");
        let pair = crate::vapid::Pair {
            public_key: "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8".to_string(),
            private_key: "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw".to_string(),
        };
        std::fs::write(&path, crate::vapid::render(&pair)).expect("the pair is writable");
        let with = Health::of(&config, &Runtime::started(Backend::Stub));
        assert_eq!(
            with.application_server_key.as_deref(),
            Some(pair.public_key.as_str()),
            "the sixth key is the pair's public half, byte for byte"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
