//! The host registry (HS-068, spec items 4 and 16).
//!
//! A machine's list of the other machines it may send a request to: one row per host, keyed by the name the
//! operator gives it, with the origin it answers on, whether it answered its own health check the last time
//! anything asked, and the sentence it failed with if it did not.
//!
//! Three things live here, and each has one home:
//!
//! - **The registration's rules** ([`name_problem`], [`origin`], [`host_problem`], [`port_problem`],
//!   [`is_local`]) — the acceptance's "the URL is http(s), the host is a MagicDNS name or an address in
//!   100.64.0.0/10, the port is in the allowed range", as ordinary functions. They are pure, which is what makes
//!   each arm of the acceptance a test rather than a request.
//! - **The probe** ([`probe`]): one `GET /api/health` out of this machine — `openssl s_client` for `https`, the
//!   hand-written plain socket for `http` — and the three answers it can give.
//! - **The seed** ([`seed`]): the optional `hosts.toml` read at startup and merged into the table, the file winning
//!   on conflicts.
//!
//! **The host predicate is deliberately not `egress::address_problem`.** That function exists to *refuse* the
//! tailnet, because a container must not reach it; a registry row exists to name it. The two are inverses over the
//! same range, and one implementation for both would be one policy with two callers who disagree (ADR-0073).
//!
//! **A row's `url` is an origin** ([`crate::config::Origin`]) rather than a free URL: the probe asks
//! `GET /api/health` at it and nothing else, HS-069 appends the path it forwards, and `Origin::parse` already
//! refuses a non-http(s) scheme, a path, a query and whitespace.

pub mod probe;
pub mod repository;
pub mod seed;

use crate::clock;
use crate::config::{Config, Origin};
use crate::db::Database;
use crate::preflight::Tools;
use crate::sessions::RepoError;

/// The lowest port a registry row may name that is not its own scheme's default.
///
/// Item 4 asks for "the port in an allowed range" and names no number, so this is a **constant** and not a config
/// key — item 11's `push::LONG_TURN` is the precedent for "a policy the spec names and no key spells". The range is
/// the unprivileged one, plus each scheme's own default (80/443): a tailnet host answers on 443 because that is the
/// port Tailscale's certificate is for, so a rule that refused it would make `https://box.tail1234.ts.net` legal
/// only when the port is left out — the same URL with two answers.
pub const MIN_PORT: u16 = 1024;

/// The longest name a row may have: DNS's own label limit, and one path segment when HS-069 forwards with it.
pub const MAX_NAME: usize = 63;

/// The one name a registry row may not take: HS-069 spells **this** machine's sessions with it.
///
/// Item 16's phase-3 neighbour, "local sessions use the host name `local`; remote ones use the registered name",
/// makes `local` a name in the URL space that means *here*. A registered host called `local` would make every
/// forwarded path ambiguous, and the ambiguity would arrive at the first request rather than at registration.
pub const LOCAL: &str = "local";

/// The suffix a MagicDNS name ends with: `<machine>.<tailnet>.ts.net`.
///
/// Item 4's "a MagicDNS name" cannot be checked by resolving it — a host that is down must still be registrable,
/// and a machine with no daemon must still validate — so the check is the shape Tailscale mints.
const MAGIC_SUFFIX: &str = ".ts.net";

/// Whether a registry row answered its own health check.
///
/// Text in the `reach` column rather than a `CHECK` constraint, for ADR-0011's reason: a corrupt row is an error at
/// read time that names the text it found, and adding a word does not need a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Reachable,
    Unreachable,
}

impl Reach {
    /// Every word, for the parser.
    pub const ALL: [Reach; 2] = [Self::Reachable, Self::Unreachable];

    /// The text the `reach` column holds, and the word the API answers.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::Unreachable => "unreachable",
        }
    }

    /// The word a row holds, or the sentence saying it is not one of the two.
    ///
    /// Not [`std::str::FromStr`], because the error is a sentence that names the text it found and it travels in the
    /// repository's own error rather than in a type of its own (ADR-0011's rule for a corrupt row).
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|word| word.as_str() == text)
            .ok_or_else(|| {
                format!(
                    "a hosts row holds the reach {text:?}, which is not one of: {}",
                    Self::ALL.map(Self::as_str).join(", ")
                )
            })
    }
}

impl std::fmt::Display for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for Reach {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One row of the registry: the name the operator gave the machine, the origin it answers on, and what the last
/// probe found.
///
/// `version` and `last_error` are one row's two facts and not a probe history (ADR-0073): the registry's question is
/// "where can I send a request now", and the sentence is what the operator reads when the answer is no.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Host {
    pub name: String,
    pub url: Origin,
    pub reach: Reach,
    /// The health reply's version, `None` when the row is unreachable.
    pub version: Option<String>,
    /// The sentence the probe failed with, `None` when the row is reachable.
    pub last_error: Option<String>,
    /// When anything last asked, unix seconds.
    pub last_checked_at: i64,
}

