//! `holosmith uninstall`: the inverse of `server deploy`'s install, keeping the data (, spec's Commands
//! table).
//!
//! Four things are removed, in this order, and each is a **read** followed by an **apply** so that a second run is
//! honestly `nothing` rather than a re-application (the order `server deploy` uses, ADR-0056):
//!
//! 1. **The unit**, at [`Root::unit`] — and it is two arms, because the file a tree names and the file the account's
//!    own manager loads are two different files whenever `--root` or `--config` moved the tree. The account's own unit
//!    ([`Tools::manager_unit`]) is retired with four children: `systemctl --user disable --now holosmith.service`
//!    while the file is still there (disable reads its `[Install]` section to know which `WantedBy` link to unlink),
//!    the file, `systemctl --user daemon-reload`, and `loginctl disable-linger`. Any other tree's unit is a file and
//!    nothing else: `systemctl --user` resolves the **name** [`UNIT_NAME`] in that manager's own search path, so a
//!    child there would reach the account's unit rather than this one. The unit goes first because it is what would
//!    restart anything.
//! 2. **The proxies**: `docker rm --force hs_egress hs_key hs_mcp`.
//! 3. **The machine's network**: `docker network rm egress`. After the containers, because a network with an
//!    endpoint refuses to be removed (the rule `docker::remove_network` already follows).
//! 4. **The data**: kept, unless `--purge`.
//!
//! # Not a deploy, and not its words
//!
//! ADR-0056's five words (`satisfied`, `needs`, `changed`, `skipped`, `failed`) are a **deploy** contract: they
//! describe a step that read a state and applied a difference. A teardown's states are different questions — was
//! there one, and is it gone — so this command owns three words of its own, `removed`, `kept` and `nothing`, plus
//! `failed` for the one arm every report has. Reusing the deploy's words would have been a second user of a shape
//! nobody documented for it, and the first thing they would lie about is `nothing`: a deploy has no word for "there
//! was nothing to do because nothing was ever installed", which is exactly a teardown's common case.
//!
//! # What it keeps, and why that is the interesting half
//!
//! The data root (`app.db`, every session's `session.log`, `secrets/keys.toml`, `secrets/vapid.toml`), the
//! projects and their worktrees and branches, the two images — and **the sessions' own containers and networks**.
//! That last one is a decision rather than an omission: a session's `agents_<id>` network and `s_<id>` container
//! belong to the row that describes them, so a boot's reconcile owns them (ADR-0053), and a plain `uninstall` that
//! killed a live session would destroy a turn it was never asked about. The `data: kept:` line says so in words.
//!
//! `--purge` is where they go: the rows that name them are being deleted, so their names are read from the rows
//! (`s_<id>`, `agents_<id>`) and removed before the data root. Never a branch, never a worktree, never an image:
//! a purge that removed the operator's projects would be a delete of something it does not own.
//!
//! # The refusal
//!
//! `--purge` with live sessions is refused **before any child runs**, because a run that removed the unit and then
//! discovered it could not purge would leave the machine half uninstalled — and the operator with no server and no
//! explanation. The count and the `--force` that overrides it are one sentence, and `--force` requires `--purge` at
//! the command line because a `--force` with nothing to force is a flag nobody meant.
//!
//! # The root, and the privilege
//!
//! [`crate::deploy::Root`] is the same value the deploy installs into: one directory, owned by the account that runs
//! the daemon — the operator's own (ADR-0088). Its unit is that account's **user** unit, at [`Root::unit`], which is
//! why this command needs no privilege at all: `systemctl --user`, `rm` and `loginctl` are children of a run made by
//! the account that owns the file, the manager and the linger flag. There is no root half to print and no line
//! carries `sudo` — a machine installed this way has nothing left for superuser to do.
//!
//! **Owning the tree is not owning the manager.** `--root` and `--config` compose a tree, so [`Root::unit`] is
//! whatever path those flags make; `systemctl --user` takes a **name** and looks it up in the manager's own search
//! path, which is `<home>/.config/systemd/user` for the account that runs it and nothing a flag can move. The manager
//! is therefore this run's to command only when the tree's unit *is* the file that manager loads
//! ([`Tools::manager_unit`], and [`unit`] is where the two arms are); a tree under `target/` or `/tmp` is a file to
//! remove and nothing more.

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::config::Config;
use crate::container::{network_listing_prefix, network_name};
use crate::db;
use crate::deploy::{Root, UNIT_NAME};
use crate::docker::{self, Docker};
use crate::sessions::Status;

