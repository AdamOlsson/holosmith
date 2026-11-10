//! The one table of config keys.
//!
//! Every key the file may hold is one row: where it lives, how it is written, and how it is
//! read into [`Config`]. Adding a key later is one field, one row here, one semantic rule
//! if the value can be impossible, and one test (ADR-0006).

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use toml_edit::{Document, Item, Key, Value};

use super::problem::{Position, Problem};
use super::{Config, Duration, Origin, Role};
use crate::runtime::Backend;

/// How a key's value is written in TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    String,
    Integer,
    Boolean,
    StringArray,
    Duration,
}

impl Kind {
    /// True when `value` has the shape this kind is written in.
    fn accepts(self, value: &Value) -> bool {
        match self {
            Self::String | Self::Duration => value.is_str(),
            Self::Integer => value.is_integer(),
            Self::Boolean => value.is_bool(),
            Self::StringArray => value
                .as_array()
                .is_some_and(|array| array.iter().all(Value::is_str)),
        }
    }

    /// How the kind is named in an "expected …, found …" message.
    fn describe(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::StringArray => "array of strings",
            Self::Duration => "duration string with a unit suffix (s, ms, m), like \"10s\"",
        }
    }
}

/// One key the config file may hold.
pub struct KeySpec {
    /// The dotted path: `role`, or `merge.targets`.
    pub path: &'static str,
    /// The section the key lives in, or `None` for a flat key.
    pub section: Option<&'static str>,
    pub kind: Kind,
    /// Read the key's value into `Config`; the message says why the value was refused.
    pub set: fn(&mut Config, &Value) -> Result<(), String>,
}

/// Every key, in the order `config show` writes them: flat keys first, then the sections.
/// TOML requires that order too, and the render round-trip test enforces it.
pub const KNOWN_KEYS: &[KeySpec] = &[
    KeySpec {
        path: "role",
        section: None,
        kind: Kind::String,
        set: set_role,
    },
    KeySpec {
        path: "backend",
        section: None,
        kind: Kind::String,
        set: set_backend,
    },
    KeySpec {
        path: "control_listen",
        section: None,
        kind: Kind::String,
        set: set_control_listen,
    },
    KeySpec {
        path: "preview_listen",
        section: None,
        kind: Kind::String,
        set: set_preview_listen,
    },
    KeySpec {
        path: "control_origin",
        section: None,
        kind: Kind::String,
        set: set_control_origin,
    },
    KeySpec {
        path: "preview_origin",
        section: None,
        kind: Kind::String,
        set: set_preview_origin,
    },
    KeySpec {
        path: "preview_upstream_port",
        section: None,
        kind: Kind::Integer,
        set: set_preview_upstream_port,
    },
    KeySpec {
        path: "allowed_hosts",
        section: None,
        kind: Kind::StringArray,
        set: set_allowed_hosts,
    },
    KeySpec {
        path: "owner",
        section: None,
        kind: Kind::String,
        set: set_owner,
    },
    KeySpec {
        path: "machine_name",
        section: None,
        kind: Kind::String,
        set: set_machine_name,
    },
    KeySpec {
        path: "data_root",
        section: None,
        kind: Kind::String,
        set: set_data_root,
    },
    KeySpec {
        path: "projects_root",
        section: None,
        kind: Kind::String,
        set: set_projects_root,
    },
    KeySpec {
        path: "agent_image",
        section: None,
        kind: Kind::String,
        set: set_agent_image,
    },
    KeySpec {
        path: "agent_toolchains",
        section: None,
        kind: Kind::StringArray,
        set: set_agent_toolchains,
    },
    KeySpec {
        path: "container_cpus",
        section: None,
        kind: Kind::Integer,
        set: set_container_cpus,
    },
    KeySpec {
        path: "container_memory_mb",
        section: None,
        kind: Kind::Integer,
        set: set_container_memory_mb,
    },
    KeySpec {
        path: "container_pids",
        section: None,
        kind: Kind::Integer,
        set: set_container_pids,
    },
    KeySpec {
        path: "docker_host",
        section: None,
        kind: Kind::String,
        set: set_docker_host,
    },
    KeySpec {
        path: "hosts_seed",
        section: None,
        kind: Kind::String,
        set: set_hosts_seed,
    },
    KeySpec {
        path: "allowed_hubs",
        section: None,
        kind: Kind::StringArray,
        set: set_allowed_hubs,
    },
    KeySpec {
        path: "network_default",
        section: None,
        kind: Kind::Boolean,
        set: set_network_default,
    },
    KeySpec {
        path: "max_live_sessions",
        section: None,
        kind: Kind::Integer,
        set: set_max_live_sessions,
    },
    KeySpec {
        path: "project_backup",
        section: None,
        kind: Kind::Boolean,
        set: set_project_backup,
    },
    KeySpec {
        path: "turn_timeout_min",
        section: None,
        kind: Kind::Integer,
        set: set_turn_timeout_min,
    },
    KeySpec {
        path: "idle_stop_min",
        section: None,
        kind: Kind::Integer,
        set: set_idle_stop_min,
    },
    KeySpec {
        path: "max_session_tokens",
        section: None,
        kind: Kind::Integer,
        set: set_max_session_tokens,
    },
    KeySpec {
        path: "abort_grace",
        section: None,
        kind: Kind::Duration,
        set: set_abort_grace,
    },
    KeySpec {
        path: "min_free_gb",
        section: None,
        kind: Kind::Integer,
        set: set_min_free_gb,
    },
    KeySpec {
        path: "session_disk_soft_gb",
        section: None,
        kind: Kind::Integer,
        set: set_session_disk_soft_gb,
    },
    KeySpec {
        path: "stopped_ttl_days",
        section: None,
        kind: Kind::Integer,
        set: set_stopped_ttl_days,
    },
    KeySpec {
        path: "history_window",
        section: None,
        kind: Kind::Integer,
        set: set_history_window,
    },
    KeySpec {
        path: "merge.targets",
        section: Some("merge"),
        kind: Kind::StringArray,
        set: set_merge_targets,
    },
    KeySpec {
        path: "publish.remotes",
        section: Some("publish"),
        kind: Kind::StringArray,
        set: set_publish_remotes,
    },
    KeySpec {
        path: "egress.allow",
        section: Some("egress"),
        kind: Kind::StringArray,
        set: set_egress_allow,
    },
];

