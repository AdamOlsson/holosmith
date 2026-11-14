//! `holosmith server deploy` and `holosmith hub deploy`: install one half of a machine, in the spec's six steps
//!
//! The command is one idempotent pass over item 10 §"What `server deploy` does": each step **reads the machine's
//! current state** and then applies only what the read found missing, so a second run changes nothing and reports
//! it (ADR-0056). That pair is why a step cannot be one function that decides and acts — a dry run has to print
//! the read's plan without running the apply, and "changed" has to be distinguishable from "was already right".
//!
//! Every step prints one line, `<n>. <step>: <word>: <sentence>`: [`DeployStep`]'s number is the spec's order and
//! [`Word`] is the read's answer, one of five. A step that prints a list — a plan, or the apply that did it — puts
//! one action per line under that line, with the count on the line itself (ADR-0085). The two words that carry the
//! design are:
//!
//! - **`skipped` is never `satisfied`.** A step that prints "already satisfied" over a check that read nothing is a
//!   lie of exactly the kind this list exists to prevent, which is why the word is in [`Word`] at all. Since
//!   its producer is a **role**: a [`Kind::Hub`] run builds no images and starts no proxies, and says so on both
//!   lines rather than reporting a pass over work it never did.
//! - **The first `failed` step stops the run.** Later steps are not evaluated at all: a machine whose preflight
//!   failed has not earned an install, and a report that guessed at the remaining steps would describe a machine
//!   nobody measured. The lines already printed are the describable state the acceptance asks for, and
//!   `commands::deploy` prints the resume command — the command that just ran — as the last line.
//!
//! Four things are deliberately narrow:
//!
//! - **This is the local deploy.** `--ssh`, `--binary` and the remote copy live in [`crate::remote`]; nothing here
//!   reads a remote.
//! - **A run installs one half of a machine.** [`Kind`] says which, the union of that half with the role the
//!   config already carries is what step 2 writes, and the two halves together are `both` (the spec's Roles
//!   table). Nothing is refused over a role: `server deploy` on a hub and `hub deploy` on a server are both
//!   ordinary runs.
//! - **Step 1 is [`crate::doctor`]'s list, not a second one.** `serve`'s refusal, `doctor`'s line and this step's
//!   failure carry the same sentences (ADR-0028, ADR-0054). The one divergence is `ports`, downgraded from a
//!   failure to a remark, and its doc comment says why.
//! - **The proxies' recipe lives in [`crate::proxies`].** A deploy and a boot must start the same two containers
//!   the same way (ADR-0056), and this module is the caller that fails the step instead of warning.
//!
//! # The root and the config, and why nothing here escalates
//!
//! [`Root`] is *where* the machine lives: `HOLOSMITH_ROOT`, `--root`, or [`crate::config::default_root`] — one
//! directory holding the data root, the release tree and the projects — and *which file* describes it,
//! `--config` or `~/.config/holosmith.toml` (ADR-0088). There is no second layout and therefore no prefixing:
//! every state path this module builds is `<root>/…`, whether the root is the machine's or a test's own tree,
//! and the config is the one path that is not under it.
//!
//! **This program never runs `sudo`.** Creating the account and making the root are root's, so the deploy **reads**
//! their state and **prints** the commands — as the tail remark on the step that needs them — and the next run
//! verifies them. The unit is not on that list: it is the operator's own file, written into the directory their user
//! manager reads (ADR-0088), and the two `systemctl --user` commands that enable it are printed beside it. The only
//! hard requirement is that this process can write the root, because the tree is written as its owner. That is what
//! makes `sudo -u holosmith holosmith deploy` the one way to run it on a machine, and what leaves step 6
//! (`/api/health` plus `doctor --full`) as the thing that notices a unit which was enabled and never started.

use std::fmt::{self, Write as _};
use std::io::{ErrorKind, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::config::{self, Config, Role};
use crate::docker;
use crate::doctor::{self, Depth};
use crate::images::{self, Expectation, Kind as ImageKind};
use crate::key;
use crate::preflight::{self, Check, Outcome};
use crate::proxies;
use crate::vapid;

/// The account step 2 reads: the spec's item 10 step 2 "service user", by name.
///
/// The unit does not render it: a user unit runs as its owner, so there is no `User=` (ADR-0088). Step 2's account
/// half still does — it reads the account with `id -u` and prints `useradd`/`usermod` for it — and that half is the
/// move still outstanding. Until it happens this name is also what the uninstall's reminder, the promotion's refusal
/// sentence and the remote note spell.
pub const SERVICE_USER: &str = "holosmith";

/// The group the service user must be in: `serve` drives the machine's daemon through its socket.
pub const SERVICE_GROUP: &str = "docker";

/// The unit file's name, which is also the argument `systemctl` gets.
pub const UNIT_NAME: &str = "holosmith.service";

/// The **user** manager's directory, inside the config's own: `<config dir>/systemd/user`.
///
/// It is the user manager's because the unit is the operator's own account (ADR-0088): a system unit would need root
/// to install, and a user unit that carries no mount or namespace directive is the one sandbox shape that does not
/// hand the daemon a `PrivateUsers=` mapping.
const USER_UNIT_DIR: &str = "systemd/user";

/// Where systemd's system-wide units live: the directory a system unit is installed into for every account.
///
/// [`Root::unit`] is not there: this install's unit is the operator's own, in [`USER_UNIT_DIR`]. This constant is what
/// `uninstall`'s reminder and `promote`'s fallback still name.
pub const UNIT_DIR: &str = "/etc/systemd/system";

/// How long the last step's apply waits for `/api/health` before it calls the unit's start a failure.
pub const HEALTH_WAIT: Duration = Duration::from_secs(60);

/// How often that wait asks again. A serving unit answers in milliseconds; the interval is what a wait costs the
/// command, and 250 ms is four asks a second for a minute.
pub const HEALTH_POLL: Duration = Duration::from_millis(250);

/// One path made absolute, or kept verbatim when the kernel cannot answer.
///
/// The two commands that print a **resume** line need it — `server deploy` and `uninstall` — and a resume that named
/// a relative path would mean a different tree from another directory. One function, because the second copy was
/// byte for byte the first (T9): a helper whose whole job is "the same path" should not exist twice.
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The remark a held port earns instead of failing the deploy — step 1's one divergence (see [`preflight`]).
const HELD_PORT: &str =
    "; the control port is held — a re-run on a serving machine is not a failure here";

/// The remark a wrong mode or owner earns instead of failing the deploy — step 1's **second** divergence, and this
/// one is ADR-0058's.
///
/// `doctor --quick` fails it on purpose: the unit runs `doctor --quick` as `ExecStartPre`, and a secrets directory
/// the service user cannot read is a reason for systemd not to start the service. But `deploy` is the caller that
/// **repairs** it — step 2's `Action::Mode` is exactly this finding's fix — so failing here would make the repair
/// unreachable and leave the operator to run one `chmod` the deploy knows how to run.
const WRONG_MODE: &str = "; a wrong mode or owner in the install tree is what step 2 repairs, so it is a remark here and not a failure";

/// The remark a broken `current` earns instead of failing the deploy — step 1's **third** divergence.
///
/// `doctor --quick` fails it on purpose: the unit runs `doctor --quick` as `ExecStartPre`, and a `current` the unit
/// executes through that resolves to nothing is a boot that cannot happen. But a deploy is one of the two commands
/// that can **repair** it — step 5 renders the unit for the binary this run is running whenever `Release::live` names
/// nothing, so the machine boots again on a plain path — and failing at step 1 would make that repair unreachable,
/// exactly ADR-0058's argument for `Perms` one finding over.
const LINK_NOT_LIVE: &str = "; the unit's `ExecStart` resolves through a `current` that is not live — step 5 rewrites \
                             the unit to the binary this run is running, which is what makes the machine boot again";

/// Which deploy a run is: the server's six steps, or the hub's four and two skips (item 10 §6).
///
/// A deploy installs **one half of a machine**, and the two halves together are `both` — so the type carries three
/// answers and they are deliberately not one: [`Kind::installs`] is the role this run adds, [`union`] is the role
/// the machine ends up with, and [`Kind::group`]/[`Kind::skip`] are the two places a hub's half is not a server's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `holosmith server deploy`: the six steps, a service user in the daemon's group, and both proxies.
    Server,
    /// `holosmith hub deploy`: steps 1, 2, 5 and 6; no image, no proxy, and no daemon group.
    Hub,
}

impl Kind {
    /// The role this run adds to whatever the machine's config already says.
    pub fn installs(self) -> Role {
        match self {
            Self::Server => Role::Server,
            Self::Hub => Role::Hub,
        }
    }

