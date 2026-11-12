//! The startup reconcile: what a crash left behind, put right before anybody is served (HS-044, V9).
//!
//! A `kill -9` of the server takes three things with it that nothing else in this build can put back:
//!
//! - **the attached `docker run`** whose stdio *is* the agent (ADR-0039), so the container keeps running
//!   with nobody talking to it and no `ExitWatcher` records its exit;
//! - **the row's word** for what was happening, which stays `running` (or `starting`) as if a turn or a
//!   provision were still in flight;
//! - **nothing else**, which is the good news: session data lives in host directories, `sessions.last_seq`
//!   is the durable counter (ADR-0009/ADR-0013) and an event a client saw is an event a crash cannot undo.
//!
//! So the reconcile is one pass over three lists, in this order (spec item 6's second bullet and item 7's
//! fifth step):
//!
//! 1. **The rows.** Every row that was live when the process died — `starting`, `idle`, `running` — is moved
//!    to `starting` through [`Transitioner::apply`] (the table's `crash_recovery` row) and then re-provisioned,
//!    because a resume *is* a provision (`ensure` is create-or-adopt, ADR-0029). A row that was `running`
//!    carries [`INTERRUPTED`] as its reason: spec item 8's "a running turn is recorded as `interrupted`",
//!    which is a reason on a `session.status` event and never a state. A `starting` row needs no transition
//!    (it is already where a provision begins); `stopped`, `error` and `deleting` are left alone — the last
//!    one because a delete is the operator's request to retry (ADR-0023) — **except** a row a drain stopped with
//!    [`crate::drain::SERVER_RESTART`], which is the second kind of row this pass brings back (HS-078, spec item
//!    10 step 6: "sessions that were live before the drain return to `idle` on startup").
//! 2. **The names no row owns.** A container `s_<id>` is removed unless `<id>` is a `deleting` row (every
//!    other container is stale or about to be replaced, and a stranded pi must stop before a new one starts
//!    on the same worktree); an `agents_<id>` network and a `.worktrees/<id>` directory are removed only when
//!    **no row** has that id. **Never a branch, never a ref** (HS-043, ADR-0052): the worktree's directory and
//!    its registration go, the branch and `refs/holosmith/wip/<id>` stay.
//! 3. **The restarts.** After the sweep, so no new container races a stale one.
//!
//! The pass is awaited **before the listener answers anything and before either policy starts** (`serve`
//! calls it right after the state exists): a sweep that ran concurrently with a create would see a row it did
//! not know about and take the worktree that row's provision had just made, and the idle-stop sweeper's
//! first pass would otherwise be free to stop a row this pass is about to bring back. The restarts it starts
//! are provisioner tasks, so provisioning stays off the serving path. The row half runs for every backend and
//! the name half only for Docker: a `running` row is a lie about the process whichever backend wrote it, while
//! containers, networks and worktrees exist only in a Docker run (ADR-0015) — and a stub boot must not touch
//! `projects_root`.
//!
//! **Idempotent in [`crate::worktree`]'s sense, not in the sense of doing nothing:** a second pass leaves the
//! same names and the same idle rows, but it restarts the live sessions again. That is what a reconcile is,
//! and it is why HS-045's `doctor` and HS-046's verification run may call it as well.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::backend::Backends;
use crate::config::Config;
use crate::container::{container_listing_prefix, network_listing_prefix};
use crate::db::Database;
use crate::docker::Docker;
use crate::events::EventWriter;
use crate::provisioner::Provisioner;
use crate::runtime::Backend;
use crate::sessions::state::{Transitioner, Trigger};
use crate::sessions::{Session, Status};
use crate::worktree::{self, Retained, WORKTREES, Worktree};

/// The reason a row that was `running` carries after the restart (spec item 8).
///
/// A *reason*, not a state: no row is ever in a state called `interrupted` (the state machine's own test
/// refuses the word), and the page renders it as `idle · interrupted` through `stateText`.
pub const INTERRUPTED: &str = "interrupted";

/// How long one daemon question may take on the startup path.
///
/// The pass is awaited before the listener answers, so it *is* the startup path: a daemon that answered the
/// preflight and then wedged costs this bound per listing and then a `warn` — a slow boot, never a refused one.
pub const BOUND: Duration = Duration::from_secs(10);

/// One row the reconcile brings back, and the state it held when the process died.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovery {
    pub id: String,
    /// `Starting`, `Idle` or `Running` for a row that was live, `Stopped` for one a drain stopped.
    pub from: Status,
}