/// Pass 1: every unknown key and every kind mismatch, each at the key that caused it.
///
/// A section may be written as `[merge]` or as `merge = { … }`: both are the same TOML
/// table, and an operator hand-editing the file will eventually write the second.
pub fn walk(text: &str, document: &Document<&str>, problems: &mut Vec<Problem>) {
    let root = document.as_table();
    for (name, item) in root.iter() {
        let position = position_of(text, root, name);
        if !is_section(name) {
            check_key(name, position, item, problems);
            continue;
        }
        if let Some(table) = item.as_table() {
            for (field, entry) in table.iter() {
                let path = format!("{name}.{field}");
                check_key(&path, position_of(text, table, field), entry, problems);
            }
        } else if let Some(inline) = item.as_value().and_then(Value::as_inline_table) {
            for (field, _) in inline.iter() {
                let Some((key, entry)) = inline.get_key_value(field) else {
                    continue;
                };
                let position = key.span().map(|span| Position::at_offset(text, span.start));
                let path = format!("{name}.{field}");
                check_key(&path, position, entry, problems);
            }
        } else {
            problems.push(Problem::new(
                position,
                Some(name.to_string()),
                format!("expected a table of settings, found {}", item.type_name()),
            ));
        }
    }
}

/// Pass 2: read every key the walk accepted. A key it rejected keeps its built-in default,
/// so the semantic pass still runs on — and still reports — the rest of a broken file.
///
/// Returns the keys this document set, which is how a two-key rule knows when it is
/// comparing a built-in default instead of a value the operator wrote.
pub fn build(
    text: &str,
    document: &Document<&str>,
    config: &mut Config,
    problems: &mut Vec<Problem>,
) -> HashSet<&'static str> {
    let mut applied = HashSet::new();
    for spec in KNOWN_KEYS {
        let Some(value) = item_at(document, spec.path).and_then(Item::as_value) else {
            continue;
        };
        if !spec.kind.accepts(value) {
            continue;
        }
        if let Err(message) = (spec.set)(config, value) {
            problems.push(Problem::new(
                key_position(document, text, spec.path),
                Some(spec.path.to_string()),
                message,
            ));
            continue;
        }
        applied.insert(spec.path);
    }
    applied
}