/// How long one daemon call may take. A teardown is not on a startup path, so the bound is generous; it exists for
/// the wedged daemon, not for a slow one.
pub const BOUND: Duration = Duration::from_secs(10);

/// The word a teardown's line carries instead of a deploy's five (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    /// The read found it and the apply removed it.
    Removed,
    /// The read found it and the command was told to leave it.
    Kept,
    /// The read found nothing to remove.
    Nothing,
    /// The read or the apply failed; the run stops here.
    Failed,
}

impl Word {
    /// The word the report prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::Kept => "kept",
            Self::Nothing => "nothing",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one line of the report is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    /// The systemd unit.
    Unit,
    /// `hs_egress`, `hs_key` and `hs_mcp`.
    Proxies,
    /// The machine's `egress` network.
    Network,
    /// The data root.
    Data,
}

impl What {
    /// The word the line starts with after `uninstall:`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Proxies => "proxies",
            Self::Network => "network",
            Self::Data => "data",
        }
    }
}

impl fmt::Display for What {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One line: what, which word, and the sentence an operator acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub what: What,
    pub word: Word,
    pub sentence: String,
}

impl fmt::Display for Line {
    /// `uninstall: unit: removed: …` — the shape a gate greps and a reader diffs between two runs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "uninstall: {}: {}: {}",
            self.what, self.word, self.sentence
        )
    }
}

/// What one uninstall did, as the operator reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<Line>,
    /// [`Root::describe`]: which tree this run tore down.
    pub root: String,
}

impl Report {
    /// Whether a step failed, which is exactly what the exit code says.
    pub fn failed(&self) -> bool {
        self.lines.iter().any(|line| line.word == Word::Failed)
    }

    /// The whole report as the operator reads it: the header line, then one line per thing.
    pub fn render(&self) -> String {
        let mut text = format!("uninstall: from {}\n", self.root);
        for line in &self.lines {
            // A `String` cannot fail to be written to, which is why this is not a `?`.
            let _ = writeln!(text, "{line}");
        }
        text
    }
}

/// The daemon questions a teardown asks, as one parameter.
///
/// Not [`crate::proxies::Containers`]: that seam is the proxy **recipe's**, and its four questions are the ones a
/// start needs. A teardown needs "is a container by this name there" (running or not — a stopped `hs_egress` still
/// holds the name a later boot would collide with), "remove it", "which networks are there" and "remove one".
/// [`Docker`] implements both, which is the point: one handle, one socket, no second client.
pub trait Machines {
    /// Whether a container by this name exists, running or stopped.
    fn exists(&self, name: &str) -> bool;

    /// `docker rm --force`: whether it happened. An absent container answers `false`, which a caller must read as
    /// "not there" only after asking [`Machines::exists`].
    fn remove_container(&self, name: &str) -> bool;

    /// Every network whose name matches `filter`, under `bound`.
    fn network_names(&self, filter: &str, bound: Duration) -> Result<Vec<String>, String>;

    /// `docker network rm`: whether it happened.
    fn remove_network(&self, name: &str) -> bool;
}

impl Machines for Docker {
    fn exists(&self, name: &str) -> bool {
        Docker::container_exists(self, name)
    }

    fn remove_container(&self, name: &str) -> bool {
        Docker::remove_container(self, name)
    }

    fn network_names(&self, filter: &str, bound: Duration) -> Result<Vec<String>, String> {
        Docker::network_names(self, filter, bound).map_err(|error| error.to_string())
    }

    fn remove_network(&self, name: &str) -> bool {
        Docker::remove_network(self, name)
    }
}

