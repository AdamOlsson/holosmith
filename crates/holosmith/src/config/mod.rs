//! The config file: one typed value, loaded once, validated strictly (ADR-0006).
//!
//! Defaults < file < CLI overrides. `serve` applies its `--port` and `--data-dir` to the value
//! [`Config::load`] returns ([`Config::apply_overrides`]) and re-checks what they can break.

mod duration;
mod editor;
mod problem;
mod render;
mod schema;

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml_edit::Document;

pub use duration::Duration;
pub use editor::{Editor, EditorError};
pub use problem::Problems;
pub use render::{RenderError, render, render_show};

use problem::{Position, Problem};

use crate::runtime::Backend;

/// The one path the spec names for the machine's config file.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/holosmith/config.toml";

/// Which parts of the system a machine runs (spec *Roles*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Hub,
    Server,
    Both,
}

impl Role {
    /// Every role and its spec word, in one table: the word that parses and the word that
    /// prints cannot drift, and a fourth role is one line here.
    const WORDS: [(Self, &'static str); 3] = [
        (Self::Hub, "hub"),
        (Self::Server, "server"),
        (Self::Both, "both"),
    ];

    /// The role a config file names, if it names one.
    pub fn parse(text: &str) -> Option<Self> {
        Self::WORDS
            .iter()
            .find(|(_, word)| *word == text)
            .map(|(role, _)| *role)
    }

    /// The word `serve` logs and the health endpoint reports.
    pub fn as_str(self) -> &'static str {
        Self::WORDS
            .iter()
            .find(|(role, _)| *role == self)
            .map(|(_, word)| *word)
            .expect("every role has a word in WORDS")
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An origin for the `Origin` and `Host` checks: scheme and authority, nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Origin(String);

impl Origin {
    /// Parse `http(s)://host[:port]`. A path, query or fragment is refused: an origin is
    /// compared with the browser's `Origin` header, which never carries one.
    pub fn parse(text: &str) -> Result<Self, String> {
        let authority = text
            .strip_prefix("http://")
            .or_else(|| text.strip_prefix("https://"))
            .ok_or_else(|| format!("must start with http:// or https://, not {text:?}"))?;
        if authority.is_empty() {
            return Err(format!("must name a host after the scheme, not {text:?}"));
        }
        if authority.contains(['/', '?', '#']) {
            return Err(format!(
                "must not have a path, query or fragment, not {text:?}"
            ));
        }
        if authority.contains(char::is_whitespace) {
            return Err(format!("must not contain whitespace, not {text:?}"));
        }
        Ok(Self(text.to_string()))
    }

    /// The origin as the config file spells it: scheme, host and port.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The same scheme and host on another port. `--port` moves the control listener and the
    /// control origin together, so the browser's `Origin` header keeps matching the address
    /// the operator was told to open. A port that is the scheme's default is left out, because
    /// a browser never sends `:80` or `:443` in `Origin` and HS-005 compares the two as text.
    /// An origin with no scheme, which [`Self::parse`] refuses, is returned unchanged rather
    /// than guessed at.
    pub fn with_port(&self, port: u16) -> Self {
        let (scheme, authority) = match self.0.split_once("://") {
            Some(parts) => parts,
            None => return self.clone(),
        };
        // A bracketed IPv6 literal may itself contain colons, so only the brackets delimit it.
        let host = match authority.strip_prefix('[') {
            Some(rest) => match rest.split_once(']') {
                Some((literal, _)) => format!("[{literal}]"),
                None => format!("[{rest}"),
            },
            None => match authority.rsplit_once(':') {
                Some((host, _)) => host.to_string(),
                None => authority.to_string(),
            },
        };
        if default_port(scheme) == Some(port) {
            Self(format!("{scheme}://{host}"))
        } else {
            Self(format!("{scheme}://{host}:{port}"))
        }
    }
}

/// The port a browser leaves out of `Origin` for this scheme, if it is one of the two we serve.
fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

/// `[merge]`: which branches a session may be merged into (item 12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Merge {
    pub targets: Vec<String>,
}

impl Default for Merge {
    fn default() -> Self {
        Self {
            targets: vec!["main".to_string(), "master".to_string()],
        }
    }
}

/// `[publish]`: which remotes a session branch may be pushed to; empty disables it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Publish {
    pub remotes: Vec<String>,
}

/// `[egress]`: the domains the egress proxy lets a session reach.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Egress {
    pub allow: Vec<String>,
}

/// The machine's settings, resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub role: Role,
    /// The backend `serve` runs unless `--backend` names another one for this run (HS-011).
    /// A machine setting rather than a flag because a dev instance should not have to remember
    /// one on every start, and `docker` because the stub is opt-in (ADR-0001).
    pub backend: Backend,
    pub control_listen: SocketAddr,
    pub preview_listen: SocketAddr,
    pub control_origin: Origin,
    pub preview_origin: Origin,
    pub allowed_hosts: Vec<String>,
    pub owner: Option<String>,
    pub data_root: PathBuf,
    pub projects_root: PathBuf,
    pub docker_host: Option<String>,
    pub hosts_seed: Option<PathBuf>,
    pub allowed_hubs: Vec<String>,
    pub network_default: bool,
    pub max_live_sessions: u32,
    pub project_backup: bool,
    pub turn_timeout_min: u64,
    pub idle_stop_min: u64,
    pub max_session_tokens: Option<u64>,
    pub abort_grace: Duration,
    pub min_free_gb: u64,
    pub session_disk_soft_gb: u64,
    pub stopped_ttl_days: u64,
    pub merge: Merge,
    pub publish: Publish,
    pub egress: Egress,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            role: Role::Both,
            backend: Backend::default(),
            control_listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 8080)),
            preview_listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 8081)),
            control_origin: Origin("http://localhost:8080".to_string()),
            preview_origin: Origin("http://localhost:8081".to_string()),
            allowed_hosts: Vec::new(),
            owner: None,
            data_root: PathBuf::from("/var/lib/holosmith"),
            projects_root: PathBuf::from("/srv/holosmith/projects"),
            docker_host: None,
            hosts_seed: None,
            allowed_hubs: Vec::new(),
            network_default: false,
            max_live_sessions: 3,
            project_backup: false,
            turn_timeout_min: 45,
            idle_stop_min: 30,
            max_session_tokens: None,
            abort_grace: Duration::from_millis(10_000),
            min_free_gb: 20,
            session_disk_soft_gb: 10,
            stopped_ttl_days: 7,
            merge: Merge::default(),
            publish: Publish::default(),
            egress: Egress::default(),
        }
    }
}