/// The item a dotted key path names, if the document holds one.
pub fn item_at<'document>(
    document: &'document Document<&str>,
    path: &str,
) -> Option<&'document Item> {
    let (head, tail) = split(path);
    let item = document.as_table().get(head)?;
    match tail {
        None => Some(item),
        Some(tail) => section_member(item, tail),
    }
}

/// A key inside a section, whether the section is written `[merge]` or `merge = { … }`.
fn section_member<'document>(item: &'document Item, field: &str) -> Option<&'document Item> {
    if let Some(table) = item.as_table() {
        return table.get(field);
    }
    item.as_value()
        .and_then(Value::as_inline_table)
        .and_then(|inline| inline.get_key_value(field))
        .map(|(_, entry)| entry)
}

/// Where a dotted key path is in the file, if the document holds one.
pub fn key_position(document: &Document<&str>, text: &str, path: &str) -> Option<Position> {
    let (head, tail) = split(path);
    let table = document.as_table();
    let key = match tail {
        None => table.key(head),
        Some(field) => {
            let item = table.get(head)?;
            match item.as_table() {
                Some(section) => section.key(field),
                None => item
                    .as_value()
                    .and_then(Value::as_inline_table)
                    .and_then(|inline| inline.get_key_value(field))
                    .map(|(key, _)| key),
            }
        }
    };
    key.and_then(Key::span)
        .map(|span| Position::at_offset(text, span.start))
}

/// One key: known or unknown, right shape or wrong shape.
fn check_key(path: &str, position: Option<Position>, item: &Item, problems: &mut Vec<Problem>) {
    let Some(spec) = spec(path) else {
        problems.push(Problem::new(
            position,
            Some(path.to_string()),
            "unknown key",
        ));
        return;
    };
    match item.as_value() {
        Some(value) if spec.kind.accepts(value) => {}
        Some(value) => problems.push(Problem::new(
            position,
            Some(path.to_string()),
            format!(
                "expected {}, found {}",
                spec.kind.describe(),
                value.type_name()
            ),
        )),
        None => problems.push(Problem::new(
            position,
            Some(path.to_string()),
            format!(
                "expected {}, found {}",
                spec.kind.describe(),
                item.type_name()
            ),
        )),
    }
}

fn spec(path: &str) -> Option<&'static KeySpec> {
    KNOWN_KEYS.iter().find(|spec| spec.path == path)
}

fn is_section(name: &str) -> bool {
    KNOWN_KEYS.iter().any(|spec| spec.section == Some(name))
}

fn split(path: &str) -> (&str, Option<&str>) {
    match path.split_once('.') {
        Some((head, tail)) => (head, Some(tail)),
        None => (path, None),
    }
}

fn position_of(text: &str, table: &toml_edit::Table, name: &str) -> Option<Position> {
    table
        .key(name)
        .and_then(Key::span)
        .map(|span| Position::at_offset(text, span.start))
}

fn string(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .ok_or_else(|| "expected a string".to_string())
}

fn strings(value: &Value) -> Result<Vec<String>, String> {
    let message = || "expected an array of strings".to_string();
    value
        .as_array()
        .ok_or_else(message)?
        .iter()
        .map(|item| item.as_str().map(str::to_string).ok_or_else(message))
        .collect()
}

fn integer(value: &Value) -> Result<i64, String> {
    value
        .as_integer()
        .ok_or_else(|| "expected an integer".to_string())
}

fn u32_setting(value: &Value) -> Result<u32, String> {
    let number = integer(value)?;
    u32::try_from(number).map_err(|_| {
        if number < 0 {
            format!("must not be negative, not {number}")
        } else {
            // No floor here: the semantic pass owns the floor, and a range that claims 0 is
            // allowed while the next message demands at least 1 contradicts itself.
            format!("must fit in 32 bits, not {number}")
        }
    })
}

fn u64_setting(value: &Value) -> Result<u64, String> {
    let number = integer(value)?;
    u64::try_from(number).map_err(|_| format!("must not be negative, not {number}"))
}