/// The programs a teardown runs, as a seam — the same shape and the same reason as [`crate::deploy::Tools`].
///
/// Three programs, because retiring the operator's own user unit is three of them: `systemctl` disables it and
/// reloads the manager that read it, `rm` removes the file, and `loginctl` turns linger off so the account stops
/// starting a user manager at the next boot. Without a privilege there is no fourth, and Docker's own work is
/// [`Machines`]' rather than a program's. The seam is also where a **machine fact** can be handed in —
/// [`Tools::manager_unit`], which decides whether that manager is this run's to command — because the struct is
/// everything a test or the gate has to supply and this machine refuses to.
#[derive(Debug, Clone)]
pub struct Tools {
    /// `rm`: how a file and a directory tree go.
    pub rm: PathBuf,
    /// `systemctl`: `--user disable --now` before the file goes, and `--user daemon-reload` after.
    pub systemctl: PathBuf,
    /// `loginctl`: `disable-linger`, so an uninstalled account starts no user manager at boot.
    pub loginctl: PathBuf,
    /// The unit the account's own user manager resolves: the file `systemctl --user` reaches, whatever tree this run
    /// was pointed at.
    ///
    /// It decides the one question [`unit`] asks about the manager — whether the manager is this run's to command —
    /// because `systemctl --user` looks [`UNIT_NAME`] up in the manager's **search path** and never at [`Root::unit`].
    /// It is a machine fact rather than a program, and [`Root::resolve`] derives it here rather than at the call site
    /// so there is one derivation of "the account's own unit": a tree's unit equal to this one is the manager's own
    /// file, and anything else is a file no manager ever reads.
    pub manager_unit: PathBuf,
    /// How long each daemon question and removal may take; [`BOUND`] on a machine.
    pub bound: Duration,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            rm: PathBuf::from("rm"),
            systemctl: PathBuf::from("systemctl"),
            loginctl: PathBuf::from("loginctl"),
            manager_unit: Root::resolve(None, None).unit(),
            bound: BOUND,
        }
    }
}

/// What the command line asked for: `--purge` and its override.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Args {
    pub purge: bool,
    /// `--force` over a live session's refusal; the command line makes it require `--purge`.
    pub force: bool,
}

/// Tear the install down and answer the report.
///
/// `config` is the value [`Root::config`] produced, so `config.data_root` is the root's own path — [`crate::deploy::run`]'s
/// contract and this function's too.
///
/// **`Err` is a refusal, not a failure**: `--purge` over live sessions, or a database the refusal could not be
/// counted in, stops the command before any child runs and is printed as one sentence — the shape the deploy's own
/// refusals established (ADR-0055's resume line, ADR-0056's steps). The difference matters to the caller: a refusal
/// carries no step lines and no resume command, because running the same command again would be refused the same way.
/// Since the deploy has no refusal of its own (a role is not a reason to refuse a deploy), so this is now the
/// only command in the build whose `Err` is one.
///
/// A failed *step* stops the run inside the report, for the deploy's reason: later steps would describe a machine
/// this run could no longer promise anything about. Re-running is the recovery, because every step reads first.
pub fn run(
    config: &Config,
    root: &Root,
    tools: &Tools,
    machine: &dyn Machines,
    args: &Args,
) -> Result<Report, String> {
    let mut report = Report {
        lines: Vec::new(),
        root: root.describe(),
    };
    if args.purge && !args.force {
        // Nothing is removed before this answer, which is the whole point of asking first: a refused purge must
        // leave a working machine rather than a half-uninstalled one.
        let live = live_sessions(config)?;
        if live > 0 {
            return Err(format!(
                "{live} {} live ({}): `--purge` would remove the data root they are described in, and nothing has \
                 been removed — stop them, or pass `--force` to purge over them",
                if live == 1 {
                    "session is"
                } else {
                    "sessions are"
                },
                live_words()
            ));
        }
    }
    for line in [
        unit(root, tools),
        proxies(config, machine),
        network(machine, tools),
        data(config, tools, machine, args),
    ] {
        let failed = line.word == Word::Failed;
        report.lines.push(line);
        if failed {
            break;
        }
    }
    Ok(report)
}

