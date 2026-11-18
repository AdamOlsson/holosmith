//! The machine's provider keys, its tool credentials, and each session's dummy key (spec item 2).
//!
//! The story's promise is a negative one: a real key never enters a session container. Two files make that true
//! and this module owns both:
//!
//! * `<data>/secrets/keys.toml`, mode **0600**, is the only place a real key is written. `hs_key` mounts the
//!   **directory** it lives in, read-only (`container::KeyProxy::mount`, ADR-0050); no session container mounts
//!   either the file or the directory.
//! * `<data>/sessions/<id>/pi-home/models.json` carries the session's **dummy** key and the key proxy's URL.
//!   That file is inside a session mount, which is exactly why it may hold nothing else, and `mcp.json` beside it is
//!   the same dummy for the tool proxy ([`crate::mcpproxy::client_config`]).
//!
//! `keys.toml` carries every table because the proxy has no database: the provider keys are what it swaps in, the
//! session table is what lets it answer "is this dummy key still valid?", and the tool table ([`TOOLS`]) is where
//! a credential for a host that is not a model provider lives — out of the session's own `pi-home`, where the agent
//! could read it, and spent by [`crate::mcpproxy`] rather than by [`crate::keyproxy`] (ADR-0098, ADR-0099). Deleting
//! a session removes its row here ([`Store::revoke`]), which is the hook's whole body.
//!
//! **Writes are temp-file + rename.** The file must never be observable half-written: a proxy reading it at
//! the wrong moment would answer 503 for a key the operator just saved. The temporary file is created with
//! its final mode before the rename, so there is no window in which a key is world-readable.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The directory under the data root the spec names.
pub const SECRETS_DIR: &str = "secrets";

/// The one key file.
pub const KEYS_FILE: &str = "keys.toml";

/// The provider hosts a request may be forwarded to (spec item 2's "forwards only to the provider hosts").
///
/// A fixed table, not a request-controlled host or port. ADR-0045's egress allow-list is exact for the same
/// reason: a proxy that can be told where to connect is a proxy that can be told to connect to the control
/// listener. The path prefix a session's `models.json` carries selects one of these rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    /// The provider id, which is also the first path segment and the `models.json` key.
    pub id: &'static str,
    /// The host the request is forwarded to over TLS.
    pub host: &'static str,
    /// The path prefix the provider's real API expects. An empty string is Anthropic's shape: pi appends
    /// `/v1/messages` itself. OpenAI-shaped APIs expect `/v1` before the endpoint.
    ///
    /// The prefix a request whose endpoint matches no [`Shape`] gets. It is the prefix of the shape this
    /// build measured the row with, which is why a row with shapes still reads as one triple.
    pub base_path: &'static str,
    /// Where the real key goes in the upstream request, for the requests that match no [`Shape`].
    pub auth: Auth,
    /// The API shapes this host serves beyond the row's own (host, [`base_path`](Self::base_path),
    /// [`auth`](Self::auth)) triple. Empty for every row but `opencode-go`.
    ///
    /// A shape exists because a provider publishes more than one API under one host, and **the shape is a
    /// property of the model**: pi's bundled catalog is the only component that knows which one a model
    /// speaks, and it says so in the path it appends to the `baseUrl` this build writes — `/v1/messages` for
    /// an `anthropic-messages` model, `/chat/completions` for an `openai-completions` one. A row per shape
    /// would need pi to know a provider id its catalog does not carry, with the model metadata (context
    /// window, cost, reasoning levels) copied into `models.json` beside it.
    ///
    /// A shape is still **not** a request-controlled route: the host stays the row's, both the endpoints and
    /// the base paths are constants here, and a `..` is refused whatever matched (ADR-0097).
    pub shapes: &'static [Shape],
}

/// One API shape a provider's host serves: the endpoint pi appends to the `baseUrl` it was given, the base
/// path that provider publishes the endpoint under, and the header that shape's own client sends the key in.
///
/// The three travel together because they change together: a host that serves a second API usually serves it
/// under a different path and reads the key from a different header. Taking any one of them from the request
/// instead is the request-controlled route ADR-0050 refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// The endpoint pi appends, with its leading `/` — `/v1/messages`, `/chat/completions` — matched exactly
    /// (or as a prefix ending at a `/`, and the longest endpoint wins, so a shape naming a longer endpoint is
    /// not shadowed by one of its prefixes).
    pub endpoint: &'static str,
    /// The base path the provider serves that endpoint under. The forwarded path is this, then the endpoint.
    pub base_path: &'static str,
    /// Where the real key goes in this shape's request.
    pub auth: Auth,
}

/// Which header carries the provider's API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// Anthropic's `x-api-key`.
    XApiKey,
    /// OpenAI-compatible `Authorization: Bearer …`.
    Bearer,
}

impl Provider {
    /// The base path and the header the request whose appended endpoint is `endpoint` must be forwarded with.
    ///
    /// `endpoint` is the path pi appended after `/\<provider\>`, with its leading `/`: `/v1/messages`,
    /// `/chat/completions`, or the empty string when pi appended nothing. A shape is selected by an exact endpoint
    /// match (or the endpoint and then further `/`-separated segments), and the **longest** endpoint wins. An
    /// endpoint that matches no shape answers the row's own [`base_path`](Self::base_path) and
    /// [`auth`](Self::auth), so every row without shapes — and every endpoint of a row with them that pi did not
    /// name — routes exactly as it did before shapes existed.
    pub fn shape_for(&self, endpoint: &str) -> (&'static str, Auth) {
        let matched = self
            .shapes
            .iter()
            .filter(|shape| is_endpoint(endpoint, shape.endpoint))
            .max_by_key(|shape| shape.endpoint.len());
        match matched {
            Some(shape) => (shape.base_path, shape.auth),
            None => (self.base_path, self.auth),
        }
    }
}