fn socket_addr(value: &Value) -> Result<SocketAddr, String> {
    let text = string(value)?;
    text.parse()
        .map_err(|_| format!("must be an address like \"127.0.0.1:8080\", not {text:?}"))
}

fn origin(value: &Value) -> Result<Origin, String> {
    Origin::parse(string(value)?)
}

fn path(value: &Value) -> Result<PathBuf, String> {
    Ok(PathBuf::from(string(value)?))
}

fn set_role(config: &mut Config, value: &Value) -> Result<(), String> {
    let text = string(value)?;
    config.role = Role::parse(text)
        .ok_or_else(|| format!("must be one of hub, server, both, not {text:?}"))?;
    Ok(())
}

fn set_backend(config: &mut Config, value: &Value) -> Result<(), String> {
    let text = string(value)?;
    config.backend = Backend::ALL
        .into_iter()
        .find(|backend| backend.as_str() == text)
        .ok_or_else(|| format!("must be stub or docker, not {text:?}"))?;
    Ok(())
}

fn set_control_listen(config: &mut Config, value: &Value) -> Result<(), String> {
    config.control_listen = socket_addr(value)?;
    Ok(())
}

fn set_preview_listen(config: &mut Config, value: &Value) -> Result<(), String> {
    config.preview_listen = socket_addr(value)?;
    Ok(())
}

fn set_control_origin(config: &mut Config, value: &Value) -> Result<(), String> {
    config.control_origin = origin(value)?;
    Ok(())
}

fn set_preview_origin(config: &mut Config, value: &Value) -> Result<(), String> {
    config.preview_origin = origin(value)?;
    Ok(())
}

fn set_allowed_hosts(config: &mut Config, value: &Value) -> Result<(), String> {
    config.allowed_hosts = strings(value)?;
    Ok(())
}

fn set_owner(config: &mut Config, value: &Value) -> Result<(), String> {
    config.owner = Some(string(value)?.to_string());
    Ok(())
}

/// What this machine calls itself (HS-074): the `host` a notification's payload carries, and the name the hub's
/// registry row for it has to use, because the tap's path is `/hosts/<host>/sessions/<id>` one hop later.
///
/// The default is [`hosts::LOCAL`], which is not a name any *row* may take: it is the reserved word that means "the
/// machine whose page this is", so a single-machine deployment needs no key at all and a phone's tap opens
/// `/sessions/<id>` on the machine that sent it. A multi-machine deployment sets the name here and names the same
/// machine in the hub's `hosts.toml`; a mismatch is visible as a `404 unknown_host` when the notification is tapped
/// (recorded as a follow-up, not guessed at boot).
fn set_machine_name(config: &mut Config, value: &Value) -> Result<(), String> {
    let name = string(value)?;
    if let Some(problem) = crate::hosts::machine_name_problem(name) {
        return Err(problem);
    }
    config.machine_name = name.to_string();
    Ok(())
}

fn set_data_root(config: &mut Config, value: &Value) -> Result<(), String> {
    config.data_root = path(value)?;
    Ok(())
}

fn set_projects_root(config: &mut Config, value: &Value) -> Result<(), String> {
    config.projects_root = path(value)?;
    Ok(())
}

/// The image a session's container runs (HS-027). A name Docker resolves, so an empty string is not a
/// typo this can catch — it is one Docker refuses with its own sentence, which is the honest place for it.
fn set_agent_image(config: &mut Config, value: &Value) -> Result<(), String> {
    let image = string(value)?;
    if image.trim().is_empty() {
        return Err("must name an image, not an empty string".to_string());
    }
    config.agent_image = image.to_string();
    Ok(())
}

/// The packages the agent image installs (HS-048). An empty list is a legitimate machine — an image with git,
/// openssl and CA certificates and no toolchain — so only a blank *name* is refused: `apt-get install ""` is a
/// build that fails on a line naming nothing, and a build is the wrong place to discover a typo.
fn set_agent_toolchains(config: &mut Config, value: &Value) -> Result<(), String> {
    let toolchains = strings(value)?;
    if let Some(blank) = toolchains.iter().find(|name| name.trim().is_empty()) {
        return Err(format!("must name a package, not {blank:?}"));
    }
    config.agent_toolchains = toolchains;
    Ok(())
}