/// The three statuses the purge refusal counts, as the words the repository counts them by.
fn live_words() -> String {
    [Status::Starting, Status::Idle, Status::Running]
        .map(Status::as_str)
        .join(", ")
}

/// How many sessions are live, read without migrating the database (ADR-0047) — the same three statuses the cap
/// counts and `live_count` answers with.
///
/// A machine with no database has no sessions, and a machine whose database cannot be read is a refusal rather than
/// a zero: purging over rows nobody could count is the one mistake this check exists to prevent.
fn live_sessions(config: &Config) -> Result<u64, String> {
    let path = config.data_root.join(db::DATABASE_FILE);
    let reader = db::Reader::open(&path).map_err(|error| {
        format!(
            "the database at {} could not be opened to be read ({error}), so the live sessions `--purge` refuses \
             over could not be counted — nothing has been removed",
            path.display()
        )
    })?;
    let Some(reader) = reader else {
        return Ok(0);
    };
    reader.sessions().live_count().map_err(|error| {
        format!(
            "the live sessions in {} could not be counted ({error}) — nothing has been removed",
            path.display()
        )
    })
}

/// Step 1: the unit.
///
/// The deliverable is [`Root::unit`], the operator's own **user** unit beside the config (ADR-0088) — but only one
/// tree's such file is a unit the manager loads, so there are two arms, and [`Tools::manager_unit`] is the predicate.
///
/// **The account's own unit** (`path == tools.manager_unit`) is retired by the account that owns it — four children,
/// in the one order that works:
///
/// 1. `systemctl --user disable --now` **while the file is still there**: disable reads the unit's `[Install]`
///    section to know which `WantedBy` link to unlink, so a file removed first leaves the link behind and a manager
///    that starts the unit again at the next login.
/// 2. `rm -f` of the file, the deliverable itself.
/// 3. `systemctl --user daemon-reload`, so the manager forgets a unit whose file is gone.
/// 4. `loginctl disable-linger`, so a removed install does not start again at the next boot.
///
/// The linger question is asked by turning it **off**, not by reading `loginctl show-user`: `disable-linger` is
/// idempotent and measured to exit `0` when linger is already off, so one child that cannot be wrong beats a parse
/// plus a second bus question that can fail.
///
/// **Any other tree's unit** is a file and nothing else. `systemctl --user` takes the **name** [`UNIT_NAME`] and
/// resolves it in the manager's search path, so a child here would not touch this tree's file at all: it would
/// disable the *account's* unit — a service this run was never asked about — and `loginctl disable-linger` would
/// turn the invoking account's own linger off. Nothing is disabled and linger is left as it is, and the sentence
/// says exactly that, in place of any claim about what starts at the next boot.
///
/// The read is the file's existence in both arms, and it gates the whole step: a machine with no file here has
/// nothing to disable and nothing left behind, which is what keeps a second run `nothing` instead of children whose
/// answer changes nothing. A child that exits non-zero is the step's `Failed`, because a report that said `removed`
/// over a refused `disable` would be describing a unit that still starts — and the sentence names the command and
/// the line it said. A context with no user bus to ask (`cron`, a container, a login that never got
/// `XDG_RUNTIME_DIR`) is that arm rather than a quiet skip: the file is not removed and the run stops there, and
/// the sentence says a login session is where a re-run succeeds.
fn unit(root: &Root, tools: &Tools) -> Line {
    let path = root.unit();
    if !path.is_file() {
        return Line {
            what: What::Unit,
            word: Word::Nothing,
            sentence: format!(
                "there was no file at {}, so nothing has installed one here or this run already removed it",
                path.display()
            ),
        };
    }
    if path != tools.manager_unit {
        if let Err(sentence) = remove(path.as_path(), false, tools) {
            return Line {
                what: What::Unit,
                word: Word::Failed,
                sentence: format!(
                    "{} is this tree's unit and not the one the account's own manager loads ({}), so nothing was \
                     disabled and linger is unchanged: {sentence}",
                    path.display(),
                    tools.manager_unit.display()
                ),
            };
        }
        return Line {
            what: What::Unit,
            word: Word::Removed,
            sentence: format!(
                "{} is gone, and nothing was disabled and linger is unchanged: this tree's unit is not the one the \
                 account's own manager loads ({}), the only file `systemctl --user {}` resolves",
                path.display(),
                tools.manager_unit.display(),
                UNIT_NAME
            ),
        };
    }
    let disable = ["--user", "disable", "--now", UNIT_NAME];
    if let Err(reason) = refusal(&tools.systemctl, &disable) {
        return Line {
            what: What::Unit,
            word: Word::Failed,
            sentence: format!(
                "`{} {}` failed ({reason}), so nothing after it ran: the unit was not disabled and {} is still \
                 there — this step asks the account's user bus, which a `cron` job or a container may not have, and \
                 re-running from a login session is the recovery",
                tools.systemctl.display(),
                disable.join(" "),
                path.display()
            ),
        };
    }
    if let Err(sentence) = remove(path.as_path(), false, tools) {
        return Line {
            what: What::Unit,
            word: Word::Failed,
            sentence: format!(
                "the unit is disabled but still on disk, and `{} --user daemon-reload` and `{} disable-linger` did \
                 not run: {sentence}",
                tools.systemctl.display(),
                tools.loginctl.display()
            ),
        };
    }
    if let Err(reason) = refusal(&tools.systemctl, &["--user", "daemon-reload"]) {
        return Line {
            what: What::Unit,
            word: Word::Failed,
            sentence: format!(
                "{} is gone, and `{} --user daemon-reload` failed ({reason}): the manager still holds the unit it read \
                 from it, and `{} disable-linger` did not run",
                path.display(),
                tools.systemctl.display(),
                tools.loginctl.display()
            ),
        };
    }
    if let Err(reason) = refusal(&tools.loginctl, &["disable-linger"]) {
        return Line {
            what: What::Unit,
            word: Word::Failed,
            sentence: format!(
                "{} is gone and the manager reloaded, and `{} disable-linger` failed ({reason}): this account still \
                 starts a user manager at the next boot",
                path.display(),
                tools.loginctl.display()
            ),
        };
    }
    Line {
        what: What::Unit,
        word: Word::Removed,
        sentence: format!(
            "{} is gone, and the unit is disabled, forgotten by the manager and set not to start at the next boot",
            path.display()
        ),
    }
}

