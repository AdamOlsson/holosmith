//! The health endpoint: the four keys of the story, and the one endpoint no role check
//! applies to (ADR-0007).

use axum::Json;
use serde::Serialize;

use crate::config::{Config, Role};
use crate::runtime::Runtime;

/// The name the health payload reports.
pub const NAME: &str = "holosmith";

/// Exactly four keys, in this order. `target` and the rest of the build metadata stay on the
/// startup line and in `version --json`: the host registration of item 4 reads name and
/// version, and a key nobody reads is a promise about a shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Health {
    pub name: &'static str,
    pub version: String,
    pub role: Role,
    pub uptime_s: u64,
}

impl Health {
    /// What `GET /api/health` reports for this config and this run.
    pub fn of(config: &Config, runtime: &Runtime) -> Self {
        Self {
            name: NAME,
            version: env!("CARGO_PKG_VERSION").to_string(),
            role: config.role,
            uptime_s: runtime.uptime_s(),
        }
    }
}

/// The handler's body: the payload as JSON.
pub fn respond(health: &Health) -> Json<Health> {
    Json(health.clone())
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
    fn the_payload_is_exactly_four_keys_in_order() {
        assert_eq!(
            serde_json::to_string(&health(Role::Hub)).expect("four fields always serialise"),
            format!(
                r#"{{"name":"holosmith","version":"{}","role":"hub","uptime_s":0}}"#,
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
