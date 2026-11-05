//! The machine's provider keys and each session's dummy key (HS-039, spec item 2).
//!
//! The story's promise is a negative one: a real provider key never enters a session container. Two files
//! make that true and this module owns both:
//!
//! * `<data>/secrets/keys.toml`, mode **0600**, is the only place a real key is written. `hs_key` mounts it
//!   read-only; no session container mounts it at all.
//! * `<data>/sessions/<id>/pi-home/models.json` carries the session's **dummy** key and the key proxy's URL.
//!   That file is inside a session mount, which is exactly why it may hold nothing else.
//!
//! `keys.toml` carries both halves because the proxy has no database: the provider keys are what it swaps in,
//! and the session table is what lets it answer "is this dummy key still valid?". Deleting a session removes
//! its row here ([`Store::revoke`]), which is the HS-019 hook's whole body.
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
    pub base_path: &'static str,
    /// Where the real key goes in the upstream request.
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

/// The providers this build knows how to forward to.
///
/// The list is deliberately small and explicit. A provider that is missing is a `key set` refusal with a
/// sentence naming what is supported, not a generic proxy that guesses a host.
pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "anthropic",
        host: "api.anthropic.com",
        base_path: "",
        auth: Auth::XApiKey,
    },
    Provider {
        id: "openai",
        host: "api.openai.com",
        base_path: "/v1",
        auth: Auth::Bearer,
    },
    Provider {
        id: "openrouter",
        host: "openrouter.ai",
        base_path: "/api/v1",
        auth: Auth::Bearer,
    },
    Provider {
        id: "deepseek",
        host: "api.deepseek.com",
        base_path: "",
        auth: Auth::Bearer,
    },
    Provider {
        id: "mistral",
        host: "api.mistral.ai",
        base_path: "/v1",
        auth: Auth::Bearer,
    },
    Provider {
        id: "groq",
        host: "api.groq.com",
        base_path: "/openai/v1",
        auth: Auth::Bearer,
    },
    Provider {
        id: "xai",
        host: "api.x.ai",
        base_path: "/v1",
        auth: Auth::Bearer,
    },
    Provider {
        id: "fireworks",
        host: "api.fireworks.ai",
        base_path: "/inference/v1",
        auth: Auth::Bearer,
    },
    Provider {
        id: "together",
        host: "api.together.xyz",
        base_path: "/v1",
        auth: Auth::Bearer,
    },
    Provider {
        id: "cerebras",
        host: "api.cerebras.ai",
        base_path: "/v1",
        auth: Auth::Bearer,
    },
    Provider {
        id: "nvidia",
        host: "integrate.api.nvidia.com",
        base_path: "/v1",
        auth: Auth::Bearer,
    },
];

/// The provider row `id` names, if this build has one.
pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

/// The provider ids, in table order, for a refusal's sentence and the CLI's help.
pub fn provider_ids() -> Vec<&'static str> {
    PROVIDERS.iter().map(|provider| provider.id).collect()
}

/// The machine's key store: one path, and no cached copy. A write is a read-modify-write under a process lock,
/// because the create path and the delete path can touch it from different tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    path: PathBuf,
}

/// The parsed shape of `keys.toml`: real keys by provider, and the dummy key each live session spends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Secrets {
    pub providers: BTreeMap<String, String>,
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
    pub fn ensure_file(&self) -> Result<(), KeyError> {
        let _guard = lock();
        let directory = self
            .path
            .parent()
            .ok_or_else(|| KeyError::new("the key file has no parent directory"))?;
        fs::create_dir_all(directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        if !self.path.exists() {
            write_file(&self.path, &empty_text())?;
        } else {
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
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
            fs::create_dir_all(directory)?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
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
            return Err(KeyError::new(format!(
                "{provider} is not a provider this build knows: {}",
                provider_ids().join(", ")
            )));
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

    /// The provider ids that have a real key, sorted by name: `key list`'s whole output.
    pub fn providers(&self) -> Result<Vec<String>, KeyError> {
        Ok(self.load()?.providers.into_keys().collect())
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
}

/// The process-wide write lock. Two creates can mint dummies at once; a read-modify-write without it drops one.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn empty_text() -> String {
    "[providers]\n[sessions]\n".to_string()
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
    let mut secrets = Secrets::default();
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
            "providers" => {
                secrets.providers.insert(key.to_string(), value);
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
    Ok(secrets)
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

fn render(secrets: &Secrets) -> String {
    let mut out = String::new();
    out.push_str("[providers]\n");
    for (provider, key) in &secrets.providers {
        out.push_str(&format!("{provider} = {}\n", quoted(key)));
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
/// the one property a session mount may not break.
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

/// The secret a `models.json` file may never contain: the real provider key. Kept here so a test can grep a
/// whole session directory for it without re-spelling the rule.
pub fn contains_real_key(secrets: &Secrets, text: &str) -> bool {
    secrets.providers.values().any(|key| text.contains(key))
}