/// One child's own answer: `Ok` when it exited zero, and the first line of its stderr — or its exit code, when it
/// said nothing — when it did not.
///
/// The reason is the child's rather than a verdict of this command's, because "the manager could not be reached" and
/// "the unit is not loaded" are different facts that only its own words separate, and the caller's sentence is what
/// names the command it came from.
fn refusal(program: &Path, arguments: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(first_line(&String::from_utf8_lossy(&output.stderr))
        .unwrap_or_else(|| format!("exit {:?}", output.status.code())))
}

/// The first non-blank line of some text.
fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// Remove a file or a directory tree with `rm`: one child per write, as the owner of the root — which is what makes
/// `--purge` an unprivileged command (ADR-0086).
fn remove(path: &Path, recursive: bool, tools: &Tools) -> Result<(), String> {
    let mut arguments: Vec<String> = vec![if recursive { "-rf" } else { "-f" }.to_string()];
    arguments.push(path.display().to_string());
    let output = Command::new(&tools.rm)
        .args(&arguments)
        .output()
        .map_err(|error| {
            format!(
                "`{} {}` could not be run ({error})",
                tools.rm.display(),
                arguments.join(" ")
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "`{} {}` failed ({})",
        tools.rm.display(),
        arguments.join(" "),
        first_line(&String::from_utf8_lossy(&output.stderr))
            .unwrap_or_else(|| format!("exit {:?}", output.status.code()))
    ))
}

/// Step 2: the proxies.
///
/// The read is **existence**, not `running`: `--restart unless-stopped` keeps a stopped proxy's name and its
/// definition, so a teardown that removed only running ones would leave containers behind that a later boot has to
/// work around.
fn proxies(config: &Config, machine: &dyn Machines) -> Line {
    // The names this run's config composes (`hs_egress`/`hs_key`/`hs_mcp`, or a smoke run's `smoke_hs_egress`/
    // `smoke_hs_key`/`smoke_hs_mcp`) — the same three [`crate::proxies::names`] answers, so a teardown and a boot never
    // disagree about which containers are "the proxies".
    let names = crate::proxies::names(config);
    let existing: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| machine.exists(name))
        .collect();
    if existing.is_empty() {
        return Line {
            what: What::Proxies,
            word: Word::Nothing,
            sentence: format!("none of {} is on this machine", names.join(", ")),
        };
    }
    for name in &existing {
        if !machine.remove_container(name) {
            return Line {
                what: What::Proxies,
                word: Word::Failed,
                sentence: format!(
                    "{name} is here and `docker rm --force {name}` refused: check the daemon, then run this \
                     command again (every step reads before it applies)"
                ),
            };
        }
    }
    Line {
        what: What::Proxies,
        word: Word::Removed,
        sentence: format!(
            "{} removed with `docker rm --force`",
            existing.join(" and ")
        ),
    }
}