impl Config {
    /// Read and validate the file named by `--config`, or the default path when there is
    /// no such argument. A missing default is the built-in defaults; a missing file the
    /// operator named is an error, because running on defaults would hide the typo.
    pub fn load(named: Option<PathBuf>) -> Result<LoadedConfig, LoadError> {
        let named = ConfigPath::resolve(named);
        match fs::read_to_string(&named.path) {
            Ok(text) => match validate_text(&named.path, &text) {
                Ok(config) => Ok(LoadedConfig {
                    config,
                    source: ConfigSource::File,
                    path: named.path,
                }),
                Err(problems) => Err(LoadError::Invalid(problems)),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => match named.origin {
                PathOrigin::Explicit => Err(LoadError::Missing(named.path)),
                PathOrigin::Default => Ok(LoadedConfig::defaults(named.path)),
            },
            Err(error) => Err(LoadError::Io {
                path: named.path,
                source: error,
            }),
        }
    }

    /// `<data_root>/secrets/keys.toml`: the key proxy's file, mode 0600. Never read here.
    pub fn secrets_path(&self) -> PathBuf {
        self.data_root.join("secrets").join("keys.toml")
    }

    /// Apply the CLI overrides to the loaded value (ADR-0006: defaults < file < CLI).
    ///
    /// `--port` moves the control listener and the control origin's port, keeping its scheme
    /// and host; the preview listener and origin are not touched. `--data-dir` replaces the
    /// data root. Neither creates a directory: HS-004 owns the data root.
    ///
    /// `--port 0` is the one case where the origin cannot be written here: the port is the
    /// kernel's to pick, so `serve` re-derives the origin from the bound port after `bind`.
    pub fn apply_overrides(&mut self, overrides: &Overrides) {
        if let Some(port) = overrides.port {
            self.control_listen.set_port(port);
            self.control_origin = self.control_origin.with_port(port);
        }
        if let Some(data_root) = &overrides.data_root {
            self.data_root = data_root.clone();
        }
    }

    /// What an override broke, in the config pass's own words: `key: message`, one line each.
    ///
    /// Every rule is re-checked with the function the semantic pass calls, so a `--port` that
    /// lands on the preview port, or a `--data-dir` the file itself must not name, is reported
    /// exactly as the file would be. The CLI is held to the file's standard (ADR-0006).
    pub fn override_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(message) = preview_listen_problem(self, "") {
            problems.push(format!("preview_listen: {message}"));
        }
        if let Some(message) = preview_origin_problem(self, "") {
            problems.push(format!("preview_origin: {message}"));
        }
        problems.extend(
            root_problems(self, "")
                .into_iter()
                .map(|(key, message)| format!("{key}: {message}")),
        );
        problems
    }
}

/// The CLI values applied to the loaded config, after the file and before anything runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `--port`: the control listener's port, and the control origin's.
    pub port: Option<u16>,
    /// `--data-dir`: the data root for this run, replacing the file's.
    pub data_root: Option<PathBuf>,
}

/// Where the path to read came from, which decides whether a missing file is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathOrigin {
    Explicit,
    Default,
}

/// The path to read, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPath {
    pub path: PathBuf,
    pub origin: PathOrigin,
}

impl ConfigPath {
    /// `--config PATH`, or the one path the spec names.
    pub fn resolve(named: Option<PathBuf>) -> Self {
        match named {
            Some(path) => Self {
                path,
                origin: PathOrigin::Explicit,
            },
            None => Self {
                path: PathBuf::from(DEFAULT_CONFIG_PATH),
                origin: PathOrigin::Default,
            },
        }
    }
}

/// Whether the resolved config came from a file or from the built-in defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    File,
    Defaults,
}

impl ConfigSource {
    /// The word `config show` prints on its second line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Defaults => "defaults",
        }
    }
}

/// The config that is actually in force, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: Config,
    pub source: ConfigSource,
    /// The path that was read, or the path that was found to hold nothing.
    pub path: PathBuf,
}

impl LoadedConfig {
    /// The built-in defaults, as if the file at `path` were missing.
    pub fn defaults(path: impl Into<PathBuf>) -> Self {
        Self {
            config: Config::default(),
            source: ConfigSource::Defaults,
            path: path.into(),
        }
    }

    /// The line `serve` logs: the file it read, or the path it found nothing at.
    pub fn describe(&self) -> String {
        match self.source {
            ConfigSource::File => self.path.display().to_string(),
            ConfigSource::Defaults => format!("none at {}, using defaults", self.path.display()),
        }
    }
}

/// Why no config was produced.
#[derive(Debug)]
pub enum LoadError {
    /// The file the operator named is not there.
    Missing(PathBuf),
    /// The file is there but cannot be read.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file was read and has problems.
    Invalid(Problems),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(path) => write!(f, "no such file: {}", path.display()),
            Self::Io { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Invalid(problems) => write!(f, "{problems}"),
        }
    }
}

/// Validate `text` as the config file at `path`, collecting every problem at once.
///
/// Three passes over one parsed document (ADR-0006): the schema walk finds unknown keys
/// and wrong shapes, the tolerant build reads what the walk accepted, and the semantic
/// pass checks what no type can express. A document that does not parse has no key tree,
/// so a syntax error or a duplicate key is the one problem reported.
pub fn validate_text(path: &Path, text: &str) -> Result<Config, Problems> {
    let document = match Document::parse(text) {
        Ok(document) => document,
        Err(error) => {
            let position = error
                .span()
                .map(|span| Position::at_offset(text, span.start));
            let problem = Problem::new(position, None, error.message().to_string());
            return Err(Problems::new(path, vec![problem]));
        }
    };

    let mut problems = Vec::new();
    let mut config = Config::default();
    schema::walk(text, &document, &mut problems);
    let applied = schema::build(text, &document, &mut config, &mut problems);

    // The consistency belt: the walk has already reported every unknown key and every wrong
    // shape, so this fires only when a `KeySpec` row is more permissive than the field it
    // feeds — a `Kind::String` row over an integer field, say. It is a type check, not a
    // value check: `Origin` is `#[serde(transparent)]`, so serde would accept
    // `control_origin = "ftp://x"`, which `Origin::parse` refuses. The misdirected-setter guarantee is the separate
    // `every_documented_key_is_read_into_its_field` test. A failure is reported, never
    // swallowed.
    if problems.is_empty() {
        let deserializer = toml_edit::de::Deserializer::from(document.clone());
        if let Err(error) = Config::deserialize(deserializer) {
            let position = error
                .span()
                .map(|span| Position::at_offset(text, span.start));
            problems.push(Problem::new(
                position,
                None,
                format!("the schema walk accepted a key the typed config refuses: {error}"),
            ));
        }
    }

    problems.extend(semantic_problems(&config, &applied, &document, text));
    if problems.is_empty() {
        Ok(config)
    } else {
        Err(Problems::new(path, problems))
    }
}