    /// The command's own name, which is also the word a sentence about it uses.
    pub fn word(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Hub => "hub",
        }
    }

    /// The group the service user joins, or `None` for the half that drives no daemon (item 10 §6's "without the
    /// docker group").
    pub fn group(self) -> Option<&'static str> {
        match self {
            Self::Server => Some(SERVICE_GROUP),
            Self::Hub => None,
        }
    }

    /// The reason this run has no work for `step`, or `None` for a step it runs.
    ///
    /// The sentence is the whole point of the line: [`Word::Skipped`] is never a pass, so a reader has to be told
    /// what is being left out and why — and "another story owns it" is not why any more.
    pub fn skip(self, step: DeployStep) -> Option<&'static str> {
        match (self, step) {
            (Self::Hub, DeployStep::Images) => Some(
                "a hub builds no images: `holosmith-proxy` and `agent-image` are what a session's container and the \
                 machine's two proxies run, and a hub runs no containers (spec item 10 §6)",
            ),
            (Self::Hub, DeployStep::Proxies) => Some(
                "a hub starts no proxies: `hs_egress` is a session's only route out and `hs_key` is the only thing \
                 that holds a real provider key, and a hub has neither",
            ),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// The role a machine holds after a deploy of `kind` has run on it: the spec's Roles table as one function.
///
/// `server` then `hub` is `both`, and so is the other order; a machine that is already `both` stays `both`. The
/// union is written into the config step 2 installs, which is why the second deploy does not have to be told what
/// the first one did.
pub fn union(found: Role, kind: Kind) -> Role {
    match (found, kind.installs()) {
        (Role::Both, _) => Role::Both,
        (Role::Server, Role::Server) | (Role::Hub, Role::Hub) => found,
        _ => Role::Both,
    }
}

/// The spec's steps this command runs, in the spec's order (item 10, "What `server deploy` does").
///
/// Six rather than seven: the origins step is Tailscale's, not this machine's, and left the deploy with ADR-0086.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeployStep {
    /// 1. The preflight, as `doctor --quick`.
    Preflight,
    /// 2. The install tree under the root: the config file, the data root, the secrets (with the provider and the
    ///    VAPID keys), and the read of the service user that will own them.
    Files,
    /// 3. The two images embedded in the binary.
    Images,
    /// 4. The `egress` network and the machine's two proxies.
    Proxies,
    /// 5. The systemd unit, rendered into the root.
    Service,
    /// 6. The `/api/health` wait and `doctor --full`.
    Health,
}

impl DeployStep {
    /// Every step, in the order the run evaluates them.
    pub const ALL: [Self; 6] = [
        Self::Preflight,
        Self::Files,
        Self::Images,
        Self::Proxies,
        Self::Service,
        Self::Health,
    ];

    /// The spec's step number, `1..=6`, which is the report line's first field.
    pub fn number(self) -> u8 {
        match self {
            Self::Preflight => 1,
            Self::Files => 2,
            Self::Images => 3,
            Self::Proxies => 4,
            Self::Service => 5,
            Self::Health => 6,
        }
    }

    /// The step's word in a report line: an operator greps these, so the spelling is a contract.
    pub fn word(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Files => "files",
            Self::Images => "images",
            Self::Proxies => "proxies",
            Self::Service => "service",
            Self::Health => "health",
        }
    }
}

impl fmt::Display for DeployStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// What a step's read answered, which is the one word on its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word {
    /// The read found nothing to do.
    Satisfied,
    /// A dry run: what the apply would do, run against the read's answer.
    Needs,
    /// The apply ran and did them.
    Changed,
    /// This run's role has no work for this step, and the line carries the reason. Since its producer is a
    /// [`Kind::Hub`] install — a hub builds no images and starts no proxies — and the word is never a pass: the tally
    /// counts it apart, and a reader is told what was left out rather than told it is already right.
    Skipped,
    /// The read or the apply failed; the run stops here.
    Failed,
}

impl Word {
    /// The word the report prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::Needs => "needs",
            Self::Changed => "changed",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One step's line: the spec's number, the step's word, the read's answer, and the sentence an operator acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub step: DeployStep,
    pub word: Word,
    pub sentence: String,
}

impl fmt::Display for Line {
    /// `2. files: satisfied: all 6 paths are present with their modes: …` — three colon-separated fields after
    /// the number, `Finding`'s shape, so a gate greps `^2. files: satisfied: `.
    ///
    /// A plan's actions are **not** on this line: they follow it, one indented line each (ADR-0085), which is why
    /// `sentence` is a `String` that may hold newlines. The step's own line is the one a gate greps, so it stays a
    /// single line whatever the plan under it is.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}. {}: {}: {}",
            self.step.number(),
            self.step,
            self.word,
            self.sentence
        )
    }
}

/// What one deploy did, as the operator reads it: a block per step evaluated, the step that stopped the run, and
/// whether anything was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<Line>,
    /// The step whose line is `failed`, which is the exit code and the resume command.
    pub failed: Option<DeployStep>,
    pub dry_run: bool,
    /// [`Root::describe`]: which tree this run installed into.
    ///
    /// Carried rather than printed by [`Report::render`], whose shape is a block per step plus the two summary
    /// lines: it is here because "which tree did this report describe" is a question a caller asks (and a test
    /// asserts), and answering it from the config would mean guessing.
    pub root: String,
}

impl Report {
    /// Whether a step failed, which is exactly what the exit code says.
    pub fn failed(&self) -> bool {
        self.failed.is_some()
    }

    /// The whole report as the operator reads it: the header, the root, one block per step and the tally.
    ///
    /// A block is the step's own line — `^N. step: word: ` — and, under a plan or an apply, one indented line per
    /// action (ADR-0085). That first line is what a gate greps, so it is the only line whose shape is a contract.
    ///
    /// The tally's four counts are always printed — `skipped` included, as zero on a run that skipped nothing —
    /// because a gate greps the line's shape and a reader diffs two runs by it (ADR-0056).
    pub fn render(&self) -> String {
        let count = |word: Word| self.lines.iter().filter(|line| line.word == word).count();
        let mut text = format!("deploy: {}\n", if self.dry_run { "dry run" } else { "run" });
        // Which tree this report describes, on its own line: the step lines name paths, and a reader needs to see
        // which root the whole run used before the paths mean anything.
        let _ = writeln!(text, "deploy: into {}", self.root);
        for line in &self.lines {
            // A blank line between blocks, the header's included: a plan is a list of its own lines, and six of
            // those stacked without a gap is the wall of text the list was meant to break up. The tally gets one
            // too, so it reads as the summary rather than as a seventh step.
            text.push('\n');
            // A `String` cannot fail to be written to, which is why this is not a `?`.
            let _ = writeln!(text, "{line}");
        }
        text.push('\n');
        let _ = writeln!(
            text,
            "deploy: {} steps: {} satisfied, {} changed, {} skipped, {} failed",
            self.lines.len(),
            count(Word::Satisfied),
            count(Word::Changed),
            count(Word::Skipped),
            count(Word::Failed)
        );
        text
    }
}

/// Where one machine lives: the config file, the root, and the paths under it.
///
/// The root is `--root`'s or `HOLOSMITH_ROOT`'s, else the account's own state directory
/// ([`config::default_root`]); nothing is prefixed and nothing is conditional, so every state path here is
/// `<root>/…` whether the root is `~/.local/share/holosmith` or `target/deploy-case/tree`.
///
/// The config file is deliberately **not** one of them (ADR-0088): it is the one file in an install a person
/// edits, so it lives at `--config`'s or [`config::config_path`]'s `~/.config/holosmith.toml`, and `--root`
/// moves the state and only the state. The pair is one value because a deploy writes both and a unit names
/// both, so a step that took them separately would be one caller away from mixing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    path: PathBuf,
    config: PathBuf,
}

impl Root {
    /// The root `--root` names, `HOLOSMITH_ROOT`, or the account's state directory; and the config file
    /// `--config` names or the account's own. Both are made absolute, so a resume command that prints them means
    /// the same paths from another directory.
    pub fn resolve(root: Option<PathBuf>, config: Option<PathBuf>) -> Self {
        Self {
            path: absolute(&root.unwrap_or_else(config::root)),
            config: config.map_or_else(config::config_path, |config| absolute(&config)),
        }
    }