/// Whether `path` is `endpoint`, or `endpoint` and then further segments under it.
fn is_endpoint(path: &str, endpoint: &str) -> bool {
    match path.strip_prefix(endpoint) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// The providers this build knows how to forward to.
///
/// The list is deliberately small and explicit. A provider that is missing is a `key set` refusal with a
/// sentence naming what is supported, not a generic proxy that guesses a host.
///
/// **The first row is the machine's default provider** ([`default_provider`]): the id every operator-facing
/// example names, the one the refusal lists first, and the one the new-session form prefills on a machine with
/// no key. `opencode-go` is that row because it is the provider this build's credential path was measured with
/// (ADR-0050, ADR-0093), so the first run asks for a key the repository has actually used. The rows are matched
/// by id ([`provider`]), so the order matters only to the sentences that enumerate them.
pub const PROVIDERS: &[Provider] = &[
    // opencode.ai's Go gateway serves more than one API under one host — `anthropic-messages` under
    // `/inference/go/anthropic` and `openai-completions` under `/inference/go/openai/v1` — and pi's bundled
    // catalog names which one a model speaks by the path it appends (`/v1/messages`, `/chat/completions`). So
    // this row carries a shape per endpoint and keeps the anthropic triple as its own default, which is the
    // shape V3 was measured with: `opencode-go/claude-haiku-5-5`. ADR-0050 predicted the openai shape as a
    // second row and ADR-0097 records why it is a shape on this one instead, and lists the endpoints
    // exactly.
    Provider {
        id: "opencode-go",
        host: "opencode.ai",
        base_path: "/inference/go/anthropic",
        auth: Auth::XApiKey,
        shapes: &[
            Shape {
                endpoint: "/v1/messages",
                base_path: "/inference/go/anthropic",
                auth: Auth::XApiKey,
            },
            Shape {
                endpoint: "/chat/completions",
                base_path: "/inference/go/openai/v1",
                auth: Auth::Bearer,
            },
        ],
    },
    Provider {
        id: "anthropic",
        host: "api.anthropic.com",
        base_path: "",
        auth: Auth::XApiKey,
        shapes: &[],
    },
    Provider {
        id: "openai",
        host: "api.openai.com",
        base_path: "/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "openrouter",
        host: "openrouter.ai",
        base_path: "/api/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "deepseek",
        host: "api.deepseek.com",
        base_path: "",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "mistral",
        host: "api.mistral.ai",
        base_path: "/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "groq",
        host: "api.groq.com",
        base_path: "/openai/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "xai",
        host: "api.x.ai",
        base_path: "/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "fireworks",
        host: "api.fireworks.ai",
        base_path: "/inference/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "together",
        host: "api.together.xyz",
        base_path: "/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "cerebras",
        host: "api.cerebras.ai",
        base_path: "/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
    Provider {
        id: "nvidia",
        host: "integrate.api.nvidia.com",
        base_path: "/v1",
        auth: Auth::Bearer,
        shapes: &[],
    },
];

/// The provider row `id` names, if this build has one.
pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

/// The machine's default provider: [`PROVIDERS`]' first row, and the one spelling of that id.
///
/// Four sentences name it — `key set`'s refusal, the session list the app reads, the form's model example and the
/// line a machine with no key draws — so the choice lives in the table's order and nowhere else. The index is on a
/// literal table with rows in it, and the test below pins which id is first.
pub fn default_provider() -> &'static str {
    PROVIDERS[0].id
}

/// The provider ids, in table order, for a refusal's sentence and the CLI's help.
pub fn provider_ids() -> Vec<&'static str> {
    PROVIDERS.iter().map(|provider| provider.id).collect()
}

/// A credential for a **tool** rather than for a model provider: one host a session's agent calls.
///
/// The file has two key tables because two different things spend them. A provider key is swapped in by the key
/// proxy at a fixed route per row of [`PROVIDERS`]; a tool key belongs to a host the agent's own client talks to,
/// whose paths the tool's protocol names. So a tool is **not** a provider: it has no entry in pi's catalog, it is
/// never [`default_provider`], [`models`] writes nothing for it, and the key proxy's routes never name it. It is a
/// row `key set` accepts, `key list` shows, and [`crate::mcpproxy::Route`] selects — from this table and never from a
/// request's bytes (ADR-0098, ADR-0099).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    /// The id `key set` and `key revoke` take, and the row's name in `keys.toml`'s `[tools]` table.
    pub id: &'static str,
    /// The host the real key is presented to, over TLS.
    pub host: &'static str,
    /// Where the real key goes in that host's requests.
    pub auth: Auth,
    /// The paths this host serves the tool under, with their leading `/`.
    ///
    /// Constants, matched exactly, for ADR-0050's reason: a caller of the table is told where a key may be spent
    /// rather than being handed a path to forward.
    pub endpoints: &'static [&'static str],
}

/// The tools this build knows how to hold a credential for.
///
/// Small and explicit like [`PROVIDERS`], and for the same reason: `key set` refuses an id no row names, because a
/// stored key for a host nothing serves is a key nothing can spend.
pub const TOOLS: &[Tool] = &[Tool {
    id: "linear",
    host: "mcp.linear.app",
    auth: Auth::Bearer,
    endpoints: &["/mcp", "/mcp/readonly"],
}];

/// The tool row `id` names, if this build has one.
pub fn tool(id: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.id == id)
}

/// The tool ids, in table order, for a refusal's sentence and the CLI's help.
pub fn tool_ids() -> Vec<&'static str> {
    TOOLS.iter().map(|tool| tool.id).collect()
}

/// The refusal an id outside [`PROVIDERS`] gets, in one spelling.
///
/// `key set` asks its own sentence ([`unknown_id`], which names both tables) **before** it reads stdin: an unknown
/// id is refused at once rather than after a key the operator then watches disappear into a command that throws it
/// away. [`Store::set_provider`] asks this one again for a caller that skipped the step, so the sentence is written
/// here once.
pub fn unknown_provider(provider: &str) -> KeyError {
    KeyError::new(format!(
        "{provider} is not a provider this build knows: {}",
        provider_ids().join(", ")
    ))
}