impl Recovery {
    /// Whether a turn was in flight when the process died.
    pub fn interrupted(&self) -> bool {
        self.from == Status::Running
    }

    /// The table row that gets this one to `starting`.
    ///
    /// Two kinds of row share everything after the transition — a provision — and differ in the word that brings them
    /// back: a row that was live needs `crash_recovery` (which admits exactly those three states), while one a drain
    /// stopped is a *resume* (spec item 10 step 6). Choosing it here rather than at each of the pass's two loops is
    /// what keeps the transition and the restart from disagreeing about which rows are being brought back.
    pub fn trigger(&self) -> Trigger {
        match self.from {
            Status::Stopped => Trigger::Resume,
            _ => Trigger::CrashRecovery,
        }
    }
}

/// The rows the reconcile brings back, in id order so a pass is repeatable.
///
/// Two kinds of row, and both pass through the same rest of the pass:
///
/// * **The rows that were live when the process died** — "live" is the cap's own word (spec item 7's step 4):
///   `starting`, `idle` and `running`, and the table's `crash_recovery` row admits exactly them. A `running` row
///   carries [`INTERRUPTED`].
/// * **The rows a drain stopped** — `stopped` with [`crate::drain::SERVER_RESTART`] as its reason. The drain stops
///   them *because the server is going down* and the boot is what knows it is back, which is spec item 10 step 6 in
///   as many words: "sessions that were live before the drain return to `idle` on startup". The reason word is the
///   whole test — the sweeper's, the cap's, the quota's, the expiry's and the operator's stops carry their own — so
///   nothing else that is `stopped` comes back.
///
/// A `stopped` row that is not a drain's, an `error` row and a `deleting` row are left alone: the last one because a
/// delete is the operator's request to retry (ADR-0023).
pub fn recoveries(rows: &[Session]) -> Vec<Recovery> {
    let mut recover: Vec<Recovery> = rows
        .iter()
        .filter(|row| {
            matches!(
                row.status,
                Status::Starting | Status::Idle | Status::Running
            ) || (row.status == Status::Stopped
                && row.status_reason.as_deref() == Some(crate::drain::SERVER_RESTART))
        })
        .map(|row| Recovery {
            id: row.id.clone(),
            from: row.status,
        })
        .collect();
    recover.sort_by(|left, right| left.id.cmp(&right.id));
    recover
}

/// The containers no row owns — plus the ones a live row is about to replace.
///
/// A `deleting` row is the one exception: its container is the delete's to remove, and a reconcile that took
/// it would be finishing somebody's teardown for them (ADR-0023).
///
/// `prefix` is the **composed** one (`container_prefix` + `s_`, HS-077): a smoke boot passes `smoke_s_`, so its
/// sweep can neither see nor take the machine's `s_<id>` containers, and the machine's own boot cannot see its.
fn containers_to_remove(rows: &[Session], names: &[String], prefix: &str) -> Vec<String> {
    names
        .iter()
        .filter(|name| match session_id_of(name, prefix) {
            Some(id) if crate::sessions::is_id(id) => {
                status_of(rows, id).is_none_or(|status| status != Status::Deleting)
            }
            // Not the build's name: `hs_egress`, `hs_key`, and anything an operator named.
            _ => false,
        })
        .cloned()
        .collect()
}

/// The networks **no row** has: a live row's network is reused by its restart (and holds the proxies'
/// endpoints), a stopped row's is what its resume reuses, and a `deleting` row's belongs to its delete.
fn networks_to_remove(rows: &[Session], names: &[String], prefix: &str) -> Vec<String> {
    names
        .iter()
        .filter(|name| match session_id_of(name, prefix) {
            Some(id) if crate::sessions::is_id(id) => status_of(rows, id).is_none(),
            _ => false,
        })
        .cloned()
        .collect()
}

/// The worktree directories **no row** has, under the one directory every project shares.
///
/// `<projects>/.worktrees/<id>` is the naming rule (`Worktree::of`), so the reader is the directory listing
/// and the rule is the row set — not the reverse: a directory the build cannot name is not the build's.
pub fn orphan_worktrees(projects_root: &Path, rows: &[Session]) -> Vec<String> {
    let root = projects_root.join(WORKTREES);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut orphans: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| crate::sessions::is_id(name) && status_of(rows, name).is_none())
        .collect();
    orphans.sort();
    orphans
}

/// What one pass over the projects root removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// The directories that are gone, in the order they were taken.
    pub directories: Vec<PathBuf>,
    /// The directories that could not be taken, with git's or the filesystem's sentence.
    pub failures: Vec<(PathBuf, String)>,
}