    /// The root itself: the directory the data root, the release tree and the projects are under, owned by the
    /// account that runs the daemon.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The config file this run installs and the unit's `ExecStart` names: `--config`'s path, or
    /// `~/.config/holosmith.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config.clone()
    }

    /// `/home/you/.config`: the directory the config file lives in, which a deploy makes when the account has none —
    /// one of the two directories a run creates that are not under the root, the other being [`Self::unit`]'s.
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config.parent().map(Path::to_path_buf)
    }

    /// `<root>/data`: the data root's default.
    pub fn data_root(&self) -> PathBuf {
        self.path.join(config::DATA_DIR)
    }

    /// `<root>/projects`: the projects root's default.
    pub fn projects_root(&self) -> PathBuf {
        self.path.join(config::PROJECTS_DIR)
    }

    /// `<root>/releases`: the release tree [`crate::release::Release::at`] takes.
    pub fn releases(&self) -> PathBuf {
        self.path.join(crate::release::RELEASES)
    }

    /// `<root>/current`: the symlink systemd executes through.
    pub fn current(&self) -> PathBuf {
        self.path.join(crate::release::CURRENT)
    }

    /// `<config dir>/systemd/user/holosmith.service`: the unit this run renders, in the directory systemd's **user**
    /// manager reads.
    ///
    /// The directory is [`USER_UNIT_DIR`] beside [`Self::config_file`], so a machine whose config is
    /// `~/.config/holosmith.toml` gets `~/.config/systemd/user/holosmith.service` — the path a user unit is enabled
    /// from, derived from the account's own home rather than spelled.
    ///
    /// **`--config` is therefore the one flag that moves the unit besides the config**, which is deliberate: the unit
    /// names the config's path in its `ExecStart` and is written by the run that installed that file, so the two are
    /// one deliverable in one tree — and a test or a gate renders a unit naming its own paths without writing the
    /// operator's real one. The consequence is worth naming: a config outside a systemd user-unit tree
    /// (`--config /etc/holosmith.toml`) puts the unit where no user manager loads it, and nothing in step 5 notices —
    /// the printed `systemctl --user enable --now` fails with `Unit … not found` *after* the report has said
    /// `5. service: changed`, and step 6's health wait is what ends up reporting it.
    pub fn unit(&self) -> PathBuf {
        self.unit_dir().join(UNIT_NAME)
    }

    /// `<config dir>/systemd/user`: the directory [`Self::unit`] is in.
    fn unit_dir(&self) -> PathBuf {
        // This arm is the **empty** config path's: every other path has a parent, and a bare file name
        // (`--config holosmith.toml`) has `Some("")`, so that input keeps a relative unit path instead of reaching
        // here. Under `/` the write fails loudly, which is the shape a path that cannot be an install should have.
        self.config_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(USER_UNIT_DIR)
    }

    /// The config this deploy installs from: a clone whose two roots are the root's, so the rendered file names the
    /// machine it was installed on rather than the defaults it may have been read with.
    ///
    /// Only the two roots are moved. Everything else in a config is a fact about the host — the listen addresses, the
    /// docker socket, an image name — and rewriting those would describe a machine that does not exist.
    pub fn config(&self, config: &Config) -> Config {
        Config {
            data_root: self.data_root(),
            projects_root: self.projects_root(),
            ..config.clone()
        }
    }

    /// The phrase [`Report::root`] carries: where this run installs.
    pub fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// The machine questions a deploy asks, as one parameter: the image builds' seam, the proxies' and `doctor`'s.
///
/// [`crate::docker::Docker`] gets this for free through the blanket impl below, which is the point —
/// `commands::deploy` builds
/// one daemon handle and the seams that need it share it. A test implements the component traits and stands in
/// for the whole machine, with no daemon at all (the [`crate::doctor::Machine`] pattern).
pub trait Machine: proxies::Containers + doctor::Machine + images::Images {}

impl<T: proxies::Containers + doctor::Machine + images::Images> Machine for T {}

/// The external programs the deploy runs, as a seam.
///
/// [`doctor::Tools`] is embedded rather than copied, for step 1's reason: the preflight is the same list `serve`
/// refuses on, so it is the same shims. The additions are the three programs the tree's own writes use — `install`
/// and `chmod` because every file action is one child process, `openssl` because the VAPID pair is generated by one
/// — and `id`, which is the **only** question this run asks about root's half of the machine. They are `PathBuf`s
/// for [`crate::preflight::Tools`]'s reason: this crate denies `unsafe` and so cannot set `PATH`, which leaves
/// naming the program as the only way a test can point a write at a script it wrote.
///
/// **This module spawns no escalation** (ADR-0086): what root would do is printed. [`Tools::sudo`] is here for the two
/// release verbs' single restart, and [`Tools::health_wait`] is the one field that is not a program.
#[derive(Debug, Clone)]
pub struct Tools {
    pub doctor: doctor::Tools,
    pub install: PathBuf,
    pub chmod: PathBuf,
    pub openssl: PathBuf,
    /// `id`: the service user's uid and its groups, which is this run's whole read of root's half.
    pub id: PathBuf,
    /// `sudo`: **not used by this module.** The two release verbs run `sudo -n systemctl restart <unit>` through it,
    /// because one restart is the only escalation a machine has (ADR-0086 D4) — and the field is here rather than in
    /// `promote`'s own tools so that a test points both programs at its recorders in one place.
    pub sudo: PathBuf,
    /// `systemctl`: the release verbs' restart, through [`Tools::sudo`].
    pub systemctl: PathBuf,
    /// How long the last step's apply waits for `/api/health`; [`HEALTH_WAIT`] on a machine.
    pub health_wait: Duration,
}

impl Default for Tools {
    /// The programs from `PATH`, which is what a machine has, and the spec's one-minute health deadline.
    fn default() -> Self {
        Self {
            doctor: doctor::Tools::default(),
            install: PathBuf::from("install"),
            chmod: PathBuf::from("chmod"),
            openssl: PathBuf::from("openssl"),
            id: PathBuf::from("id"),
            sudo: PathBuf::from("sudo"),
            systemctl: PathBuf::from("systemctl"),
            health_wait: HEALTH_WAIT,
        }
    }
}

/// Run the six steps in the spec's order and answer the report: a **server** deploy.
///
/// This is [`run_as`] with [`Kind::Server`], which is what the name means: the six steps, the daemon's group and
/// both proxies. `holosmith hub deploy` calls [`run_as`] with its own half.
pub fn run(
    config: &Config,
    root: &Root,
    tools: &Tools,
    machine: &dyn Machine,
    dry_run: bool,
) -> Report {
    run_as(config, root, tools, machine, dry_run, Kind::Server)
}

/// The same run, for either half of a machine.
///
/// Two configs come out of the one the caller handed over, and the split is [`Kind`]'s whole substance:
///
/// - **The installed config** is the machine's own with `kind.union(found)` in its role and the two roots moved
///   under [`Root`]. Step 2 renders it, so the file that boots next says `both` after a `server deploy` and a
///   `hub deploy` — the spec's Roles table, made a property of the install tree instead of a by-hand edit.
/// - **The checked config** carries the half this run installs, and only steps 1 and 6 see it, because they are the
///   two that ask `doctor`. A `hub deploy` therefore asks no Docker and no git question even on a machine that is
///   now `both` — item 10 §6's "without the Docker and git checks", read literally. The cost is recorded: the
///   *other* half's health is that deploy's business, and `serve` on a `both` machine refuses at its own boot when
///   its preflight fails.
pub fn run_as(
    config: &Config,
    root: &Root,
    tools: &Tools,
    machine: &dyn Machine,
    dry_run: bool,
    kind: Kind,
) -> Report {
    let installed = Config {
        role: union(config.role, kind),
        ..root.config(config)
    };
    let checked = Config {
        role: kind.installs(),
        ..installed.clone()
    };
    // The three install-tree paths `doctor` reads are the root's, not the machine's: a run against a test's tree
    // must not read `/var/lib/holosmith`'s unit or release tree. Everything else about the tools — the programs,
    // the systemd directory, the tailscale binary — is a fact about the host and stays as it is.
    let mut doctor_tools = tools.doctor.clone();
    doctor_tools.config_path = root.config_file();
    doctor_tools.release_root = root.path().to_path_buf();
    doctor_tools.unit_path = root.unit();
    let mut report = Report {
        lines: Vec::new(),
        failed: None,
        dry_run,
        root: root.describe(),
    };
    for step in DeployStep::ALL {
        // A step this half has no work for is a `skipped` line and the run continues: the remaining steps are still
        // this run's, and a stop here would leave steps 5–6 of a hub unevaluated for no reason (ADR-0056).
        if let Some(reason) = kind.skip(step) {
            report.lines.push(Line {
                step,
                word: Word::Skipped,
                sentence: reason.to_string(),
            });
            continue;
        }
        let line = match step {
            DeployStep::Preflight => preflight(&checked, &doctor_tools, machine),
            DeployStep::Files => files(&installed, root, tools, dry_run, kind),
            DeployStep::Images => images_line(&installed, machine, dry_run),
            DeployStep::Proxies => proxies_line(&installed, machine, dry_run),
            DeployStep::Service => service(&installed, root, tools, dry_run),
            DeployStep::Health => health(&checked, &doctor_tools, tools, machine, dry_run),
        };
        if line.word == Word::Failed {
            report.failed = Some(step);
        }
        report.lines.push(line);
        if report.failed.is_some() {
            break;
        }
    }
    report
}