/// The refusal an id outside [`TOOLS`] gets, in one spelling: [`Store::set_tool`]'s guard, for a caller that
/// stored without asking the CLI first — the proxy that spends a tool key is that caller (ADR-0098).
pub fn unknown_tool(id: &str) -> KeyError {
    KeyError::new(format!(
        "{id} is not a tool this build knows: {}",
        tool_ids().join(", ")
    ))
}

/// The refusal an id that is in **neither** table gets from `key set`, naming both.
///
/// `set` takes one positional id and stores it in the table that names it, so a typo has to be told the whole
/// vocabulary this build accepts rather than half of it: the sentence is the two one-table sentences, in the
/// order the file has its credential tables. The provider list opens with [`default_provider`], because that is
/// the id a machine with no key is told to store.
pub fn unknown_id(id: &str) -> KeyError {
    KeyError::new(format!("{}; {}", unknown_provider(id), unknown_tool(id)))
}

/// The refusal `key remove` gives a tool's id: that command exists for the provider table, so the sentence names
/// the command that will work instead.
///
/// `None` for every id that is not a tool, a typo included: `key remove bogus` keeps its own "has no key" answer,
/// because a typo is that command's business and not the other table's.
pub fn refusal_for_remove(id: &str) -> Option<KeyError> {
    tool(id).map(|_| {
        KeyError::new(format!(
            "{id} is a tool, not a model provider: use `holosmith key revoke {id}`"
        ))
    })
}

/// The refusal `key revoke` gives a model provider's id, for [`refusal_for_remove`]'s reason and with its mirror
/// sentence. `None` for every id that is not a provider, a typo included.
pub fn refusal_for_revoke(id: &str) -> Option<KeyError> {
    provider(id).map(|_| {
        KeyError::new(format!(
            "{id} is a model provider, not a tool: use `holosmith key remove {id}`"
        ))
    })
}

/// The machine's key store: one path, and no cached copy. A write is a read-modify-write under a process lock,
/// because the create path and the delete path can touch it from different tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    path: PathBuf,
}

/// The parsed shape of `keys.toml`: real keys by provider, real keys by tool, and the dummy key each live session
/// spends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Secrets {
    pub providers: BTreeMap<String, String>,
    /// Tool credentials, keyed by [`TOOLS`] id. A `[providers]` row whose id names a tool is read as one of these.
    pub tools: BTreeMap<String, String>,
    pub sessions: BTreeMap<String, String>,
}

/// Why the key store could not be read or written.
#[derive(Debug)]
pub struct KeyError(String);

impl KeyError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KeyError {}

impl From<io::Error> for KeyError {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

impl Store {
    /// The store at an explicit path, which is what tests use.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The store a machine's data root names.
    pub fn from_data_root(data_root: &Path) -> Self {
        Self::new(data_root.join(SECRETS_DIR).join(KEYS_FILE))
    }

    /// The path, for the proxy container's mount and for tests.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Make the secrets directory and an empty key file, mode 0600, if they are not there.
    ///
    /// Called at boot and before the key proxy starts. Both modes are enforced: the directory 0700 and the
    /// file 0600. A pre-existing directory is reset rather than trusted, because a secret left group-readable
    /// is the one failure mode this file cannot recover from by being read again.
    ///
    /// A file that already exists is prepared too, in the one way it needs: a legacy `[providers]` row whose id
    /// names a tool moves into `[tools]` ([`normalize`](Self::normalize)). Nothing else about an existing file is
    /// touched — not its whitespace and not its table order — so a deploy's "kept" file survives a boot unchanged.
    /// The move is best effort: a row that names a tool already **reads** as a tool key without it, so a file this
    /// step cannot rewrite is not a reason to refuse the boot that mounts it, and the proxy's own
    /// [`load`](Self::load) still reports a file it cannot parse, per request, as it did before this step existed.
    pub fn ensure_file(&self) -> Result<(), KeyError> {
        let _guard = lock();
        let directory = self
            .path
            .parent()
            .ok_or_else(|| KeyError::new("the key file has no parent directory"))?;
        create_directories(directory)?;
        if !self.path.exists() {
            write_file(&self.path, &empty_text())?;
        } else {
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
            let _ = self.normalize_unlocked();
        }
        Ok(())
    }