/// Step 3: the machine's `egress` network.
///
/// After the containers, and the read is the daemon's list matched **exactly** — `docker`'s own name filter is a
/// substring match, so `egress` would also answer `my-egress` (`proxies_line`'s rule, ADR-0056).
fn network(machine: &dyn Machines, tools: &Tools) -> Line {
    let names = match machine.network_names(docker::EGRESS_NETWORK, tools.bound) {
        Ok(names) => names,
        Err(sentence) => {
            return Line {
                what: What::Network,
                word: Word::Failed,
                sentence: format!(
                    "the daemon could not list its networks ({sentence}): this teardown removes the machine's \
                     network, so it stops rather than reporting a machine it could not look at"
                ),
            };
        }
    };
    if !names.iter().any(|name| name == docker::EGRESS_NETWORK) {
        return Line {
            what: What::Network,
            word: Word::Nothing,
            sentence: format!("the {} network is not there", docker::EGRESS_NETWORK),
        };
    }
    if !machine.remove_network(docker::EGRESS_NETWORK) {
        return Line {
            what: What::Network,
            word: Word::Failed,
            sentence: format!(
                "`docker network rm {}` refused: a container is still attached to it — `docker network inspect {}` \
                 names it",
                docker::EGRESS_NETWORK,
                docker::EGRESS_NETWORK
            ),
        };
    }
    Line {
        what: What::Network,
        word: Word::Removed,
        sentence: format!("the {} network is gone", docker::EGRESS_NETWORK),
    }
}