/// Step 1: `doctor --quick`'s list, with three findings downgraded.
///
/// Every other failure stops the deploy **with that finding's own sentence, verbatim**, so `serve`'s refusal,
/// `doctor`'s line and this step's failure cannot say different things about a machine (ADR-0028, ADR-0054).
///
/// **`ports`, `perms` and `release` are the three divergences, and all three are recorded rather than discovered.**
/// `doctor
/// --quick` fails a held port on purpose: its two moments are *before a start* and, through `--full`, *after one*.
/// `deploy` is the one caller that legitimately runs on a machine that is already serving, because the spec
/// promises that re-running is safe — so a held port becomes a remark on this line and the run continues, while
/// `doctor`'s own exit code is unchanged. Without that, the second deploy on a working server would report a
/// failure that is not one. `perms` is the same shape one finding later: `serve` refuses on a wrong mode through
/// `ExecStartPre`, but the deploy *repairs* one, so refusing here would make step 2's chmod actions unreachable
/// (ADR-0058). `release` is the third for that same reason, one step later: a `current` that does not resolve is a
/// boot that cannot happen, and step 5 writes the unit for the binary this run is running whenever `Release::live`
/// names nothing — refusing at step 1 would make the repair unreachable too (ADR-0084).
fn preflight(config: &Config, tools: &doctor::Tools, machine: &dyn Machine) -> Line {
    let report = doctor::run(config, Depth::Quick, tools, machine);
    let mut findings = report.findings;
    // `preflight::disk` answers `unknown` for a data root that does not exist yet, which is exactly the **first**
    // deploy's situation — the run the floor is for. So an unknown disk finding is re-asked against the nearest
    // existing ancestor: the same check, an honest subject. The line says which path was measured, because a
    // reader who asked about `/var/lib/holosmith/data` and was told `/` is a different fact from one who was not
    // told.
    let mut elsewhere = None;
    if let Some(index) = findings
        .iter()
        .position(|finding| finding.check == Check::Disk && finding.outcome == Outcome::Unknown)
        && let Some((path, finding)) = disk_of_the_nearest_ancestor(config, tools)
    {
        findings[index] = finding;
        elsewhere = Some(path);
    }
    if let Some(finding) = findings.iter().find(|finding| {
        finding.is_failure()
            && !matches!(finding.check, Check::Ports | Check::Perms | Check::Release)
    }) {
        return Line {
            step: DeployStep::Preflight,
            word: Word::Failed,
            sentence: finding.sentence.clone(),
        };
    }
    let count = |outcome: Outcome| {
        findings
            .iter()
            .filter(|finding| finding.outcome == outcome)
            .count()
    };
    let unknown: Vec<&str> = findings
        .iter()
        .filter(|finding| finding.outcome == Outcome::Unknown)
        .map(|finding| finding.check.as_str())
        .collect();
    let mut sentence = format!(
        "{} findings: {} pass, {} fail, {} unknown",
        findings.len(),
        count(Outcome::Pass),
        count(Outcome::Fail),
        count(Outcome::Unknown)
    );
    if !unknown.is_empty() {
        sentence.push_str(&format!(" ({})", unknown.join(", ")));
    }
    if let Some(path) = &elsewhere {
        sentence.push_str(&format!(
            "; there is no data root yet, so the filesystem that will hold it ({}) is what was measured",
            path.display()
        ));
    }
    if findings
        .iter()
        .any(|finding| finding.is_failure() && finding.check == Check::Ports)
    {
        sentence.push_str(HELD_PORT);
    }
    if findings
        .iter()
        .any(|finding| finding.is_failure() && finding.check == Check::Perms)
    {
        sentence.push_str(WRONG_MODE);
    }
    if findings
        .iter()
        .any(|finding| finding.is_failure() && finding.check == Check::Release)
    {
        sentence.push_str(LINK_NOT_LIVE);
    }
    Line {
        step: DeployStep::Preflight,
        word: Word::Satisfied,
        sentence,
    }
}

/// The disk check asked about the nearest directory that exists, and that path.
///
/// One ancestor at a time so the answer is the filesystem the data root will land on rather than "somewhere"; a
/// path with no existing ancestor at all (`/` always exists, so this is unreachable in practice) answers `None`
/// and the finding stays `unknown`, which is the honest third verdict rather than a guess.
fn disk_of_the_nearest_ancestor(
    config: &Config,
    tools: &doctor::Tools,
) -> Option<(PathBuf, preflight::Finding)> {
    let mut probe = config.data_root.clone();
    while !probe.exists() {
        if !probe.pop() {
            return None;
        }
    }
    let mut asking = config.clone();
    asking.data_root.clone_from(&probe);
    preflight::report_with(&asking, &tools.preflight)
        .into_iter()
        .find(|finding| finding.check == Check::Disk)
        .map(|finding| (probe, finding))
}

/// What `id` answered about the service user, before this run wrote anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ServiceUser {
    /// The uid, when the account exists.
    uid: Option<String>,
    /// Whether the group that owns the daemon's socket has the user.
    grouped: bool,
}

impl ServiceUser {
    /// The commands that would make the account what the unit's `User=` needs, in the order they must run.
    ///
    /// These are **printed**, never run: `useradd` and `usermod` are root's (ADR-0086). The list is empty when the
    /// account is already what the unit names, which is what makes the step's line stop saying anything about it.
    fn commands(&self, root: &Root, group: Option<&str>) -> Vec<String> {
        let mut commands = Vec::new();
        if self.uid.is_none() {
            commands.push(match group {
                Some(group) => format!(
                    "sudo useradd --system --create-home --home-dir {} --shell /usr/sbin/nologin \
                     --groups {group} {SERVICE_USER}",
                    root.path().display()
                ),
                None => format!(
                    "sudo useradd --system --create-home --home-dir {} --shell /usr/sbin/nologin {SERVICE_USER}",
                    root.path().display()
                ),
            });
        } else if let (Some(group), false) = (group, self.grouped) {
            commands.push(format!("sudo usermod -aG {group} {SERVICE_USER}"));
        }
        commands
    }

    /// The clause a step's line carries about the account, or an empty string when there is nothing to say.
    fn remark(&self, group: Option<&str>) -> String {
        match (self.uid.is_some(), group) {
            (true, Some(group)) if self.grouped => {
                format!("; the service user {SERVICE_USER} is in the {group} group")
            }
            (true, None) => format!(
                "; the service user {SERVICE_USER} exists, and this run joined it to no group because a hub \
                 drives no daemon"
            ),
            _ => String::new(),
        }
    }
}

/// Step 2: the install tree — the root, four paths inside the data root, and the two files the machine owns.
///
/// The three files are treated differently on purpose, and that difference is the security property of this step:
/// the config is the deploy's own rendering and is rewritten when it differs; `keys.toml` holds every stored
/// provider key and is **never** written over — created when absent and mode-repaired with `chmod`, never with
/// `install`, which truncates, and its bytes are not even read; and `vapid.toml` is generated once by `openssl`
/// when it is absent and read-only afterwards, because regenerating it would silence every existing push
/// subscription.
///
/// The account is not an item: it is a machine fact, and it is the operator's to create. Its state is read, its
/// commands are printed as the line's remark, and the run continues — because the tree's writes are this process's
/// and the account's absence does not change them.
fn files(config: &Config, root: &Root, tools: &Tools, dry_run: bool, kind: Kind) -> Line {
    let staging = staging_dir();
    let line = install(config, root, tools, dry_run, &staging, kind);
    let _ = std::fs::remove_dir_all(&staging);
    line
}

/// One item of the install tree: a path, the mode it must have, and what its bytes are.
struct Item {
    path: PathBuf,
    mode: u32,
    kind: ItemKind,
}

/// What a path is, which decides how a wrong one is put right.
enum ItemKind {
    /// A directory: `install -d -m`.
    Directory,
    /// A file whose bytes are this deploy's own: installed when absent, and installed again when they differ.
    Rendered(Vec<u8>),
    /// A file that belongs to the operator once it exists: created when absent, and never rewritten.
    Kept(Vec<u8>),
    /// A file a child generates when absent (`openssl`'s key pair): never generated when it is there, and never
    /// rewritten at all — a present file that is not a pair is a failure, not a file to replace.
    Vapid,
}

/// One write, as a value: the read's answer and the apply's work are the same list.
enum Action {
    /// `install -d -m <mode>`: the directory is absent, is not a directory, or has the wrong mode.
    Directory { path: PathBuf, mode: u32 },
    /// `install -m <mode> <staged> <path>`: the file is absent, or its bytes differ from this deploy's.
    File {
        path: PathBuf,
        mode: u32,
        bytes: Vec<u8>,
    },
    /// `chmod <mode> <path>`: the bytes are right and the mode is not — and `install` would truncate them.
    Mode { path: PathBuf, mode: u32 },
    /// The key pair is absent and a child generates it: `openssl` prints the SEC1 DER, and the rendered pair is
    /// installed by the same `install -m 0600` every other file action uses.
    Vapid { path: PathBuf },
}

impl Action {
    /// The rendering a report line uses: the program, the mode, the destination — and, for a file, a placeholder
    /// where the staging path goes, because that path is a temp file rather than a fact about the machine.
    fn describe(&self) -> String {
        match self {
            Self::Directory { path, mode } => {
                format!("install -d -m {mode:04o} {}", path.display())
            }
            Self::File { path, mode, .. } => {
                format!("install -m {mode:04o} <staged> {}", path.display())
            }
            Self::Mode { path, mode } => format!("chmod {mode:04o} {}", path.display()),
            // The child that writes the pair, as the program name rather than its output: the DER goes to a
            // staging file on its way to `install`, and the destination is what an operator needs to see.
            Self::Vapid { path } => format!(
                "openssl ecparam -name prime256v1 -genkey -noout -outform DER > <staged>; install -m 0600 \
                 <staged> {}",
                path.display()
            ),
        }
    }
}