    /// Read the store. A missing file is the empty store: boot creates it, and a machine that never ran
    /// `key set` has nothing to read.
    pub fn load(&self) -> Result<Secrets, KeyError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => parse(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Secrets::default()),
            Err(error) => Err(KeyError::from(error)),
        }
    }

    /// Save `secrets`, mode 0600, through a temporary file in the same directory.
    pub fn save(&self, secrets: &Secrets) -> Result<(), KeyError> {
        let _guard = lock();
        self.save_unlocked(secrets)
    }

    fn save_unlocked(&self, secrets: &Secrets) -> Result<(), KeyError> {
        if let Some(directory) = self.path.parent() {
            create_directories(directory)?;
        }
        write_file(&self.path, &render(secrets))
    }

    /// Store one provider's real key, replacing an existing one. The provider must be in [`PROVIDERS`]:
    /// accepting any string would let an operator save a key the proxy can never use.
    pub fn set_provider(&self, provider: &str, key: &str) -> Result<(), KeyError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(KeyError::new("the key is empty"));
        }
        if crate::key::provider(provider).is_none() {
            return Err(unknown_provider(provider));
        }
        let _guard = lock();
        let mut secrets = self.load()?;
        secrets
            .providers
            .insert(provider.to_string(), key.to_string());
        self.save_unlocked(&secrets)
    }

    /// Remove a provider's real key. `false` means there was none.
    pub fn remove_provider(&self, provider: &str) -> Result<bool, KeyError> {
        let _guard = lock();
        let mut secrets = self.load()?;
        let removed = secrets.providers.remove(provider).is_some();
        if removed {
            self.save_unlocked(&secrets)?;
        }
        Ok(removed)
    }

    /// Store one tool's real key, replacing an existing one. The tool must be in [`TOOLS`], for
    /// [`set_provider`](Self::set_provider)'s reason: a key for an id no row names is a key nothing can spend.
    pub fn set_tool(&self, tool: &str, key: &str) -> Result<(), KeyError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(KeyError::new("the key is empty"));
        }
        if crate::key::tool(tool).is_none() {
            return Err(unknown_tool(tool));
        }
        let _guard = lock();
        let mut secrets = self.load()?;
        secrets.tools.insert(tool.to_string(), key.to_string());
        self.save_unlocked(&secrets)
    }

    /// Remove a tool's real key. `false` means there was none, which `key revoke` answers with a sentence and exit
    /// 0: a key that is already gone is the state the operator asked for.
    pub fn remove_tool(&self, tool: &str) -> Result<bool, KeyError> {
        let _guard = lock();
        let mut secrets = self.load()?;
        let removed = secrets.tools.remove(tool).is_some();
        if removed {
            self.save_unlocked(&secrets)?;
        }
        Ok(removed)
    }

    /// The provider ids that have a real key, sorted by name: `key list`'s first half.
    pub fn providers(&self) -> Result<Vec<String>, KeyError> {
        Ok(self.load()?.providers.into_keys().collect())
    }

    /// The tool ids that have a real key, sorted by name: `key list`'s second half.
    pub fn tools(&self) -> Result<Vec<String>, KeyError> {
        Ok(self.load()?.tools.into_keys().collect())
    }

    /// Move a legacy `[providers]` row whose id names a tool into `[tools]`, on disk. `true` means the file was
    /// rewritten.
    ///
    /// [`load`](Self::load) already **reads** such a file correctly — the row is a tool key in memory and never a
    /// provider — so nothing refuses it; this is what makes the file itself carry the shape this build renders, which
    /// is what `server deploy`'s "kept" file and an operator reading it by hand both see. Only those rows move: a
    /// file without one is left byte for byte alone, whatever its whitespace or table order.
    pub fn normalize(&self) -> Result<bool, KeyError> {
        let _guard = lock();
        self.normalize_unlocked()
    }

    fn normalize_unlocked(&self) -> Result<bool, KeyError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(KeyError::from(error)),
        };
        let (secrets, legacy_row) = parse_tables(&text)?;
        if !legacy_row {
            return Ok(false);
        }
        write_file(&self.path, &render(&secrets))?;
        Ok(true)
    }

    /// The session's dummy key, minted on first use and stable afterwards.
    ///
    /// A resume finds the same dummy, so the `models.json` a previous container left keeps working; a
    /// restart finds it in the file for the same reason. The value is prefixed so a log line or a grep
    /// cannot confuse it with a provider key.
    pub fn dummy_for(&self, session_id: &str) -> Result<String, KeyError> {
        let _guard = lock();
        let mut secrets = self.load()?;
        if let Some(dummy) = secrets.sessions.get(session_id) {
            return Ok(dummy.clone());
        }
        let dummy = format!("hs-dummy-{}", random_hex());
        secrets
            .sessions
            .insert(session_id.to_string(), dummy.clone());
        self.save_unlocked(&secrets)?;
        Ok(dummy)
    }

    /// Forget the session's dummy key (spec item 9's "key revoked on delete").
    pub fn revoke(&self, session_id: &str) -> Result<bool, KeyError> {
        let _guard = lock();
        let mut secrets = self.load()?;
        let removed = secrets.sessions.remove(session_id).is_some();
        if removed {
            self.save_unlocked(&secrets)?;
        }
        Ok(removed)
    }

    /// The session id a dummy key belongs to, which is what the proxy's log line attributes.
    pub fn session_of<'a>(secrets: &'a Secrets, dummy: &str) -> Option<&'a str> {
        secrets
            .sessions
            .iter()
            .find(|(_, candidate)| candidate.as_str() == dummy)
            .map(|(session, _)| session.as_str())
    }

    /// The stored key for `tool` in this read of the store, or `None` when the machine holds none for that row.
    ///
    /// [`session_of`](Self::session_of)'s shape rather than a read of its own: a proxy has already called
    /// [`load`](Self::load) for the request's dummy, and the key half has to come from the **same** generation of the
    /// file. Nothing caches a load, so `key set linear` on a running machine is spent by the next request with no
    /// restart — the per-request read is the rotation mechanism (ADR-0098).
    pub fn tool_key<'a>(secrets: &'a Secrets, tool: &str) -> Option<&'a str> {
        secrets.tools.get(tool).map(String::as_str)
    }
}

