//! Machine-to-machine trust: which machines may drive this one (HS-070, spec item 4, V11).
//!
//! Item 4's last bullet is two rules, and this module is the second: "a machine accepts forwarded calls only from
//! peers listed in `allowed_hubs`, verified with the Tailscale LocalAPI whois of the remote address". The first —
//! the browser-facing checks — is [`crate::http::guard`]'s, and item 4 says those "apply at the hub".
//!
//! The hole it closes is the one ADR-0074 recorded rather than discovered: [`crate::forward::request_head`] authors a
//! call whose `Host` and `Origin` are the peer's own and which drops `tailscale-user-login`, so a peer cannot tell a
//! forwarded call from a call at its own origin — and its `owner` check is vacuous for one. Two hops reach a peer,
//! the session API ([`crate::http::forward`]) and the preview ([`crate::preview`]), so there is **one** decision
//! here and each listener spells its refusal in its own words.
//!
//! # Who is calling
//!
//! The identity is **not** a header: the peer reads the address the connection came from and asks the Tailscale
//! daemon who that address is. `tailscale whois --json <ip>` is one child ([`PROGRAM`], one deadline), and its answer
//! names a node. The list holds **machine names**, so the comparison is the node's short name — `hub` out of
//! `hub.tailnet1a2b.ts.net` — because that is the name the registry keys rows by, the name item 16's one file
//! copied to every machine spells, and the name `hub host add` (HS-072) will collect.
//!
//! `UserProfile.Login` is deliberately **not** what is compared: the login is the *owner*, and it is identical on
//! every node the operator has — so comparing it would admit the stray node this story exists to refuse.
//!
//! # Three arms, and the one that admits without asking
//!
//! | `allowed_hubs` | peer | answer |
//! | --- | --- | --- |
//! | empty | anything | [`Admitted::Unchecked`] — the operator has chosen tailnet ACLs; V11's "If it fails" cell |
//! | non-empty | `127.0.0.1` or `::1` | [`Admitted::FrontDoor`] — `tailscale serve` proxies both origins to loopback |
//! | non-empty | anything else the whois names | [`Admitted::Node`] if listed, [`Refusal::NotListed`] if not |
//! | non-empty | anything else the whois cannot name | [`Refusal::Unverifiable`] |
//!
//! The front door is those two addresses **exactly**, not `IpAddr::is_loopback()`: `tailscale serve` (deploy step 6)
//! is what connects from `127.0.0.1`, and the rest of `127/8` is an address no deployment uses — so a peer at
//! `127.0.0.2` is a peer, which is also what lets one box measure the refusal at all.
//!
//! An identity that cannot be read is **not** an identity: a whois that fails refuses the call. The spec's fallback
//! to ACLs only is spelled by an **empty list** — a decision the operator makes in the config — and never by a
//! daemon that stopped answering.
//!
//! # What this machine cannot measure
//!
//! There is no `tailscaled` here (`tailscale status` answers "failed to connect to local tailscaled"), so every real
//! answer in this file's test is a shim's and the by-hand demo's failing arm is the real one. The shape of the child
//! is [`crate::hosts::probe`]'s — one child, one deadline, its answer read from pipes after it has exited — because a
//! whois answer is a kilobyte and fits a pipe.

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::preflight::Tools;

/// The program a whois runs: `tailscale` from `PATH`, or a shim in a test.
pub const PROGRAM: &str = "tailscale";

/// How long one whois may take, and the reason there is a bound: it runs on a request's path, where a wedged daemon
/// must cost a refusal rather than a hang. `hosts::probe`'s three seconds, because both ask one local daemon.
pub const WHOIS_TIMEOUT: Duration = Duration::from_secs(3);

/// The pause between two `try_wait` calls.
const POLL: Duration = Duration::from_millis(5);

/// The longest stderr a refusal's log line carries, so one broken child cannot push a log line to a kilobyte.
const DETAIL_MAX: usize = 200;

