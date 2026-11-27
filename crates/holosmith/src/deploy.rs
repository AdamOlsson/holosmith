//! `holosmith server deploy` and `holosmith hub deploy`: install one half of a machine, in the spec's six steps
//!
//! The command is one idempotent pass over item 10 §"What `server deploy` does": each step **reads the machine's
//! current state** and then applies only what the read found missing, so a second run changes nothing and reports
//! it (ADR-0056). That pair is why a step cannot be one function that decides and acts — a dry run has to print
//! the read's plan without running the apply, and "changed" has to be distinguishable from "was already right".
//!
//! Every step prints one line: [`DeployStep`]'s number is the spec's order and [`Word`] is the read's answer, one of
//! five. There are two renderings of those lines and they are one vocabulary (ADR-0085). [`Report::render`] is the
//! default and prints the mark for each word — `2. files ✓ (3 actions)` — with the detail only where a step wrote
//! something, needs something or failed; [`Report::render_verbose`] is `--verbose` and prints what every release
//! before this one printed, `<n>. <step>: <word>: <sentence>`, with a step's list — a plan, or the apply that did it —
//! one action per line under it and the count on the line itself (ADR-0085). The two words that carry the design are:
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
//! - **The proxies' recipe lives in [`crate::proxies`].** A deploy and a boot must start the same containers
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
//! **The deploy runs as whoever invokes it, and it never escalates.** There is no account to create and no ownership
//! to hand over: the root, the config and the unit are the invoking account's own files (ADR-0088), and the unit is
//! written into the directory that account's user manager reads. The one subject left is the same account, and the
//! one question about it is whether it may open the machine's daemon socket: step 2 **reads** that and **prints** the
//! one `usermod` when the answer is no — never runs it, and never a `sudo` of its own. The unit's two
//! `systemctl --user` commands are printed the same way. The one hard requirement is that this process can write the
//! root, because the tree is written as its owner, and step 6 (`/api/health` plus `doctor --full`) is what notices a
//! unit that was enabled and never started.