/// A limit of zero is not a limit: Docker reads `--cpus 0` and `--memory 0m` as "unlimited", which is the
/// opposite of what the key is for, so each of the three refuses it.
fn positive(value: &Value, what: &str) -> Result<u64, String> {
    let number = value
        .as_integer()
        .ok_or_else(|| "expected an integer".to_string())?;
    if number < 1 {
        return Err(format!("must be at least 1 {what}, not {number}"));
    }
    Ok(number as u64)
}

fn set_preview_upstream_port(config: &mut Config, value: &Value) -> Result<(), String> {
    let port = positive(value, "port")?;
    config.preview_upstream_port = u16::try_from(port)
        .map_err(|_| format!("must be a TCP port between 1 and 65535, not {port}"))?;
    Ok(())
}

fn set_container_cpus(config: &mut Config, value: &Value) -> Result<(), String> {
    config.container_cpus = u32::try_from(positive(value, "cpu")?)
        .map_err(|_| "must fit in a 32-bit count of CPUs".to_string())?;
    Ok(())
}

fn set_container_memory_mb(config: &mut Config, value: &Value) -> Result<(), String> {
    config.container_memory_mb = positive(value, "MiB")?;
    Ok(())
}

fn set_container_pids(config: &mut Config, value: &Value) -> Result<(), String> {
    config.container_pids = positive(value, "process")?;
    Ok(())
}

fn set_docker_host(config: &mut Config, value: &Value) -> Result<(), String> {
    config.docker_host = Some(string(value)?.to_string());
    Ok(())
}

fn set_hosts_seed(config: &mut Config, value: &Value) -> Result<(), String> {
    config.hosts_seed = Some(path(value)?);
    Ok(())
}

fn set_allowed_hubs(config: &mut Config, value: &Value) -> Result<(), String> {
    config.allowed_hubs = strings(value)?;
    Ok(())
}

fn set_network_default(config: &mut Config, value: &Value) -> Result<(), String> {
    config.network_default = value
        .as_bool()
        .ok_or_else(|| "expected a boolean".to_string())?;
    Ok(())
}

fn set_max_live_sessions(config: &mut Config, value: &Value) -> Result<(), String> {
    config.max_live_sessions = u32_setting(value)?;
    Ok(())
}

fn set_project_backup(config: &mut Config, value: &Value) -> Result<(), String> {
    config.project_backup = value
        .as_bool()
        .ok_or_else(|| "expected a boolean".to_string())?;
    Ok(())
}

fn set_turn_timeout_min(config: &mut Config, value: &Value) -> Result<(), String> {
    config.turn_timeout_min = u64_setting(value)?;
    Ok(())
}

fn set_idle_stop_min(config: &mut Config, value: &Value) -> Result<(), String> {
    config.idle_stop_min = u64_setting(value)?;
    Ok(())
}

fn set_max_session_tokens(config: &mut Config, value: &Value) -> Result<(), String> {
    config.max_session_tokens = Some(u64_setting(value)?);
    Ok(())
}

fn set_abort_grace(config: &mut Config, value: &Value) -> Result<(), String> {
    config.abort_grace = Duration::from_str(string(value)?)?;
    Ok(())
}

fn set_min_free_gb(config: &mut Config, value: &Value) -> Result<(), String> {
    config.min_free_gb = u64_setting(value)?;
    Ok(())
}

fn set_session_disk_soft_gb(config: &mut Config, value: &Value) -> Result<(), String> {
    config.session_disk_soft_gb = u64_setting(value)?;
    Ok(())
}

fn set_stopped_ttl_days(config: &mut Config, value: &Value) -> Result<(), String> {
    config.stopped_ttl_days = u64_setting(value)?;
    Ok(())
}

fn set_history_window(config: &mut Config, value: &Value) -> Result<(), String> {
    config.history_window = u32_setting(value)?;
    Ok(())
}

fn set_merge_targets(config: &mut Config, value: &Value) -> Result<(), String> {
    config.merge.targets = strings(value)?;
    Ok(())
}

fn set_publish_remotes(config: &mut Config, value: &Value) -> Result<(), String> {
    config.publish.remotes = strings(value)?;
    Ok(())
}