/// Which machines this one takes forwarded calls from, resolved once per process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    /// The `allowed_hubs` entries, lowercased: machine names, as the config's own validator admits them.
    hubs: Vec<String>,
    /// The whois program ([`PROGRAM`] unless a test says otherwise).
    program: PathBuf,
}

impl Trust {
    /// The trust a boot applies: the config's list, and the program a machine runs.
    pub fn of(config: &Config, tools: &Tools) -> Self {
        Self {
            hubs: config
                .allowed_hubs
                .iter()
                .map(|hub| hub.trim().to_ascii_lowercase())
                .filter(|hub| !hub.is_empty())
                .collect(),
            program: tools.tailscale.clone(),
        }
    }

    /// Whether this machine verifies its callers at all. `false` is the ACLs-only fallback, and it is the default
    /// config's answer.
    pub fn guarded(&self) -> bool {
        !self.hubs.is_empty()
    }

    /// What this machine makes of a caller whose connection came from `peer`.
    ///
    /// Blocking, because a whois is a child: every caller runs it on `spawn_blocking`. `None` is an address the
    /// listener could not read at all, which is a refusal and not a pass (a machine that cannot say who is calling
    /// has not verified anybody).
    pub fn admit(&self, peer: Option<IpAddr>) -> Result<Admitted, Refusal> {
        if !self.guarded() {
            return Ok(Admitted::Unchecked);
        }
        let Some(peer) = peer.map(|address| address.to_canonical()) else {
            return Err(Refusal::NoPeer {
                detail: "the listener did not report the connection's address".to_string(),
            });
        };
        if front_door(peer) {
            return Ok(Admitted::FrontDoor(peer));
        }
        match whois(&self.program, peer) {
            Ok(name) if self.hubs.iter().any(|hub| hub == &name) => {
                Ok(Admitted::Node { peer, name })
            }
            Ok(name) => Err(Refusal::NotListed { peer, node: name }),
            Err(detail) => Err(Refusal::Unverifiable { peer, detail }),
        }
    }
}

/// What a caller was, once [`Trust::admit`] has answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admitted {
    /// This machine verifies nobody: `allowed_hubs` is empty, so tailnet ACLs are the only control (V11's fallback).
    Unchecked,
    /// The machine's own front door — `tailscale serve`, the CLI, `doctor`, a host probe, the gate. Not a peer.
    FrontDoor(IpAddr),
    /// A tailnet peer the LocalAPI named, and whose name is one of this machine's hubs.
    Node { peer: IpAddr, name: String },
}

impl Admitted {
    /// One `debug` line naming who was let in, which is the other half of [`Refusal::log`]: an operator reading a
    /// log can see which machine a forwarded call came from, and "the peer's log names the verified peer" is a
    /// measurement rather than a claim.
    pub fn log(&self) {
        match self {
            Self::Node { peer, name } => tracing::debug!(
                target: "holosmith::trust",
                %peer,
                node = %name,
                "a call from a listed hub was accepted"
            ),
            Self::FrontDoor(peer) => tracing::debug!(
                target: "holosmith::trust",
                peer = %peer,
                "a call arrived at this machine's front door"
            ),
            Self::Unchecked => {}
        }
    }
}

/// Why a caller was refused, and the words both listeners answer with.
///
/// One decision and two spellings (`crate::http::error`'s envelope on the control listener, a sentence in plain text
/// on the preview listener), for ADR-0051's reason: the preview listener has no envelope, and a refusal that named a
/// JSON code there would be an API this listener does not have.
///
/// **No sentence names the configuration.** It names the peer, and — when there is one — the node the tailnet named
/// for it, because that is the caller's own identity and not this machine's business; the list's entries, its length
/// and the config's path are never in an answer (acceptance bullet 5). The `detail` a failed whois carries is for the
/// log line only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The listener could not report the connection's address, so nobody was verified.
    NoPeer { detail: String },
    /// The tailnet named this node, and it is not a hub this machine accepts calls from.
    NotListed { peer: IpAddr, node: String },
    /// The tailnet identity could not be read: no daemon, no CLI, no node in the answer.
    Unverifiable { peer: IpAddr, detail: String },
}