/// The root, the two directories beside the config when the account has none of them, the data root, the secrets and
/// the three files, in the order they must be created — a file's directory first, because `install -m` does not make
/// one, and the key pair after the 0700 directory it lives in.
///
/// The root itself is first and is the only item whose outcome can be a *failure* rather than a write: a root this
/// process cannot write is a run that cannot do anything, and its sentence is the command that would fix it.
///
/// The config's directory and [`Root::unit`]'s directory are the two items that are **not** under the root, and each is
/// an item only while it is absent: `~/.config` and `~/.config/systemd/user` are the operator's own directories,
/// already there on a machine that has a session, and their modes (0700 and 0755 are both ordinary) are not this run's
/// to change. A fresh account gets them made; an existing one is never re-moded — which is also what keeps a second
/// deploy from reporting them as work.
///
/// The config's directory item is skipped when the directory is the root or under it, which is what a config *inside*
/// the state tree asks for — `--root R --config R/holosmith.toml`, this repository's previous spelling. The root is
/// already the list's first item at 0700, so a second entry for one path would be two contradictory modes: applied in
/// order it would leave the state root at 0755 and put it back on the next run, and a root is 0700 because the keys
/// and the sessions are under it. A directory *below* the root is the operator's to make — a state tree's layout is
/// theirs, and this step makes `data` and the secrets under it — so a config named there fails on that install rather
/// than acquiring a directory in the tree beside it.
///
/// The only `Err` is [`config::render`]'s: a path in the config that is not UTF-8, which no config file can carry
/// and which this step will not paper over with an empty body — a config this deploy wrote and could not render is a
/// step that failed, not a step that changed something.
fn items(config: &Config, root: &Root) -> Result<Vec<Item>, String> {
    let secrets = config.data_root.join(key::SECRETS_DIR);
    let rendered = config::render(config).map_err(|error| {
        format!(
            "the config that would be written to {} could not be rendered as TOML ({error}): this is the file \
             `serve` reads, so an empty one would be worse than a failure",
            root.config_file().display()
        )
    })?;
    let mut items = vec![Item {
        path: root.path().to_path_buf(),
        mode: 0o700,
        kind: ItemKind::Directory,
    }];
    if let Some(directory) = root.config_dir()
        && !directory.starts_with(root.path())
        && !directory.is_dir()
    {
        items.push(Item {
            path: directory,
            mode: 0o755,
            kind: ItemKind::Directory,
        });
    }
    // The user manager's directory is step 5's file's parent, and `install -m` does not make one. It is emitted on the
    // same rule as the config's directory: only while it is absent, so an existing 0700 tree of the operator's is
    // never re-moded and never counted as work.
    let unit_dir = root.unit_dir();
    if !unit_dir.is_dir() {
        items.push(Item {
            path: unit_dir,
            mode: 0o755,
            kind: ItemKind::Directory,
        });
    }
    items.extend([
        Item {
            path: root.config_file(),
            mode: 0o644,
            kind: ItemKind::Rendered(rendered.into_bytes()),
        },
        Item {
            path: config.data_root.clone(),
            mode: 0o755,
            kind: ItemKind::Directory,
        },
        Item {
            path: secrets.clone(),
            mode: 0o700,
            kind: ItemKind::Directory,
        },
        Item {
            path: secrets.join(key::KEYS_FILE),
            mode: 0o600,
            kind: ItemKind::Kept(key::empty_text().into_bytes()),
        },
        Item {
            path: vapid::store(&config.data_root),
            mode: 0o600,
            kind: ItemKind::Vapid,
        },
    ]);
    Ok(items)
}

/// The apply: read the service user, then every item, then run the pending actions in order, answering the step's
/// line.
///
/// The account is read **first** and its commands are the line's remark whatever happens, because they are the half
/// of this step no unprivileged run can do and a reader has to see them beside the list (ADR-0085).
fn install(
    config: &Config,
    root: &Root,
    tools: &Tools,
    dry_run: bool,
    staging: &Path,
    kind: Kind,
) -> Line {
    let user = match read_user(tools, kind.group()) {
        Ok(user) => user,
        Err(sentence) => return failed(DeployStep::Files, sentence),
    };
    let account = user.commands(root, kind.group());
    // The half this run cannot do is the line's remark, in both a dry run and an apply: a tree that is written but
    // whose account is missing is a unit that boots into `ExecStartPre` failing (ADR-0086), so the words that fix it
    // sit beside the count rather than being lost with a failure that only a broken run prints.
    let remark = match account.is_empty() {
        true => user.remark(kind.group()),
        false => format!("; still root's to run: {}", account.join("; ")),
    };
    // The root is one of the items, and it is the only one this process may not be able to make: its parent belongs
    // to root on a real machine. The apply below attempts it like any other write and turns its failure into the
    // command the operator runs instead (ADR-0086).
    let items = match items(config, root) {
        Ok(items) => items,
        Err(sentence) => return failed(DeployStep::Files, sentence),
    };
    let mut actions = Vec::new();
    for item in &items {
        match read(item) {
            Ok(Some(action)) => actions.push(action),
            Ok(None) => {}
            Err(sentence) => return failed(DeployStep::Files, sentence),
        }
    }
    let plan: Vec<String> = actions.iter().map(Action::describe).collect();
    if actions.is_empty() {
        return Line {
            step: DeployStep::Files,
            word: Word::Satisfied,
            sentence: format!(
                "{} holds the data root, the secrets and the keys with their modes, and the config is {}{remark}",
                root.describe(),
                root.config_file().display()
            ),
        };
    }
    if dry_run {
        return Line {
            step: DeployStep::Files,
            word: Word::Needs,
            sentence: counted(&plan, &remark),
        };
    }
    let mut command = None;
    for action in &actions {
        // The root itself is the one action this process may not be able to do: its parent is root's on a real
        // machine, so the child fails there with `permission denied`. The failure is turned into the command the
        // operator runs instead — `install -d -o holosmith` — because "carry on with a partial tree" is not an option
        // and "the tree could not be made" is not a description anyone can act on (ADR-0086).
        if matches!(action, Action::Directory { path, .. } if path == root.path()) {
            command = Some(format!(
                "sudo install -d -o {SERVICE_USER} -g {SERVICE_USER} -m 0700 {}",
                root.path().display()
            ));
        }
        if let Err(sentence) = apply(action, tools, staging) {
            return failed(
                DeployStep::Files,
                match command {
                    Some(command) => format!("{sentence} — run `{command}`, then re-run"),
                    None => sentence,
                },
            );
        }
        command = None;
    }
    Line {
        step: DeployStep::Files,
        word: Word::Changed,
        sentence: counted(&plan, &remark),
    }
}

/// What `id` answers about the account the unit names.
///
/// `id -u holosmith` is the existence question and `id -nG holosmith` the group one — two reads, because "the user
/// exists" and "the user may talk to the daemon" are two facts with two fixes. A spawn failure is a failed step
/// rather than "there is no user": an `id` this machine does not have cannot answer the question, and a deploy that
/// guessed would print a `useradd` for an account that is already there.
fn read_user(tools: &Tools, group: Option<&str>) -> Result<ServiceUser, String> {
    let uid = ask(
        &tools.id,
        &["-u", SERVICE_USER],
        "this step reads the service user before it says anything about it",
    )?;
    let grouped = match (uid.is_some(), group) {
        (true, Some(group)) => ask(
            &tools.id,
            &["-nG", SERVICE_USER],
            "this step asks whether the service user is in the daemon's group before it prints the command",
        )?
        .is_some_and(|groups| groups.split_whitespace().any(|found| found == group)),
        _ => false,
    };
    Ok(ServiceUser { uid, grouped })
}

/// The same shape for a list of sentences: the count on the step's own line, then one action per line below it —
/// `3 actions:` and three `    - …` lines, or `1 action:` and one.
///
/// One helper rather than two, so a reader can rely on the leading number in every plan and on the word after it
/// being "action"/"actions" rather than "1 things".
///
/// **The list is one action per line, and the count stays on the step's line** (ADR-0085). Ten actions joined with
/// `; ` are one 700-character line on a phone, and the entry that matters most — the first `install` that has to
/// succeed before the rest can — is the first thing the wrap loses. The count stays where it was, so the step's
/// first line keeps its `N. files: needs: 10 actions:` shape and a gate greps it the way it always has.
///
/// `remark` is the clause a few steps carry about what the run does *not* do — the account's commands, the unit's
/// install line, an `agent_image` this step does not build. It sits after the count rather than after the last
/// action, because it is a statement about the whole step: a reader who skimmed the list would otherwise take the
/// actions for the run's whole effect. It starts with `; `, which is why the count's own colon gives way to it.
fn counted(items: &[String], remark: &str) -> String {
    let count = format!(
        "{} action{}",
        items.len(),
        if items.len() == 1 { "" } else { "s" },
    );
    let mut text = match remark.is_empty() {
        true => format!("{count}:"),
        false => format!("{count}{remark}"),
    };
    for item in items {
        text.push_str(&format!("\n    - {item}"));
    }
    text
}