/// The secrets directory at 0700, and — when this call is the one that makes it — the data root above it at 0755.
///
/// Two creators make the data root (`serve`'s boot and this store, which `key set` calls before any deploy), and the
/// deploy's step 2 writes it with a third: they have to agree, or a machine that ran the documented next step
/// (`holosmith key set <provider>`) would leave a data root at the umask's 0775 and `doctor`'s `perms` finding would
/// report a mode nothing in the product chose. `create_dir_all` is what reaches the data root here — the
/// store lives one level below it — so the mode is set only when the parent did not exist, and an operator's own
/// mode is left alone.
fn create_directories(directory: &Path) -> io::Result<()> {
    let data_root = directory.parent();
    let created = data_root.is_some_and(|root| !root.exists());
    fs::create_dir_all(directory)?;
    if let Some(root) = data_root
        && created
    {
        fs::set_permissions(root, fs::Permissions::from_mode(0o755))?;
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
}

/// The process-wide write lock. Two creates can mint dummies at once; a read-modify-write without it drops one.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The empty store, which is what `ensure_file` creates and what `server deploy` stages.
///
/// `pub(crate)` rather than private because the deploy writes this file's *creation* from its own step: the one
/// spelling of "an empty key store" has to be the one `key list` reads, or a deploy would create a file the key
/// commands then refuse to parse. It is [`render`] of the empty [`Secrets`] rather than a third spelling of the
/// tables, so a table added there appears here without an edit — and because [`render`] writes `[tools]` only when
/// a tool key is stored, this is the previous release's `[providers]\n[sessions]\n` exactly (ADR-0098).
pub(crate) fn empty_text() -> String {
    render(&Secrets::default())
}

fn write_file(path: &Path, text: &str) -> Result<(), KeyError> {
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    fs::rename(&temporary, path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn parse(text: &str) -> Result<Secrets, KeyError> {
    Ok(parse_tables(text)?.0)
}

/// [`parse`], and whether the text carried a `[providers]` row whose id names a tool.
///
/// That row is a file an earlier release wrote: `key set linear` had one table, so it stored a tool's key under
/// `[providers]`, where [`models`] silently skipped it and nothing could spend it. The row is **read as a tool key**
/// and not as a provider, so `models()` still names no tool; the flag is what lets [`Store::normalize`] rewrite the
/// file into the shape this build renders, once, and only for a file that has such a row. A row whose id is neither
/// a provider nor a tool stays exactly where it is — that is a stale id, not a mislaid table.
///
/// A file naming the same id in both tables is one this build never writes; `[tools]` wins, because that is the
/// table the key belongs in.
fn parse_tables(text: &str) -> Result<(Secrets, bool), KeyError> {
    let mut secrets = Secrets::default();
    let mut legacy_row = false;
    let mut section = "";
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = match name.trim() {
                "providers" => "providers",
                "tools" => "tools",
                "sessions" => "sessions",
                other => {
                    return Err(KeyError::new(format!(
                        "the key file's line {} names an unknown table [{other}]",
                        number + 1
                    )));
                }
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(KeyError::new(format!(
                "the key file's line {} is not key = value",
                number + 1
            )));
        };
        let key = key.trim();
        let value = parse_string(value.trim()).map_err(|error| {
            KeyError::new(format!("the key file's line {}: {error}", number + 1))
        })?;
        if key.is_empty() {
            return Err(KeyError::new(format!(
                "the key file's line {} has an empty key",
                number + 1
            )));
        }
        match section {
            "providers" if tool(key).is_some() => {
                legacy_row = true;
                secrets.tools.entry(key.to_string()).or_insert(value);
            }
            "providers" => {
                secrets.providers.insert(key.to_string(), value);
            }
            "tools" => {
                secrets.tools.insert(key.to_string(), value);
            }
            "sessions" => {
                secrets.sessions.insert(key.to_string(), value);
            }
            _ => {
                return Err(KeyError::new(format!(
                    "the key file's line {} is outside a table",
                    number + 1
                )));
            }
        }
    }
    Ok((secrets, legacy_row))
}

/// One TOML basic string, which is the only value shape this file writes. A string that is not quoted is
/// refused rather than guessed at: an API key pasted without quotes is a file a human can see is wrong.
fn parse_string(text: &str) -> Result<String, &'static str> {
    let Some(text) = text.strip_prefix('"') else {
        return Err("a value must be a quoted string");
    };
    let Some(text) = text.strip_suffix('"') else {
        return Err("a quoted string does not end in a quote");
    };
    let mut out = String::new();
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        let escaped = characters.next().ok_or("a backslash ends the value")?;
        match escaped {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let digits: String = characters.by_ref().take(4).collect();
                let number = u32::from_str_radix(&digits, 16)
                    .map_err(|_| "a \\u escape is not four hexadecimal digits")?;
                out.push(char::from_u32(number).ok_or("a \\u escape is not a character")?);
            }
            _ => return Err("a backslash escape is not one this file writes"),
        }
    }
    Ok(out)
}

/// The file's text: `[providers]`, then `[tools]` **only when a tool key is stored**, then `[sessions]`.
///
/// The conditional table is what keeps a store without a tool key byte-identical to the file the release before
/// this one writes, because that release's parser is a closed list: it refuses a file naming `[tools]` and answers
/// 503 for every model call. A machine that never stored a tool key is therefore unaffected by a rollback; one that
/// did makes its file readable again with `key revoke <id>` (ADR-0098, "Rollback").
fn render(secrets: &Secrets) -> String {
    let mut out = String::new();
    out.push_str("[providers]\n");
    for (provider, key) in &secrets.providers {
        out.push_str(&format!("{provider} = {}\n", quoted(key)));
    }
    if !secrets.tools.is_empty() {
        out.push_str("[tools]\n");
        for (tool, key) in &secrets.tools {
            out.push_str(&format!("{tool} = {}\n", quoted(key)));
        }
    }
    out.push_str("[sessions]\n");
    for (session, dummy) in &secrets.sessions {
        out.push_str(&format!("{session} = {}\n", quoted(dummy)));
    }
    out
}

fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{:04X}", u32::from(character)))
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