impl Refusal {
    /// The status both listeners answer with. `403`: the caller is a machine this one will not be driven by.
    pub fn status(&self) -> u16 {
        403
    }

    /// The word a control-listener envelope carries, and the word this refusal is logged with.
    ///
    /// The constants are [`crate::http::error`]'s because the envelope is: this module owns the decision, the
    /// listener owns the code, and a literal here would be the second spelling of a word a client keys on
    /// (`docs/notes.md`'s envelope rule).
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotListed { .. } => crate::http::error::PEER_NOT_ALLOWED,
            Self::NoPeer { .. } | Self::Unverifiable { .. } => {
                crate::http::error::PEER_UNVERIFIABLE
            }
        }
    }

    /// The one sentence a caller reads.
    pub fn sentence(&self) -> String {
        match self {
            Self::NoPeer { .. } => {
                "this machine could not read the address this call came from, so it could not \
                                     verify a hub: only a listed hub may drive this machine"
                    .to_string()
            }
            Self::NotListed { peer, node } => format!(
                "the machine at {peer} is not a hub this machine accepts forwarded calls from: the tailnet names \
                 it {node}, and only a listed hub may drive this machine"
            ),
            Self::Unverifiable { peer, .. } => format!(
                "the machine at {peer} could not be verified as a hub: this machine could not read its tailnet \
                 identity, so it will not accept a call from it"
            ),
        }
    }

    /// The peer this refusal is about, when the listener reported one.
    pub fn peer(&self) -> Option<IpAddr> {
        match self {
            Self::NoPeer { .. } => None,
            Self::NotListed { peer, .. } | Self::Unverifiable { peer, .. } => Some(*peer),
        }
    }

    /// The node the tailnet named for the peer, when it named one.
    pub fn node(&self) -> Option<&str> {
        match self {
            Self::NotListed { node, .. } => Some(node.as_str()),
            Self::NoPeer { .. } | Self::Unverifiable { .. } => None,
        }
    }

    /// What could not be read, when the refusal is about a measurement rather than a name.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::NoPeer { detail } | Self::Unverifiable { detail, .. } => Some(detail.as_str()),
            Self::NotListed { .. } => None,
        }
    }

    /// One `warn` line per refusal, naming the peer and the reason — the acceptance's "logged with the peer address
    /// and reason", and the same line whichever listener refused.
    pub fn log(&self) {
        tracing::warn!(
            target: "holosmith::trust",
            peer = %self.peer().map(|peer| peer.to_string()).unwrap_or_else(|| "unknown".to_string()),
            reason = %self.code(),
            node = %self.node().unwrap_or("-"),
            detail = %self.detail().unwrap_or("-"),
            "a call from a peer this machine does not trust was refused"
        );
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence())
    }
}

/// Whether an address is the machine's own front door: what `tailscale serve` connects from, and nothing else in
/// `127/8` (`Tailscale`'s serve proxies to `127.0.0.1`, and the gate's own client binds `127.0.0.2` to be a peer).
pub fn front_door(peer: IpAddr) -> bool {
    match peer.to_canonical() {
        IpAddr::V4(address) => address == std::net::Ipv4Addr::LOCALHOST,
        IpAddr::V6(address) => address == std::net::Ipv6Addr::LOCALHOST,
    }
}