/// Step 4: the data root — kept, or purged with the sessions' own containers and networks.
///
/// The purge reads the rows **before** it deletes them, because the rows are what name `s_<id>` and `agents_<id>`;
/// a name it cannot derive is not removed, and a daemon that refuses is counted in the sentence rather than failing
/// the run: the data root is the deliverable, and a leftover container is not a reason to keep a database.
fn data(config: &Config, tools: &Tools, machine: &dyn Machines, args: &Args) -> Line {
    let root = &config.data_root;
    if !args.purge {
        return Line {
            what: What::Data,
            word: Word::Kept,
            sentence: format!(
                "{} holds the database, every session's log and secrets/ (the provider keys and the VAPID pair), \
                 and the sessions' own containers and networks stay with them — `--purge` removes the data root",
                root.display()
            ),
        };
    }
    let swept = match db::Reader::open(&root.join(db::DATABASE_FILE)) {
        Ok(Some(reader)) => match reader.sessions().list(None) {
            Ok(rows) => sweep(config, machine, &rows, tools),
            Err(error) => Swept {
                containers: 0,
                networks: 0,
                refused: vec![format!("the sessions could not be listed ({error})")],
            },
        },
        // No database: nothing names a container or a network, and the tree still goes.
        Ok(None) => Swept::default(),
        Err(error) => Swept {
            containers: 0,
            networks: 0,
            refused: vec![format!("the database could not be read ({error})")],
        },
    };
    if !root.exists() {
        return Line {
            what: What::Data,
            word: Word::Nothing,
            sentence: format!(
                "{} is not there, so there was no data to remove",
                root.display()
            ),
        };
    }
    if let Err(sentence) = remove(root, true, tools) {
        // The sweep ran **before** this, so a failure here cannot claim nothing was removed: the containers and
        // networks are gone, and a line that said otherwise would be the report contradicting itself one line later.
        return Line {
            what: What::Data,
            word: Word::Failed,
            sentence: format!(
                "{sentence}; the sessions' own names went before it: {} container{} and {} network{} removed{}",
                swept.containers,
                if swept.containers == 1 { "" } else { "s" },
                swept.networks,
                if swept.networks == 1 { "" } else { "s" },
                if swept.refused.is_empty() {
                    String::new()
                } else {
                    format!(
                        ", and {} the daemon would not remove",
                        swept.refused.join(", ")
                    )
                }
            ),
        };
    }
    let mut sentence = format!(
        "{} is gone (the database, every session's log and secrets/), with {} session container{} and {} session \
         network{}",
        root.display(),
        swept.containers,
        if swept.containers == 1 { "" } else { "s" },
        swept.networks,
        if swept.networks == 1 { "" } else { "s" }
    );
    if !swept.refused.is_empty() {
        sentence.push_str(&format!(
            "; what the daemon would not remove stayed: {}",
            swept.refused.join("; ")
        ));
    }
    sentence.push_str("; the projects, their worktrees and their branches are untouched");
    Line {
        what: What::Data,
        word: Word::Removed,
        sentence,
    }
}

/// What a purge's sweep of the rows' own names did, and what it could not do.
#[derive(Debug, Default)]
struct Swept {
    containers: usize,
    networks: usize,
    refused: Vec<String>,
}

/// Remove the containers and networks the rows name: `s_<id>` then `agents_<id>`.
///
/// A container first, for [`network`]'s reason — a network with an endpoint refuses to be removed — and only names
/// this build derives from a row's id, so nothing an operator named can be swept by accident.
fn sweep(
    config: &Config,
    machine: &dyn Machines,
    rows: &[crate::sessions::Session],
    tools: &Tools,
) -> Swept {
    let mut swept = Swept::default();
    // Two compositions, deliberately named differently: a **name** is the raw prefix plus the build's own
    // (`container_name`/`network_name`), and a **listing filter** is the composed one the daemon is asked for.
    // Composing twice asks for `s_s_<id>`, which is what `tests/uninstall.rs`'s
    // `a_forced_purge_removes_the_data_and_the_rows_own_names` caught on this story's first pass.
    let container_prefix = &config.container_prefix;
    let network_prefix = &config.network_prefix;
    for row in rows {
        let container = crate::container::container_name(container_prefix, &row.id);
        if machine.exists(&container) {
            if machine.remove_container(&container) {
                swept.containers += 1;
            } else {
                swept.refused.push(container);
            }
        }
    }
    // The **network** prefix, not the container's: `agents_` happens to contain `s_`, so a listing filtered by
    // `s_` would answer both by accident, and a rule that is right by accident is one edit away from being wrong.
    // A daemon that cannot list is recorded as a refusal rather than read as "no session networks" — the count in
    // the line is a measurement or it is a sentence saying what was not measured.
    let names = match machine.network_names(&network_listing_prefix(network_prefix), tools.bound) {
        Ok(names) => names,
        Err(sentence) => {
            swept.refused.push(format!(
                "the daemon could not list its networks ({sentence})"
            ));
            Vec::new()
        }
    };
    for row in rows {
        let wanted = network_name(network_prefix, &row.id);
        if names.contains(&wanted) {
            if machine.remove_network(&wanted) {
                swept.networks += 1;
            } else {
                swept.refused.push(wanted);
            }
        }
    }
    swept
}