fn random_hex() -> String {
    let mut bytes = [0u8; 32];
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut file, &mut bytes);
    } else {
        // A machine without `/dev/urandom` is not one this build supports, but a deterministic fallback is
        // worse than no key: it would make every session's dummy equal. The timestamp is at least unique per
        // process and session, and the caller will fail its own `key set` if it cannot write the file.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&now.to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The `models.json` a session's pi-home carries: the key proxy's URL and the session's dummy key.
///
/// `providers` is the set with a real key. An empty set writes an empty provider object, which is honest: pi
/// will have no model available until the operator runs `key set`. The real key never appears here, which is
/// the one property a session mount may not break — and a tool's key is not a model provider, so it is not a row
/// here either, whatever the store holds (ADR-0098).
pub fn models(secrets: &Secrets, dummy: &str) -> String {
    let mut providers = serde_json::Map::new();
    for id in secrets.providers.keys() {
        let Some(provider) = provider(id) else {
            continue;
        };
        let mut row = serde_json::Map::new();
        row.insert(
            "baseUrl".to_string(),
            serde_json::Value::String(format!(
                "http://{KEY_PROXY_ALIAS}:{KEY_PROXY_PORT}/{}",
                provider.id
            )),
        );
        row.insert(
            "apiKey".to_string(),
            serde_json::Value::String(dummy.to_string()),
        );
        providers.insert(provider.id.to_string(), serde_json::Value::Object(row));
    }
    let document = serde_json::json!({
        "$schema": "https://pi.dev/schemas/models.schema.json",
        "providers": providers,
    });
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_default();
    text.push('\n');
    text
}

/// The alias a session's container resolves the key proxy by. A network alias, not the container's name,
/// for ADR-0045's reason: Docker's embedded DNS answers an alias only on networks the asker shares.
pub const KEY_PROXY_ALIAS: &str = "key-proxy";

/// The port the key proxy listens on and the port `models.json` names.
pub const KEY_PROXY_PORT: u16 = 8080;

/// The container's name. `hs_key`, one per machine.
pub const KEY_PROXY_NAME: &str = "hs_key";

/// The name a run with this container prefix gives it: `hs_key`, or `smoke_hs_key`.
///
/// Prefixed for [`crate::container::proxy_name`]'s reason, and the key proxy is the sharper half of that reason: it
/// mounts the key store of the config it was started from, so an unprefixed smoke run's proxy would replace the
/// machine's with one holding an empty store.
pub fn key_proxy_name(prefix: &str) -> String {
    format!("{prefix}{KEY_PROXY_NAME}")
}

/// The secrets a session's `models.json` or `mcp.json` may never contain: the real provider keys **and** the real tool
/// keys. Kept here so a test can grep a whole session directory for one without re-spelling the rule: a session mount
/// holds a dummy and no real key of either table.
pub fn contains_real_key(secrets: &Secrets, text: &str) -> bool {
    secrets
        .providers
        .values()
        .chain(secrets.tools.values())
        .any(|key| text.contains(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A temporary directory that removes itself, so the tests leave nothing in `/tmp`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("holosmith-key-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("the scratch directory is creatable");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// `key set` is the documented step after a deploy, and it can be the *first* thing that makes the data root.
    ///
    /// Three creators write that directory's mode — `serve`'s boot, this store, and `server deploy`'s step 2 — and
    /// `doctor`'s `perms` finding compares it with the deploy's 0755. `create_dir_all` would give the umask's 0775,
    /// which is the mode this machine's umask produces, so a permissive-umask machine is the one this test bites on
    ///
    #[test]
    fn the_data_root_the_key_store_makes_is_the_mode_the_deploy_writes() {
        let scratch = Scratch::new("root-mode");
        let data_root = scratch.0.join("data");
        Store::from_data_root(&data_root)
            .ensure_file()
            .expect("the store is creatable");
        let mode = fs::metadata(&data_root)
            .expect("the store made the data root")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o755,
            "the key store made the data root {mode:04o}, which the deploy and `serve` do not write"
        );
        let secrets = data_root.join(SECRETS_DIR);
        let secrets_mode = fs::metadata(&secrets)
            .expect("the store made the secrets directory")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(secrets_mode, 0o700, "the secrets directory is not 0700");

        // An operator's own mode is theirs: a data root that already exists is not reset.
        fs::set_permissions(&data_root, fs::Permissions::from_mode(0o700))
            .expect("the data root's mode is settable");
        Store::from_data_root(&data_root)
            .ensure_file()
            .expect("the store is readable");
        let kept = fs::metadata(&data_root)
            .expect("the data root is there")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(kept, 0o700, "the store reset a mode it did not choose");
    }

    /// The table's first row is the machine's default provider, and the sentence that lists the table opens with it.
    ///
    /// The order is load-bearing (ADR-0093): the app prefills its model with this id, a machine with no key is told
    /// to store a key for it, and `key set bogus` names it before the other eleven. Two spellings of "the default"
    /// would be a release where the form and the refusal disagree.
    ///
    #[test]
    fn the_first_row_is_the_default_provider_and_the_refusal_opens_with_it() {
        assert_eq!(default_provider(), "opencode-go");
        assert_eq!(
            provider_ids().first().copied(),
            Some(default_provider()),
            "the default provider is not the first id a refusal names"
        );
        assert!(
            provider(default_provider()).is_some(),
            "the default provider is not a row this build forwards to"
        );

        let scratch = Scratch::new("default-provider");
        let refusal = Store::from_data_root(&scratch.0)
            .set_provider("bogus", "sk-bogus")
            .expect_err("an unknown provider is refused")
            .to_string();
        assert!(
            refusal.contains(&format!("knows: {}, ", default_provider())),
            "the refusal does not open with the default provider: {refusal}"
        );
        assert!(
            refusal.ends_with(&provider_ids().join(", ")),
            "the refusal does not name the whole table: {refusal}"
        );
    }

    /// `opencode-go` serves two APIs under one host, and the table says so exactly: two endpoints, two base
    /// paths, two headers, and the anthropic triple as the row's own default.
    ///
    /// A model's API shape is the catalog's fact, not the row's (ADR-0097): pi's bundled catalog names an
    /// `openai-completions` model `opencode-go/deepseek-v4.1-flash` and an `anthropic-messages` one
    /// `opencode-go/claude-haiku-5-5`, and appends `/chat/completions` or `/v1/messages` to the base URL this
    /// build wrote. A row that took one shape only 404s every model of the other — the defect this table fixes.
    /// The row's own `base_path` stays Anthropic's so a tail that names no shape routes as it always has.
    ///
    #[test]
    fn opencode_go_names_one_shape_per_endpoint_and_every_other_row_names_none() {
        let row = provider("opencode-go").expect("the measured provider is a row");
        assert_eq!(row.base_path, "/inference/go/anthropic");
        assert_eq!(row.auth, Auth::XApiKey);
        assert_eq!(
            row.shapes,
            &[
                Shape {
                    endpoint: "/v1/messages",
                    base_path: "/inference/go/anthropic",
                    auth: Auth::XApiKey,
                },
                Shape {
                    endpoint: "/chat/completions",
                    base_path: "/inference/go/openai/v1",
                    auth: Auth::Bearer,
                },
            ],
            "the shapes are the two API roots opencode.ai publishes for this key, and nothing else"
        );
        assert_eq!(
            row.shape_for("/v1/messages"),
            ("/inference/go/anthropic", Auth::XApiKey)
        );
        assert_eq!(
            row.shape_for("/chat/completions"),
            ("/inference/go/openai/v1", Auth::Bearer)
        );
        assert_eq!(
            row.shape_for("/something/else"),
            (row.base_path, row.auth),
            "a tail that names no shape must keep the row's own triple"
        );
        assert_eq!(
            row.shape_for("/v1/messagesfoo"),
            (row.base_path, row.auth),
            "an endpoint is matched exactly, not as a prefix of a longer segment"
        );

        for other in PROVIDERS.iter().filter(|row| row.id != "opencode-go") {
            assert!(
                other.shapes.is_empty(),
                "{} carries a shape this test does not pin",
                other.id
            );
            assert_eq!(
                other.shape_for("/chat/completions"),
                (other.base_path, other.auth),
                "{} answered another row's shape",
                other.id
            );
        }
    }

    /// The one tool row, exactly, and the four facts that make a tool key not a provider key.
    ///
    /// A tool key is a credential this machine holds, so it lives in the same 0600 file; it is not a model
    /// provider, so no catalog row, no default and no `models.json` row may name it (ADR-0098). Those four facts are
    /// the difference between "a second table" and "a provider row under another heading".
    ///
    #[test]
    fn the_tools_table_names_one_host_and_no_tool_is_a_provider() {
        assert_eq!(
            TOOLS,
            &[Tool {
                id: "linear",
                host: "mcp.linear.app",
                auth: Auth::Bearer,
                endpoints: &["/mcp", "/mcp/readonly"],
            }],
            "the tools this build holds a key for are one MCP host and the paths it serves"
        );
        assert_eq!(tool_ids(), vec!["linear"]);
        assert_eq!(tool("linear").map(|row| row.host), Some("mcp.linear.app"));
        assert!(
            tool("not-a-tool").is_none(),
            "an id no row names is not a tool"
        );

        assert!(provider("linear").is_none(), "linear is a provider row");
        assert!(!provider_ids().contains(&"linear"));
        assert_ne!(default_provider(), "linear");
        for id in tool_ids() {
            assert!(!provider_ids().contains(&id), "{id} is in both tables");
        }

        let scratch = Scratch::new("tool-not-provider");
        let store = Store::from_data_root(&scratch.0);
        store
            .set_tool("linear", "lin_api_the_tool_key")
            .expect("a tool key is storable");
        let secrets = store.load().expect("the store reads");
        assert!(
            secrets.providers.is_empty(),
            "a tool key landed in the provider table: {:?}",
            secrets.providers
        );
        assert_eq!(
            secrets.tools.get("linear").map(String::as_str),
            Some("lin_api_the_tool_key")
        );
        let models = models(&secrets, "hs-dummy-test");
        assert!(
            !models.contains("linear"),
            "models.json named the tool: {models}"
        );
        assert!(
            !models.contains("lin_api_the_tool_key"),
            "models.json leaked the tool key: {models}"
        );
    }

    /// A tool key round-trips through the file, setting it again replaces the one row, and the file's own text is
    /// the rendered `[tools]` shape between the two tables it has always had.
    #[test]
    fn a_tool_key_round_trips_replaces_itself_and_renders_under_tools() {
        let scratch = Scratch::new("tool-round-trip");
        let store = Store::from_data_root(&scratch.0);
        store
            .set_provider("anthropic", "sk-ant-real")
            .expect("the provider key is storable");
        store
            .set_tool("linear", "lin_api_first")
            .expect("the tool key is storable");
        store
            .set_tool("linear", "lin_api_second")
            .expect("the tool key is replaceable");
        assert_eq!(
            fs::read_to_string(store.path()).expect("the file is readable"),
            "[providers]\nanthropic = \"sk-ant-real\"\n[tools]\nlinear = \"lin_api_second\"\n\
             [sessions]\n",
            "the file is providers, then tools, then sessions, and one tool is one row"
        );
        let secrets = store.load().expect("the store reads");
        assert_eq!(secrets.tools.len(), 1, "setting a tool twice left two rows");
        assert_eq!(
            Store::tool_key(&secrets, "linear"),
            Some("lin_api_second"),
            "the second value did not win"
        );
        assert_eq!(store.tools().expect("the store reads"), vec!["linear"]);
        assert!(
            store.remove_tool("linear").expect("the store writes"),
            "the stored tool key was not removed"
        );
        assert!(
            !store.remove_tool("linear").expect("the store writes"),
            "a second remove found something"
        );
        assert_eq!(
            Store::tool_key(&store.load().expect("the store reads"), "linear"),
            None
        );
    }

    /// `set_tool` refuses what `set_provider` refuses: an id no row names, and a key with nothing in it.
    #[test]
    fn set_tool_refuses_an_empty_key_and_an_id_no_row_names() {
        let scratch = Scratch::new("tool-refusals");
        let store = Store::from_data_root(&scratch.0);
        let empty = store
            .set_tool("linear", "  \n")
            .expect_err("an empty key is refused")
            .to_string();
        assert_eq!(empty, "the key is empty");
        let unknown = store
            .set_tool("not-a-tool", "some-key")
            .expect_err("an id no row names is refused")
            .to_string();
        assert!(
            unknown.contains(&tool_ids().join(", ")),
            "the refusal does not name the tools this build knows: {unknown}"
        );
        assert!(
            store.load().expect("the store reads").tools.is_empty(),
            "a refused set stored something"
        );
    }

    /// A `[providers]` row whose id names a tool is a file an earlier release wrote: `load` reads it as a tool key
    /// without refusing, and the next prepare of the file carries `[tools]` instead. An unknown provider id stays
    /// exactly where it is.
    #[test]
    fn a_legacy_provider_row_naming_a_tool_is_read_and_persisted_as_a_tool_key() {
        let scratch = Scratch::new("tool-legacy");
        let store = Store::from_data_root(&scratch.0);
        let path = store.path().to_path_buf();
        fs::create_dir_all(path.parent().expect("the store has a directory"))
            .expect("the secrets directory is creatable");
        fs::write(
            &path,
            "[providers]\nlinear = \"lin_api_legacy\"\ngone-provider = \"sk-gone\"\n[sessions]\n",
        )
        .expect("the legacy file is writable");

        let secrets = store
            .load()
            .expect("a file this build can normalize is not refused");
        assert_eq!(
            secrets.tools.get("linear").map(String::as_str),
            Some("lin_api_legacy"),
            "the legacy row is not read as a tool key"
        );
        assert!(
            !secrets.providers.contains_key("linear"),
            "the legacy row is still a provider"
        );
        assert_eq!(
            secrets.providers.get("gone-provider").map(String::as_str),
            Some("sk-gone"),
            "an id that is neither a provider nor a tool was moved"
        );

        assert!(
            store.normalize().expect("the store writes"),
            "the file carrying a legacy row was not rewritten"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the file is readable"),
            "[providers]\ngone-provider = \"sk-gone\"\n[tools]\nlinear = \"lin_api_legacy\"\n\
             [sessions]\n",
            "the persisted form still carries the row under [providers], or lost the one beside it"
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("the file exists")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the rewrite left the key file readable by others"
        );
        assert!(
            !store.normalize().expect("the store writes"),
            "a normalized file was rewritten again"
        );

        // The boot path prepares the file, so it is the one that migrates a machine's store.
        fs::write(
            &path,
            "[providers]\nlinear = \"lin_api_legacy\"\n[sessions]\n",
        )
        .expect("the legacy file is writable");
        store.ensure_file().expect("the store prepares the file");
        assert_eq!(
            fs::read_to_string(&path).expect("the file is readable"),
            "[providers]\n[tools]\nlinear = \"lin_api_legacy\"\n[sessions]\n",
            "preparing the file did not move the legacy row"
        );
    }

    /// A file naming the same id in both tables is one this build never writes; `[tools]` wins, because that is
    /// the table the key belongs in — in either order the two tables can appear, and the rewrite then leaves one row.
    #[test]
    fn the_tools_table_wins_when_a_file_names_one_id_in_both() {
        let scratch = Scratch::new("tool-both-tables");
        let store = Store::from_data_root(&scratch.0);
        let path = store.path().to_path_buf();
        fs::create_dir_all(path.parent().expect("the store has a directory"))
            .expect("the secrets directory is creatable");
        for (order, text) in [
            (
                "providers first",
                "[providers]\nlinear = \"lin_api_old\"\n[tools]\nlinear = \"lin_api_new\"\n[sessions]\n",
            ),
            (
                "tools first",
                "[tools]\nlinear = \"lin_api_new\"\n[providers]\nlinear = \"lin_api_old\"\n[sessions]\n",
            ),
        ] {
            fs::write(&path, text).expect("the file is writable");
            assert_eq!(
                Store::tool_key(&store.load().expect("the store reads"), "linear"),
                Some("lin_api_new"),
                "{order}: the [providers] row won"
            );
            assert!(
                store.load().expect("the store reads").providers.is_empty(),
                "{order}: the id is still a provider too"
            );
            store.normalize().expect("the store writes");
            assert_eq!(
                fs::read_to_string(&path).expect("the file is readable"),
                "[providers]\n[tools]\nlinear = \"lin_api_new\"\n[sessions]\n",
                "{order}: the rewrite left two rows for one id, or kept the losing value"
            );
        }
    }

    /// A file with no legacy row is left **byte for byte** alone: this is a migration of one row, not a formatter,
    /// and a deploy's kept file (or an operator's) must survive a boot unchanged.
    #[test]
    fn normalize_leaves_a_file_without_a_legacy_row_alone() {
        let scratch = Scratch::new("tool-no-legacy");
        let store = Store::from_data_root(&scratch.0);
        let path = store.path().to_path_buf();
        fs::create_dir_all(path.parent().expect("the store has a directory"))
            .expect("the secrets directory is creatable");
        let untouched = "[providers]\nanthropic = \"sk-ant-real\"\n\n[sessions]\n";
        fs::write(&path, untouched).expect("the file is writable");
        assert!(
            !store.normalize().expect("the store writes"),
            "a file with no legacy row was rewritten"
        );
        store.ensure_file().expect("the store prepares the file");
        assert_eq!(
            fs::read_to_string(&path).expect("the file is readable"),
            untouched,
            "preparing the file rewrote one whose rows need no moving"
        );

        // Preparing the file is not the place to judge it: a boot that mounts an unparseable store still starts,
        // and the proxy's own read is what reports the problem, per request, as it did before the move existed.
        fs::write(&path, "[not-a-table]\n").expect("the file is writable");
        store
            .ensure_file()
            .expect("an existing file is prepared, not judged");
        assert_eq!(
            fs::read_to_string(&path).expect("the file is readable"),
            "[not-a-table]\n",
            "a file this step cannot parse was rewritten"
        );
    }

    /// One spelling of the empty store: what a deploy stages is what `key list` reads, table for table — and it is
    /// the **previous release's** text exactly, because `[tools]` is written only when it holds a key.
    ///
    /// The conditional is pinned in both directions here: an empty store has no `[tools]` heading, and one stored
    /// tool key is one heading and one row.
    #[test]
    fn the_empty_store_is_the_rendering_of_the_empty_secrets() {
        assert_eq!(empty_text(), render(&Secrets::default()));
        assert_eq!(
            empty_text(),
            "[providers]\n[sessions]\n",
            "the empty store is not the file the release before the tool table writes"
        );
        assert_eq!(
            parse(&empty_text()).expect("the empty store parses"),
            Secrets::default()
        );

        let mut stored = Secrets::default();
        stored
            .tools
            .insert("linear".to_string(), "lin_api_one".to_string());
        assert_eq!(
            render(&stored),
            "[providers]\n[tools]\nlinear = \"lin_api_one\"\n[sessions]\n",
            "a stored tool key does not render the table it is stored in"
        );
    }
}