/// The tailnet's name for one address, as its short machine name, from one `whois` child.
///
/// Blocking, with one deadline covering the child's whole life: on expiry the child is killed and the call is a
/// sentence, because a daemon that has stopped answering is a refusal and not a hang.
pub fn whois(program: &Path, peer: IpAddr) -> Result<String, String> {
    let mut command = Command::new(program);
    command
        .args(["whois", "--json", &peer.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("`{}` could not be run ({error})", program.display()))?;
    let deadline = Instant::now() + WHOIS_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{} whois {peer}` did not answer within {} s",
                    program.display(),
                    WHOIS_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{} whois {peer}` could not be waited for ({error})",
                    program.display()
                ));
            }
        }
    }
    // The child has exited, so both pipes are at EOF: this read is bounded by the child's own end, and a whois
    // answer is a kilobyte rather than a stream.
    let output = child.wait_with_output().map_err(|error| {
        format!(
            "`{} whois {peer}` printed nothing readable ({error})",
            program.display()
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "`{} whois {peer}` exited {}: {}",
            program.display(),
            match output.status.code() {
                Some(code) => code.to_string(),
                None => "on a signal".to_string(),
            },
            first_line(&String::from_utf8_lossy(&output.stderr))
        ));
    }
    let answer = String::from_utf8_lossy(&output.stdout);
    identity(&answer).ok_or_else(|| {
        format!(
            "`{} whois {peer}` named no tailnet node: {}",
            program.display(),
            first_line(&answer)
        )
    })
}

/// The machine name out of a `tailscale whois --json` answer: `Node.Name` folded to its first label, lowercased.
///
/// `None` for an answer that is not that shape — a daemon that printed prose, a node with no name — because a name
/// this function invented is a name the list might match. The trailing dot a MagicDNS name sometimes carries is
/// dropped with the rest of the domain, since the first label is taken rather than the name compared as text.
pub fn identity(answer: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(answer).ok()?;
    let name = value.get("Node")?.get("Name")?.as_str()?.trim();
    let short = name.split('.').next().unwrap_or(name).trim();
    if short.is_empty() {
        return None;
    }
    Some(short.to_ascii_lowercase())
}