/// Why a registration is refused, before anything is stored.
///
/// The three arms are three codes in ADR-0008's table ([`crate::http::error`]), because the client's next step
/// differs: a name it has to change, a URL it has to change, or a machine that is not a Holosmith.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The name is not one a row may have (its alphabet, its length, or it is [`LOCAL`]).
    Name(String),
    /// The URL is not an origin this registry accepts (its scheme, its shape, its host, its port).
    Url(String),
    /// The host answered, and what it answered is not a Holosmith health reply.
    NotHolosmith(String),
}

impl Refusal {
    /// The one sentence a client reads.
    pub fn sentence(&self) -> &str {
        match self {
            Self::Name(sentence) | Self::Url(sentence) | Self::NotHolosmith(sentence) => sentence,
        }
    }
}

/// Why a registration did not happen: a refusal a client can act on, or the database.
#[derive(Debug)]
pub enum RegisterError {
    Refused(Refusal),
    Store(RepoError),
}

impl From<Refusal> for RegisterError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// Why `name` is not a name a row may take, or `None` when it is.
///
/// A name is one path segment in HS-069's URL space, so the alphabet is DNS's: lowercase ASCII letters, digits and
/// `-`, not starting or ending with `-`, at most [`MAX_NAME`] characters. Uppercase is refused rather than folded,
/// because a name is compared as text in a URL and two spellings of one host is the ambiguity this rule exists to
/// prevent.
pub fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("`name` must not be empty".to_string());
    }
    if name == LOCAL {
        return Some(format!(
            "`name` must not be {LOCAL:?}: that is the name this machine's own sessions are reached by"
        ));
    }
    if name.len() > MAX_NAME {
        return Some(format!(
            "`name` must be at most {MAX_NAME} characters, because it is one path segment"
        ));
    }
    let bytes = name.as_bytes();
    let bad = !bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
    if bad || bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return Some(
            "`name` must be lowercase letters, digits and `-`, not beginning or ending with `-`"
                .to_string(),
        );
    }
    None
}

/// The origin `url` names, or the sentence saying why it is not one this registry accepts.
///
/// Three questions in one place, in the order an operator fixes them: is it an http(s) origin at all
/// ([`Origin::parse`]), does its host satisfy item 4's tailnet rule ([`host_problem`]), and is its port in the
/// allowed range ([`port_problem`]).
pub fn origin(url: &str) -> Result<Origin, String> {
    let origin = Origin::parse(url)?;
    let host = origin
        .host()
        .ok_or_else(|| format!("`url` must name a host, not {url:?}"))?;
    if let Some(problem) = host_problem(host) {
        return Err(problem);
    }
    if let Some(problem) = port_problem(&origin) {
        return Err(problem);
    }
    Ok(origin)
}

/// Why `host` is not a host a registry row may name, or `None` when it is.
///
/// Item 4's rule, which exists to keep a registry row inside the tailnet: a **MagicDNS name**
/// (`<machine>.<tailnet>.ts.net`) or an **IPv4 literal in 100.64.0.0/10**. Everything else — loopback, RFC 1918, a
/// public name, an IPv6 literal — is refused with a sentence naming what is allowed.
pub fn host_problem(host: &str) -> Option<String> {
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(address) = bare.parse::<std::net::IpAddr>() {
        return match address {
            std::net::IpAddr::V4(v4) if is_tailnet(v4) => None,
            std::net::IpAddr::V4(_) => Some(format!(
                "`url`'s host {host:?} is not in 100.64.0.0/10, the range a tailnet address is in"
            )),
            std::net::IpAddr::V6(_) => Some(format!(
                "`url`'s host {host:?} is an IPv6 literal: item 4 names a MagicDNS name or an address in \
                 100.64.0.0/10, and a tailnet's IPv6 range is neither"
            )),
        };
    }
    if is_magic_name(bare) {
        return None;
    }
    Some(format!(
        "`url`'s host {host:?} is neither a MagicDNS name (lowercase labels ending `{MAGIC_SUFFIX}`) nor an \
         address in 100.64.0.0/10: a registry row names a machine on the tailnet"
    ))
}