/// The rules a type cannot express, all of them, each at the key that broke one.
///
/// `applied` holds the keys this document actually set, so a rule that compares two keys
/// can say so when one of them comes from the built-in defaults instead.
fn semantic_problems(
    config: &Config,
    applied: &HashSet<&'static str>,
    document: &Document<&str>,
    text: &str,
) -> Vec<Problem> {
    let mut problems = Vec::new();

    if let Some(message) = preview_listen_problem(
        config,
        &default_note(applied, ["control_listen", "preview_listen"]),
    ) {
        report(&mut problems, document, text, "preview_listen", message);
    }
    if let Some(message) = preview_origin_problem(
        config,
        &default_note(applied, ["control_origin", "preview_origin"]),
    ) {
        report(&mut problems, document, text, "preview_origin", message);
    }

    let roots_note = default_note(applied, ["data_root", "projects_root"]);
    for (key, message) in root_problems(config, &roots_note) {
        report(&mut problems, document, text, key, message);
    }

    if let Some(docker_host) = &config.docker_host
        && !["unix://", "tcp://", "ssh://"]
            .iter()
            .any(|scheme| docker_host.starts_with(scheme))
    {
        report(
            &mut problems,
            document,
            text,
            "docker_host",
            format!("must start with unix://, tcp:// or ssh://, not {docker_host:?}"),
        );
    }
    if let Some(hosts_seed) = &config.hosts_seed
        && !hosts_seed.is_absolute()
    {
        report(
            &mut problems,
            document,
            text,
            "hosts_seed",
            "must be an absolute path",
        );
    }
    if let Some(owner) = &config.owner {
        if owner.trim().is_empty() {
            report(&mut problems, document, text, "owner", "must not be empty");
        } else if owner.contains(char::is_whitespace) {
            report(
                &mut problems,
                document,
                text,
                "owner",
                "must not contain whitespace: it is a Tailscale login name",
            );
        }
    }

    if config.max_live_sessions < 1 {
        report(
            &mut problems,
            document,
            text,
            "max_live_sessions",
            "must be at least 1",
        );
    }
    if config.stopped_ttl_days < 1 {
        report(
            &mut problems,
            document,
            text,
            "stopped_ttl_days",
            "must be at least 1",
        );
    }
    if config.max_session_tokens == Some(0) {
        report(
            &mut problems,
            document,
            text,
            "max_session_tokens",
            "must be at least 1, or unset for unlimited",
        );
    }

    for name in &config.allowed_hosts {
        if !is_host_name(name) {
            report(
                &mut problems,
                document,
                text,
                "allowed_hosts",
                format!(
                    "{name:?} must be a bare host name or [IPv6] literal: letters, digits, \
                     dot, dash and underscore only, no scheme, path or port"
                ),
            );
        }
    }
    check_duplicates(
        &mut problems,
        document,
        text,
        "allowed_hosts",
        &config.allowed_hosts,
    );

    for name in &config.allowed_hubs {
        if !is_host_name(name) {
            report(
                &mut problems,
                document,
                text,
                "allowed_hubs",
                format!(
                    "{name:?} must be a bare host name or [IPv6] literal: letters, digits, \
                     dot, dash and underscore only, no scheme or path"
                ),
            );
        }
    }
    check_duplicates(
        &mut problems,
        document,
        text,
        "allowed_hubs",
        &config.allowed_hubs,
    );

    for name in &config.merge.targets {
        if !is_ref_name(name) {
            report(
                &mut problems,
                document,
                text,
                "merge.targets",
                format!(
                    "{name:?} must be a plain branch name: no whitespace or control \
                     characters, no \"..\", no \"@{{\", not \"@\", no leading \"-\", none of \
                     ~^:?*[\\, no trailing \"/\" or \".\""
                ),
            );
        }
    }
    check_duplicates(
        &mut problems,
        document,
        text,
        "merge.targets",
        &config.merge.targets,
    );

    for name in &config.publish.remotes {
        if !is_remote_name(name) {
            report(
                &mut problems,
                document,
                text,
                "publish.remotes",
                format!(
                    "{name:?} must be a remote name: letters, digits, dot, underscore and \
                     dash, not starting with \"-\""
                ),
            );
        }
    }
    check_duplicates(
        &mut problems,
        document,
        text,
        "publish.remotes",
        &config.publish.remotes,
    );

    for name in &config.egress.allow {
        if let Some(message) = domain_problem(name) {
            report(
                &mut problems,
                document,
                text,
                "egress.allow",
                format!("{name:?} {message}"),
            );
        }
    }
    check_duplicates(
        &mut problems,
        document,
        text,
        "egress.allow",
        &config.egress.allow,
    );

    problems
}

/// `preview_listen` must differ from `control_listen`, or a preview is served on the control
/// port. One implementation, called by the config pass and by the `--port` override, so the
/// two cannot word the same collision differently. `note` says when one of the values being
/// compared is a built-in default.
fn preview_listen_problem(config: &Config, note: &str) -> Option<String> {
    (config.control_listen == config.preview_listen).then(|| {
        format!(
            "must differ from control_listen{note}, both are {}",
            config.control_listen
        )
    })
}

/// `preview_origin` must differ from `control_origin` (item 3). One implementation, for the
/// same reason as [`preview_listen_problem`].
fn preview_origin_problem(config: &Config, note: &str) -> Option<String> {
    (config.control_origin == config.preview_origin).then(|| {
        format!(
            "must differ from control_origin{note}: a preview must not share the control \
             origin (item 3)"
        )
    })
}