/// One item's state read: `None` when it is already what the deploy wants, the action that would make it so
/// otherwise, and a sentence when the path cannot be read at all (which is not "absent").
fn read(item: &Item) -> Result<Option<Action>, String> {
    let path = &item.path;
    let mode = item.mode;
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(Some(match &item.kind {
                ItemKind::Directory => Action::Directory {
                    path: path.clone(),
                    mode,
                },
                ItemKind::Rendered(bytes) | ItemKind::Kept(bytes) => Action::File {
                    path: path.clone(),
                    mode,
                    bytes: bytes.clone(),
                },
                // Absent is the one state that earns a generated pair.
                ItemKind::Vapid => Action::Vapid { path: path.clone() },
            }));
        }
        Err(error) => {
            return Err(format!(
                "{} could not be read ({error}): the deploy checks each path before it writes it, and a path it \
                 cannot read is one it cannot promise to leave alone",
                path.display()
            ));
        }
    };
    let wrong_mode = metadata.permissions().mode() & 0o777 != mode;
    let wrong_owner = metadata.uid() != std::fs::metadata(".").map_or(0, |here| here.uid());
    Ok(match &item.kind {
        ItemKind::Directory => {
            (!metadata.is_dir() || wrong_mode || wrong_owner).then(|| Action::Directory {
                path: path.clone(),
                mode,
            })
        }
        ItemKind::Kept(_) => {
            if !metadata.is_file() {
                return Err(format!(
                    "{} is not a file, and it is the key store `serve` and the key proxy read: move it aside — \
                     the deploy never writes over this path, so it cannot be the one to put it right",
                    path.display()
                ));
            }
            // The file's bytes are the operator's, so they are not read and not compared: a mode repair is the only
            // write this item can ever earn.
            wrong_mode.then(|| Action::Mode {
                path: path.clone(),
                mode,
            })
        }
        ItemKind::Rendered(bytes) => {
            let same = metadata.is_file() && std::fs::read(path).is_ok_and(|found| found == *bytes);
            if !same {
                Some(Action::File {
                    path: path.clone(),
                    mode,
                    bytes: bytes.clone(),
                })
            } else {
                wrong_mode.then(|| Action::Mode {
                    path: path.clone(),
                    mode,
                })
            }
        }
        ItemKind::Vapid => {
            if !metadata.is_file() {
                return Err(format!(
                    "{} is not a file, and it is where the machine's Web Push key pair lives: move it aside — the \
                     deploy generates this path only when it is absent",
                    path.display()
                ));
            }
            // The read never generates: absent is the only state that earns a key, which is what makes a second
            // deploy leave the pair — and every push subscription made with it — alone.
            match vapid::is_pair(path) {
                Ok(true) => wrong_mode.then(|| Action::Mode {
                    path: path.clone(),
                    mode,
                }),
                Ok(false) => Some(Action::Vapid { path: path.clone() }),
                Err(sentence) => {
                    return Err(format!(
                        "{sentence}; it will not be regenerated, because every push subscription made against \
                         this pair would stop working"
                    ));
                }
            }
        }
    })
}

/// Whether `program` answered exit 0, and its stdout when it did.
///
/// These are the deploy's **questions** — `id -u`, `id -nG` — so none of them is prefixed by anything: a question
/// needs no privilege, and a spawn failure is a sentence rather than a "no" (a program this machine does not have
/// cannot answer whether a user exists).
fn ask(program: &Path, arguments: &[&str], because: &str) -> Result<Option<String>, String> {
    let rendered = format!("{} {}", program.display(), arguments.join(" "));
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("`{rendered}` could not be run ({error}): {because}"))?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
}

/// One action as one child process, or — for the key pair — two.
///
/// `install` for a directory or a file, `chmod` for a mode, and for the pair a generation followed by the same
/// `install -m 0600` a file action uses. Every one goes through [`child`], so a spawn failure and a non-zero answer
/// are one sentence shape wherever they happen.
fn apply(action: &Action, tools: &Tools, staging: &Path) -> Result<(), String> {
    match action {
        Action::Directory { path, mode } => child(
            &tools.install,
            vec![
                "-d".to_string(),
                "-m".to_string(),
                format!("{mode:04o}"),
                path.display().to_string(),
            ],
        )
        .map(drop),
        Action::File { path, mode, bytes } => {
            let staged = stage(staging, path, bytes)?;
            install_staged(tools, &staged, path, *mode)
        }
        Action::Mode { path, mode } => child(
            &tools.chmod,
            vec![format!("{mode:04o}"), path.display().to_string()],
        )
        .map(drop),
        Action::Vapid { path } => {
            // The generation is a **read of a program's output**, not a write: it prints the DER to stdout, and the
            // file is written by the `install` below.
            let generated = child(
                &tools.openssl,
                vec![
                    "ecparam".to_string(),
                    "-name".to_string(),
                    "prime256v1".to_string(),
                    "-genkey".to_string(),
                    "-noout".to_string(),
                    "-outform".to_string(),
                    "DER".to_string(),
                ],
            )?;
            let pair = vapid::from_der(&generated.stdout)?;
            let staged = stage(staging, path, vapid::render(&pair).as_bytes())?;
            install_staged(tools, &staged, path, 0o600)
        }
    }
}

/// `install -m <mode> <staged> <path>`: the one write every file action ends in, staged or generated.
fn install_staged(tools: &Tools, staged: &Path, path: &Path, mode: u32) -> Result<(), String> {
    child(
        &tools.install,
        vec![
            "-m".to_string(),
            format!("{mode:04o}"),
            staged.display().to_string(),
            path.display().to_string(),
        ],
    )
    .map(drop)
}

/// One action's child: the one place an action's argv and its failure sentence are spelled, so a child that cannot
/// be spawned and one that answers non-zero read the same whichever program ran. A successful child's [`Output`] is
/// handed back because generating the key pair is a child whose **stdout** is the answer.
fn child(program: &Path, arguments: Vec<String>) -> Result<Output, String> {
    let rendered = format!("{} {}", program.display(), arguments.join(" "));
    let output = Command::new(program)
        .args(&arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            format!(
                "`{rendered}` could not be run ({error}): this deploy writes its own tree one child at a time, \
                 each inside the root the run was given"
            )
        })?;
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "`{rendered}` answered {} ({})",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
    ))
}