/// Whether `address` is in 100.64.0.0/10 — the CGNAT range Tailscale gives a node.
///
/// Its own two lines rather than `egress`'s range table: that module refuses this range, and a predicate that
/// imported it would be one rule with two callers who want opposite answers (ADR-0073).
fn is_tailnet(address: std::net::Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();
    first == 100 && (64..=127).contains(&second)
}

/// Whether `host` has the shape Tailscale's MagicDNS mints: `<machine>.<tailnet>.ts.net`.
///
/// Checked as text and not by resolving: a host that is asleep must still be registrable, and this machine has no
/// resolver for the tailnet at all. A name needs something before the suffix, every label is lowercase letters,
/// digits and `-`, and no label is empty.
fn is_magic_name(host: &str) -> bool {
    let Some(prefix) = host.strip_suffix(MAGIC_SUFFIX) else {
        return false;
    };
    // `<machine>.<tailnet>`: at least one dot, so the name is not just `ts.net`.
    if !prefix.contains('.') {
        return false;
    }
    prefix.split('.').all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

/// Why the origin's port is not one a registry row may name, or `None` when it is.
///
/// [`MIN_PORT`], plus the port the scheme itself means when the URL leaves it out — the rule is one sentence and a
/// URL that names its own default port is the same URL.
pub fn port_problem(origin: &Origin) -> Option<String> {
    let port = origin.port()?;
    if port >= MIN_PORT || Some(port) == default_port(origin) {
        return None;
    }
    Some(format!(
        "`url`'s port {port} is not in the allowed range: {MIN_PORT} or above, or the scheme's own default \
         (80 for http, 443 for https)"
    ))
}

/// The scheme's own default port, from the origin's text.
fn default_port(origin: &Origin) -> Option<u16> {
    match origin.as_str().split_once("://").map(|(scheme, _)| scheme) {
        Some("http") => Some(80),
        Some("https") => Some(443),
        _ => None,
    }
}

/// Whether `url` names **this** machine.
///
/// Spec item 4 gives a delete one refusal, "removing a host is refused while it has live sessions", and this is the
/// only row whose sessions this machine can count: the one whose origin is its own `control_origin`. Compared with
/// each origin's own port folded through [`Origin::with_port`], so `https://box.tail.ts.net` and
/// `https://box.tail.ts.net:443` are one answer — which is exactly what the `/api/health` round trip would say.
pub fn is_local(url: &Origin, control_origin: &Origin) -> bool {
    match (url.port(), control_origin.port()) {
        (Some(url_port), Some(our_port)) => {
            url.with_port(url_port) == control_origin.with_port(our_port)
        }
        _ => false,
    }
}

/// This build's version: what a row naming this machine reports, and what every other row's probe compares.
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Register one host: the rules, then the probe, and the row that goes in the table.
///
/// The row is *stored* by the caller under its own lock scope (the probe leaves the machine and must not hold it),
/// which is why this answers a [`Host`] rather than writing one.
pub fn register(config: &Config, tools: &Tools, name: &str, url: &str) -> Result<Host, Refusal> {
    if let Some(problem) = name_problem(name) {
        return Err(Refusal::Name(problem));
    }
    let origin = origin(url).map_err(Refusal::Url)?;
    accept(probe_one(config, tools, &origin), name, &origin)
}

/// What one probe of one origin means for a **registration**.
///
/// The three outcomes become two: an answer that is not a Holosmith health reply is `422 not_holosmith` (item 4's
/// "anything else is rejected with a distinct code"), while "nothing answered" is a **row** marked
/// [`Reach::Unreachable`] with the sentence — item 16's "marked unreachable with the last error, not removed". A
/// registration of a machine that is merely asleep is not a mistake.
pub fn accept(outcome: probe::Outcome, name: &str, origin: &Origin) -> Result<Host, Refusal> {
    match outcome {
        probe::Outcome::Refused(sentence) => Err(Refusal::NotHolosmith(sentence)),
        other => Ok(merge_outcome(other, name, origin)),
    }
}

/// What one probe of one origin means for the **seed's merge**: always a row.
///
/// The seed has no client to refuse, so an answer that is not a Holosmith health reply is one more unreachable row
/// whose sentence says what answered instead. [`accept`] is the other mapping, and the difference is deliberate — an
/// HTTP request can be told "that is not a Holosmith", a file cannot.
pub fn merge_outcome(outcome: probe::Outcome, name: &str, origin: &Origin) -> Host {
    let mut host = Host {
        name: name.to_string(),
        url: origin.clone(),
        reach: Reach::Unreachable,
        version: None,
        last_error: None,
        last_checked_at: clock::unix_now(),
    };
    match outcome {
        probe::Outcome::Healthy { version } => {
            host.reach = Reach::Reachable;
            host.version = Some(version);
        }
        probe::Outcome::Unreachable(sentence) | probe::Outcome::Refused(sentence) => {
            host.last_error = Some(sentence);
        }
    }
    host
}

/// Probe one row's origin for the **merge**: the local shortcut, then the request.
pub fn probe_row(config: &Config, tools: &Tools, name: &str, origin: &Origin) -> Host {
    merge_outcome(probe_one(config, tools, origin), name, origin)
}

/// One probe, with the local shortcut every caller shares.
///
/// A row naming this machine is not probed: asking this process to reach itself over its own origin is a question
/// about the boot order — the merge runs before the listener serves, so the answer would be "nothing is there" — and
/// not about health. It is stored `reachable` with this build's version, which is what the round trip would say once
/// the process is up.
fn probe_one(config: &Config, tools: &Tools, origin: &Origin) -> probe::Outcome {
    if is_local(origin, &config.control_origin) {
        return probe::Outcome::Healthy { version: version() };
    }
    probe::ask(origin, tools)
}

/// The startup merge: read `config.hosts_seed`, store what it says, and probe every row in the table.
///
/// Item 16's "each machine reads an optional `hosts.toml` at startup and merges it into its hosts table (the file
/// wins on conflicts), so one file copied to every machine keeps the lists aligned". Three rules shape it:
///
/// - **The file wins** on a name, because a name is the identity: a file row replaces the API row of the same name.
/// - **A row the file does not mention survives** — an operator's registration made over the API is not undone by a
///   file that predates it.
/// - **The parse is all-or-nothing; the probes are per row.** A malformed file merges nothing (a half-read seed
///   would leave the table saying something the operator never wrote), while a host that does not answer is one row
///   marked unreachable — the acceptance's "not removed".
///
/// Probe failures are rows here and never refusals: the seed has no client to answer, and a boot that refused over
/// a hosts file would take the control UI down for a machine whose sessions are fine.
pub fn merge(
    database: &Database,
    config: &Config,
    tools: &Tools,
    now: i64,
) -> Result<Merge, String> {
    let Some(path) = &config.hosts_seed else {
        // No key, no file: the seed is optional (item 16's own word), and a machine that has not been given one must
        // still boot — its own registration over the API is the whole registry then.
        return Ok(Merge::default());
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{} is not there, so no host was seeded from it (the key names an optional file)",
                path.display()
            ));
        }
        Err(error) => {
            return Err(format!("{} could not be read ({error})", path.display()));
        }
    };
    let rows = seed::parse(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut merge = Merge {
        path: Some(path.clone()),
        from_file: rows.len(),
        ..Merge::default()
    };
    // The file's rows first, each stored as it is read: the probe runs with the lock dropped, so a row is written
    // twice — once as it arrives (`unreachable` until something asks) and once with what the probe measured.
    let mut stored: Vec<Host> = Vec::new();
    for row in &rows {
        let host = Host {
            name: row.name.clone(),
            url: row.url.clone(),
            reach: Reach::Unreachable,
            version: None,
            last_error: Some("not probed yet".to_string()),
            last_checked_at: now,
        };
        database
            .hosts()
            .upsert(&host)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        merge.stored += 1;
        stored.push(host);
    }
    // Then every row in the table, seeded or not: reachability is a fact of this moment, and a row registered over
    // the API last week is as interesting as one from the file.
    let mut all = database
        .hosts()
        .all()
        .map_err(|error| format!("the hosts table could not be read ({error})"))?;
    all.extend(stored);
    let mut seen = std::collections::BTreeSet::new();
    for host in all {
        if !seen.insert(host.name.clone()) {
            continue;
        }
        let probed = probe_row(config, tools, &host.name, &host.url);
        merge.probed += 1;
        if probed.reach == Reach::Unreachable {
            merge.unreachable += 1;
        }
        database
            .hosts()
            .upsert(&probed)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(merge)
}

/// What one startup merge did, for the boot's log line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Merge {
    /// The seed file, when one was named and read.
    pub path: Option<std::path::PathBuf>,
    /// Rows the file named.
    pub from_file: usize,
    /// Rows written from the file.
    pub stored: usize,
    /// Rows probed (the whole table, not only the file's).
    pub probed: usize,
    /// Rows the probe left unreachable.
    pub unreachable: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control() -> Origin {
        Origin::parse("http://127.0.0.1:8080").expect("the default control origin is one")
    }

    #[test]
    fn a_name_is_one_lowercase_path_segment() {
        for good in ["a", "workshop", "workshop-2", "attic", "x1"] {
            assert_eq!(name_problem(good), None, "{good}");
        }
        for bad in ["", "Local", "LOCAL", "-x", "x-", "a b", "a/b", "a.b", "ü"] {
            assert!(name_problem(bad).is_some(), "{bad:?} was accepted");
        }
        assert!(
            name_problem(LOCAL)
                .expect("reserved")
                .contains("own sessions")
        );
        assert!(name_problem(&"x".repeat(MAX_NAME + 1)).is_some());
        assert_eq!(name_problem(&"x".repeat(MAX_NAME)), None);
    }

    #[test]
    fn the_host_is_magic_dns_or_the_tailnet_range_and_nothing_else() {
        for good in [
            "workshop.tail1234.ts.net",
            "box.tail-scale.ts.net",
            "100.64.0.1",
            "100.100.1.2",
            "100.127.255.254",
        ] {
            assert_eq!(host_problem(good), None, "{good}");
        }
        for bad in [
            "localhost",
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "8.8.8.8",
            "100.63.0.1",
            "100.128.0.1",
            "ts.net",
            "example.com",
            "box.tail1234.ts.net.example.com",
            "Box.tail1234.ts.net",
            "box..ts.net",
            "fd7a:115c:a1e0::1",
            "[fd7a:115c:a1e0::1]",
        ] {
            assert!(host_problem(bad).is_some(), "{bad} was accepted");
        }
    }

    #[test]
    fn the_port_is_unprivileged_or_the_schemes_own_default() {
        let ok = |url: &str| port_problem(&Origin::parse(url).expect("an origin"));
        assert_eq!(ok("https://box.tail1234.ts.net"), None);
        assert_eq!(ok("https://box.tail1234.ts.net:443"), None);
        assert_eq!(ok("http://box.tail1234.ts.net"), None);
        assert_eq!(ok("http://box.tail1234.ts.net:80"), None);
        assert_eq!(ok("http://box.tail1234.ts.net:8080"), None);
        assert_eq!(ok("https://box.tail1234.ts.net:65535"), None);
        for bad in [
            "https://box.tail1234.ts.net:22",
            "https://box.tail1234.ts.net:80",
            "http://box.tail1234.ts.net:443",
            "https://box.tail1234.ts.net:1023",
            "https://box.tail1234.ts.net:0",
        ] {
            let problem = ok(bad).unwrap_or_else(|| panic!("{bad} was accepted"));
            assert!(problem.contains("allowed range"), "{problem}");
        }
    }

    #[test]
    fn an_origin_is_http_s_and_has_no_path() {
        assert_eq!(
            origin("https://box.tail1234.ts.net").expect("a tailnet origin"),
            Origin::parse("https://box.tail1234.ts.net").expect("the same")
        );
        for bad in [
            "ftp://box.tail1234.ts.net",
            "https://box.tail1234.ts.net/api",
            "https://box.tail1234.ts.net?x=1",
            "box.tail1234.ts.net",
            "",
            "https://localhost",
            "https://example.com",
        ] {
            assert!(origin(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn the_local_row_is_the_one_that_names_this_machine() {
        assert!(is_local(
            &Origin::parse("http://127.0.0.1:8080").expect("an origin"),
            &control()
        ));
        assert!(is_local(
            &Origin::parse("http://127.0.0.1:80").expect("an origin"),
            &Origin::parse("http://127.0.0.1").expect("an origin")
        ));
        assert!(is_local(
            &Origin::parse("https://box.tail1234.ts.net:443").expect("an origin"),
            &Origin::parse("https://box.tail1234.ts.net").expect("an origin")
        ));
        assert!(!is_local(
            &Origin::parse("http://127.0.0.1:8081").expect("an origin"),
            &control()
        ));
        // A different scheme is a different origin even on the same host and port.
        assert!(!is_local(
            &Origin::parse("https://127.0.0.1:8080").expect("an origin"),
            &control()
        ));
        assert!(!is_local(
            &Origin::parse("https://attic.tail1234.ts.net").expect("an origin"),
            &control()
        ));
    }

    #[test]
    fn a_local_row_is_stored_reachable_without_a_round_trip() {
        // The merge runs before the listener serves, so asking this process to reach itself would answer
        // "connection refused" for a machine that is about to be perfectly healthy.
        let dir =
            std::env::temp_dir().join(format!("holosmith-hosts-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = Config {
            data_root: dir.clone(),
            ..Config::default()
        };
        let origin = Origin::parse(config.control_origin.as_str()).expect("an origin");
        let row = probe_row(&config, &Tools::default(), "thisbox", &origin);
        assert_eq!(row.reach, Reach::Reachable);
        assert_eq!(row.version.as_deref(), Some(version().as_str()));
        assert_eq!(row.last_error, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