/// What the two roots broke: both must be absolute, and neither may contain the other, because
/// deleting data must never touch a repository.
///
/// One implementation, called by the config pass and by the `--data-dir` override, so a data
/// root the file must not name is not accepted from the command line either. `note` says when
/// one of the compared values is a built-in default.
fn root_problems(config: &Config, note: &str) -> Vec<(&'static str, String)> {
    let mut problems = Vec::new();
    if !config.data_root.is_absolute() {
        problems.push(("data_root", "must be an absolute path".to_string()));
    }
    if !config.projects_root.is_absolute() {
        problems.push(("projects_root", "must be an absolute path".to_string()));
    }
    // Compare the folded spelling of both roots: `Path::starts_with` is component-wise, so
    // without folding a `..` either hides a nested root or invents a nested one.
    let data_root = normalized(&config.data_root);
    let projects_root = normalized(&config.projects_root);
    if data_root == projects_root {
        problems.push((
            "projects_root",
            format!(
                "must differ from data_root{note}: deleting data must never touch a repository"
            ),
        ));
    } else if projects_root.starts_with(&data_root) {
        problems.push((
            "projects_root",
            format!(
                "must not be inside data_root{note}: deleting data must never touch a repository"
            ),
        ));
    } else if data_root.starts_with(&projects_root) {
        problems.push((
            "data_root",
            format!(
                "must not be inside projects_root{note}: deleting data must never touch a \
                 repository"
            ),
        ));
    }
    problems
}

fn report(
    problems: &mut Vec<Problem>,
    document: &Document<&str>,
    text: &str,
    path: &str,
    message: impl Into<String>,
) {
    let position = schema::key_position(document, text, path);
    problems.push(Problem::new(position, Some(path.to_string()), message));
}

/// Report the first value a list holds twice, if there is one.
fn check_duplicates(
    problems: &mut Vec<Problem>,
    document: &Document<&str>,
    text: &str,
    path: &str,
    values: &[String],
) {
    let mut seen = HashSet::new();
    for value in values {
        if !seen.insert(value.as_str()) {
            report(
                problems,
                document,
                text,
                path,
                format!("{value:?} is listed twice"),
            );
            return;
        }
    }
}

/// ` (data_root is the built-in default)` for each of the two keys a rule compares that the
/// document did not set.
///
/// A rule that compares two resolved values still has to fire — two listeners on one port
/// are a real collision whoever set the port — but a message that reads as if the file wrote
/// both values sends the operator looking in the wrong place.
fn default_note(applied: &HashSet<&'static str>, keys: [&str; 2]) -> String {
    let unset: Vec<String> = keys
        .iter()
        .filter(|key| !applied.contains(*key))
        .map(|key| format!("{key} is the built-in default"))
        .collect();
    if unset.is_empty() {
        String::new()
    } else {
        format!(" ({})", unset.join("; "))
    }
}

/// `path` with every `.` and `..` folded away lexically, so two spellings of one place
/// compare equal.
///
/// A `..` that would climb above the start is kept: a relative path must not turn into an
/// absolute one. Symlinks are not resolved — that needs the filesystem, and a config file is
/// validated whether or not the directories it names exist yet.
fn normalized(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !folded.pop() {
                    folded.push(component);
                }
            }
            component => folded.push(component),
        }
    }
    folded
}

/// A host name or bracketed IPv6 literal as the `Host` allow-list spells it: no scheme, no
/// path, no port. A browser sends `Host: [::1]:8080`, so the literal needs its brackets and
/// `allowed_hosts` must be able to hold one (HS-005 matches it against that header).
fn is_host_name(name: &str) -> bool {
    if let Some(literal) = name
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return literal.parse::<Ipv6Addr>().is_ok();
    }
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_')
        })
}

/// A branch name `merge` may write, kept conservative on purpose (item 12).
fn is_ref_name(name: &str) -> bool {
    !name.is_empty()
        && name != "@"
        && !name.starts_with('-')
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
        && !name.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
}

/// A remote name `publish` may push to (item 12).
fn is_remote_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

/// Why a domain may not go in the egress allow-list, if it may not (item 2).
fn domain_problem(name: &str) -> Option<&'static str> {
    if name.chars().any(|character| character.is_ascii_uppercase()) {
        return Some("must be lowercase");
    }
    let bare = !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_')
        });
    if bare {
        None
    } else {
        Some("must be a bare domain with no scheme, path, port or wildcard")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    /// The path every problem line is prefixed with in these tests.
    fn test_path() -> &'static Path {
        Path::new("config.toml")
    }

    /// One file that sets every documented key to a value that is not the default.
    const EVERY_KEY: &str = r#"
role = "hub"
backend = "stub"
control_listen = "127.0.0.1:9000"
preview_listen = "127.0.0.1:9001"
control_origin = "https://control.example:8443"
preview_origin = "https://preview.example:8444"
allowed_hosts = ["control.example", "preview.example"]
owner = "me@example.com"
data_root = "/var/lib/holosmith"
projects_root = "/srv/holosmith/projects"
docker_host = "unix:///run/user/1000/docker.sock"
hosts_seed = "/etc/holosmith/hosts.toml"
allowed_hubs = ["hub.example"]
network_default = true
max_live_sessions = 4
project_backup = true
turn_timeout_min = 20
idle_stop_min = 15
max_session_tokens = 1000000
abort_grace = "1500ms"
min_free_gb = 5
session_disk_soft_gb = 2
stopped_ttl_days = 3

[merge]
targets = ["develop"]

[publish]
remotes = ["origin"]