/// Remove the worktree directories no row owns, and **never a branch or a ref** (HS-043, ADR-0052).
///
/// Two shapes, because the repository is what decides which one applies:
///
/// - **registered**: git has an administrative entry for the path, so `worktree remove --force` takes both the
///   directory and the entry. [`Worktree::remove`] is told [`Retained::Unknown`], which keeps the branch —
///   "cannot tell" must never mean "delete", and the same is true of "there is no row to ask".
/// - **unregistered**: nothing registered it, so nothing needs cleaning up but the files. A crashed start can
///   leave the directory before git ever heard of it.
///
/// A project directory that is not a repository is skipped rather than reported: a directory under the
/// projects root that git cannot read has no worktrees to own.
pub fn sweep_worktrees(projects_root: &Path, orphans: &[String]) -> Swept {
    let mut swept = Swept::default();
    if orphans.is_empty() {
        return swept;
    }
    let root = projects_root.join(WORKTREES);
    if let Ok(entries) = std::fs::read_dir(projects_root) {
        for entry in entries.flatten() {
            let project = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_dir())
                || entry.file_name().to_str() == Some(WORKTREES)
                || !project.join(".git").exists()
            {
                continue;
            }
            let registrations = match worktree::registrations(&project) {
                Ok(registrations) => registrations,
                Err(_) => continue,
            };
            for path in registrations {
                let Some(id) = worktree_id(&path, &root) else {
                    continue;
                };
                if !orphans.iter().any(|orphan| orphan == &id) {
                    continue;
                }
                let Some(name) = project.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let worktree = Worktree::of(projects_root, name, &id);
                match worktree.remove(&Retained::Unknown) {
                    Ok(()) => swept.directories.push(worktree.path),
                    Err(error) => swept.failures.push((worktree.path, error.to_string())),
                }
            }
        }
    }
    // Whatever is still there was registered by nothing: the directory goes, and no branch with it.
    for id in orphans {
        let directory = root.join(id);
        if !directory.exists() {
            continue;
        }
        match std::fs::remove_dir_all(&directory) {
            Ok(()) => {
                if !swept.directories.contains(&directory) {
                    swept.directories.push(directory);
                }
            }
            Err(error) => swept.failures.push((directory, error.to_string())),
        }
    }
    swept
}

/// The id a registered worktree path names, when it sits directly under `<projects>/.worktrees/`.
///
/// Both sides are canonicalised because `git worktree list --porcelain` prints the **resolved** path while
/// the projects root may be reached through a symlink (HS-025's measurement).
fn worktree_id(path: &Path, root: &Path) -> Option<String> {
    let root = std::fs::canonicalize(root).ok()?;
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    if parent != root {
        return None;
    }
    path.file_name()?.to_str().map(str::to_string)
}

/// The session id `name` carries when it starts with `prefix`, without deciding whether it is a real id.
fn session_id_of<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix)
}

/// The row `id` names, when there is one.
fn status_of(rows: &[Session], id: &str) -> Option<Status> {
    rows.iter().find(|row| row.id == id).map(|row| row.status)
}

/// The reconcile, wired to this run's handles.
///
/// A value rather than free functions for [`crate::compactor`]'s reason: it is a task with handles, `serve`
/// builds it once, and a test can `run` it without racing a spawn.
#[derive(Debug, Clone)]
pub struct Reconciler {
    database: Arc<Mutex<Database>>,
    events: EventWriter,
    backend: Backends,
    docker: Docker,
    projects_root: PathBuf,
    /// `container_prefix` + `s_`: the prefix this run's own containers carry (HS-077), and the prefix its sweep
    /// strips. Composed once here, because the two halves must be the same string — a sweep that listed with one and
    /// stripped with another would either miss its own leftovers or take somebody else's container.
    container_prefix: String,
    /// `network_prefix` + `agents_`, composed the same way for the networks.
    network_prefix: String,
    /// Which backend this run uses: the row half is every backend's, the name half Docker's.
    kind: Backend,
}

impl Reconciler {
    /// The two prefixes this run **lists and strips** with: `container_prefix` + `s_`, `network_prefix` + `agents_`.
    ///
    /// A function of the config rather than two initialisers inside [`Reconciler::new`], so a test can ask what a
    /// smoke run's reconciler will sweep without a database — the wiring a unit test of the composer cannot see
    /// (HS-077's review, S4).
    pub fn prefixes(config: &Config) -> (String, String) {
        (
            container_listing_prefix(&config.container_prefix),
            network_listing_prefix(&config.network_prefix),
        )
    }