fn set_egress_allow(config: &mut Config, value: &Value) -> Result<(), String> {
    config.egress.allow = strings(value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// The key paths and value shapes `Config` serialises to, walked to leaves.
    fn struct_keys() -> BTreeMap<String, String> {
        let resolved = serde_json::to_value(Config::default())
            .expect("a Config holds only values JSON can carry");
        let mut keys = BTreeMap::new();
        walk_json(&resolved, "", &mut keys);
        keys
    }

    fn walk_json(value: &serde_json::Value, path: &str, keys: &mut BTreeMap<String, String>) {
        match value {
            serde_json::Value::Object(fields) => {
                for (name, field) in fields {
                    let path = if path.is_empty() {
                        name.clone()
                    } else {
                        format!("{path}.{name}")
                    };
                    walk_json(field, &path, keys);
                }
            }
            // An unset optional is a key with no value, not a missing key.
            serde_json::Value::Null => {
                keys.insert(path.to_string(), "unset".to_string());
            }
            other => {
                keys.insert(path.to_string(), json_shape(other).to_string());
            }
        }
    }

    /// The shape a JSON value has, named the way [`Kind`] names its TOML shape.
    fn json_shape(value: &serde_json::Value) -> &'static str {
        match value {
            serde_json::Value::String(_) => "string",
            serde_json::Value::Number(_) => "integer",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Array(_) => "array of strings",
            serde_json::Value::Object(_) | serde_json::Value::Null => "not a value",
        }
    }

    /// The shape [`KNOWN_KEYS`] says a key is written in.
    fn table_shape(kind: Kind) -> &'static str {
        match kind {
            Kind::String | Kind::Duration => "string",
            Kind::Integer => "integer",
            Kind::Boolean => "boolean",
            Kind::StringArray => "array of strings",
        }
    }

    #[test]
    fn the_table_and_the_struct_agree_in_both_directions() {
        let struct_keys = struct_keys();
        let table_keys: BTreeMap<String, String> = KNOWN_KEYS
            .iter()
            .map(|spec| (spec.path.to_string(), table_shape(spec.kind).to_string()))
            .collect();

        let missing_row: Vec<&String> = struct_keys
            .keys()
            .filter(|path| !table_keys.contains_key(*path))
            .collect();
        assert!(
            missing_row.is_empty(),
            "these Config fields have no KeySpec row: {missing_row:?}"
        );

        let missing_field: Vec<&String> = table_keys
            .keys()
            .filter(|path| !struct_keys.contains_key(*path))
            .collect();
        assert!(
            missing_field.is_empty(),
            "these KeySpec rows name no Config field: {missing_field:?}"
        );

        for (path, table_shape) in &table_keys {
            let struct_shape = &struct_keys[path];
            assert!(
                struct_shape == table_shape || struct_shape == "unset",
                "{path} is a {struct_shape} in Config but a {table_shape} in the table"
            );
        }
    }

    #[test]
    fn every_section_a_row_names_is_reachable_by_that_name() {
        for spec in KNOWN_KEYS {
            let Some(section) = spec.section else {
                continue;
            };
            assert!(
                is_section(section),
                "{} names the section {section}, which no row resolves",
                spec.path
            );
            assert!(
                spec.path.starts_with(&format!("{section}.")),
                "{} is outside the section it names",
                spec.path
            );
        }
    }

    #[test]
    fn the_table_holds_the_documented_keys() {
        assert_eq!(KNOWN_KEYS.len(), 34);
    }

    #[test]
    fn a_setter_refuses_a_shape_the_walk_would_have_caught() {
        let mut config = Config::default();
        let integer = Value::from(3);
        let string = Value::from("three");
        assert_eq!(
            set_role(&mut config, &integer),
            Err("expected a string".to_string())
        );
        assert_eq!(
            set_max_live_sessions(&mut config, &string),
            Err("expected an integer".to_string())
        );
        assert_eq!(
            set_allowed_hosts(&mut config, &string),
            Err("expected an array of strings".to_string())
        );
        assert_eq!(
            set_network_default(&mut config, &string),
            Err("expected a boolean".to_string())
        );
        assert_eq!(config, Config::default(), "a refused value changes nothing");
    }
}