[egress]
allow = ["crates.io"]
"#;

    fn parse(text: &str) -> Result<Config, Problems> {
        validate_text(test_path(), text)
    }

    fn problems_of(text: &str) -> String {
        parse(text)
            .expect_err("the config is expected to be invalid")
            .to_string()
    }

    fn socket(text: &str) -> SocketAddr {
        text.parse().expect("a literal socket address")
    }

    fn settings() -> Config {
        parse(EVERY_KEY).expect("EVERY_KEY is a valid config")
    }

    #[test]
    fn an_empty_file_is_the_built_in_defaults() {
        assert_eq!(
            parse("").expect("an empty file is valid"),
            Config::default()
        );
    }

    #[test]
    fn a_partial_file_fills_in_the_defaults() {
        let config = parse("role = \"hub\"\n").expect("one key is a valid file");
        assert_eq!(config.role, Role::Hub);
        assert_eq!(config.control_listen, Config::default().control_listen);
        assert_eq!(config.merge, Config::default().merge);
    }

    #[test]
    fn every_documented_key_is_read_into_its_field() {
        let config = settings();
        assert_eq!(config.role, Role::Hub);
        assert_eq!(config.backend, Backend::Stub);
        assert_eq!(config.control_listen, socket("127.0.0.1:9000"));
        assert_eq!(config.preview_listen, socket("127.0.0.1:9001"));
        assert_eq!(
            config.control_origin,
            Origin::parse("https://control.example:8443").expect("a literal origin")
        );
        assert_eq!(
            config.preview_origin,
            Origin::parse("https://preview.example:8444").expect("a literal origin")
        );
        assert_eq!(config.allowed_hosts, ["control.example", "preview.example"]);
        assert_eq!(config.owner.as_deref(), Some("me@example.com"));
        assert_eq!(config.data_root, Path::new("/var/lib/holosmith"));
        assert_eq!(config.projects_root, Path::new("/srv/holosmith/projects"));
        assert_eq!(
            config.docker_host.as_deref(),
            Some("unix:///run/user/1000/docker.sock")
        );
        assert_eq!(
            config.hosts_seed.as_deref(),
            Some(Path::new("/etc/holosmith/hosts.toml"))
        );
        assert_eq!(config.allowed_hubs, ["hub.example"]);
        assert!(config.network_default);
        assert_eq!(config.max_live_sessions, 4);
        assert!(config.project_backup);
        assert_eq!(config.turn_timeout_min, 20);
        assert_eq!(config.idle_stop_min, 15);
        assert_eq!(config.max_session_tokens, Some(1_000_000));
        assert_eq!(config.abort_grace, Duration::from_millis(1500));
        assert_eq!(config.min_free_gb, 5);
        assert_eq!(config.session_disk_soft_gb, 2);
        assert_eq!(config.stopped_ttl_days, 3);
        assert_eq!(config.merge.targets, ["develop"]);
        assert_eq!(config.publish.remotes, ["origin"]);
        assert_eq!(config.egress.allow, ["crates.io"]);
    }

    #[test]
    fn an_unknown_top_level_key_is_reported_with_its_position() {
        assert_eq!(
            problems_of("turn_timeout_mni = 45\n"),
            "config.toml:1:1: turn_timeout_mni: unknown key\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn an_unknown_key_inside_a_section_keeps_its_full_path() {
        assert_eq!(
            problems_of("[merge]\ntargetz = [\"main\"]\n"),
            "config.toml:2:1: merge.targetz: unknown key\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("[egress]\nalow = [\"x\"]\n"),
            "config.toml:2:1: egress.alow: unknown key\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn every_problem_in_a_broken_file_is_reported_at_once_in_file_order() {
        assert_eq!(
            problems_of("turn_timeout_mni = 45\nmax_live_sessions = 0\n\n[egress]\nalow = \"x\"\n"),
            "config.toml:1:1: turn_timeout_mni: unknown key\n\
             config.toml:2:1: max_live_sessions: must be at least 1\n\
             config.toml:5:1: egress.alow: unknown key\n\
             3 problems in config.toml\n"
        );
    }

    #[test]
    fn two_unknown_keys_are_both_reported() {
        assert_eq!(
            problems_of("turn_timeout_mni = 45\nstopped_ttl_dayz = 7\n"),
            "config.toml:1:1: turn_timeout_mni: unknown key\n\
             config.toml:2:1: stopped_ttl_dayz: unknown key\n\
             2 problems in config.toml\n"
        );
    }

    #[test]
    fn a_wrong_kind_names_what_was_expected_and_what_was_found() {
        assert_eq!(
            problems_of("max_live_sessions = \"three\"\n"),
            "config.toml:1:1: max_live_sessions: expected integer, found string\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_section_written_as_a_value_is_refused_once() {
        assert_eq!(
            problems_of("merge = 3\n"),
            "config.toml:1:1: merge: expected a table of settings, found integer\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn an_inline_table_is_read_like_a_table_header() {
        let config = parse("merge = { targets = [\"trunk\"] }\n")
            .expect("an inline table is the same TOML table");
        assert_eq!(config.merge.targets, ["trunk"]);
        // Its keys keep their positions, so problems inside one are still fixable.
        assert_eq!(
            problems_of("merge = { targetz = [\"main\"] }\n"),
            "config.toml:1:11: merge.targetz: unknown key\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("egress = { allow = [\"Crates.io\"] }\n"),
            "config.toml:1:12: egress.allow: \"Crates.io\" must be lowercase\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_syntax_error_is_the_one_problem_reported_and_it_has_a_span() {
        assert_eq!(
            problems_of("role = \n"),
            "config.toml:1:8: string values must be quoted, expected literal string\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_duplicate_key_is_the_one_problem_reported_and_it_has_a_span() {
        assert_eq!(
            problems_of("role = \"hub\"\nrole = \"server\"\n"),
            "config.toml:2:1: duplicate key\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn backend_must_be_stub_or_docker() {
        // HS-011's config key: the two words are the ones `--backend` accepts, so a third spelling
        // is a problem here rather than a backend nobody can select.
        assert_eq!(
            problems_of("backend = \"Podman\"\n"),
            "config.toml:1:1: backend: must be stub or docker, not \"Podman\"\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn max_live_sessions_must_be_at_least_one() {
        assert_eq!(
            problems_of("max_live_sessions = 0\n"),
            "config.toml:1:1: max_live_sessions: must be at least 1\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn stopped_ttl_days_must_be_at_least_one() {
        assert_eq!(
            problems_of("stopped_ttl_days = 0\n"),
            "config.toml:1:1: stopped_ttl_days: must be at least 1\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn max_session_tokens_must_be_positive_when_it_is_set() {
        assert_eq!(
            problems_of("max_session_tokens = 0\n"),
            "config.toml:1:1: max_session_tokens: must be at least 1, or unset for unlimited\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_negative_number_is_refused_with_its_value() {
        assert_eq!(
            problems_of("turn_timeout_min = -1\n"),
            "config.toml:1:1: turn_timeout_min: must not be negative, not -1\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_number_too_large_for_the_field_is_refused_without_claiming_a_floor() {
        assert_eq!(
            problems_of("max_live_sessions = 5000000000\n"),
            "config.toml:1:1: max_live_sessions: must fit in 32 bits, not 5000000000\n\
             1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("max_live_sessions = -1\n"),
            "config.toml:1:1: max_live_sessions: must not be negative, not -1\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn abort_grace_needs_a_unit_and_a_non_zero_value() {
        assert_eq!(
            problems_of("abort_grace = 10\n"),
            "config.toml:1:1: abort_grace: expected duration string with a unit suffix (s, ms, m), \
             like \"10s\", found integer\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("abort_grace = \"10\"\n"),
            "config.toml:1:1: abort_grace: \"10\" has no unit: write \"10s\", \"1500ms\" or \"2m\", \
             because a bare number does not say which unit it means\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("abort_grace = \"0s\"\n"),
            "config.toml:1:1: abort_grace: \"0s\" must be greater than zero: the abort ladder needs \
             a grace period\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_role_must_be_one_of_the_three() {
        assert_eq!(
            problems_of("role = \"boss\"\n"),
            "config.toml:1:1: role: must be one of hub, server, both, not \"boss\"\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn the_two_listeners_must_differ() {
        // `preview_listen` is not in the file, so the rule compares the built-in default and
        // the message says where that value came from.
        assert_eq!(
            problems_of("control_listen = \"127.0.0.1:8081\"\n"),
            "config.toml: preview_listen: must differ from control_listen (preview_listen is \
             the built-in default), both are 127.0.0.1:8081\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of(
                "control_listen = \"127.0.0.1:8081\"\npreview_listen = \"127.0.0.1:8081\"\n"
            ),
            "config.toml:2:1: preview_listen: must differ from control_listen, both are \
             127.0.0.1:8081\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn the_two_origins_must_differ() {
        assert_eq!(
            problems_of("control_origin = \"http://localhost:8081\"\n"),
            "config.toml: preview_origin: must differ from control_origin (preview_origin is \
             the built-in default): a preview must not share the control origin (item 3)\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_two_key_rule_says_when_it_compares_a_built_in_default() {
        // S8: `data_root` is a kind error, so the typed value keeps the built-in default and
        // the roots rule compares `projects_root` against a path the file never named.
        assert_eq!(
            problems_of("data_root = 5\nprojects_root = \"/var/lib/holosmith\"\n"),
            "config.toml:1:1: data_root: expected string, found integer\n\
             config.toml:2:1: projects_root: must differ from data_root (data_root is the \
             built-in default): deleting data must never touch a repository\n\
             2 problems in config.toml\n"
        );
    }

    #[test]
    fn an_origin_must_be_a_scheme_and_an_authority() {
        assert_eq!(
            problems_of("control_origin = \"http://localhost:8080/ui\"\n"),
            "config.toml:1:1: control_origin: must not have a path, query or fragment, \
             not \"http://localhost:8080/ui\"\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("control_origin = \"ftp://localhost:8080\"\n"),
            "config.toml:1:1: control_origin: must start with http:// or https://, \
             not \"ftp://localhost:8080\"\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn both_roots_must_be_absolute() {
        assert_eq!(
            problems_of("data_root = \"relative/data\"\n"),
            "config.toml:1:1: data_root: must be an absolute path\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("projects_root = \"relative/projects\"\n"),
            "config.toml:1:1: projects_root: must be an absolute path\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn the_roots_must_not_be_equal_or_nested_in_either_direction() {
        assert_eq!(
            problems_of("projects_root = \"/var/lib/holosmith\"\n"),
            "config.toml:1:1: projects_root: must differ from data_root (data_root is the \
             built-in default): deleting data must never touch a repository\n\
             1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("projects_root = \"/var/lib/holosmith/projects\"\n"),
            "config.toml:1:1: projects_root: must not be inside data_root (data_root is the \
             built-in default): deleting data must never touch a repository\n\
             1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("data_root = \"/srv/holosmith/projects/data\"\n"),
            "config.toml:1:1: data_root: must not be inside projects_root (projects_root is the \
             built-in default): deleting data must never touch a repository\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn the_roots_are_compared_after_dropping_and_folding_dot_segments() {
        // A `..` that climbs out of the data root resolves inside it: the dangerous
        // direction, and the one a lexical comparison used to miss.
        assert_eq!(
            problems_of(
                "data_root = \"/var/lib/holosmith\"\n\
                 projects_root = \"/srv/../var/lib/holosmith/projects\"\n"
            ),
            "config.toml:2:1: projects_root: must not be inside data_root: deleting data must \
             never touch a repository\n1 problem in config.toml\n"
        );
        // A `..` that climbs out of the data root resolves outside it: not a collision.
        parse("projects_root = \"/var/lib/holosmith/../projects\"\n")
            .expect("a projects root outside the data root is valid, however it is spelled");
        // Folding also makes two spellings of one directory equal.
        assert_eq!(
            problems_of("projects_root = \"/var/lib/./holosmith\"\n"),
            "config.toml:1:1: projects_root: must differ from data_root (data_root is the \
             built-in default): deleting data must never touch a repository\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_docker_host_must_name_a_transport() {
        for transport in [
            "unix:///run/user/1000/docker.sock",
            "tcp://127.0.0.1:2375",
            "ssh://me@box",
        ] {
            parse(&format!("docker_host = \"{transport}\"\n"))
                .expect("unix, tcp and ssh are the transports docker -H takes");
        }
        assert_eq!(
            problems_of("docker_host = \"npipe://x\"\n"),
            "config.toml:1:1: docker_host: must start with unix://, tcp:// or ssh://, not \
             \"npipe://x\"\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn the_hosts_seed_must_be_absolute() {
        assert_eq!(
            problems_of("hosts_seed = \"hosts.toml\"\n"),
            "config.toml:1:1: hosts_seed: must be an absolute path\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn an_owner_must_be_one_word_and_not_empty() {
        assert_eq!(
            problems_of("owner = \"  \"\n"),
            "config.toml:1:1: owner: must not be empty\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("owner = \"me now\"\n"),
            "config.toml:1:1: owner: must not contain whitespace: it is a Tailscale login name\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn the_host_allow_lists_take_bare_names() {
        assert_eq!(
            problems_of("allowed_hosts = [\"http://x:80\"]\n"),
            "config.toml:1:1: allowed_hosts: \"http://x:80\" must be a bare host name or [IPv6] \
             literal: letters, digits, dot, dash and underscore only, no scheme, path or port\n\
             1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("allowed_hubs = [\"hub/x\"]\n"),
            "config.toml:1:1: allowed_hubs: \"hub/x\" must be a bare host name or [IPv6] literal: \
             letters, digits, dot, dash and underscore only, no scheme or path\n\
             1 problem in config.toml\n"
        );
    }

    #[test]
    fn a_bracketed_ipv6_literal_can_be_allow_listed() {
        // A browser sends `Host: [::1]:8080`, so HS-005 has to be able to match `[::1]`.
        let config = parse(
            "control_listen = \"[::1]:8080\"\npreview_listen = \"[::1]:8081\"\n\
             allowed_hosts = [\"[::1]\", \"[2001:db8::1]\"]\n",
        )
        .expect("an IPv6 binding must be allow-listable");
        assert_eq!(config.allowed_hosts, ["[::1]", "[2001:db8::1]"]);
        for name in ["[::1]:8080", "[not-an-address]", "::1"] {
            assert_eq!(
                problems_of(&format!("allowed_hosts = [\"{name}\"]\n")),
                format!(
                    "config.toml:1:1: allowed_hosts: \"{name}\" must be a bare host name or \
                     [IPv6] literal: letters, digits, dot, dash and underscore only, no scheme, \
                     path or port\n1 problem in config.toml\n"
                )
            );
        }
    }

    #[test]
    fn merge_targets_must_be_plain_branch_names() {
        let message = "must be a plain branch name: no whitespace or control characters, no \"..\", \
                       no \"@{\", not \"@\", no leading \"-\", none of ~^:?*[\\, no trailing \
                       \"/\" or \".\"";
        for name in ["a..b", "-bad", "a/", "@", "a.", "@{upstream}"] {
            assert_eq!(
                problems_of(&format!("[merge]\ntargets = [\"{name}\"]\n")),
                format!(
                    "config.toml:2:1: merge.targets: {name:?} {message}\n1 problem in config.toml\n"
                )
            );
        }
        // A C0 control character arrives escaped in the file and is named escaped again.
        assert_eq!(
            problems_of("[merge]\ntargets = [\"a\\u0001b\"]\n"),
            format!(
                "config.toml:2:1: merge.targets: \"a\\u{{1}}b\" {message}\n\
                 1 problem in config.toml\n"
            )
        );
    }

    #[test]
    fn publish_remotes_must_be_plain_remote_names() {
        assert_eq!(
            problems_of("[publish]\nremotes = [\"-origin\"]\n"),
            "config.toml:2:1: publish.remotes: \"-origin\" must be a remote name: letters, digits, \
             dot, underscore and dash, not starting with \"-\"\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn egress_domains_must_be_lowercase_bare_names() {
        assert_eq!(
            problems_of("[egress]\nallow = [\"Crates.io\"]\n"),
            "config.toml:2:1: egress.allow: \"Crates.io\" must be lowercase\n1 problem in config.toml\n"
        );
        for name in ["*.io", "https://crates.io", "crates.io:443"] {
            assert_eq!(
                problems_of(&format!("[egress]\nallow = [\"{name}\"]\n")),
                format!(
                    "config.toml:2:1: egress.allow: \"{name}\" must be a bare domain with no scheme, \
                     path, port or wildcard\n1 problem in config.toml\n"
                )
            );
        }
    }

    #[test]
    fn a_list_may_not_hold_the_same_value_twice() {
        assert_eq!(
            problems_of("[merge]\ntargets = [\"main\", \"main\"]\n"),
            "config.toml:2:1: merge.targets: \"main\" is listed twice\n1 problem in config.toml\n"
        );
        assert_eq!(
            problems_of("[egress]\nallow = [\"a.io\", \"a.io\"]\n"),
            "config.toml:2:1: egress.allow: \"a.io\" is listed twice\n1 problem in config.toml\n"
        );
    }

    #[test]
    fn an_empty_list_is_allowed_because_it_disables_the_thing() {
        let config =
            parse("[merge]\ntargets = []\n[publish]\nremotes = []\n[egress]\nallow = []\n")
                .expect("empty lists are valid");
        assert!(config.merge.targets.is_empty());
        assert!(config.publish.remotes.is_empty());
        assert!(config.egress.allow.is_empty());
    }

    #[test]
    fn the_optional_keys_are_absent_unless_the_file_sets_them() {
        let config = Config::default();
        assert_eq!(config.owner, None);
        assert_eq!(config.docker_host, None);
        assert_eq!(config.hosts_seed, None);
        assert_eq!(config.max_session_tokens, None);
    }

    #[test]
    fn secrets_live_below_the_data_root() {
        assert_eq!(
            settings().secrets_path(),
            Path::new("/var/lib/holosmith/secrets/keys.toml")
        );
        assert_eq!(
            Config::default().secrets_path(),
            Path::new("/var/lib/holosmith/secrets/keys.toml")
        );
    }

    #[test]
    fn describe_names_the_file_or_the_path_that_held_none() {
        let loaded = LoadedConfig::defaults(Path::new("/etc/holosmith/config.toml"));
        assert_eq!(
            loaded.describe(),
            "none at /etc/holosmith/config.toml, using defaults"
        );
        let from_file = LoadedConfig {
            config: Config::default(),
            source: ConfigSource::File,
            path: PathBuf::from("/etc/holosmith/config.toml"),
        };
        assert_eq!(from_file.describe(), "/etc/holosmith/config.toml");
    }

    #[test]
    fn the_default_path_is_the_one_the_spec_names() {
        // Pure, so it runs on the machine class HS-003 deploys to, where a real config file
        // makes the loader tests below take their skip.
        let resolved = ConfigPath::resolve(None);
        assert_eq!(resolved.path, PathBuf::from(DEFAULT_CONFIG_PATH));
        assert_eq!(resolved.origin, PathOrigin::Default);
        assert_eq!(DEFAULT_CONFIG_PATH, "/etc/holosmith/config.toml");
    }

    #[test]
    fn load_finds_the_default_path_and_does_not_need_a_file_there() {
        let default = Path::new(DEFAULT_CONFIG_PATH);
        // A machine with a real (and readable) config is the deployed case, not a failure.
        if default.exists() {
            eprintln!("SKIP: {DEFAULT_CONFIG_PATH} exists on this machine");
            return;
        }
        let loaded = Config::load(None).expect("a missing default file is not an error");
        assert_eq!(loaded.path, PathBuf::from(DEFAULT_CONFIG_PATH));
        assert_eq!(loaded.source, ConfigSource::Defaults);
        assert_eq!(loaded.config, Config::default());
    }

    #[test]
    fn load_refuses_a_missing_file_the_operator_named() {
        let path = PathBuf::from("/nonexistent/holosmith/config.toml");
        match Config::load(Some(path.clone())) {
            Err(LoadError::Missing(missing)) => assert_eq!(missing, path),
            other => panic!("expected a missing-file error, got {other:?}"),
        }
    }

    #[test]
    fn the_serde_belt_rejects_what_the_walk_rejects() {
        // The belt never runs on a document the walk called clean, so this is the only way
        // to prove it is wired at all: `deny_unknown_fields` must actually refuse.
        for text in ["turn_timeout_mni = 45\n", "[merge]\ntargetz = [\"main\"]\n"] {
            let document = toml_edit::Document::parse(text).expect("the document parses");
            let refused = Config::deserialize(toml_edit::de::Deserializer::from(document));
            assert!(
                refused.is_err(),
                "the belt accepted {text:?}, so deny_unknown_fields is not doing its job"
            );
        }
    }

    fn overridden(port: Option<u16>, data_root: Option<&str>) -> Config {
        let mut config = settings();
        config.apply_overrides(&Overrides {
            port,
            data_root: data_root.map(PathBuf::from),
        });
        config
    }

    #[test]
    fn the_port_override_moves_the_listener_and_the_origin_but_not_the_preview() {
        let config = overridden(Some(9123), None);
        assert_eq!(config.control_listen, socket("127.0.0.1:9123"));
        assert_eq!(
            config.control_origin.as_str(),
            "https://control.example:9123",
            "a browser sends the origin it read; the scheme and host stay"
        );
        assert_eq!(config.preview_listen, socket("127.0.0.1:9001"));
        assert_eq!(
            config.preview_origin.as_str(),
            "https://preview.example:8444"
        );
    }

    #[test]
    fn an_origin_moves_to_another_port_and_keeps_scheme_and_host() {
        let cases = [
            // A browser never sends a default port, so the rewrite does not write one either.
            ("http://localhost:8080", 80, "http://localhost"),
            ("https://hub.example", 443, "https://hub.example"),
            ("https://hub.example", 8443, "https://hub.example:8443"),
            ("http://[::1]:8080", 9999, "http://[::1]:9999"),
            ("https://[2001:db8::1]", 443, "https://[2001:db8::1]"),
        ];
        for (text, port, expected) in cases {
            let origin = Origin::parse(text).expect("a valid origin");
            assert_eq!(origin.with_port(port).as_str(), expected);
        }
    }

    #[test]
    fn the_data_dir_override_replaces_the_data_root() {
        let config = overridden(None, Some("/tmp/holosmith-data"));
        assert_eq!(config.data_root, Path::new("/tmp/holosmith-data"));
        assert_eq!(config.projects_root, Path::new("/srv/holosmith/projects"));
    }

    #[test]
    fn a_data_dir_that_is_not_utf8_is_still_a_path() {
        let mut config = Config::default();
        let odd = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff, b'x']));
        config.apply_overrides(&Overrides {
            port: None,
            data_root: Some(odd.clone()),
        });
        assert_eq!(config.data_root, odd);
    }

    #[test]
    fn an_override_that_collides_is_reported_in_the_config_passes_words() {
        // The file names `control_listen` but not `preview_listen`, so the file alone is
        // valid; `--port` walks the control listener onto the preview one.
        let mut config = settings();
        config.apply_overrides(&Overrides {
            port: Some(9001),
            data_root: None,
        });
        assert_eq!(
            config.override_problems(),
            vec!["preview_listen: must differ from control_listen, both are 127.0.0.1:9001"]
        );
    }

    #[test]
    fn a_valid_override_reports_nothing() {
        assert!(
            overridden(Some(9123), Some("/tmp/holosmith-data"))
                .override_problems()
                .is_empty()
        );
    }

    #[test]
    fn the_override_collision_message_is_the_ones_one_implementation_produces() {
        // Both keys written by the file: the semantic pass reports the collision with the same
        // sentence the override path uses, because both call one function.
        let file = "control_listen = \"127.0.0.1:8081\"\npreview_listen = \"127.0.0.1:8081\"\n";
        let reported = problems_of(file);
        assert_eq!(
            reported,
            "config.toml:2:1: preview_listen: must differ from control_listen, both are \
             127.0.0.1:8081\n1 problem in config.toml\n"
        );
        let mut config = Config::default();
        config.apply_overrides(&Overrides {
            port: Some(8081),
            data_root: None,
        });
        let from_override = config.override_problems();
        // `--port 8081` also walks the control origin onto the built-in preview origin, so
        // both collision rules fire; the listener sentence is the one compared here.
        let listener = from_override
            .iter()
            .find(|problem| problem.starts_with("preview_listen: "))
            .expect("the listener collision is reported");
        assert!(
            reported.contains(listener.as_str()),
            "{reported:?} does not contain {listener:?}"
        );
        assert_eq!(
            from_override
                .iter()
                .filter(|problem| problem.starts_with("preview_origin: "))
                .count(),
            1
        );
    }

    #[test]
    fn a_relative_data_dir_is_refused_in_the_config_passes_words() {
        // The config pass refuses a relative `data_root` in the file, and `--data-dir` answers
        // to the same rule: that `serve` creates no directory does not make the root valid
        // (ADR-0006 — one typed value, validated once, whichever layer set it).
        assert_eq!(
            overridden(None, Some("relative/data")).override_problems(),
            vec!["data_root: must be an absolute path"]
        );
    }

    #[test]
    fn a_data_dir_that_overlaps_projects_root_is_refused() {
        // `/srv/holosmith/projects` is the built-in projects_root: the same path the file may
        // not name, refused from the CLI, which makes the repro for this story's second P1.
        assert_eq!(
            overridden(None, Some("/srv/holosmith/projects")).override_problems(),
            vec![
                "projects_root: must differ from data_root: deleting data must never touch a \
                 repository"
            ]
        );
        assert_eq!(
            overridden(None, Some("/srv/holosmith/projects/data")).override_problems(),
            vec![
                "data_root: must not be inside projects_root: deleting data must never touch a \
                 repository"
            ]
        );
    }

    #[test]
    fn the_role_word_is_the_specs_word() {
        assert_eq!(Role::Hub.as_str(), "hub");
        assert_eq!(Role::Server.as_str(), "server");
        assert_eq!(Role::Both.as_str(), "both");
        for word in ["hub", "server", "both"] {
            assert_eq!(Role::parse(word).map(Role::as_str), Some(word));
        }
    }

    #[test]
    fn the_role_word_is_spelled_the_same_by_serde_display_and_parse() {
        // Serde's `rename_all` is the one spelling the table in `Role` cannot supply, so this
        // test holds it to the same word the banner prints and the file parses.
        for role in [Role::Hub, Role::Server, Role::Both] {
            let json = serde_json::to_string(&role).expect("a role serialises");
            assert_eq!(json, format!("\"{}\"", role.as_str()));
            assert_eq!(role.to_string(), role.as_str());
        }
    }

    #[test]
    fn only_the_config_module_names_toml_edit() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let config = source.join("config");
        let mut offenders = Vec::new();
        let mut scanned = 0;
        let mut pending = vec![source.clone()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                scanned += 1;
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                if text.contains("toml_edit") && !path.starts_with(&config) {
                    offenders.push(path.display().to_string());
                }
            }
        }
        // Without this a moved source tree would leave the walk empty and the guard green.
        assert!(scanned > 0, "no .rs files under {}", source.display());
        assert!(
            offenders.is_empty(),
            "only src/config/ may parse TOML, but these files name toml_edit: {offenders:?}"
        );
    }
}