    pub fn new(
        database: Arc<Mutex<Database>>,
        events: EventWriter,
        backend: Backends,
        config: &Config,
        kind: Backend,
    ) -> Self {
        let (container_prefix, network_prefix) = Self::prefixes(config);
        Self {
            database,
            events,
            backend,
            docker: Docker::new(config.docker_host.clone()),
            projects_root: config.projects_root.clone(),
            container_prefix,
            network_prefix,
            kind,
        }
    }

    /// One pass: the rows, then the names, then the restarts (the restarts are tasks of their own).
    ///
    /// **Awaited by `serve` before the first request is answered**, not spawned: a sweep that runs beside a
    /// create can take the worktree that create just made, because the sweep's row snapshot predates the row
    /// (this story's demo found it). The pass itself is bounded daemon questions and one `git` per project;
    /// the provisioning it starts is background work like any other.
    pub async fn run(&self) {
        let rows = match self.rows() {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::reconcile",
                    %error,
                    "the session rows could not be read, so nothing was reconciled"
                );
                return;
            }
        };
        // The transitions come first, before the sweep and before anything is restarted: every row this pass
        // brings back is `starting` before the sweeper's first tick can call it idle, and a row cannot be
        // re-provisioned while something else is still allowed to stop it.
        for recovery in recoveries(&rows) {
            if recovery.from == Status::Starting {
                continue;
            }
            let reason = recovery.interrupted().then_some(INTERRUPTED);
            if let Err(error) = self
                .transitioner()
                .apply(&recovery.id, recovery.trigger(), reason)
                .await
            {
                tracing::warn!(
                    target: "holosmith::reconcile",
                    session = %recovery.id,
                    from = recovery.from.as_str(),
                    %error,
                    "a row this pass brings back could not be moved to starting"
                );
            }
        }
        if self.kind == Backend::Docker {
            self.sweep(&rows).await;
        }
        // The restarts, after the names: the container step removes its own leftover before it runs, but a
        // sweep first means the stranded pi is gone before a new one mounts the same worktree.
        for recovery in recoveries(&rows) {
            let Some(session) = rows.iter().find(|row| row.id == recovery.id).cloned() else {
                continue;
            };
            tracing::info!(
                target: "holosmith::reconcile",
                session = %session.id,
                from = recovery.from.as_str(),
                "bringing back a session this pass holds: it was live when the process died, or a drain stopped it"
            );
            self.provisioner()
                .with_settle_reason(recovery.interrupted().then_some(INTERRUPTED))
                .spawn(session);
        }
    }

    /// Remove what no row owns: containers, then networks, then worktree directories.
    async fn sweep(&self, rows: &[Session]) {
        let containers = match self.docker.container_names(&self.container_prefix, BOUND) {
            Ok(names) => containers_to_remove(rows, &names, &self.container_prefix),
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::reconcile",
                    %error,
                    "the daemon would not list containers, so no container was reconciled"
                );
                Vec::new()
            }
        };
        for name in containers {
            if self.docker.remove_container_bounded(&name, BOUND) {
                tracing::info!(
                    target: "holosmith::reconcile",
                    container = %name,
                    "removed a container the crash left behind"
                );
            } else {
                tracing::warn!(
                    target: "holosmith::reconcile",
                    container = %name,
                    "a leftover container could not be removed"
                );
            }
        }
        let networks = match self.docker.network_names(&self.network_prefix, BOUND) {
            Ok(names) => networks_to_remove(rows, &names, &self.network_prefix),
            Err(error) => {
                tracing::warn!(
                    target: "holosmith::reconcile",
                    %error,
                    "the daemon would not list networks, so no network was reconciled"
                );
                Vec::new()
            }
        };
        for name in networks {
            if self.docker.remove_network_bounded(&name, BOUND) {
                tracing::info!(
                    target: "holosmith::reconcile",
                    network = %name,
                    "removed a network no session row owns"
                );
            } else {
                tracing::warn!(
                    target: "holosmith::reconcile",
                    network = %name,
                    "a network no row owns could not be removed"
                );
            }
        }
        let orphans = orphan_worktrees(&self.projects_root, rows);
        if orphans.is_empty() {
            return;
        }
        let root = self.projects_root.clone();
        let swept = tokio::task::spawn_blocking(move || sweep_worktrees(&root, &orphans)).await;
        match swept {
            Ok(swept) => {
                for directory in swept.directories {
                    tracing::info!(
                        target: "holosmith::reconcile",
                        directory = %directory.display(),
                        "removed a worktree directory no session row owns, and kept its branch"
                    );
                }
                for (directory, sentence) in swept.failures {
                    tracing::warn!(
                        target: "holosmith::reconcile",
                        directory = %directory.display(),
                        sentence = %sentence,
                        "a worktree directory no row owns could not be removed"
                    );
                }
            }
            Err(error) => tracing::warn!(
                target: "holosmith::reconcile",
                %error,
                "the worktree sweep did not finish"
            ),
        }
    }

    /// Every row, live or not: the reconcile decides, the repository reads.
    fn rows(&self) -> Result<Vec<Session>, crate::sessions::RepoError> {
        let database = self.database.lock().unwrap_or_else(PoisonError::into_inner);
        database.sessions().list(None)
    }

    /// The one state function, wired to this run's database and writer.
    fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }

    /// A provisioner for a restart, wired to this run's backend, database and writer.
    fn provisioner(&self) -> Provisioner {
        Provisioner::new(
            self.backend.clone(),
            Arc::clone(&self.database),
            self.events.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{NAME_PREFIX, NETWORK_PREFIX};

    /// A row with only the fields the rules read, built the way the repository builds one.
    fn row(id: &str, status: Status) -> Session {
        Session {
            id: id.to_string(),
            project: "holosmith".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            thinking: "high".to_string(),
            worktree: String::new(),
            base_commit: None,
            status,
            status_reason: None,
            client_key: None,
            network: true,
            created_at: 0,
            last_active_at: 0,
            stopped_at: None,
            usage_in: 0,
            usage_out: 0,
        }
    }

    const LIVE: &str = "0123456789abcdef";
    const IDLE: &str = "fedcba9876543210";

    #[test]
    fn the_live_rows_are_the_ones_that_come_back_and_running_is_the_interrupted_one() {
        let rows = vec![
            row(LIVE, Status::Running),
            row(IDLE, Status::Idle),
            row("1111111111111111", Status::Starting),
            row("2222222222222222", Status::Stopped),
            row("3333333333333333", Status::Error),
            row("4444444444444444", Status::Deleting),
        ];
        // In id order, so a pass is repeatable however the repository happened to answer.
        assert_eq!(
            recoveries(&rows),
            vec![
                Recovery {
                    id: LIVE.to_string(),
                    from: Status::Running
                },
                Recovery {
                    id: "1111111111111111".to_string(),
                    from: Status::Starting
                },
                Recovery {
                    id: IDLE.to_string(),
                    from: Status::Idle
                },
            ]
        );
        // Only a turn that was in flight is an interruption; a `starting` row never had one.
        assert!(recoveries(&rows)[0].interrupted());
        assert!(!recoveries(&rows)[1].interrupted());
    }

    #[test]
    fn a_container_goes_unless_its_row_is_a_delete_in_progress() {
        let rows = vec![
            row(LIVE, Status::Running),
            row(IDLE, Status::Stopped),
            row("4444444444444444", Status::Deleting),
        ];
        let names = vec![
            // A live row's container is replaced by the restart, so it goes too.
            format!("{NAME_PREFIX}{LIVE}"),
            // A stopped row's is stale.
            format!("{NAME_PREFIX}{IDLE}"),
            // A row that does not exist is the orphan the sweep is for.
            format!("{NAME_PREFIX}9999999999999999"),
            // A delete's container is the delete's to remove.
            format!("{NAME_PREFIX}4444444444444444"),
            // Not the build's names: no prefix, or an id that is not a session id.
            "hs_egress".to_string(),
            "hs_key".to_string(),
            "agents_0123456789abcdef".to_string(),
            format!("{NAME_PREFIX}not-a-session-id"),
            format!("{NAME_PREFIX}0123456789ABCDEF"),
        ];
        assert_eq!(
            containers_to_remove(&rows, &names, NAME_PREFIX),
            vec![
                format!("{NAME_PREFIX}{LIVE}"),
                format!("{NAME_PREFIX}{IDLE}"),
                format!("{NAME_PREFIX}9999999999999999"),
            ]
        );
    }

    #[test]
    fn a_network_goes_only_when_no_row_has_its_id() {
        let rows = vec![
            row(LIVE, Status::Running),
            row(IDLE, Status::Stopped),
            row("4444444444444444", Status::Deleting),
        ];
        let names = vec![
            format!("{NETWORK_PREFIX}{LIVE}"),
            format!("{NETWORK_PREFIX}{IDLE}"),
            format!("{NETWORK_PREFIX}4444444444444444"),
            format!("{NETWORK_PREFIX}9999999999999999"),
            // The machine's own networks are not session networks.
            "egress".to_string(),
            "bridge".to_string(),
            format!("{NETWORK_PREFIX}not-a-session-id"),
        ];
        assert_eq!(
            networks_to_remove(&rows, &names, NETWORK_PREFIX),
            vec![format!("{NETWORK_PREFIX}9999999999999999")]
        );
    }

    #[test]
    fn a_second_pass_over_the_names_the_first_removed_has_nothing_left_to_take() {
        let rows = vec![row(LIVE, Status::Idle)];
        let container = format!("{NAME_PREFIX}9999999999999999");
        let network = format!("{NETWORK_PREFIX}9999999999999999");
        assert_eq!(
            containers_to_remove(&rows, std::slice::from_ref(&container), NAME_PREFIX),
            vec![container.clone()]
        );
        assert_eq!(
            networks_to_remove(&rows, std::slice::from_ref(&network), NETWORK_PREFIX),
            vec![network.clone()]
        );
        // The pass that follows the removals sees only what it kept — no name to take, and the same rows to
        // bring back. That is what "safe to run twice" means here: not a no-op (live sessions are restarted
        // again), but nothing new to remove.
        assert!(containers_to_remove(&rows, &[], NAME_PREFIX).is_empty());
        assert!(networks_to_remove(&rows, &[], NETWORK_PREFIX).is_empty());
        assert_eq!(recoveries(&rows).len(), 1);
    }

    /// The wiring, not the composer: a smoke config's reconciler lists and strips `smoke_s_`/`smoke_agents_`, and a
    /// default config's is the build's own two prefixes. Mutating `Reconciler::prefixes` to hand back the raw
    /// config values — the mistake that would leave a smoke boot unable to sweep its own leftovers — fails here.
    #[test]
    fn a_smoke_configs_reconciler_sweeps_only_its_own_names() {
        let mut config = Config::default();
        assert_eq!(
            Reconciler::prefixes(&config),
            (NAME_PREFIX.to_string(), NETWORK_PREFIX.to_string())
        );
        config.container_prefix = "smoke_".to_string();
        config.network_prefix = "smoke_".to_string();
        assert_eq!(
            Reconciler::prefixes(&config),
            ("smoke_s_".to_string(), "smoke_agents_".to_string())
        );
    }

    /// HS-077's other half: a sweep with a **composed** prefix takes only its own run's names.
    ///
    /// The empty prefix is today's behaviour (the two tests above) and this one is the smoke run's: a boot whose
    /// config says `smoke_` lists `smoke_s_`, strips `smoke_s_`, and must neither see the machine's `s_<id>` nor its
    /// proxies. The four names in each list are exactly the four a real `docker ps` answers with both runs on the
    /// machine, because `name=` is a substring filter.
    #[test]
    fn a_prefixed_sweep_takes_its_own_names_and_not_the_machines() {
        let rows: Vec<Session> = Vec::new();
        let smoke = container_listing_prefix("smoke_");
        assert_eq!(smoke, "smoke_s_");
        let names = vec![
            format!("{smoke}{LIVE}"),
            format!("{NAME_PREFIX}{LIVE}"),
            "hs_egress".to_string(),
            "smoke_hs_egress".to_string(),
        ];
        assert_eq!(
            containers_to_remove(&rows, &names, &smoke),
            vec![format!("{smoke}{LIVE}")],
            "a smoke boot took a name that is not its own: {names:?}"
        );
        let networks = vec![
            format!("{}{LIVE}", network_listing_prefix("smoke_")),
            format!("{NETWORK_PREFIX}{LIVE}"),
            "egress".to_string(),
        ];
        assert_eq!(
            networks_to_remove(&rows, &networks, &network_listing_prefix("smoke_")),
            vec![format!("smoke_agents_{LIVE}")]
        );
    }

    #[test]
    fn a_name_that_is_not_the_convention_is_not_the_builds() {
        let rows: Vec<Session> = Vec::new();
        for name in [
            "_s_0123456789abcdef",
            "S_0123456789abcdef",
            "s-0123456789abcdef",
        ] {
            assert!(
                containers_to_remove(&rows, &[name.to_string()], NAME_PREFIX).is_empty(),
                "{name} was taken for a session's container"
            );
        }
    }
}