use std::fmt::{self, Write as _};
use std::io::{ErrorKind, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{self, Config, Role};
use crate::docker;
use crate::doctor::{self, Depth};
use crate::images::{self, Expectation, Kind as ImageKind};
use crate::key;
use crate::preflight::{self, Check, Outcome};
use crate::progress::{Progress, Silent};
use crate::proxies;
use crate::release::{self, Release};
use crate::vapid;

/// The group that may open the machine's daemon socket: the one fact step 2 reads about the account running it.
///
/// A group's **name** is a real fact about a host and stays a constant, unlike the account ADR-0088 deleted: `serve`
/// reaches the daemon through a socket this group owns, so an account outside it is a machine that cannot run a
/// session, and the fix is one `usermod` the operator runs.
pub const DOCKER_GROUP: &str = "docker";

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
/// `promote`'s fallback still names.
pub const UNIT_DIR: &str = "/etc/systemd/system";

/// How long the last step's apply waits for `/api/health` before it calls the unit's start a failure.
pub const HEALTH_WAIT: Duration = Duration::from_secs(60);

/// How often that wait asks again. A serving unit answers in milliseconds; the interval is what a wait costs the
/// command, and 250 ms is four asks a second for a minute.
pub const HEALTH_POLL: Duration = Duration::from_millis(250);

/// The exit code `server deploy` answers when step 3 failed because a **pinned fetch did not answer**.
///
/// `4`, because 0, 1, 2 and 3 are spoken for: 1 is a run that failed, 2 is a gate invoked wrongly, and 3 is
/// `scripts/check-crate-set.sh`'s stale image. The code exists so that a caller can tell an outage from a verdict,
/// which is what it is for in this product: `scripts/verify.sh` prints a line that measured nothing and exits the same
/// 4 rather than a `FAIL` a `holosmith update` would record as a refusal, and [`Report::exit_code`] is the one place
/// a report becomes it (ADR-0111).
pub const FETCH_UNANSWERED: u8 = 4;

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
/// the account that runs the daemon cannot read is a reason for systemd not to start the service. But `deploy` is the
/// caller that **repairs** it — step 2's `Action::Mode` is exactly this finding's fix — so failing here would make the
/// repair unreachable and leave the operator to run one `chmod` the deploy knows how to run.
const WRONG_MODE: &str = "; a wrong mode or owner in the install tree is what step 2 repairs, so it is a remark here and not a failure";

/// The remark a broken `current` earns instead of failing the deploy — step 1's **third** divergence.
///
/// `doctor --quick` fails it on purpose: the unit runs `doctor --quick` as `ExecStartPre`, and a `current` the unit
/// executes through that resolves to nothing is a boot that cannot happen. But a deploy is one of the two commands
/// that can **repair** it — step 5 plants the binary this run is running as a release and points `current` at it, so
/// the machine boots again through a live link — and failing at step 1 would make that repair unreachable, exactly
/// ADR-0058's argument for `Perms` one finding over.
const LINK_NOT_LIVE: &str = "; the unit's `ExecStart` resolves through a `current` that is not live — step 5 plants \
                             the binary this run is running as a release and points `current` at it, which is what \
                             makes the machine boot again";

/// Which deploy a run is: the server's six steps, or the hub's four and two skips (item 10 §6).
///
/// A deploy installs **one half of a machine**, and the two halves together are `both` — so the type carries three
/// answers and they are deliberately not one: [`Kind::installs`] is the role this run adds, [`union`] is the role
/// the machine ends up with, and [`Kind::group`]/[`Kind::skip`] are the two places a hub's half is not a server's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `holosmith server deploy`: the six steps, the daemon's group asked about, and all three proxies.
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

    /// The group the account running this deploy must be in, or `None` when there is no such question — the half
    /// that drives no daemon (item 10 §6's "without the docker group"), and the endpoint that is not a socket.
    ///
    /// **The endpoint decides it.** A socket on this machine is opened through the group that owns it, so there — and
    /// only there — does "is this account in `docker`?" say whether the machine can run a session. A `tcp:` endpoint
    /// is another host's daemon reached over the network, where no group of this account's has any bearing and a
    /// `usermod` would change nothing; answering `Some` there would print a command and report `needs` on a machine
    /// that already works, instead of the `satisfied` it earned.
    ///
    /// It is the whole of the account question: a [`Self::Hub`] opens no socket, so a hub deploy asks `id` nothing
    /// about the account and prints no command for it.
    pub fn group(self, docker_host: Option<&str>) -> Option<&'static str> {
        match (self, local_socket(docker_host)) {
            (Self::Server, true) => Some(DOCKER_GROUP),
            _ => None,
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
                 machine's three proxies run, and a hub runs no containers (spec item 10 §6)",
            ),
            (Self::Hub, DeployStep::Proxies) => Some(
                "a hub starts no proxies: `hs_egress` is a session's only route out, `hs_key` is the only thing \
                 that holds a real provider key and `hs_mcp` the only thing that spends a tool's, and a hub has \
                 none of the three",
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

/// Whether `docker_host` names a socket on this machine — the one endpoint a group decides access to.
///
/// Unset is `preflight::DEFAULT_DOCKER_HOST`, the rootful daemon's socket, which is local; `unix://` and a bare path
/// are local too. Anything with another scheme is a daemon reached over the network, where the account's groups are
/// not what opens it.
fn local_socket(docker_host: Option<&str>) -> bool {
    match docker_host
        .unwrap_or(preflight::DEFAULT_DOCKER_HOST)
        .split_once("://")
    {
        Some((scheme, _)) => scheme == "unix",
        None => true,
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
    ///    VAPID keys), and the read of whether the account running this deploy may open the daemon's socket.
    Files,
    /// 3. The three images embedded in the binary.
    Images,
    /// 4. The `egress` network and the machine's three proxies.
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

    /// The bracket a progress line carries: `3/6 images`, the number and word the report's own line also uses.
    ///
    /// It is the report's spelling and not a second vocabulary, because the two lines describe one step: the progress
    /// line arrives while it runs and the report's when it is over, and a reader matches them by what they say.
    pub fn label(self) -> String {
        format!("{}/6 {}", self.number(), self.word())
    }

    /// What the step is about to do, present tense, for the begin line [`Progress::begin`] carries.
    ///
    /// The phrase exists for the one silence a report cannot break: step 3 builds three images and the first build
    /// downloads pi, so a first install is minutes of nothing. A phrase is a **duration expectation** as much as a
    /// description — "a first install downloads pi, so this takes minutes" is what tells a reader that the silence is
    /// work rather than a hang — which is why it lives here, beside [`Self::word`], and not in a caller's formatting.
    /// The two steps that name no duration are the ones that cannot take long: a read of this machine, two
    /// `docker run`s, one file's bytes.
    pub fn beginning(self) -> &'static str {
        match self {
            Self::Preflight => "asking `doctor --quick` about this machine",
            Self::Files => "reading and writing the install tree, the config and the secrets",
            Self::Images => {
                "building the three images — a first install downloads pi, so this takes minutes"
            }
            Self::Proxies => "starting the `egress` network and the machine's three proxies",
            Self::Service => "writing the systemd unit",
            Self::Health => "asking `/api/health` and `doctor --full` about the machine",
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
    /// The read found work this run has not done. A dry run's plan is the common case, and the other one is a real
    /// run whose remaining action is the operator's — step 2's one `usermod`, which is root's and which this program
    /// never runs (ADR-0088).
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

    /// The mark the terse report prints instead of the word: `✓` for the two words that mean the step is right
    /// (nothing to do, or done), `–` for [`Self::Skipped`], `!` for [`Self::Needs`], `✗` for [`Self::Failed`].
    ///
    /// One function rather than four `match`es, because the marks and the slice of the tree that prints them are
    /// one vocabulary: a second copy is a second spelling of what a step's outcome looks like. `satisfied` and
    /// `changed` share `✓` deliberately — a reader asking what a run did not need to do reads the parentheses, and a
    /// run that applied everything and one that needed nothing are both green.
    pub fn mark(self) -> &'static str {
        match self {
            Self::Satisfied | Self::Changed => "✓",
            Self::Skipped => "–",
            Self::Needs => "!",
            Self::Failed => "✗",
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
    /// The one command that puts a **failed** step right, when the step knows a better answer than re-running the
    /// whole run: a `chmod`, `systemctl --user enable --now`, `holosmith doctor --quick`.
    ///
    /// It is here rather than as a second sentence because the terse report prints it as its own indented line —
    /// `    fix: chmod 0700 …` — and a value a reader pastes into a shell has to travel as a value, not as prose a
    /// renderer has to find and cut out of it. The command layer fills `None` with the run's own resume command
    /// ([`Report::fill_fix`]), so every failed step a reader sees carries exactly one pasteable line.
    pub fix: Option<String>,
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
    /// Whether the step that stopped the run failed because a **pinned fetch did not answer** — step 3's build of an
    /// image whose toolchain download never arrived, which is a machine that could not reach a host rather than a tree
    /// that is wrong. [`Report::exit_code`] is the one reader (ADR-0111).
    pub unanswered: bool,
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

    /// The exit code this report earns: [`FETCH_UNANSWERED`] when a fetch that did not answer is what stopped the run,
    /// `FAILURE` for every other failed step, and success for a report that failed nowhere.
    ///
    /// A failed build is normally the tree's or the machine's business and exit 1 is right for it — a compile error, a
    /// `COPY` of a file the context does not carry, a tarball that failed the digest the recipe pins. The one failure
    /// that is not a verdict about the tree is step 3's build whose pinned download never arrived, and giving it its own
    /// code is what lets `scripts/verify.sh` print a case that could not measure instead of a `FAIL` (ADR-0111).
    pub fn exit_code(&self) -> ExitCode {
        match self.failed {
            Some(_) if self.unanswered => ExitCode::from(FETCH_UNANSWERED),
            Some(_) => ExitCode::FAILURE,
            None => ExitCode::SUCCESS,
        }
    }

    /// Put `fallback` on the failed step's [`Line::fix`] when the step itself named no repair: the run's own resume
    /// command, which is the command that just failed and every step's recovery (ADR-0056).
    ///
    /// The command layer owns the flags, so it owns that command: `--config`/`--root` made absolute, `--dry-run` only
    /// on a dry run. A step that knew a more precise repair has already filled the field and keeps it.
    pub fn fill_fix(&mut self, fallback: &str) {
        if let Some(line) = self.lines.iter_mut().find(|line| line.word == Word::Failed)
            && line.fix.is_none()
        {
            line.fix = Some(fallback.to_string());
        }
    }

    /// The report as an operator reads a run: one line per step, the mark for its word, and the detail only where
    /// there is something to know or to run.
    ///
    /// ```text
    /// 1. preflight ✓
    /// 2. files ✓ (3 actions)
    /// 3. images ✓ (built 2 images: holosmith-proxy:0.1.0, agent-image:0.1.0)
    /// ```
    ///
    /// Three facts decide the shape, and each is a section of ADR-0085:
    ///
    /// - **The mark is the verdict**, and a step with nothing to act on prints only it and the remark it earns: the
    ///   sentences are written for the operator who must *do* something about a step, which is right for the one that
    ///   needs a `chmod` and a wall of text when all six passed — and the `; `-separated remark is the exception,
    ///   because it carries a command or a fact about which binary the unit ends up running.
    /// - **The detail is the sentence's own, cut to its first clause** — `3 actions`, `built 2 images` — so the terse
    ///   line says what the run wrote without a second vocabulary. Under a `needs` line the action lines follow
    ///   indented exactly as the verbose report prints them; under a `satisfied` or a `changed` one only the remark
    ///   does — what the step wrote is the parenthetical, and the remark is the clause the operator is left with.
    /// - **A failure keeps its whole sentence and gains one `fix:` line**, which is the command that puts it right
    ///   ([`Report::fill_fix`]).
    ///
    /// The header and the tally do not survive here: a run is one line per step, and which tree it installed into is
    /// a question the verbose report answers. `dry_run` has no header of its own either — "nothing was written" is
    /// the command's line to print, because it is the command's flag ([`crate::commands::deploy::run`]).
    pub fn render(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            // A `String` cannot fail to be written to, which is why this is not a `?`.
            let _ = writeln!(text, "{}", line.terse());
        }
        text
    }

    /// The report as any earlier release printed it: the header, the root, one block per step and the tally.
    ///
    /// A block is the step's own line — `^N. step: word: ` — and, under a plan or an apply, one indented line per
    /// action (ADR-0085). That first line is what a gate greps, so it is the only line whose shape is a contract.
    ///
    /// The tally's five counts are always printed — `skipped` included, as zero on a run that skipped nothing, and
    /// `needs` included because a step whose remaining work is the operator's is still one of the steps the header
    /// counts: a tally of four words would sum to fewer steps than the run reports, and leave out the one that says
    /// the machine is not ready (ADR-0056, amended by ADR-0088), because a gate greps the line's shape and a reader
    /// diffs two runs by it.
    pub fn render_verbose(&self) -> String {
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
            "deploy: {} steps: {} satisfied, {} needs, {} changed, {} skipped, {} failed",
            self.lines.len(),
            count(Word::Satisfied),
            count(Word::Needs),
            count(Word::Changed),
            count(Word::Skipped),
            count(Word::Failed)
        );
        text
    }
}

impl Line {
    /// One terse line: `3. images ✓ (built 2 images)`, or the `fix:` line a failure earns, indented under it.
    pub fn terse(&self) -> String {
        let mut text = format!("{}. {} {}", self.step.number(), self.step, self.word.mark());
        terse_detail(&mut text, self.word, &self.sentence, self.fix.as_deref());
        text
    }
}

/// What follows a terse line's mark, whatever the line is about — one function so that the deploy's lines and
/// install's own say the same thing about the same five words.
///
/// - `satisfied` and `skipped` carry the mark alone, and the remark when the step has one: a satisfied line whose
///   sentence carries a clause — which binary the unit runs, which filesystem an absent data root will land on — prints
///   that clause on the indented line a `changed` line's remark uses, because the terse rendering is the one an
///   operator reads and a clause is the whole reason the line was written (ADR-0085).
/// - `needs` carries the sentence's first line, which for a plan is its count and the remark about what the run does
///   not do, and then every indented line under it — a step whose remaining work is the operator's keeps all of it,
///   because dropping a `usermod` from the one line that says the operator must act is losing the line's subject.
/// - `changed` carries the first clause in parentheses — what it wrote, in the sentence's own words — and then the
///   remark, if the step has one, because a remark is a command the operator may still have to run.
/// - `failed` carries the whole sentence and then `fix:`, one line, pasteable.
pub(crate) fn terse_detail(text: &mut String, word: Word, sentence: &str, fix: Option<&str>) {
    let mut lines = sentence.lines();
    let first = lines.next().unwrap_or_default();
    let (clause, remark) = split_remark(first);
    match word {
        Word::Satisfied | Word::Skipped => {
            if let Some(remark) = remark {
                let _ = write!(text, "\n    {remark}");
            }
        }
        Word::Needs => {
            let _ = write!(text, " {}", clause.trim_end_matches(':'));
            if let Some(remark) = remark {
                let _ = write!(text, "; {remark}");
            }
            for line in lines {
                text.push('\n');
                text.push_str(line);
            }
        }
        Word::Changed => {
            let _ = write!(text, " ({})", clause.trim_end_matches(':'));
            if let Some(remark) = remark {
                let _ = write!(text, "\n    {remark}");
            }
        }
        Word::Failed => {
            let _ = write!(text, " {sentence}");
            if let Some(fix) = fix {
                let _ = write!(text, "\n    fix: {fix}");
            }
        }
    }
}

/// A sentence's first line cut into the clause that names the work and the remark a few steps carry about what the run
/// does **not** do (`; the account … is in the docker group`; `; still yours to run: systemctl …`).
///
/// The remark is split on the sentence's own separator rather than by a second field on [`Line`], because a sentence is
/// the one thing both renderings print and the split is the same two characters `deploy`'s steps already write it
/// with: the count's colon gives way to a `; ` remark ([`counted`]), so the first of those is where the clause ends.
fn split_remark(first: &str) -> (&str, Option<&str>) {
    match first.split_once("; ") {
        Some((clause, remark)) => (clause, Some(remark)),
        None => (first, None),
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
/// — and `id`, which answers the **only** question this run asks about an account: who is running it, and is that
/// account in the daemon's group. They are `PathBuf`s for [`crate::preflight::Tools`]'s reason: this crate denies
/// `unsafe` and so cannot set `PATH`, which leaves naming the program as the only way a test can point a write at a
/// script it wrote.
///
/// **The subtree this module writes is the operator's own** (ADR-0088): no step here escalates, and what root would
/// do is printed. [`Tools::systemctl`] and [`Tools::systemd_run`] are the two fields **other** modules reach for — the
/// release verbs' single restart, and a promotion's transient unit, both through the operator's own user manager —
/// and [`Tools::health_wait`] is the one field that is not a program at all.
#[derive(Debug, Clone)]
pub struct Tools {
    pub doctor: doctor::Tools,
    pub install: PathBuf,
    pub chmod: PathBuf,
    pub openssl: PathBuf,
    /// `id`: the name of the account running this deploy and its groups — `id -un`, `id -nG` — which is step 2's
    /// whole read of an account.
    pub id: PathBuf,
    /// `systemctl`: **not called by this module.** The two release verbs run `systemctl --user restart <unit>` through
    /// it — the unit is the operator's own, so the restart needs no privilege and `--user` is the whole of what picks
    /// the manager (ADR-0088 D2) — and the same manager is asked `systemctl --user is-active --quiet <unit>` about a
    /// promotion already in flight ([`crate::update::start`]), the question that turns a refused start into
    /// [`crate::update::Answer::AlreadyRunning`] or into a failure, and its `--version` is the version a promotion's
    /// job needs ([`crate::update::below_floor`], asked at the press rather than at boot). The field lives here so that
    /// a test points it at its recorder beside the others.
    pub systemctl: PathBuf,
    /// `systemd-run`: **not called by this module either.** [`crate::update::start`] runs
    /// `systemd-run --user --unit=holosmith-update …` through it, so that a promotion started from a request is a unit
    /// of the operator's user manager rather than a child of the service it restarts (ADR-0110 D4). It sits beside
    /// [`Tools::systemctl`] because the two are one manager's two verbs: one to begin a job outside the service's
    /// cgroup, one to restart the service that job replaces.
    pub systemd_run: PathBuf,
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
            systemctl: PathBuf::from("systemctl"),
            systemd_run: PathBuf::from("systemd-run"),
            health_wait: HEALTH_WAIT,
        }
    }
}

/// Run the six steps in the spec's order and answer the report: a **server** deploy.
///
/// This is [`run_as`] with [`Kind::Server`], which is what the name means: the six steps, the daemon's group and
/// its proxies. `holosmith hub deploy` calls [`run_as`] with its own half. Every one of these three is quiet: a
/// deploy's report is the whole of what it prints, so the run tells a sink nothing ([`run_as_with`] is where a caller
/// that has someone to tell comes in).
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
    run_as_with(config, root, tools, machine, dry_run, kind, &mut Silent)
}

/// [`run_as`] with a sink told what the run is doing, which is `install`'s shape of it.
///
/// The split is the decision [`crate::progress::Progress`] records: a `deploy` prints its report and nothing else, so
/// it has no one to tell and takes [`Silent`]; an install is a first run on a bare machine, minutes long, and silent
/// until the end unless a caller says otherwise.
pub fn run_as_with(
    config: &Config,
    root: &Root,
    tools: &Tools,
    machine: &dyn Machine,
    dry_run: bool,
    kind: Kind,
    progress: &mut dyn Progress,
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
        unanswered: false,
        dry_run,
        root: root.describe(),
    };
    for step in DeployStep::ALL {
        // A step this half has no work for is a `skipped` line and the run continues: the remaining steps are still
        // this run's, and a stop here would leave steps 5–6 of a hub unevaluated for no reason (ADR-0056). It is also
        // the one arm that says nothing to the sink: there is nothing to wait for, so a begin line over it would be a
        // lie about the silence.
        if let Some(reason) = kind.skip(step) {
            report.lines.push(Line {
                step,
                word: Word::Skipped,
                sentence: reason.to_string(),
                fix: None,
            });
            continue;
        }
        // The begin line goes **before** the step's first read: it is the only thing a reader waiting on a
        // minutes-long step has, and the reason it is a sink rather than part of the report is that a report is
        // written when the work is over (ADR-0092).
        progress.begin(&step.label(), step.beginning());
        let line = match step {
            DeployStep::Preflight => preflight(&checked, &doctor_tools, machine),
            DeployStep::Files => files(&installed, root, tools, dry_run, kind),
            // Step 3 is the one step whose failure can be the machine's network rather than the tree, so its line and
            // that one bit travel together ([`Report::exit_code`]).
            DeployStep::Images => {
                let (line, unanswered) = images_line(&installed, machine, dry_run, progress);
                report.unanswered = unanswered;
                line
            }
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
/// boot that cannot happen, and step 5 plants the binary this run is running as a release and points `current` at it
/// whenever `Release::live` names nothing — refusing at step 1 would make the repair unreachable too (ADR-0084).
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
        // `doctor --quick` is this step's own list with every check and every sentence in it, so the repair is not a
        // second reading of the finding: it is the command that prints the whole of what this line cut to one.
        return failed_with_fix(
            DeployStep::Preflight,
            finding.sentence.clone(),
            "holosmith doctor --quick".to_string(),
        );
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
        "{} findings: {} pass, {} fail, {} unknown, {} info",
        findings.len(),
        count(Outcome::Pass),
        count(Outcome::Fail),
        count(Outcome::Unknown),
        count(Outcome::Information)
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
        fix: None,
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

/// What `id` answered about the account running this deploy: its name, and the groups it is in.
///
/// The subject is deliberately **this** account and not a second one: a machine is the operator's own (ADR-0088), so
/// the only thing step 2 can get wrong about an account is whether it may open the daemon's socket, and the only fix
/// is a command the operator runs. `install` asks the same question before it writes anything, which is why this and
/// its read are visible to the crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Account {
    /// `id -un`: the name the one printed command spells.
    pub(crate) name: String,
    /// `id -nG`, split on whitespace: every group the account is in.
    groups: Vec<String>,
}

impl Account {
    /// Whether the account may open the daemon's socket, which is `group`'s membership and nothing else.
    pub(crate) fn may_open(&self, group: &str) -> bool {
        self.groups.iter().any(|found| found == group)
    }

    /// The one command that would put that right.
    ///
    /// It is **printed**, never run: `usermod` is root's, and this program never escalates (ADR-0088).
    fn command(&self, group: &str) -> String {
        format!("sudo usermod -aG {group} {}", self.name)
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
/// The account is not an item either: it is the machine's own, and the whole question about it is whether it may open
/// the daemon's socket. That is read, its one command is printed as the line's remark, and the run continues — because
/// the tree's writes are this process's and the membership does not change them.
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
///
/// The first four are the install tree's, read by [`read`] and run by [`apply`] as `install` and `chmod` children. The
/// last two are the release tree's, which [`crate::release`] owns: their read is a placement question the tree
/// answers itself ([`Release::placement`], [`Release::link`]) and their apply is its own two writes, because a
/// release directory may hold only one binary per tag and a binary is staged beside its name and renamed into place.
/// They are `Action`s all the same, so a plan and an apply stay one list in one vocabulary.
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
    /// One release's two files, `releases/<tag>/holosmith` and its `.sha256`, from this run's own bytes.
    ///
    /// The bytes are a shared handle rather than a copy of the payload: [`own_copy`] cached them for the process, a
    /// plan lives as long as the run, and a deploy that held two copies of a 40 MB artifact to write one of them
    /// would be paying memory for a value it already has.
    Release {
        tree: Release,
        tag: String,
        bytes: Arc<Vec<u8>>,
        digest: String,
    },
    /// `current` is pointed at `releases/<tag>`: absent, or naming another tag. A `current` that names this tag with
    /// no binary behind it needs no switch — the release above is what makes that case resolve.
    Current { tree: Release, tag: String },
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
            // The two files and the digest that names them, in the tree's own words: no child writes these, so the
            // line names the paths rather than an argv.
            Self::Release {
                tree, tag, digest, ..
            } => format!(
                "{} and {}, from this binary (sha256 {})",
                tree.binary(tag).display(),
                tree.dir_of(tag).join(release::DIGEST).display(),
                &digest[..12]
            ),
            // A switch as an operator would write it; the apply makes it atomic with a staged link and a rename.
            Self::Current { tree, tag } => format!(
                "{} -> {}",
                tree.current().display(),
                Path::new(release::RELEASES).join(tag).display()
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

/// The apply: read the account, then every item, then run the pending actions in order, answering the step's line.
///
/// The account is read **first** and its clause is the line's remark whatever happens, because that membership is the
/// one part of this step no unprivileged run can change and a reader has to see it beside the list (ADR-0085).
fn install(
    config: &Config,
    root: &Root,
    tools: &Tools,
    dry_run: bool,
    staging: &Path,
    kind: Kind,
) -> Line {
    let group = kind.group(config.docker_host.as_deref());
    // No group means no account question: a hub drives no daemon, and a daemon reached over the network is not entered
    // through a group — so in either case this step asks `id` nothing and says nothing about an account (item 10 §6's
    // "without the docker group").
    let account = match group {
        None => None,
        Some(group) => match read_account(tools, group) {
            Ok(account) => Some(account),
            Err(sentence) => return failed(DeployStep::Files, sentence),
        },
    };
    // The clause the line carries about the account, and — when the answer is no — the **one** command that would put
    // it right. It is printed in both a dry run and an apply: a tree this run writes into an account that cannot open
    // the daemon's socket is a machine that boots into `ExecStartPre` failing, so the words that fix it sit beside the
    // count rather than being lost with a failure that only a broken run prints.
    let mut still_to_run = false;
    let remark = match (group, &account) {
        (Some(group), Some(account)) if account.may_open(group) => format!(
            "; the account {} is in the {group} group, so it may open the daemon's socket",
            account.name
        ),
        (Some(group), Some(account)) => {
            still_to_run = true;
            format!(
                "; the daemon's socket is owned by the {group} group, and this run never escalates: still yours to \
                 run is `{}`, then log out and back in, because a group reaches a login only at the next one",
                account.command(group)
            )
        }
        _ => String::new(),
    };
    // The root is one of the items, and it is the only one this process may not be able to make: on a machine its
    // parent is the account's own home directory. The apply below attempts it like any other write and says what to
    // do with its failure.
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
            // `satisfied` is "nothing for this run to do", and an account outside the socket's group is something
            // left to do — not by this run, which never escalates, but by the operator whose command it printed.
            word: match still_to_run {
                true => Word::Needs,
                false => Word::Satisfied,
            },
            sentence: format!(
                "{} holds the data root, the secrets and the keys with their modes, and the config is {}{remark}",
                root.describe(),
                root.config_file().display()
            ),
            fix: None,
        };
    }
    if dry_run {
        return Line {
            step: DeployStep::Files,
            word: Word::Needs,
            sentence: counted(&plan, &remark),
            fix: None,
        };
    }
    for action in &actions {
        if let Err(sentence) = apply(action, tools, staging) {
            // The root is the one action whose failure is not a tree's problem: it is the account's own directory
            // (ADR-0088), so a parent this process cannot write is a tree to make or a `--root` to name, never an
            // ownership to hand over.
            let at_root = matches!(action, Action::Directory { path, .. } if path == root.path());
            return failed(
                DeployStep::Files,
                match at_root {
                    true => format!(
                        "{sentence} — {} is the root, the account's own state directory: make it as the account \
                         that runs this deploy, or name a tree you can write with `--root`",
                        root.path().display()
                    ),
                    false => sentence,
                },
            );
        }
    }
    Line {
        step: DeployStep::Files,
        word: Word::Changed,
        sentence: counted(&plan, &remark),
        fix: None,
    }
}

/// What `id` answers about the account running this deploy: `id -un` for the name and `id -nG` for the groups.
///
/// Two reads rather than one, because they are two facts: the name is what the one printed command spells, and the
/// group list is the answer to "may this account open the daemon's socket". A spawn failure, or an `id` that answers
/// nothing — an empty line for either question — is a **failed step** rather than a guess: a name this machine cannot
/// read is a `usermod` this step cannot spell, the empty string is not an account, and no groups is not "in none",
/// which would print a `usermod` for an account that may already be in the group.
///
/// Visible to the crate because `install` asks the same question **before** it writes anything, and asks it of the
/// same account: the command its refusal carries is the one this name spells.
pub(crate) fn read_account(tools: &Tools, group: &str) -> Result<Account, String> {
    let name = read_name(tools)?;
    let groups = ask(
        &tools.id,
        &["-nG"],
        "this step asks whether the account running it is in the daemon's group before it prints the command",
    )?
    .filter(|groups| !groups.is_empty())
    .ok_or_else(|| {
        format!(
            "`{} -nG` named no groups, so whether this account may open the daemon's socket is a question this run \
             cannot answer — and the one command it would print is `sudo usermod -aG {group} {name}`",
            tools.id.display()
        )
    })?;
    Ok(Account {
        name,
        groups: groups.split_whitespace().map(str::to_string).collect(),
    })
}

/// `id -un`: the account this process is running as, which is also the account every printed command names.
///
/// Apart from [`read_account`] because a run with no daemon's-group question still needs the name: `install` reads
/// this account's linger flag with it, and a half that opens no socket asks `id` nothing about its groups.
pub(crate) fn read_name(tools: &Tools) -> Result<String, String> {
    ask(
        &tools.id,
        &["-un"],
        "this step names the account it is running as before it prints a command for it",
    )?
    .filter(|name| !name.is_empty())
    .ok_or_else(|| {
        format!(
            "`{} -un` named no account, and the one command this step prints is a `usermod` for the account that \
             ran it: nothing here can guess the name",
            tools.id.display()
        )
    })
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
/// `remark` is the clause a few steps carry about what the run does *not* do — the account's group membership, the
/// unit's install line, a `base_image` this step does not build. It sits after the count rather than after the last
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
/// These are the deploy's **questions** — `id -un`, `id -nG` — so none of them is prefixed by anything: a question
/// needs no privilege, and a spawn failure is a sentence rather than a "no" (a program this machine does not have
/// cannot answer who is running the deploy).
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
/// are one sentence shape wherever they happen. The release tree's two actions are the exception, and deliberately:
/// they are [`Release`]'s own writes, because a release is a directory holding one binary per tag and a binary that
/// must be whole before it is named.
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
        // The release tree's two writes are its own module's, not children's: one binary per tag, written beside its
        // name and renamed into place, and a link replaced in one syscall.
        Action::Release {
            tree,
            tag,
            bytes,
            digest,
        } => tree.write(tag, bytes.as_slice(), digest).map(drop),
        Action::Current { tree, tag } => tree.switch(tag).map(drop),
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
        fix: None,
    }
}

/// A failed step's line whose repair this step knows better than a re-run: the one command a terse report's
/// `fix:` line carries ([`Line::fix`]).
fn failed_with_fix(step: DeployStep, sentence: String, fix: String) -> Line {
    Line {
        step,
        word: Word::Failed,
        sentence,
        fix: Some(fix),
    }
}

/// Step 3: the three images, built from the Dockerfiles embedded in this binary (ADR-0057, ADR-0114).
///
/// The answer is the step's line **and** whether the build that stopped it could not fetch a pinned source: that second
/// bit is the one failure here that is not a verdict about the tree, and [`Report::unanswered`] carries it to
/// [`Report::exit_code`] (ADR-0111).
///
/// The read is each image's **labels**, compared with what this binary would build: not the tag, which any
/// `docker tag` can move, and not a build timestamp, which says nothing about what is inside. One of those labels
/// is a digest of the binary the build copies, because the git tag cannot tell two builds of the same commit (or
/// of the same dirty tree) apart — a rebuilt artifact is a rebuild, and the acceptance's word is "the binary".
///
/// The binary is read before the read as well as before the apply: the digest is a property of those bytes, so a
/// deploy that cannot read its own path cannot say whether an image is current, and says that instead of claiming
/// `satisfied` on three strings.
fn images_line(
    config: &Config,
    machine: &dyn Machine,
    dry_run: bool,
    progress: &mut dyn Progress,
) -> (Line, bool) {
    // The digest of *this* binary, hashed once: it is the label that makes a rebuilt artifact a rebuild, and the
    // read needs it as much as the apply does — a read that skipped it would answer `satisfied` over an image
    // holding the previous program (the review found exactly that before this line existed).
    let digest = match images::own_digest() {
        Ok(digest) => digest,
        Err(sentence) => return (failed(DeployStep::Images, sentence), false),
    };
    // One expectation's values are the `satisfied` sentence's, because all three images carry the same four labels:
    // version, git tag, pinned pi and the binary's digest. One spelling of what step 3 would build, for both
    // sentences.
    //
    // `outstanding` is the read's one home: a promotion asks the same question for another release's
    // version and digest, so the *rule* -- labels, proxy first, the read's own sentence as the reason -- must not
    // live twice.
    let release = crate::version::VersionInfo::current();
    let built = Expectation::of_release(ImageKind::Base, &release, &digest);
    let pending: Vec<(ImageKind, String)> = images::outstanding(machine, &release, &digest);
    let remark = image_remark(config);
    if pending.is_empty() {
        // The digest is clipped: a line an operator reads with 64 hex characters in it is a line nobody finishes,
        // and the twelve characters it does carry are enough to see two builds differ.
        return (
            Line {
                step: DeployStep::Images,
                word: Word::Satisfied,
                sentence: format!(
                    "{}, {} and {} carry this binary's labels: version {}, git tag {}, pi {}, binary sha256 {}…{remark}",
                    ImageKind::Proxy.image(),
                    ImageKind::Base.image(),
                    ImageKind::Agent.image(),
                    built.version,
                    built.git_tag,
                    built.pi_version,
                    &built.binary[..12],
                ),
                fix: None,
            },
            false,
        );
    }
    let reasons: Vec<String> = pending.iter().map(|(_, reason)| reason.clone()).collect();
    if dry_run {
        return (
            Line {
                step: DeployStep::Images,
                word: Word::Needs,
                sentence: counted(&reasons, &remark),
                fix: None,
            },
            false,
        );
    }
    let binary = match std::env::current_exe() {
        Ok(binary) => binary,
        Err(error) => {
            return (
                failed(
                    DeployStep::Images,
                    format!(
                        "no image was built: this process's own path could not be read ({error}), and the build's \
                         context is an archive holding the Dockerfile and this program"
                    ),
                ),
                false,
            );
        }
    };
    let label = DeployStep::Images.label();
    // No log file: a deploy's sentence is the whole of what it prints, and a build here leaves nothing behind but
    // the image it tagged.
    match images::ensure(
        machine, &binary, &digest, &release, &pending, &label, None, progress,
    ) {
        Ok(sentence) => (
            Line {
                step: DeployStep::Images,
                word: Word::Changed,
                sentence: format!("{sentence}{remark}"),
                fix: None,
            },
            false,
        ),
        // The bit is the log's own answer, read where the log was (`images::fetch_did_not_answer`): a compile error or
        // a tarball that failed its digest is a `false` and stays exit 1.
        Err(error) => (
            failed(DeployStep::Images, error.sentence),
            error.fetch_did_not_answer,
        ),
    }
}

/// What step 3 must say when the config's `base_image` is not one of the names it builds.
///
/// Never a failure: `base_image` is the image Holosmith owns and a project extends, and an operator may point it at
/// an image built by hand (`pi-agent:local`) — that is the escape hatch a development machine and the gate use. But
/// a deploy that has just built step 3's three images while the config names something else has installed an image
/// no session will use, and a line that said nothing would leave that to be discovered at a launch.
fn image_remark(config: &Config) -> String {
    // Every name the step builds is a name the config may carry: a machine whose projects are this tree names
    // `agent-image:<version>`, and a line saying of it "which this step does not build" would be false.
    let built = [
        images::proxy_image(),
        images::base_image(),
        images::agent_image(),
    ];
    if built.contains(&config.base_image) {
        return String::new();
    }
    format!(
        "; the config's `base_image` names {}, which this step does not build",
        config.base_image
    )
}

/// Step 4: the machine's `egress` network and its three proxies.
///
/// The read is a real measurement of the daemon — [`proxies::running`] for the three containers and
/// `docker network ls` for the network — which is what makes a second deploy honestly `satisfied` rather than a
/// re-application. Everything present is exactly the state a deploy is for, so the apply only runs when something
/// is missing.
///
/// **`[egress] allow` is not reconciled here, and that is the design rather than a gap.** The list is an argument
/// of `docker run`, so a changed allow-list takes effect on the next `serve` boot, which replaces every proxy
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
    let [egress, key, mcp] = proxies::running(config, machine);
    // The names this run's config composes: `hs_egress`/`hs_key`/`hs_mcp` on a deployed machine, and a smoke
    // config's own three if a deploy is ever run with one.
    let names = proxies::names(config);
    if network && egress && key && mcp {
        return Line {
            step: DeployStep::Proxies,
            word: Word::Satisfied,
            sentence: format!(
                "{} is there and {}, {} and {} are running on it",
                docker::EGRESS_NETWORK,
                names[0],
                names[1],
                names[2]
            ),
            fix: None,
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
        (
            mcp,
            names[2].as_str(),
            "the only thing that spends a tool's real key",
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
            fix: None,
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
    if let Err(sentence) = proxies::start_mcp(config, machine) {
        return failed(DeployStep::Proxies, sentence);
    }
    Line {
        step: DeployStep::Proxies,
        word: Word::Changed,
        sentence,
        fix: None,
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
/// `role` is the role the **machine** will hold (`deploy::union`'s answer), not the half this run installs, and
/// `Description=` is the only place it is read.
///
/// **The unit names no system unit at all**, because a user manager does not own those names. `After=` and `Wants=`
/// against one are inert — the unit starts and the clause buys nothing — and `Requires=` is not: systemd refuses the
/// whole job before `ExecStartPre` runs, so a `server`/`both` unit carrying `Requires=docker.service` never started
/// and left no journal entry to say why (measured on a `both` machine; ADR-0088 D2 carries the correction). Docker
/// readiness is a runtime fact instead: `ExecStartPre` runs `doctor --quick`, whose `docker` finding refuses with
/// the sentence that names the fix, and `Restart=on-failure` retries. `StartLimitIntervalSec=0` is what makes that
/// retry indefinite rather than a burst the default five-per-ten-seconds limiter could end.
pub fn unit_text(exe: &Path, role: Role, root: &Root) -> String {
    format!(
        "[Unit]\n\
         Description=Holosmith {role}\n\
         StartLimitIntervalSec=0\n\
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

/// What the operator must run to make the rendered unit the one their user manager loads, in order — the argv a
/// caller runs, which [`unit_commands_printed`] renders for the sentence an operator reads.
///
/// No `sudo` and no copy: the bytes are already at [`Root::unit`], so the pair is the reload that makes the manager
/// notice the file and the enable that starts it now, at every login, and at boot once linger is on.
///
/// **One value, because two callers disagree about who runs them and nothing about what they say.** A deploy prints
/// these and never runs one; an install runs them and repeats them in the sentence a foreign tree gets instead. A flag
/// or a unit name added here therefore reaches both, and a sentence cannot describe a command nobody ran.
pub(crate) fn unit_commands() -> [&'static [&'static str]; 2] {
    [
        &["--user", "daemon-reload"],
        &["--user", "enable", "--now", UNIT_NAME],
    ]
}

/// [`unit_commands`] as lines, with `program` in front of each argv — the program the reader would type, which is
/// the bare name in the deploy's remark and the path an install actually ran in its own.
pub(crate) fn unit_commands_printed(program: &Path) -> Vec<String> {
    unit_commands()
        .iter()
        .map(|arguments| format!("{} {}", program.display(), arguments.join(" ")))
        .collect()
}

/// Step 5's read and apply: the file's bytes, the release tree that binary runs from, and the commands that enable
/// the unit.
fn service(config: &Config, root: &Root, tools: &Tools, dry_run: bool) -> Line {
    let staging = staging_dir();
    let line = service_line(config, root, tools, dry_run, &staging);
    let _ = std::fs::remove_dir_all(&staging);
    line
}

/// Where `ExecStart` will point, and the release-tree writes that put a binary there.
///
/// The two are one value because they are one decision: the unit names `current/holosmith` only when the tree can be
/// made to hold this binary, and a tree that cannot be used is a unit naming [`Self::path`] as it stands.
struct Executable {
    /// The path the unit names: a live release's `current/holosmith`, or the binary this run is running.
    path: PathBuf,
    /// The release tree's writes, in the order they must run — the release, then `current`, and the unit after them.
    /// Empty when the tree is live (a deploy never reseeds one) or could not be used.
    actions: Vec<Action>,
    /// One clause for the step's line: why the tree was not seeded, or what a live tree does not replace.
    remark: String,
    /// The binary this run is running, which is what the unit names when the tree's own writes fail: the fourth arm,
    /// and the reason it is carried rather than read again is that a seed that got this far has already read it once.
    /// `None` when there is no seed to fail — a live tree, or a tree that could not be read, neither of which plans a
    /// release write.
    running: Option<PathBuf>,
}

fn service_line(
    config: &Config,
    root: &Root,
    tools: &Tools,
    dry_run: bool,
    staging: &Path,
) -> Line {
    // `ExecStart` names the **release tree's** binary: the live release's when `current` resolves, and this binary
    // planted as a release when it does not.
    //
    // The live arm is the one fact a promotion changes and a deploy must not undo: a machine whose `current` symlink
    // resolves is a machine that systemd should start *through* it, so a switch is what a restart picks up. The
    // seeding arm is what makes the path this run was started from stop mattering — a binary in a build directory or
    // a `deploy --ssh` copy under `~/.cache` is the file the tree is planted from, never the file the unit runs. The
    // unit's bytes are rendered from the same function either way, which is why this read and the promotion's write
    // agree byte for byte -- and why a second deploy on either tree says `satisfied`.
    let executable = match executable(root) {
        Ok(executable) => executable,
        Err(sentence) => return failed(DeployStep::Service, sentence),
    };
    // The release tree's writes run **before** the unit's, because the unit names a path through `current` and the
    // link has to resolve when systemd first reads the file. A write that fails here is the **fourth** arm, and it
    // follows the rule the three reads share: it costs the seed and not the install. So the unit is rendered from the
    // running path instead, and the line says what failed — a machine whose `current` is a directory still ends with
    // a unit that names a binary that exists.
    let mut plan: Vec<String> = Vec::new();
    for action in &executable.actions {
        if !dry_run && let Err(sentence) = apply(action, tools, staging) {
            let Some(running) = executable.running.as_deref() else {
                return failed(DeployStep::Service, sentence);
            };
            return unit_line(
                config,
                root,
                tools,
                dry_run,
                staging,
                running,
                Vec::new(),
                &unseeded(&sentence),
            );
        }
        plan.push(action.describe());
    }
    unit_line(
        config,
        root,
        tools,
        dry_run,
        staging,
        &executable.path,
        plan,
        &executable.remark,
    )
}

/// The unit's own write, for the path this run settled on, and the line that says what happened.
///
/// `plan` is the release tree's actions already applied; `remark` is the clause the step earned — why the tree was
/// not seeded, or what a live one does not replace — and the two commands that are still the operator's follow it.
#[allow(clippy::too_many_arguments)]
fn unit_line(
    config: &Config,
    root: &Root,
    tools: &Tools,
    dry_run: bool,
    staging: &Path,
    path: &Path,
    mut plan: Vec<String>,
    remark: &str,
) -> Line {
    let bytes = unit_text(path, config.role, root);
    let unit = Item {
        path: root.unit(),
        mode: 0o644,
        kind: ItemKind::Rendered(bytes.clone().into_bytes()),
    };
    let write = match read(&unit) {
        Ok(write) => write,
        Err(sentence) => return failed(DeployStep::Service, sentence),
    };
    if let Some(action) = &write {
        plan.push(action.describe());
    }
    // The clause the line carries: why the release tree could not be used (or why a live one was left alone), then the
    // two commands that are still the operator's. Both renderings take it — the terse one as the indented line under
    // the mark — because which binary `ExecStart` names is the fact a reader came to this line for.
    let remark = format!(
        "{}; still yours to run: {}",
        remark,
        unit_commands_printed(Path::new("systemctl")).join("; ")
    );
    if plan.is_empty() {
        return Line {
            step: DeployStep::Service,
            word: Word::Satisfied,
            sentence: format!(
                "{} holds this binary's unit ({} bytes){remark}",
                root.unit().display(),
                bytes.len()
            ),
            fix: None,
        };
    }
    if dry_run {
        return Line {
            step: DeployStep::Service,
            word: Word::Needs,
            sentence: counted(&plan, &remark),
            fix: None,
        };
    }
    if let Some(action) = &write
        && let Err(sentence) = apply(action, tools, staging)
    {
        return failed(DeployStep::Service, sentence);
    }
    Line {
        step: DeployStep::Service,
        word: Word::Changed,
        sentence: counted(&plan, &remark),
        fix: None,
    }
}

/// The binary `ExecStart` names, and the release-tree writes that make it one inside the install tree.
///
/// The arms, in the order they are asked: a **live release** (`current` resolves) is named and left alone; **no live
/// release** means this binary is planted as one and named through `current`; and a tree that **cannot be seeded**
/// falls back to naming the binary this run is running. That last arm is why nothing here refuses over a tree it
/// cannot use: a machine with a unit that names a build directory is better than a machine with no unit at all, so an
/// unusable tag or a directory already holding another binary under this tag costs the seeding and not the install.
fn executable(root: &Root) -> Result<Executable, String> {
    let release = Release::of(root);
    if let Some(live) = release.live() {
        return Ok(Executable {
            remark: live_remark(&live),
            path: live,
            actions: Vec::new(),
            running: None,
        });
    }
    let exe = std::env::current_exe().map_err(|error| {
        format!(
            "no unit was written: this process's own path could not be read ({error}), and `ExecStart` names it \
             because a program resolved through systemd's `PATH` is not the binary that wrote the unit"
        )
    })?;
    let unusable = |why: &str| Executable {
        remark: unseeded(why),
        path: exe.clone(),
        actions: Vec::new(),
        running: None,
    };
    // The tag is **this process's own**, read from the value the build put in it: a promotion spawns
    // `<candidate> version --json` because it is reading a foreign file, and that spawn here would answer a question
    // about bytes this run has not got.
    let tag = crate::version::VersionInfo::current().git_tag;
    if release::tag_problem(&tag).is_some() {
        return Ok(unusable(&format!(
            "this binary's git tag {tag:?} cannot name a release directory, and a release is `releases/<tag>/`"
        )));
    }
    let (bytes, digest) = match own_copy() {
        Ok(copy) => copy,
        Err(why) => return Ok(unusable(&why)),
    };
    let placement = match release.placement(&tag, digest) {
        Ok(placement) => placement,
        Err(why) => return Ok(unusable(&why)),
    };
    // A release is a **write** only when there is something to write: `Written::Current` with the digest file already
    // holding this digest is the tree saying the release is here, and `write` would rewrite one file with the content
    // it has. Counting that as an action would make the plan's number a thing the run did not do.
    let recorded = std::fs::read_to_string(release.dir_of(&tag).join(release::DIGEST))
        .ok()
        .and_then(|line| crate::remote::digest_of(&line));
    let held = matches!(placement, release::Written::Current)
        && recorded.as_deref() == Some(digest.as_str());
    let mut actions = Vec::new();
    if !held {
        actions.push(Action::Release {
            tree: release.clone(),
            tag: tag.clone(),
            bytes: Arc::clone(bytes),
            digest: digest.clone(),
        });
    }
    // A `current` that already names this release needs no switch: the release above is what makes it resolve. Every
    // other state — no link, a link to another tag, a link whose directory holds no binary — is repaired by one.
    let link = release.link();
    let named = link.as_deref().and_then(|link| release.tag_of(link));
    if named.as_deref() != Some(tag.as_str()) {
        actions.push(Action::Current {
            tree: release.clone(),
            tag: tag.clone(),
        });
    }
    // A link this tree does not understand — one naming something that is not `releases/<tag>` — is **repaired**
    // rather than refused, because a deploy is the command that puts a machine back in a state it can boot from, and
    // a promotion is the one that refuses this state instead (it needs a release to go back to, ADR-0082). The line
    // says which target it replaced: a link that moved with no word is the one thing a `(2 actions)` cannot show.
    let remark = match &link {
        Some(link) if named.is_none() => format!(
            "; `{}` named {}, which is not a release of this tree, and it now names `releases/{tag}`: a deploy \
             repairs the link a promotion refuses",
            release.current().display(),
            link.display()
        ),
        _ => String::new(),
    };
    Ok(Executable {
        path: release.current().join(release::BINARY),
        actions,
        remark,
        running: Some(exe),
    })
}

/// The clause a step earns when the release tree could not be used: why, and what `ExecStart` names instead.
///
/// One function so the four arms — an unusable tag, unreadable bytes, a refused placement, a failed write — are one
/// sentence shape, and so the fallback the read took and the fallback the write takes read the same.
fn unseeded(why: &str) -> String {
    format!("; no release was planted, so `ExecStart` names the binary this run is running: {why}")
}

/// What [`own_copy`] caches: this process's bytes, and the digest that names them.
///
/// One name rather than the tuple written twice, because the cache's type and the reader's answer are the same value
/// and a reader that spelled it out again would be one edit away from disagreeing with the cache.
type OwnCopy = (Arc<Vec<u8>>, String);

/// This process's own bytes and their digest, read and hashed **once per process**.
///
/// The bytes are why this is not [`images::own_digest`]: the digest is the same value that function caches for step
/// 3's image labels, and a seed needs the bytes beside it, which `own_digest` throws away. Cached for that function's
/// own reason — this process's file cannot change while the process runs, so a second deploy in one process would pay
/// for the read and the hash again for one answer — and handed out as an [`Arc`] so a plan holds a handle to those
/// bytes rather than a second copy of them.
fn own_copy() -> Result<&'static OwnCopy, String> {
    static COPY: std::sync::OnceLock<Result<OwnCopy, String>> = std::sync::OnceLock::new();
    COPY.get_or_init(|| {
        let exe = std::env::current_exe().map_err(|error| {
            format!("this process's own path could not be read ({error})")
        })?;
        let bytes = std::fs::read(&exe).map_err(|error| {
            format!(
                "this process's own bytes could not be read ({error}): a release is a copy of the binary that \
                 plants it"
            )
        })?;
        let digest = images::hex(&images::sha256(&bytes));
        Ok((Arc::new(bytes), digest))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// The clause a live release earns when this run is a **different** binary: the tree is left alone, and the command
/// that does replace a live release is named.
///
/// `server deploy` after a `cargo build` is the case: the unit keeps naming the release that is live, which is the
/// property that makes a deploy unable to undo a promotion, and an operator who expected their new binary to go live
/// needs the one word for what would.
fn live_remark(live: &Path) -> String {
    let (Ok(own), Ok(found)) = (images::own_digest(), images::digest(live)) else {
        return String::new();
    };
    if own == found {
        return String::new();
    }
    format!(
        "; this run's binary is not the live release's (sha256 {} and {}), and a deploy does not replace a live \
         release — `holosmith upgrade` does",
        &own[..12],
        &found[..12]
    )
}

/// Step 6: wait for `/api/health`, then ask `doctor --full` (spec item 10 step 7).
///
/// The read is the same two questions the apply ends on — the endpoint answers **and** nothing in the report blocks
/// the step — so a `satisfied` line here is "this machine is serving, measured", not "the unit was installed".
/// Anything short of that goes to `needs`, because an apply is what can change it (it waits, and a start is what the
/// wait is for).
///
/// The apply's failure is `doctor`'s own sentence, verbatim: step 1's rule, one level later — a deploy, `serve`'s
/// refusal and `doctor`'s line cannot say different things about one machine. `blocking` is the one list the read is
/// made of, and the stored tool key is the failure it leaves out: a refused key is a fact this step reports, not one
/// it stops for (ADR-0101's D2).
fn health(
    config: &Config,
    doctor_tools: &doctor::Tools,
    tools: &Tools,
    machine: &dyn Machine,
    dry_run: bool,
) -> Line {
    let answering = doctor::health_answers(config.control_listen);
    let measured = doctor::run(config, Depth::Full, doctor_tools, machine);
    if answering && blocking(&measured).next().is_none() {
        return Line {
            step: DeployStep::Health,
            word: Word::Satisfied,
            sentence: health_satisfied(config.control_listen, &measured),
            fix: None,
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
            fix: None,
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
    if let Some(finding) = blocking(&measured).next() {
        return failed(DeployStep::Health, finding.sentence.clone());
    }
    Line {
        step: DeployStep::Health,
        word: Word::Changed,
        sentence: counted(&plan, &stored_key_remark(&measured)),
        fix: None,
    }
}

/// The failures of `report` that refuse step 6: every one except the stored tool key's.
///
/// A stored tool key is not a machine requirement (ADR-0101's D2). A machine whose workspace revoked one runs
/// sessions that never call a tool, and an operator who has to re-deploy, re-install or upgrade it must be able to —
/// `holosmith key set <tool>` is the fix, and it is not this command. `doctor`'s own exit code is unchanged: the
/// report measures the machine, this step is the gate, and only one of them is about what the machine needs.
fn blocking(report: &doctor::Report) -> impl Iterator<Item = &preflight::Finding> + '_ {
    report
        .failures()
        .filter(|finding| finding.check != Check::ToolKey)
}

/// The remark a green health line carries when the report holds a refused stored tool key: `doctor`'s sentence for it,
/// verbatim, and the one fact that sentence cannot say — the key is a credential a workspace issues rather than this
/// machine's fitness. Empty when there is no such finding, because a line with nothing to add prints nothing.
///
/// The `; ` is what makes it [`terse`](Line::terse)'s remark rather than a clause of the count, so the report keeps
/// the sentence an operator acts on while the step stays green.
fn stored_key_remark(report: &doctor::Report) -> String {
    let Some(refused) = report
        .failures()
        .find(|finding| finding.check == Check::ToolKey)
    else {
        return String::new();
    };
    format!(
        "; {} — a stored key is a workspace's credential, not this machine's fitness, so neither this step nor \
         `serve` refuses over it",
        refused.sentence
    )
}

/// Step 6's `satisfied` sentence: the origin answers and nothing blocks this step.
///
/// A report whose only failure is a refused stored tool key is this line's case, and the failing clause says so
/// rather than "none failing" — the count stays the report's own, the remark is [`stored_key_remark`]'s.
fn health_satisfied(listen: SocketAddr, measured: &doctor::Report) -> String {
    let remark = stored_key_remark(measured);
    format!(
        "the control origin ({listen}) answers `GET /api/health`, and `doctor --full` reports {} findings with \
         {}{remark}",
        measured.findings.len(),
        match remark.is_empty() {
            true => "none failing",
            false => "only the stored tool key failing",
        },
    )
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

    /// A refused stored tool key is the one failure step 6 reports rather than stops for: no session on a machine
    /// whose workspace revoked a key needs it to boot, so the same fact cannot refuse the deploy that installs one.
    ///
    /// The report is built by hand, because what is being measured is a rule over findings, not a machine: the deploy
    /// tests reach step 6 through a real `doctor` run, which no fixture makes hold an unreadable store.
    #[test]
    fn a_refused_tool_key_is_named_on_the_health_line_and_blocks_no_step() {
        let listen: SocketAddr = "127.0.0.1:8080".parse().expect("a listener address");
        let report = |findings: Vec<preflight::Finding>| doctor::Report {
            depth: Depth::Full,
            findings,
        };
        let refused = preflight::Finding::fail(
            Check::ToolKey,
            "the stored linear key is refused: POST /mcp on mcp.linear.app answered 401, so the key is wrong or has \
             been revoked — issue a new one and store it with `holosmith key set linear`",
        );

        // A machine that is serving, whose only failing finding is the key: nothing here refuses the step, and the
        // line it prints is green with the finding's own sentence on it.
        let serving = report(vec![
            preflight::Finding::pass(
                Check::Docker,
                "the daemon at /var/run/docker.sock answers /_ping",
            ),
            preflight::Finding::unknown(Check::Tailscale, "this machine has no Tailscale"),
            refused.clone(),
        ]);
        assert!(
            blocking(&serving).next().is_none(),
            "a refused stored tool key blocked step 6"
        );
        assert!(
            serving.failed(),
            "doctor's own exit code is the report's, and a refused key still fails it"
        );
        let sentence = health_satisfied(listen, &serving);
        assert!(
            sentence.contains(&refused.sentence),
            "the health line does not carry doctor's sentence verbatim: {sentence}"
        );
        assert!(
            sentence.contains("only the stored tool key failing")
                && sentence.contains("127.0.0.1:8080"),
            "the failing clause is the wrong one for a refused key: {sentence}"
        );
        assert!(
            !sentence.contains('\n'),
            "the step's line is one line: {sentence}"
        );
        // The same report with no such finding: the sentence is the old one, word for word, and no remark.
        let clean = report(vec![
            preflight::Finding::pass(
                Check::Docker,
                "the daemon at /var/run/docker.sock answers /_ping",
            ),
            preflight::Finding::unknown(Check::Tailscale, "this machine has no Tailscale"),
        ]);
        assert_eq!(stored_key_remark(&clean), String::new());
        assert!(
            health_satisfied(listen, &clean).ends_with("reports 2 findings with none failing"),
            "a report with no failure did not say so: {}",
            health_satisfied(listen, &clean)
        );

        // And a machine fact is still blocking: the filter is the tool key alone, not every `--full` check.
        let unserving = report(vec![
            refused,
            preflight::Finding::fail(Check::Proxy, "hs_mcp is not running"),
        ]);
        let blockers: Vec<&preflight::Finding> = blocking(&unserving).collect();
        assert_eq!(blockers.len(), 1, "the step's blockers are {blockers:?}");
        assert_eq!(blockers[0].check, Check::Proxy);
        assert_eq!(blockers[0].sentence, "hs_mcp is not running");
    }

    /// A satisfied line's remark is the one thing the terse rendering keeps, because it is the clause that says
    /// something the mark cannot: which binary `ExecStart` names, or why the release tree was not seeded.
    ///
    /// The default report is what `holosmith install` and `server deploy` print, so a clause that only `--verbose`
    /// showed would be a clause written for nobody (ADR-0085, ADR-0094 D3).
    #[test]
    fn a_satisfied_lines_remark_is_printed_under_its_mark() {
        let line = |word: Word, sentence: &str| Line {
            step: DeployStep::Service,
            word,
            sentence: sentence.to_string(),
            fix: None,
        };
        assert_eq!(
            line(
                Word::Satisfied,
                "/tree/holosmith.service holds this binary's unit (9 bytes)"
            )
            .terse(),
            "5. service ✓",
            "a satisfied line with no remark grew a detail"
        );
        assert_eq!(
            line(
                Word::Satisfied,
                "/tree/holosmith.service holds this binary's unit (9 bytes); still yours to run: systemctl \
                 --user daemon-reload"
            )
            .terse(),
            "5. service ✓\n    still yours to run: systemctl --user daemon-reload",
            "a satisfied line's remark did not reach the terse rendering"
        );
        // The same arm for `skipped`, which shares it: a skip with a remark keeps it, a bare one is the mark alone.
        assert_eq!(
            line(
                Word::Skipped,
                "a hub builds no images: it runs no containers"
            )
            .terse(),
            "5. service –"
        );
        assert_eq!(
            line(
                Word::Skipped,
                "a hub builds no images: it runs no containers; nothing was asked of it"
            )
            .terse(),
            "5. service –\n    nothing was asked of it"
        );
    }
}