/// The first line of a child's output, trimmed and capped: a refusal's log line is one line.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    line.chars().take(DETAIL_MAX).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// A `tailscale` that answers from `answer` when it exists, or fails with `failure` when `answer` is absent, and
    /// records every argv in the file beside it.
    fn shim(dir: &Path, answer: Option<&str>, failure: Option<&str>) -> PathBuf {
        if let Some(answer) = answer {
            fs::write(dir.join("answer"), answer).expect("the answer is written");
        }
        let program = dir.join("tailscale");
        let source = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>{log}\nif [ -f {answer} ]; then cat {answer}; exit 0; fi\n\
             printf '%s\\n' {failure} >&2\nexit 1\n",
            log = dir.join("argv").display(),
            answer = dir.join("answer").display(),
            failure = shell_quote(failure.unwrap_or("no answer")),
        );
        fs::write(&program, source).expect("the shim is written");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
            .expect("the shim is executable");
        program
    }

    fn shell_quote(text: &str) -> String {
        format!("'{}'", text.replace('\'', r"'\''"))
    }

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hs-trust-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the test directory is created");
        path
    }

    fn tools(program: &Path) -> Tools {
        Tools {
            tailscale: program.to_path_buf(),
            ..Tools::default()
        }
    }

    #[test]
    fn an_empty_list_verifies_nobody_and_says_so() {
        let trust = Trust::of(&Config::default(), &Tools::default());
        assert!(!trust.guarded());
        assert_eq!(trust.admit(None), Ok(Admitted::Unchecked));
    }

    #[test]
    fn the_front_door_is_two_addresses_and_the_rest_of_the_range_is_a_peer() {
        assert!(front_door("127.0.0.1".parse().expect("an address")));
        assert!(front_door("::1".parse().expect("an address")));
        assert!(front_door("::ffff:127.0.0.1".parse().expect("an address")));
        assert!(
            !front_door("127.0.0.2".parse().expect("an address")),
            "the rest of 127/8 is an address no deployment uses, so a peer there is a peer"
        );
        assert!(!front_door("100.64.0.7".parse().expect("an address")));
    }

    #[test]
    fn a_listed_node_is_admitted_and_the_child_was_asked_about_the_address() {
        let root = dir("listed");
        let program = shim(
            &root,
            Some(r#"{"Node":{"Name":"hub.tail1a2b.ts.net","ComputedName":"hub"}}"#),
            None,
        );
        let config = Config {
            allowed_hubs: vec!["hub".to_string()],
            ..Config::default()
        };
        let trust = Trust::of(&config, &tools(&program));
        assert!(trust.guarded());
        let peer: IpAddr = "100.64.0.7".parse().expect("an address");
        assert_eq!(
            trust.admit(Some(peer)),
            Ok(Admitted::Node {
                peer,
                name: "hub".to_string()
            })
        );
        let argv = fs::read_to_string(root.join("argv")).expect("the shim recorded its call");
        assert_eq!(argv.trim(), "whois --json 100.64.0.7", "{argv}");
    }

    #[test]
    fn a_node_that_is_not_listed_is_refused_by_name() {
        let root = dir("stranger");
        let program = shim(
            &root,
            Some(r#"{"Node":{"Name":"stranger.tail1a2b.ts.net"}}"#),
            None,
        );
        let config = Config {
            allowed_hubs: vec!["Hub".to_string()],
            ..Config::default()
        };
        let trust = Trust::of(&config, &tools(&program));
        let peer: IpAddr = "100.64.0.7".parse().expect("an address");
        let refusal = trust.admit(Some(peer)).expect_err("a stranger is refused");
        assert_eq!(refusal.code(), "peer_not_allowed");
        assert_eq!(refusal.peer(), Some(peer));
        assert_eq!(refusal.node(), Some("stranger"));
        assert!(
            refusal.sentence().contains("100.64.0.7"),
            "{}",
            refusal.sentence()
        );
        assert!(
            !refusal.sentence().contains("Hub") && !refusal.sentence().contains("hub\""),
            "the answer names no configured entry: {}",
            refusal.sentence()
        );
    }

    #[test]
    fn the_list_and_the_name_are_compared_without_case() {
        let root = dir("case");
        let program = shim(
            &root,
            Some(r#"{"Node":{"Name":"HUB.tail1a2b.ts.net."}}"#),
            None,
        );
        let config = Config {
            allowed_hubs: vec!["Hub".to_string()],
            ..Config::default()
        };
        let trust = Trust::of(&config, &tools(&program));
        assert!(matches!(
            trust.admit(Some("100.64.0.7".parse().expect("an address"))),
            Ok(Admitted::Node { .. })
        ));
    }

    #[test]
    fn a_whois_that_fails_refuses_the_call() {
        let root = dir("fails");
        let program = shim(&root, None, Some("failed to connect to local tailscaled"));
        let config = Config {
            allowed_hubs: vec!["hub".to_string()],
            ..Config::default()
        };
        let trust = Trust::of(&config, &tools(&program));
        let peer: IpAddr = "100.64.0.7".parse().expect("an address");
        let refusal = trust
            .admit(Some(peer))
            .expect_err("an unreadable identity refuses");
        assert_eq!(refusal.code(), "peer_unverifiable");
        let detail = refusal.detail().expect("a failed whois carries a detail");
        assert!(detail.contains("exited 1"), "{detail}");
        assert!(
            detail.contains("failed to connect to local tailscaled"),
            "the log line carries the daemon's own words, which is where they belong: {detail}"
        );
        assert!(
            !refusal.sentence().contains("tailscaled"),
            "the body carries no daemon prose: {}",
            refusal.sentence()
        );
    }

    #[test]
    fn a_whois_that_cannot_be_run_refuses_the_call() {
        let root = dir("missing");
        let config = Config {
            allowed_hubs: vec!["hub".to_string()],
            ..Config::default()
        };
        let trust = Trust::of(&config, &tools(&root.join("nothing-here")));
        let refusal = trust
            .admit(Some("100.64.0.7".parse().expect("an address")))
            .expect_err("a missing program refuses");
        assert_eq!(refusal.code(), "peer_unverifiable");
    }

    #[test]
    fn an_answer_with_no_node_is_unverifiable() {
        for answer in [
            r#"{}"#,
            r#"{"Node":null}"#,
            r#"{"Node":{"Name":null}}"#,
            r#"{"Node":{"Name":""}}"#,
            "failed to connect to local tailscaled",
            "",
        ] {
            assert_eq!(identity(answer), None, "{answer}");
            let root = dir("no-node");
            let program = shim(&root, Some(answer), None);
            let config = Config {
                allowed_hubs: vec!["hub".to_string()],
                ..Config::default()
            };
            let refusal = Trust::of(&config, &tools(&program))
                .admit(Some("100.64.0.7".parse().expect("an address")))
                .expect_err("an answer with no node refuses");
            assert_eq!(refusal.code(), "peer_unverifiable", "{answer}");
        }
    }

    #[test]
    fn the_identity_is_the_nodes_short_name() {
        assert_eq!(
            identity(r#"{"Node":{"Name":"hub.tail1a2b.ts.net","UserProfile":{"Login":"me@x"}}}"#),
            Some("hub".to_string())
        );
        assert_eq!(
            identity(r#"{"Node":{"Name":"hub.tail1a2b.ts.net."}}"#),
            Some("hub".to_string())
        );
        assert_eq!(
            identity(r#"{"Node":{"Name":"hub"}}"#),
            Some("hub".to_string())
        );
    }

    #[test]
    fn the_login_is_not_the_identity() {
        // The owner's login is on every node the operator has, so an answer that carries one and no node is not an
        // identity — the arm this test exists to keep: comparing the login would admit the stray node.
        assert_eq!(
            identity(r#"{"UserProfile":{"Login":"me@example.com"}}"#),
            None
        );
    }

    #[test]
    fn a_peer_with_no_address_is_unverifiable() {
        let root = dir("no-peer");
        let program = shim(
            &root,
            Some(r#"{"Node":{"Name":"hub.tail1a2b.ts.net"}}"#),
            None,
        );
        let config = Config {
            allowed_hubs: vec!["hub".to_string()],
            ..Config::default()
        };
        let refusal = Trust::of(&config, &tools(&program))
            .admit(None)
            .expect_err("an address the listener did not report refuses");
        assert_eq!(refusal.code(), "peer_unverifiable");
        assert_eq!(refusal.peer(), None);
        assert!(
            !root.join("argv").exists(),
            "the front door's answer must not come from a failed whois"
        );
    }

    #[test]
    fn the_front_door_is_admitted_without_a_whois() {
        let root = dir("front-door");
        let program = shim(&root, None, Some("nothing should ask"));
        let config = Config {
            allowed_hubs: vec!["hub".to_string()],
            ..Config::default()
        };
        let trust = Trust::of(&config, &tools(&program));
        for address in ["127.0.0.1", "::1"] {
            let peer: IpAddr = address.parse().expect("an address");
            assert_eq!(trust.admit(Some(peer)), Ok(Admitted::FrontDoor(peer)));
        }
        assert!(
            !root.join("argv").exists(),
            "the front door costs no child: {:?}",
            fs::read_to_string(root.join("argv"))
        );
    }

    #[test]
    fn every_refusal_has_a_status_a_code_and_a_sentence() {
        let peer: IpAddr = "100.64.0.7".parse().expect("an address");
        for (refusal, code) in [
            (
                Refusal::NoPeer {
                    detail: "no address".to_string(),
                },
                "peer_unverifiable",
            ),
            (
                Refusal::NotListed {
                    peer,
                    node: "stranger".to_string(),
                },
                "peer_not_allowed",
            ),
            (
                Refusal::Unverifiable {
                    peer,
                    detail: "no daemon".to_string(),
                },
                "peer_unverifiable",
            ),
        ] {
            assert_eq!(refusal.status(), 403);
            assert_eq!(refusal.code(), code);
            assert!(!refusal.sentence().is_empty());
            assert_eq!(refusal.to_string(), refusal.sentence());
            // The envelope a client reads carries the same word, from the same constant: the mapping is not a second
            // spelling, and this is the assertion that keeps it one.
            let envelope = crate::http::error::ApiError::from(&refusal);
            assert_eq!(envelope.body.code, code);
            assert_eq!(envelope.status.as_u16(), refusal.status());
            assert_eq!(envelope.body.message, refusal.sentence());
        }
    }
}