/// Write one file's bytes into the staging directory, mode 0600, and answer where they are.
///
/// A private directory rather than the destination's own: `install` reads its source, so the source must not be
/// somewhere this user cannot write, and it is removed with the directory whatever happened.
fn stage(staging: &Path, path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    if !staging.is_dir()
        && let Err(error) = std::fs::create_dir_all(staging)
    {
        return Err(format!(
            "the staging directory {} could not be created ({error}): the deploy writes each file to a private \
             temporary directory first and installs it from there",
            staging.display()
        ));
    }
    if let Err(error) = std::fs::set_permissions(staging, std::fs::Permissions::from_mode(0o700)) {
        return Err(format!(
            "the staging directory {} could not be made private ({error}): it holds a copy of the config file and \
             of the key file",
            staging.display()
        ));
    }
    let name = path.file_name().map_or_else(
        || "staged".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let staged = staging.join(name);
    // `create_new` rather than `write`: an entry a local user planted in the staging directory (a symlink to a path
    // this run may not write, say) must be a refusal rather than a followed link. The directory is unique per run
    // and emptied first; this is the half that does not trust it.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)
        .map_err(|error| {
            format!(
                "{} could not be created ({error}): the deploy stages what it installs in a private temporary \
                 directory and refuses to reuse a path already there",
                staged.display()
            )
        })?;
    file.write_all(bytes).map_err(|error| {
        format!(
            "{} could not be written ({error}): the deploy stages what it installs in a temporary directory",
            staged.display()
        )
    })?;
    Ok(staged)
}

/// The private staging directory's path, named uniquely per run so a previous run's copy — or one a local user
/// planted before this run started — cannot be installed by mistake.
fn staging_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let staging =
        std::env::temp_dir().join(format!("holosmith-deploy-{}-{nanos}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    staging
}

/// A failed step's line.
fn failed(step: DeployStep, sentence: String) -> Line {
    Line {
        step,
        word: Word::Failed,
        sentence,
    }
}

/// Step 3: the two images, built from the Dockerfiles embedded in this binary (ADR-0057).
///
/// The read is each image's **labels**, compared with what this binary would build: not the tag, which any
/// `docker tag` can move, and not a build timestamp, which says nothing about what is inside. One of those labels
/// is a digest of the binary the build copies, because the git tag cannot tell two builds of the same commit (or
/// of the same dirty tree) apart — a rebuilt artifact is a rebuild, and the acceptance's word is "the binary".
///
/// The binary is read before the read as well as before the apply: the digest is a property of those bytes, so a
/// deploy that cannot read its own path cannot say whether an image is current, and says that instead of claiming
/// `satisfied` on three strings.
fn images_line(config: &Config, machine: &dyn Machine, dry_run: bool) -> Line {
    // The digest of *this* binary, hashed once: it is the label that makes a rebuilt artifact a rebuild, and the
    // read needs it as much as the apply does — a read that skipped it would answer `satisfied` over an image
    // holding the previous program (the review found exactly that before this line existed).
    let digest = match images::own_digest() {
        Ok(digest) => digest,
        Err(sentence) => return failed(DeployStep::Images, sentence),
    };
    // Both expectations up front, so the loop below and the `satisfied` sentence read the same two values.
    //
    // `outstanding` is the read's one home: a promotion asks the same question for another release's
    // version and digest, so the *rule* -- labels, proxy first, the read's own sentence as the reason -- must not
    // live twice.
    let release = crate::version::VersionInfo::current();
    let agent = Expectation::of_release(ImageKind::Agent, config, &release, &digest);
    let pending: Vec<(ImageKind, String)> = images::outstanding(machine, config, &release, &digest);
    let remark = image_remark(config);
    if pending.is_empty() {
        // The proxy image carries four labels and the agent image five — the toolchain list is the agent's, because
        // the proxy installs no toolchain — so the sentence gives the shared four one clause and the agent's list
        // its own. The digest is clipped: a line an operator reads with 64 hex characters in it is a line nobody
        // finishes, and the twelve characters it does carry are enough to see two builds differ.
        return Line {
            step: DeployStep::Images,
            word: Word::Satisfied,
            sentence: format!(
                "{} and {} carry this binary's labels: version {}, git tag {}, pi {}, binary sha256 {}…, and \
                 the agent image's toolchains {}{remark}",
                ImageKind::Proxy.image(),
                ImageKind::Agent.image(),
                agent.version,
                agent.git_tag,
                agent.pi_version,
                &agent.binary[..12],
                agent.toolchains.unwrap_or_default().join(" ")
            ),
        };
    }
    let reasons: Vec<String> = pending.iter().map(|(_, reason)| reason.clone()).collect();
    if dry_run {
        return Line {
            step: DeployStep::Images,
            word: Word::Needs,
            sentence: counted(&reasons, &remark),
        };
    }
    let binary = match std::env::current_exe() {
        Ok(binary) => binary,
        Err(error) => {
            return failed(
                DeployStep::Images,
                format!(
                    "no image was built: this process's own path could not be read ({error}), and the build's \
                     context is an archive holding the Dockerfile and this program"
                ),
            );
        }
    };
    match images::ensure(machine, &binary, &digest, &release, config, &pending) {
        Ok(sentence) => Line {
            step: DeployStep::Images,
            word: Word::Changed,
            sentence: format!("{sentence}{remark}"),
        },
        Err(sentence) => failed(DeployStep::Images, sentence),
    }
}

/// What step 3 must say when the config's `agent_image` is not the name it builds.
///
/// Never a failure: `agent_image` is what a session's container runs, and an operator may point it at an image
/// built by hand (`pi-agent:local`). But a deploy that has just built `agent-image:<version>` while the config
/// names something else has installed an image no session will use, and a line that said nothing would leave
/// that to be discovered at a launch.
fn image_remark(config: &Config) -> String {
    let built = images::agent_image();
    if config.agent_image == built {
        return String::new();
    }
    format!(
        "; the config's `agent_image` names {}, which this step does not build",
        config.agent_image
    )
}

/// Step 4: the machine's `egress` network and its two proxies.
///
/// The read is a real measurement of the daemon — [`proxies::running`] for the two containers and
/// `docker network ls` for the network — which is what makes a second deploy honestly `satisfied` rather than a
/// re-application. Everything present is exactly the state a deploy is for, so the apply only runs when something
/// is missing.
///
/// **`[egress] allow` is not reconciled here, and that is the design rather than a gap.** The list is an argument
/// of `docker run`, so a changed allow-list takes effect on the next `serve` boot, which replaces both proxies
/// unconditionally (ADR-0037). Comparing a running container's arguments with the vector this deploy would pass
/// was rejected: it is a second reading of [`crate::container::EgressProxy::run_args`] and would drift from the
/// first. So a deploy ensures the proxies are **up**, and the boot is what makes them current — which is also what
/// lets this step's second run say `satisfied`.
fn proxies_line(config: &Config, machine: &dyn Machine, dry_run: bool) -> Line {
    // An unreadable network list is treated as "not read as present" rather than as a failure of its own: the apply
    // then asks the daemon to create it, and a daemon that cannot do that answers with its own sentence — one
    // failure with one cause, instead of two.
    let network = machine
        .network_names(docker::EGRESS_NETWORK, proxies::START_TIMEOUT)
        .is_ok_and(|names| names.iter().any(|name| name == docker::EGRESS_NETWORK));
    let [egress, key] = proxies::running(config, machine);
    // The names this run's config composes: `hs_egress`/`hs_key` on a deployed machine, and a smoke config's own two
    // if a deploy is ever run with one.
    let names = proxies::names(config);
    if network && egress && key {
        return Line {
            step: DeployStep::Proxies,
            word: Word::Satisfied,
            sentence: format!(
                "{} is there and {} and {} are running on it",
                docker::EGRESS_NETWORK,
                names[0],
                names[1]
            ),
        };
    }
    let mut pending = Vec::new();
    if !network {
        pending.push(format!("create the {} network", docker::EGRESS_NETWORK));
    }
    for (running, name, what) in [
        (
            egress,
            names[0].as_str(),
            "the only route out of a session's container",
        ),
        (
            key,
            names[1].as_str(),
            "the only thing that holds a real provider key",
        ),
    ] {
        if !running {
            pending.push(format!(
                "replace {name}, {what}, detached with `--restart unless-stopped`"
            ));
        }
    }
    let sentence = counted(&pending, "");
    if dry_run {
        return Line {
            step: DeployStep::Proxies,
            word: Word::Needs,
            sentence,
        };
    }
    if !network && let Err(sentence) = machine.ensure_network(docker::EGRESS_NETWORK) {
        return failed(DeployStep::Proxies, sentence);
    }
    // One at a time, in the order the containers depend on nothing but the network: a start that failed stops the
    // step, and its sentence is the one that explains what this machine will not have.
    if let Err(sentence) = proxies::start_egress(config, machine) {
        return failed(DeployStep::Proxies, sentence);
    }
    if let Err(sentence) = proxies::start_key(config, machine) {
        return failed(DeployStep::Proxies, sentence);
    }
    Line {
        step: DeployStep::Proxies,
        word: Word::Changed,
        sentence,
    }
}

/// Step 5: the systemd **user** unit, rendered into the directory the operator's user manager reads (ADR-0088).
///
/// The bytes are the deliverable, which is why the rendering is a public function of exactly three facts — where the
/// binary is, the role the machine will hold, and the config's path — and the read compares the file with it byte for
/// byte. The path is [`std::env::current_exe`] and not the literal `holosmith`: this binary does not install itself,
/// and an `ExecStart` resolved through systemd's `PATH` would be a coin flip between the artifact the operator just
/// built and whatever else answers to the name.
///
/// **The unit is written straight into [`Root::unit`], and only the enable is the operator's.** The file lands where
/// the user manager reads it, at 0644, so the two commands left are the reload that makes systemd notice it and the
/// enable that starts it now and at every boot — printed as the line's remark, in a dry run and in a run alike, with
/// no `sudo` anywhere in the pair. A run whose enable was never obeyed is caught one step later, by a `/api/health`
/// that does not answer.
///
/// The sandbox is the reduced set, and the reduction is measured rather than preferred: any mount or namespace
/// directive in a **user** unit makes systemd enable `PrivateUsers=` implicitly — `PrivateTmp=`, `ProtectSystem=`,
/// `ProtectHome=`, `ProtectKernelTunables=`, `ProtectControlGroups=`, `ProtectHostname=`, any one of them — and the
/// mapping that implies leaves the process holding gid 65534, collapses its supplementary groups (the `docker` group
/// included), reads every host file it does not own as `nobody:nogroup`, and makes `/etc/shadow` readable.
/// `PrivateUsers=no` beside them does not opt out. So the directives that are left are the ones that need no
/// namespace: they remove privilege from the process (`NoNewPrivileges`, `RestrictSUIDSGID`, `UMask=0077`), forbid it
/// from making a namespace of its own (`RestrictNamespaces`), and bound what it may ask the kernel for
/// (`LockPersonality`, `RestrictRealtime`, `SystemCallFilter`). What the system unit's `ProtectSystem=strict` and
/// `ReadWritePaths=` gave — a compromised `serve` unable to write `/etc` — is gone, and that is a loss this rendering
/// states by omission rather than by pretending the directive still works.
///
/// `role` is the role the **machine** will hold (`deploy::union`'s answer), not the half this run installs, and it
/// decides two things: `Description=`, and whether the unit requires the daemon. A `hub`-only machine has no
/// Docker, so a unit that requires `docker.service` there is a unit systemd never starts — and a `both` machine
/// runs sessions, so its unit keeps the requirement even when the run that wrote it was a `hub deploy`.
pub fn unit_text(exe: &Path, role: Role, root: &Root) -> String {
    let daemon = if role == Role::Hub {
        ""
    } else {
        " docker.service"
    };
    let requires = if role == Role::Hub {
        ""
    } else {
        "Requires=docker.service\n"
    };
    format!(
        "[Unit]\n\
         Description=Holosmith {role}\n\
         After=network-online.target{daemon}\n\
         Wants=network-online.target\n\
         {requires}\
         \n\
         [Service]\n\
         Type=simple\n\
         UMask=0077\n\
         NoNewPrivileges=yes\n\
         LockPersonality=yes\n\
         RestrictSUIDSGID=yes\n\
         RestrictNamespaces=yes\n\
         RestrictRealtime=yes\n\
         SystemCallFilter=@system-service\n\
         ExecStartPre={exe} doctor --quick\n\
         ExecStart={exe} serve --config {config}\n\
         Restart=on-failure\n\
         RestartSec=5s\n\
         KillMode=mixed\n\
         TimeoutStopSec=30s\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        role = role.as_str(),
        exe = exe.display(),
        config = root.config_file().display(),
    )
}

/// What the operator must run to make the rendered unit the one their user manager loads, in order.
///
/// No `sudo` and no copy: the bytes are already at [`Root::unit`], so the pair is the reload that makes the manager
/// notice the file and the enable that starts it now, at every login, and at boot once linger is on.
fn unit_commands() -> Vec<String> {
    vec![
        "systemctl --user daemon-reload".to_string(),
        format!("systemctl --user enable --now {UNIT_NAME}"),
    ]
}

/// Step 5's read and apply: the file's bytes, and the commands that enable them.
fn service(config: &Config, root: &Root, tools: &Tools, dry_run: bool) -> Line {
    let staging = staging_dir();
    let line = service_line(config, root, tools, dry_run, &staging);
    let _ = std::fs::remove_dir_all(&staging);
    line
}

fn service_line(
    config: &Config,
    root: &Root,
    tools: &Tools,
    dry_run: bool,
    staging: &Path,
) -> Line {
    // `ExecStart` names the **release tree's** binary when one is live, and this process's own path otherwise.
    //
    // That is the one fact a promotion changes and a deploy must not undo: a machine whose `current`
    // symlink resolves is a machine that systemd should start *through* it, so a switch is what a restart picks up.
    // The unit's bytes are rendered from the same function either way, which is why this read and the promotion's
    // write agree byte for byte -- and why a second deploy on a promoted machine says `satisfied`.
    let exe = match crate::release::Release::of(root).live() {
        Some(live) => live,
        None => match std::env::current_exe() {
            Ok(exe) => exe,
            Err(error) => {
                return failed(
                    DeployStep::Service,
                    format!(
                        "no unit was written: this process's own path could not be read ({error}), and `ExecStart` \
                         names it because a program resolved through systemd's `PATH` is not the binary that wrote the \
                         unit"
                    ),
                );
            }
        },
    };
    let bytes = unit_text(&exe, config.role, root);
    let unit = Item {
        path: root.unit(),
        mode: 0o644,
        kind: ItemKind::Rendered(bytes.clone().into_bytes()),
    };
    let actions: Vec<Action> = match read(&unit) {
        Ok(Some(action)) => vec![action],
        Ok(None) => Vec::new(),
        Err(sentence) => return failed(DeployStep::Service, sentence),
    };
    let remark = format!("; still yours to run: {}", unit_commands().join("; "));
    if actions.is_empty() {
        return Line {
            step: DeployStep::Service,
            word: Word::Satisfied,
            sentence: format!(
                "{} holds this binary's unit ({} bytes){remark}",
                root.unit().display(),
                bytes.len()
            ),
        };
    }
    let plan: Vec<String> = actions.iter().map(Action::describe).collect();
    if dry_run {
        return Line {
            step: DeployStep::Service,
            word: Word::Needs,
            sentence: counted(&plan, &remark),
        };
    }
    for action in &actions {
        if let Err(sentence) = apply(action, tools, staging) {
            return failed(DeployStep::Service, sentence);
        }
    }
    Line {
        step: DeployStep::Service,
        word: Word::Changed,
        sentence: counted(&plan, &remark),
    }
}

/// Step 6: wait for `/api/health`, then ask `doctor --full` (spec item 10 step 7).
///
/// The read is the same two questions the apply ends on — the endpoint answers **and** no finding fails — so a
/// `satisfied` line here is "this machine is serving, measured", not "the unit was installed". Anything short of
/// that goes to `needs`, because an apply is what can change it (it waits, and a start is what the wait is for).
///
/// The apply's failure is `doctor`'s own sentence, verbatim: step 1's rule, one level later — a deploy, `serve`'s
/// refusal and `doctor`'s line cannot say different things about one machine.
fn health(
    config: &Config,
    doctor_tools: &doctor::Tools,
    tools: &Tools,
    machine: &dyn Machine,
    dry_run: bool,
) -> Line {
    let answering = doctor::health_answers(config.control_listen);
    let measured = doctor::run(config, Depth::Full, doctor_tools, machine);
    if answering && !measured.failed() {
        return Line {
            step: DeployStep::Health,
            word: Word::Satisfied,
            sentence: format!(
                "the control origin ({}) answers `GET /api/health`, and `doctor --full` reports {} findings with \
                 none failing",
                config.control_listen,
                measured.findings.len()
            ),
        };
    }
    let plan = vec![
        format!(
            "wait for `/api/health` on {} for up to {} s",
            config.control_listen,
            tools.health_wait.as_secs()
        ),
        "doctor --full".to_string(),
    ];
    if dry_run {
        return Line {
            step: DeployStep::Health,
            word: Word::Needs,
            sentence: counted(&plan, ""),
        };
    }
    if !wait_for_health(config.control_listen, tools.health_wait) {
        return failed(
            DeployStep::Health,
            format!(
                "the control origin ({}) did not answer `/api/health` within {} s: `journalctl --user -u \
                 {UNIT_NAME} -n 50` names why the unit did not come up",
                config.control_listen,
                tools.health_wait.as_secs()
            ),
        );
    }
    let measured = doctor::run(config, Depth::Full, doctor_tools, machine);
    if let Some(finding) = measured.failures().next() {
        return failed(DeployStep::Health, finding.sentence.clone());
    }
    Line {
        step: DeployStep::Health,
        word: Word::Changed,
        sentence: counted(&plan, ""),
    }
}

/// Whether `listen` answers `GET /api/health` before `bound` elapses.
///
/// One function with two callers — this step's apply and `doctor`'s port check — so that "the endpoint answers"
/// means one thing wherever the deploy or a report says it. The bound is a parameter rather than [`HEALTH_WAIT`]
/// so both arms are testable in milliseconds: a wait that took a minute to prove it gives up would be a test
/// nobody runs.
pub fn wait_for_health(listen: SocketAddr, bound: Duration) -> bool {
    let start = Instant::now();
    loop {
        if doctor::health_answers(listen) {
            return true;
        }
        let elapsed = start.elapsed();
        if elapsed >= bound {
            return false;
        }
        // The last sleep is short: a wait that has 40 ms left should ask again in 40, not in a poll interval.
        std::thread::sleep(HEALTH_POLL.min(bound - elapsed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One machine's list of items, for a deploy whose `--config` is `spelling` — the config file inside the root
    /// it names. [`items`] reads no file and writes none, so no path here has to exist.
    fn items_for(spelling: &Path) -> Vec<Item> {
        let root = Root::resolve(
            Some(spelling.to_path_buf()),
            Some(spelling.join(config::CONFIG_FILE)),
        );
        let config = Config {
            data_root: root.data_root(),
            ..Config::default()
        };
        items(&config, &root).expect("the config renders, so every item is known")
    }

    #[test]
    fn a_config_inside_the_root_is_one_item_and_that_item_is_the_roots_0700() {
        // The previous layout's own spelling, `--root R --config R/holosmith.toml`, on a machine where `R` is not
        // there yet: the config's directory **is** the root, so an entry for it beside the root's own would be two
        // modes for one path — applied in order, a state tree at 0755 that the next run puts back to 0700.
        let scratch =
            std::env::temp_dir().join(format!("holosmith-deploy-items-{}", std::process::id()));
        let spelling = scratch.join("root");
        assert!(
            !spelling.exists(),
            "{} is not a tree this test made",
            spelling.display()
        );

        let items = items_for(&spelling);
        let listed: Vec<&Item> = items.iter().filter(|item| item.path == spelling).collect();
        assert_eq!(
            listed.len(),
            1,
            "the root is {} items: {:?}",
            listed.len(),
            listed.iter().map(|item| item.mode).collect::<Vec<u32>>()
        );
        assert_eq!(listed[0].mode, 0o700, "the root is not the 0700 item");
    }
}
