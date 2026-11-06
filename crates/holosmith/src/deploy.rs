//! `holosmith server deploy`: install a server on this machine, in the spec's seven steps (HS-047).
//!
//! The command is one idempotent pass over item 10 §"What `server deploy` does": each step **reads the machine's
//! current state** and then applies only what the read found missing, so a second run changes nothing and reports
//! it (ADR-0056). That pair is why a step cannot be one function that decides and acts — a dry run has to print
//! the read's plan without running the apply, and "changed" has to be distinguishable from "was already right".
//!
//! Every step prints one line, `<n>. <step>: <word>: <sentence>`: [`DeployStep`]'s number is the spec's order and
//! [`Word`] is the read's answer, one of five. The two that carry the design are:
//!
//! - **`skipped` is never `satisfied`.** A step that prints "already satisfied" over a check that read nothing is a
//!   lie of exactly the kind this list exists to prevent, which is why the word is in [`Word`] at all. Since HS-049
//!   every one of the seven steps has a body, so this release has no producer for it — ADR-0056's five words stay
//!   the contract, and the tally line keeps printing the fourth count as zero.
//! - **The first `failed` step stops the run.** Later steps are not evaluated at all: a machine whose preflight
//!   failed has not earned an install, and a report that guessed at the remaining steps would describe a machine
//!   nobody measured. The lines already printed are the describable state the acceptance asks for, and
//!   `commands::deploy` prints the resume command — the command that just ran — as the last line.
//!
//! Three things are deliberately narrow:
//!
//! - **This is the local deploy.** `--ssh`, `--binary` and the remote copy are phase 3 (HS-072); nothing here
//!   reads a remote.
//! - **Step 1 is [`crate::doctor`]'s list, not a second one.** `serve`'s refusal, `doctor`'s line and this step's
//!   failure carry the same sentences (ADR-0028, ADR-0054). The one divergence is `ports`, downgraded from a
//!   failure to a remark, and its doc comment says why.
//! - **The proxies' recipe lives in [`crate::proxies`].** A deploy and a boot must start the same two containers
//!   the same way (ADR-0056), and this module is the caller that fails the step instead of warning.
//!
//! # `--root`, and what it prefixes
//!
//! [`Target`] is *where* the deploy installs: the system paths, or a tree an operator names with `--root` so the
//! whole install can be driven — and tested — by an unprivileged user. It prefixes `data_root` and
//! `projects_root` (and therefore everything inside them), and **nothing the deploy reads about the machine**: the
//! listen addresses, `docker_host` and [`crate::doctor::SYSTEMD_DIR`] stay as they are, because they describe
//! the host rather than the install tree (step 4's proxies used to mount this binary; since ADR-0057 they run
//! the image the machine built, so there is no install-tree path in their vector at all). Privilege follows the same flag — with no
//! `--root` each write is one child prefixed by `sudo`, and with one the tree is the invoking user's — and the flag
//! decides rather than a uid, since "may I write this tree" is a property of the tree.
//!
//! The caller passes a `Config` that [`Target::config`] has already prefixed; that is the contract, and it is why
//! [`run`] reads `config.data_root` directly instead of asking `Target` to prefix it a second time.

use std::fmt::{self, Write as _};
use std::io::{ErrorKind, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::config::{self, Config, Role};
use crate::container;
use crate::docker;
use crate::doctor::{self, Depth};
use crate::images::{self, Expectation, Kind as ImageKind, Status};
use crate::key;
use crate::preflight::{self, Check, Outcome};
use crate::proxies;
use crate::vapid;

/// The admin socket's directory (spec item 10 step 2's "admin socket directory", path fixed by HS-051).
///
/// Public because it is a path `doctor`'s `perms` finding and HS-050's `uninstall` both have to know, and a second
/// spelling of `/run/holosmith` is a second thing to change.
pub const ADMIN_DIR: &str = "/run/holosmith";

/// The user `serve` runs as (spec item 10 step 2's "service user", named by the unit file's `User=`).
pub const SERVICE_USER: &str = "holosmith";

/// The group the service user must be in: `serve` drives the machine's daemon through its socket.
pub const SERVICE_GROUP: &str = "docker";

/// The unit file's name, which is also the argument `systemctl` gets.
pub const UNIT_NAME: &str = "holosmith.service";

/// systemd's system-wide unit directory.
pub const UNIT_DIR: &str = "/etc/systemd/system";

/// The HTTPS port Tailscale serves the control origin on.
pub const CONTROL_HTTPS: u16 = 443;

/// The HTTPS port Tailscale serves the preview origin on.
pub const PREVIEW_HTTPS: u16 = 8444;

/// How long step 7's apply waits for `/api/health` before it calls the unit's start a failure.
pub const HEALTH_WAIT: Duration = Duration::from_secs(60);

/// How often that wait asks again. A serving unit answers in milliseconds; the interval is what a wait costs the
/// command, and 250 ms is four asks a second for a minute.
pub const HEALTH_POLL: Duration = Duration::from_millis(250);

/// The remark a held port earns instead of failing the deploy — step 1's one divergence (see [`preflight`]).
const HELD_PORT: &str =
    "; the control port is held — a re-run on a serving machine is not a failure here";

/// The remark a `--root` install carries where a system one names its service user: the half of step 2 no
/// unprivileged tree can have.
///
/// Without it, "all 7 paths are present with their modes" reads as the whole of item 10 step 2 when the user and
/// the ownership were read about nothing — the same "say what you did not do" shape [`Word::Skipped`] carries.
const ROOT_REMARK: &str =
    "; --root: no service user is created, because the tree belongs to the invoking user";

/// The remark step 5's line carries under `--root`: a unit in an install tree is not one systemd can load.
const ROOT_UNIT_REMARK: &str = "; --root: the unit is in the tree, and this run did not ask systemd to load it (a unit under --root is not one \
     systemd can read)";

/// The spec's seven steps, in the spec's order (item 10, "What `server deploy` does").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeployStep {
    /// 1. The preflight, as `doctor --quick`.
    Preflight,
    /// 2. The install tree: the config file, the data root, the secrets (with the provider and the VAPID keys) and
    ///    the admin socket directory, plus the service user and the tree's ownership.
    Files,
    /// 3. The two images embedded in the binary.
    Images,
    /// 4. The `egress` network and the machine's two proxies.
    Proxies,
    /// 5. The systemd unit.
    Service,
    /// 6. `tailscale serve` for the two origins.
    Origins,
    /// 7. The `/api/health` wait and `doctor --full`.
    Health,
}

impl DeployStep {
    /// Every step, in the order the run evaluates them.
    pub const ALL: [Self; 7] = [
        Self::Preflight,
        Self::Files,
        Self::Images,
        Self::Proxies,
        Self::Service,
        Self::Origins,
        Self::Health,
    ];

    /// The spec's step number, `1..=7`, which is the report line's first field.
    pub fn number(self) -> u8 {
        match self {
            Self::Preflight => 1,
            Self::Files => 2,
            Self::Images => 3,
            Self::Proxies => 4,
            Self::Service => 5,
            Self::Origins => 6,
            Self::Health => 7,
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
            Self::Origins => "origins",
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
    /// Another story owns this step's body. Kept because it is one of ADR-0056's five words and the tally line is a
    /// contract; since HS-049 every step has a body, so nothing in this release produces it.
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

/// What one deploy did, as the operator reads it: a line per step evaluated, the step that stopped the run, and
/// whether anything was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<Line>,
    /// The step whose line is `failed`, which is the exit code and the resume command.
    pub failed: Option<DeployStep>,
    pub dry_run: bool,
    /// [`Target::describe`]: where this run installed, and with which privilege.
    ///
    /// Carried rather than printed by [`Report::render`], whose shape is one line per step plus the two summary
    /// lines: it is here because "which tree did this report describe" is a question a caller asks (and a test
    /// asserts), and answering it from the config would mean guessing whether the paths were prefixed.
    pub target: String,
}

impl Report {
    /// Whether a step failed, which is exactly what the exit code says.
    pub fn failed(&self) -> bool {
        self.failed.is_some()
    }

    /// The whole report as the operator reads it: the header, where it installed, one line per step and the tally.
    ///
    /// The tally's four counts are always printed — `skipped` included, as zero — because a gate greps the line's
    /// shape and a reader diffs two runs by it (ADR-0056). Since HS-049 there is no list of who owns what remains,
    /// because nothing remains: the `deploy: incomplete:` line is gone, not hidden.
    pub fn render(&self) -> String {
        let count = |word: Word| self.lines.iter().filter(|line| line.word == word).count();
        let mut text = format!("deploy: {}\n", if self.dry_run { "dry run" } else { "run" });
        // Which tree this report describes, on its own line: the step lines name paths, and an operator reading a
        // `--root` run needs to see the privilege the whole run used before the paths mean anything.
        let _ = writeln!(text, "deploy: into {}", self.target);
        for line in &self.lines {
            // A `String` cannot fail to be written to, which is why this is not a `?`.
            let _ = writeln!(text, "{line}");
        }
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

/// The machine questions a deploy asks, as one parameter: the image builds' seam, the proxies' and `doctor`'s.
///
/// [`crate::docker::Docker`] gets this for free through the blanket impl below, which is the point —
/// `commands::deploy` builds
/// one daemon handle and the seams that need it share it. A test implements the component traits and stands in
/// for the whole machine, with no daemon at all (the [`crate::doctor::Machine`] pattern).
pub trait Machine: proxies::Containers + doctor::Machine + images::Images {}

impl<T: proxies::Containers + doctor::Machine + images::Images> Machine for T {}

/// The external programs and paths the deploy and its preflight run, as a seam.
///
/// [`doctor::Tools`] is embedded rather than copied, for step 1's reason: the preflight is the same list `serve`
/// refuses on, so it is the same shims. The additions are the programs the steps run — `sudo` because the system
/// paths need it; `install` and `chmod` because every file action is one child process; `openssl` because the VAPID
/// pair is generated by one; `id`, `useradd`, `usermod`, `stat` and `chown` because the service user and the tree's
/// ownership are read and written by them; and `systemctl` because step 5 asks systemd two questions. They are
/// `PathBuf`s for [`crate::preflight::Tools`]'s reason: this crate denies `unsafe` and so cannot set `PATH`, which
/// leaves naming the program as the only way a test can point a write at a script it wrote.
///
/// [`Tools::health_wait`] is the one field that is not a program, and it is here for the same seam's sake: step 7's
/// deadline is a minute on a machine and a test may not spend it, so the caller that would otherwise wait owns the
/// bound. Step 6 runs [`doctor::Tools::tailscale`] rather than a second field — the same program, asked with
/// `serve` instead of `status`.
#[derive(Debug, Clone)]
pub struct Tools {
    pub doctor: doctor::Tools,
    pub sudo: PathBuf,
    pub install: PathBuf,
    pub chmod: PathBuf,
    pub openssl: PathBuf,
    pub id: PathBuf,
    pub useradd: PathBuf,
    pub usermod: PathBuf,
    pub stat: PathBuf,
    pub chown: PathBuf,
    pub systemctl: PathBuf,
    /// How long step 7's apply waits for `/api/health`; [`HEALTH_WAIT`] on a machine.
    pub health_wait: Duration,
}

impl Default for Tools {
    /// The programs from `PATH`, which is what a machine has, and the spec's one-minute health deadline.
    fn default() -> Self {
        Self {
            doctor: doctor::Tools::default(),
            sudo: PathBuf::from("sudo"),
            install: PathBuf::from("install"),
            chmod: PathBuf::from("chmod"),
            openssl: PathBuf::from("openssl"),
            id: PathBuf::from("id"),
            useradd: PathBuf::from("useradd"),
            usermod: PathBuf::from("usermod"),
            stat: PathBuf::from("stat"),
            chown: PathBuf::from("chown"),
            systemctl: PathBuf::from("systemctl"),
            health_wait: HEALTH_WAIT,
        }
    }
}

/// Why `server deploy` may not run here at all, if it may not.
///
/// One case, and it is a role rather than a machine: item 10 §6's `hub deploy` runs steps 1 (without the Docker
/// and git checks), 2 (without the docker group), 5, 6 and 7, and installs no server — so a machine whose config
/// says `hub` is a machine whose `serve` runs no sessions, and `server deploy` there would install the wrong
/// thing and then report success. `hub deploy` itself is phase 3 (HS-072).
pub fn refusal(config: &Config) -> Option<String> {
    (config.role == Role::Hub).then(|| {
        "this machine's role is `hub`, and a hub runs no sessions: `holosmith server deploy` installs a *server*, \
         and installing a hub is `holosmith hub deploy`, which is phase 3 (HS-072)"
            .to_string()
    })
}

/// Where the deploy installs, and whether that takes privilege.
///
/// `None` is the system's own tree (`/etc/holosmith`, `/var/lib/holosmith`, `/run/holosmith`) written through
/// `sudo`; `Some(root)` is an install tree under `root`, written as the invoking user. **Only paths the deploy
/// writes are prefixed** — `data_root` and `projects_root` (and everything inside them), plus the three fixed
/// paths above — while the listen addresses, `docker_host`, [`crate::doctor::SYSTEMD_DIR`] and step 4's binary
/// mount source are facts about the machine and stay as they are. That asymmetry is the flag's whole meaning, so
/// it is stated here rather than left to be inferred.
///
/// **[`run`] is handed a `Config` this type has already prefixed** ([`Target::config`]). The doc comment on
/// [`run`] is where that contract lives; do not prefix a second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    root: Option<PathBuf>,
}

impl Target {
    /// The target an operator's `--root` names, made absolute so the resume command it prints is absolute too.
    ///
    /// `std::path::absolute` rather than canonicalising: the directory may not exist yet, and a deploy's path is
    /// a name rather than something the kernel has already resolved. A path that cannot be made absolute is kept
    /// verbatim, because a relative install tree is still one this run can describe.
    pub fn resolve(root: Option<PathBuf>) -> Self {
        Self {
            root: root.map(|root| std::path::absolute(&root).unwrap_or(root)),
        }
    }

    /// Whether each write needs `sudo`: the system paths do, a tree the operator named does not.
    pub fn privileged(&self) -> bool {
        self.root.is_none()
    }

    /// `<root>/etc/holosmith/config.toml`, or [`config::DEFAULT_CONFIG_PATH`] without a root.
    pub fn etc_config(&self) -> PathBuf {
        self.prefixed(Path::new(config::DEFAULT_CONFIG_PATH))
    }

    /// The data root a **machine** config names, prefixed.
    ///
    /// [`run`] holds the already-prefixed config and reads `config.data_root` instead; this is the question as an
    /// unprefixed config asks it.
    pub fn data_root(&self, config: &Config) -> PathBuf {
        self.prefixed(&config.data_root)
    }

    /// `<data root>/secrets`, the 0700 directory the key file lives in.
    ///
    /// It answers for an **unprefixed** config, which is what a caller outside [`run`] has; the step itself uses
    /// `config.data_root.join(key::SECRETS_DIR)` because the config it holds is already prefixed.
    pub fn secrets_dir(&self, config: &Config) -> PathBuf {
        self.data_root(config).join(key::SECRETS_DIR)
    }

    /// `<root>/run/holosmith`: the admin socket's directory.
    pub fn admin_dir(&self) -> PathBuf {
        self.prefixed(Path::new(ADMIN_DIR))
    }

    /// `<root>/etc/systemd/system/holosmith.service`: the unit file step 5 writes.
    ///
    /// Prefixed like every other installed path, which is exactly why a `--root` unit is one systemd can never
    /// load — the line says so rather than pretending the machine half happened.
    pub fn unit_path(&self) -> PathBuf {
        self.prefixed(&Path::new(UNIT_DIR).join(UNIT_NAME))
    }

    /// The config this deploy installs from: a clone whose two roots are under `root`.
    ///
    /// The two roots are the only fields prefixed, and for a reason each: `data_root` is the tree step 2 creates,
    /// and `projects_root` is what a config the deploy writes would otherwise point at a directory the target does
    /// not have. Nothing else in the config is a path inside the install tree — `control_listen` and
    /// `preview_listen` are addresses, `docker_host` is a socket on the machine, and `agent_image` is an image
    /// name — so prefixing them would describe a host that does not exist.
    pub fn config(&self, config: &Config) -> Config {
        Config {
            data_root: self.data_root(config),
            projects_root: self.prefixed(&config.projects_root),
            ..config.clone()
        }
    }

    /// The phrase [`Report::target`] carries: where this run installs, and with which privilege.
    pub fn describe(&self) -> String {
        match &self.root {
            None => "the system paths, with sudo where each write needs it".to_string(),
            Some(root) => format!("{}, as the invoking user (no sudo)", root.display()),
        }
    }

    /// One path moved under `root`, keeping its own directories: `/etc/holosmith` is `<root>/etc/holosmith`.
    fn prefixed(&self, path: &Path) -> PathBuf {
        match &self.root {
            None => path.to_path_buf(),
            Some(root) => root.join(path.strip_prefix("/").unwrap_or(path)),
        }
    }
}

/// Run the seven steps in the spec's order and answer the report.
///
/// `config` is the value [`Target::config`] produced — already prefixed — so `config.data_root` and
/// `config.projects_root` are the target's own paths and this function prefix nothing.
///
/// `dry_run` is the only thing that separates the two paths this function has: the reads always run, because a
/// plan that read nothing is not a plan, and an apply is only ever reached when `dry_run` is `false`. The first
/// failing line stops the run ([`Report`]'s docs say why), and a `skipped` or `satisfied`/`changed` line
/// continues.
pub fn run(
    config: &Config,
    target: &Target,
    tools: &Tools,
    machine: &dyn Machine,
    dry_run: bool,
) -> Report {
    let mut report = Report {
        lines: Vec::new(),
        failed: None,
        dry_run,
        target: target.describe(),
    };
    for step in DeployStep::ALL {
        let line = match step {
            DeployStep::Preflight => preflight(config, &tools.doctor, machine),
            DeployStep::Files => files(config, target, tools, dry_run),
            DeployStep::Images => images_line(config, machine, dry_run),
            DeployStep::Proxies => proxies_line(config, machine, dry_run),
            DeployStep::Service => service(target, tools, dry_run),
            DeployStep::Origins => origins(config, tools, dry_run),
            DeployStep::Health => health(config, tools, machine, dry_run),
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

/// Step 1: `doctor --quick`'s list, with one finding downgraded.
///
/// Every non-`ports` failure stops the deploy **with that finding's own sentence, verbatim**, so `serve`'s refusal,
/// `doctor`'s line and this step's failure cannot say different things about a machine (ADR-0028, ADR-0054).
///
/// **`ports` is the one divergence, and it is recorded rather than discovered.** `doctor --quick` fails a held
/// port on purpose: its two moments are *before a start* and, through `--full`, *after one*. `deploy` is the one
/// caller that legitimately runs on a machine that is already serving, because the spec promises that re-running
/// is safe — so a held port becomes a remark on this line and the run continues, while `doctor`'s own exit code is
/// unchanged. Without that, the second deploy on a working server would report a failure that is not one.
fn preflight(config: &Config, tools: &doctor::Tools, machine: &dyn Machine) -> Line {
    let report = doctor::run(config, Depth::Quick, tools, machine);
    let mut findings = report.findings;
    // `preflight::disk` answers `unknown` for a data root that does not exist yet, which is exactly the **first**
    // deploy's situation — the run the floor is for. So an unknown disk finding is re-asked against the nearest
    // existing ancestor: the same check, an honest subject. The line says which path was measured, because a
    // reader who asked about `/var/lib/holosmith` and was told `/` is a different fact from one who was not told.
    let mut elsewhere = None;
    if let Some(index) = findings
        .iter()
        .position(|finding| finding.check == Check::Disk && finding.outcome == Outcome::Unknown)
        && let Some((path, finding)) = disk_of_the_nearest_ancestor(config, tools)
    {
        findings[index] = finding;
        elsewhere = Some(path);
    }
    if let Some(finding) = findings
        .iter()
        .find(|finding| finding.is_failure() && finding.check != Check::Ports)
    {
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

/// Step 2: the install tree — four directories and three files, the service user, and the tree's ownership.
///
/// The three files are treated differently on purpose, and that difference is the security property of this step:
/// `config.toml` is the deploy's own rendering and is rewritten when it differs; `keys.toml` holds every stored
/// provider key and is **never** written over — created when absent and mode-repaired with `chmod`, never with
/// `install`, which truncates, and its bytes are not even read; and `vapid.toml` is generated once by `openssl`
/// when it is absent and read-only afterwards, because regenerating it would silence every existing push
/// subscription (HS-049).
///
/// The user and the ownership are not items: they are machine facts rather than install-tree paths, and under
/// `--root` neither is read at all — the tree belongs to the invoking user, and the line says so instead.
///
/// The staging directory is removed whatever happened: a failed step must leave nothing behind but the paths it
/// managed to install.
fn files(config: &Config, target: &Target, tools: &Tools, dry_run: bool) -> Line {
    let staging = staging_dir();
    let line = install(config, target, tools, dry_run, &staging);
    let _ = std::fs::remove_dir_all(&staging);
    line
}

/// One item of the install tree: a path, the mode it must have, and what its bytes are.
struct Item {
    path: PathBuf,
    mode: u32,
    kind: Kind,
}

/// What a path is, which decides how a wrong one is put right.
enum Kind {
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
    /// `useradd --system --groups <group> --home-dir <home> --shell /usr/sbin/nologin <name>`: the service user
    /// does not exist.
    UserAdd {
        name: String,
        groups: String,
        home: PathBuf,
    },
    /// `usermod -aG <group> <name>`: the user exists and the group does not have it.
    UserMod { name: String, group: String },
    /// `chown[-R] <name>:<name> <path>`: the tree is not the service user's, so `serve` (running as it) could not
    /// write it.
    Chown { path: PathBuf, recursive: bool },
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
                "openssl ecparam -name prime256v1 -genkey -noout -outform DER > {}",
                path.display()
            ),
            Self::UserAdd { name, groups, home } => format!(
                "useradd --system --groups {groups} --home-dir {} --shell /usr/sbin/nologin {name}",
                home.display()
            ),
            Self::UserMod { name, group } => format!("usermod -aG {group} {name}"),
            Self::Chown { path, recursive } => format!(
                "chown {}{SERVICE_USER}:{SERVICE_USER} {}",
                if *recursive { "-R " } else { "" },
                path.display()
            ),
        }
    }
}

/// The four directories and three files of item 10 step 2, in the order they must be created — a file's directory
/// first, because `install -m` does not make one, and the key pair after the 0700 directory it lives in.
///
/// The only `Err` is [`config::render`]'s: a path in the config that is not UTF-8, which no config file can carry
/// and which this step will not paper over with an empty body — a `config.toml` this deploy wrote and could not
/// render is a step that failed, not a step that changed something.
fn items(config: &Config, target: &Target) -> Result<Vec<Item>, String> {
    let etc_dir = target
        .etc_config()
        .parent()
        .map_or_else(|| PathBuf::from("/etc"), Path::to_path_buf);
    let secrets = config.data_root.join(key::SECRETS_DIR);
    let rendered = config::render(config).map_err(|error| {
        format!(
            "the config that would be written to {} could not be rendered as TOML ({error}): this is the file \
             `serve` reads, so an empty one would be worse than a failure",
            target.etc_config().display()
        )
    })?;
    Ok(vec![
        Item {
            path: etc_dir,
            mode: 0o755,
            kind: Kind::Directory,
        },
        Item {
            path: target.etc_config(),
            mode: 0o644,
            kind: Kind::Rendered(rendered.into_bytes()),
        },
        Item {
            path: config.data_root.clone(),
            mode: 0o755,
            kind: Kind::Directory,
        },
        Item {
            path: secrets.clone(),
            mode: 0o700,
            kind: Kind::Directory,
        },
        Item {
            path: secrets.join(key::KEYS_FILE),
            mode: 0o600,
            kind: Kind::Kept(key::empty_text().into_bytes()),
        },
        Item {
            path: vapid::store(&config.data_root),
            mode: 0o600,
            kind: Kind::Vapid,
        },
        Item {
            path: target.admin_dir(),
            mode: 0o700,
            kind: Kind::Directory,
        },
    ])
}

/// What `id` answered about the service user before this run wrote anything.
struct ServiceUser {
    /// The uid, when the user exists — the owner the install tree is compared with.
    uid: Option<String>,
    /// Whether the group that owns the daemon's socket has the user.
    grouped: bool,
}

impl ServiceUser {
    /// The action that would make the user what the unit file's `User=` needs, or `None` when it already is.
    fn pending(&self, home: &Path) -> Option<Action> {
        if self.uid.is_none() {
            return Some(Action::UserAdd {
                name: SERVICE_USER.to_string(),
                groups: SERVICE_GROUP.to_string(),
                home: home.to_path_buf(),
            });
        }
        (!self.grouped).then(|| Action::UserMod {
            name: SERVICE_USER.to_string(),
            group: SERVICE_GROUP.to_string(),
        })
    }
}

/// The apply: read the service user, then every item, then the ownership; then run the pending actions in order,
/// answering the step's line.
///
/// The read is complete before the first write, so a plan and a run describe the same machine (ADR-0056). The three
/// groups have an order the apply depends on: `useradd` before `chown`, because the chown names the user; the items
/// before the chowns, because a chown of a path this run creates needs the path to exist; and the user before the
/// items, because the tree a system-path install writes is `sudo`'s and belongs to the service user only after the
/// chowns at the end.
fn install(config: &Config, target: &Target, tools: &Tools, dry_run: bool, staging: &Path) -> Line {
    let items = match items(config, target) {
        Ok(items) => items,
        Err(sentence) => return failed(DeployStep::Files, sentence),
    };
    // 1. The service user, read only where a service user is installed: under `--root` the tree is the invoking
    // user's and there is nobody to create.
    let mut user = Vec::new();
    let mut uid = None;
    if target.privileged() {
        let read = match read_user(tools) {
            Ok(read) => read,
            Err(sentence) => return failed(DeployStep::Files, sentence),
        };
        uid.clone_from(&read.uid);
        user.extend(read.pending(&config.data_root));
    }
    // 2. The seven paths.
    let mut pending = Vec::new();
    for item in &items {
        match read(item) {
            Ok(Some(action)) => pending.push(action),
            Ok(None) => {}
            Err(sentence) => return failed(DeployStep::Files, sentence),
        }
    }
    // 3. The ownership — the service user's, because `serve` writes the data root and the unit runs it as that user.
    if target.privileged() {
        match read_owner(config, target, tools, uid.as_deref()) {
            Ok(chowns) => pending.extend(chowns),
            Err(sentence) => return failed(DeployStep::Files, sentence),
        }
    }
    let pending: Vec<Action> = user.into_iter().chain(pending).collect();
    if pending.is_empty() {
        return Line {
            step: DeployStep::Files,
            word: Word::Satisfied,
            sentence: format!(
                "all {} paths are present with their modes: {}{}",
                items.len(),
                present(&items),
                match target.privileged() {
                    true => format!(
                        ", the service user {SERVICE_USER} is in the {SERVICE_GROUP} group, and the VAPID keys are in \
                         {}",
                        vapid::store(&config.data_root).display()
                    ),
                    false => ROOT_REMARK.to_string(),
                }
            ),
        };
    }
    let remark = if target.privileged() {
        String::new()
    } else {
        ROOT_REMARK.to_string()
    };
    if dry_run {
        return Line {
            step: DeployStep::Files,
            word: Word::Needs,
            sentence: format!("{}{remark}", planned(&pending)),
        };
    }
    for action in &pending {
        if let Err(sentence) = apply(action, target, tools, staging) {
            return failed(DeployStep::Files, sentence);
        }
    }
    Line {
        step: DeployStep::Files,
        word: Word::Changed,
        sentence: format!("{}{remark}", planned(&pending)),
    }
}

/// Whether `id` says the service user exists and is in the daemon's group (spec item 10 step 2).
///
/// Two questions, each `id`'s exit code: `id -u <user>` answers a uid when the user exists, and `id -nG <user>` the
/// groups it is in. A spawn failure is a **failure of the read**, not "there is no user": an `id` this machine does
/// not have cannot tell the deploy whether to create one, and a deploy that guessed would run `useradd` over a user
/// that is already there.
fn read_user(tools: &Tools) -> Result<ServiceUser, String> {
    let uid = ask(
        &tools.id,
        &["-u", SERVICE_USER],
        "the deploy reads whether the service user exists before it creates it",
    )?;
    let Some(uid) = uid else {
        return Ok(ServiceUser {
            uid: None,
            grouped: false,
        });
    };
    let groups = ask(
        &tools.id,
        &["-nG", SERVICE_USER],
        "the group that owns the daemon's socket is a fact the unit's `User=` depends on",
    )?;
    let groups = groups.ok_or_else(|| {
        format!(
            "`{} -nG {SERVICE_USER}` answered non-zero for a user `id -u` found: without its groups this step \
             cannot say whether `usermod` is needed, and it will not add a user to a group it did not read",
            tools.id.display()
        )
    })?;
    Ok(ServiceUser {
        uid: Some(uid),
        grouped: groups
            .split_whitespace()
            .any(|group| group == SERVICE_GROUP),
    })
}

/// The ownership writes the install tree needs, read from the uid `id` answered and the uids `stat` reports.
///
/// The reference is the **service user's** uid, read rather than spelled: that is the identity the unit's `User=`
/// names and the one `serve` writes the tree as. All three installed roots are compared, not only the data root,
/// because the admin directory lives on a tmpfs and is recreated by a privileged `install` after every reboot — a
/// read that only looked at the data root would leave a root-owned 0700 directory the service user cannot put its
/// socket in.
///
/// A path `stat` cannot answer for is one this run is about to create with `sudo install`, which makes it root's
/// until the chown at the end of the same apply; a path that exists and is already the service user's earns
/// nothing, which is what makes the second run's step 2 `satisfied`. A run that is creating the user has no uid to
/// compare, so its tree is the chowns' too.
fn read_owner(
    config: &Config,
    target: &Target,
    tools: &Tools,
    uid: Option<&str>,
) -> Result<Vec<Action>, String> {
    let roots = [
        (config.data_root.clone(), true),
        (target.etc_config(), false),
        (target.admin_dir(), false),
    ];
    let Some(uid) = uid else {
        return Ok(roots
            .into_iter()
            .map(|(path, recursive)| Action::Chown { path, recursive })
            .collect());
    };
    let mut pending = Vec::new();
    for (path, recursive) in roots {
        let owner = ask(
            &tools.stat,
            &["-c", "%u", &path.display().to_string()],
            "the tree `serve` writes must be the service user's, and each path's owner is what says whether it is",
        )?;
        if owner.as_deref().map(str::trim) != Some(uid) {
            pending.push(Action::Chown { path, recursive });
        }
    }
    Ok(pending)
}

/// The item list as a `satisfied` line prints it: each path with its mode, so a reader can see *what* was checked.
fn present(items: &[Item]) -> String {
    items
        .iter()
        .map(|item| format!("{} {:04o}", item.path.display(), item.mode))
        .collect::<Vec<String>>()
        .join(", ")
}

/// A pending list as a line prints it: how many writes, and each one as the program it would run.
fn planned(pending: &[Action]) -> String {
    counted(
        &pending
            .iter()
            .map(Action::describe)
            .collect::<Vec<String>>(),
    )
}

/// The same shape for a list of sentences: the count, then the list — `3 actions: …`, `1 action: …`.
///
/// One helper rather than two, so a reader can rely on the leading number in both steps' lines and on the word
/// after it being "action"/"actions" rather than "1 things".
fn counted(items: &[String]) -> String {
    format!(
        "{} action{}: {}",
        items.len(),
        if items.len() == 1 { "" } else { "s" },
        items.join("; ")
    )
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
                Kind::Directory => Action::Directory {
                    path: path.clone(),
                    mode,
                },
                Kind::Rendered(bytes) | Kind::Kept(bytes) => Action::File {
                    path: path.clone(),
                    mode,
                    bytes: bytes.clone(),
                },
                // Absent is the one state that earns a generated pair (HS-049).
                Kind::Vapid => Action::Vapid { path: path.clone() },
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
    Ok(match &item.kind {
        Kind::Directory => (!metadata.is_dir() || wrong_mode).then(|| Action::Directory {
            path: path.clone(),
            mode,
        }),
        Kind::Kept(_) => {
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
        Kind::Rendered(bytes) => {
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
        Kind::Vapid => {
            if !metadata.is_file() {
                return Err(format!(
                    "{} is not a file, and it is where the machine's Web Push key pair lives: move it aside — the \
                     deploy generates this path only when it is absent",
                    path.display()
                ));
            }
            // The read never generates: absent is the only state that earns a key, which is what makes a second
            // deploy leave the pair — and every push subscription made with it — alone (HS-049).
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
/// These are the deploy's **questions** — `id`, `stat`, `systemctl is-enabled`, `tailscale serve status` — so none
/// of them is prefixed by `sudo`: a question needs no privilege, and a spawn failure is a sentence rather than a
/// "no" (a program this machine does not have cannot answer whether a user exists).
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
/// `install` for a directory or a file, `chmod` for a mode, `useradd`/`usermod`/`chown` for the user and the
/// ownership, and for the pair a generation followed by the same `install -m 0600` a file action uses. Every one
/// goes through [`child`], so a spawn failure and a non-zero answer are one sentence shape wherever they happen: a
/// `sudo` that is not installed is a fact about this machine, not a panic.
fn apply(action: &Action, target: &Target, tools: &Tools, staging: &Path) -> Result<(), String> {
    match action {
        Action::Directory { path, mode } => child(
            tools,
            target,
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
            install_staged(tools, target, &staged, path, *mode)
        }
        Action::Mode { path, mode } => child(
            tools,
            target,
            &tools.chmod,
            vec![format!("{mode:04o}"), path.display().to_string()],
        )
        .map(drop),
        Action::Vapid { path } => {
            // The generation is a **read of a program's output**, not a write: it prints the DER to stdout, and the
            // file is written by the `install` below. So it is not `sudo`-prefixed — asking for the password twice
            // to write one file would be the only thing escalation bought here.
            let generated = execute(
                &tools.sudo,
                false,
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
            install_staged(tools, target, &staged, path, 0o600)
        }
        Action::UserAdd { name, groups, home } => child(
            tools,
            target,
            &tools.useradd,
            vec![
                "--system".to_string(),
                "--groups".to_string(),
                groups.clone(),
                "--home-dir".to_string(),
                home.display().to_string(),
                "--shell".to_string(),
                "/usr/sbin/nologin".to_string(),
                name.clone(),
            ],
        )
        .map(drop),
        Action::UserMod { name, group } => child(
            tools,
            target,
            &tools.usermod,
            vec!["-aG".to_string(), group.clone(), name.clone()],
        )
        .map(drop),
        Action::Chown { path, recursive } => {
            let mut arguments = Vec::new();
            if *recursive {
                arguments.push("-R".to_string());
            }
            arguments.push(format!("{SERVICE_USER}:{SERVICE_USER}"));
            arguments.push(path.display().to_string());
            child(tools, target, &tools.chown, arguments).map(drop)
        }
    }
}

/// `install -m <mode> <staged> <path>`: the one write every file action ends in, staged or generated.
fn install_staged(
    tools: &Tools,
    target: &Target,
    staged: &Path,
    path: &Path,
    mode: u32,
) -> Result<(), String> {
    child(
        tools,
        target,
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

/// One action's child, prefixed by `sudo` when the target is the system's tree.
///
/// The one place an action's argv and its failure sentence are spelled, so a child that cannot be spawned and one
/// that answers non-zero read the same whichever program ran. A successful child's [`Output`] is handed back
/// because generating the key pair is a child whose **stdout** is the answer.
fn child(
    tools: &Tools,
    target: &Target,
    program: &Path,
    arguments: Vec<String>,
) -> Result<Output, String> {
    execute(&tools.sudo, target.privileged(), program, arguments)
}

/// [`child`] without a target: the caller decides whether the system's privilege is involved.
fn execute(
    sudo: &Path,
    escalate: bool,
    program: &Path,
    arguments: Vec<String>,
) -> Result<Output, String> {
    let (program, arguments) = match escalate {
        true => {
            let mut all = vec![program.display().to_string()];
            all.extend(arguments);
            (sudo.to_path_buf(), all)
        }
        false => (program.to_path_buf(), arguments),
    };
    let rendered = format!("{} {}", program.display(), arguments.join(" "));
    // `stdin` is inherited so that `sudo` can ask for a password on the terminal it was run from: a deploy is an
    // operator's foreground command, and the alternative is `sudo: a terminal is required to read the password` on
    // every machine whose credential is not already cached (ADR-0056).
    let output = Command::new(&program)
        .args(&arguments)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| {
            format!(
                "`{rendered}` could not be run ({error}): the deploy reads and writes this machine one child at a \
                 time, each prefixed by `sudo` when it changes the system's own tree"
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
    // `create_new` rather than `write`: the system-path run hands this file to `sudo install`, so an entry a local
    // user planted in the staging directory (a symlink to a root-owned path, say) must be a refusal rather than a
    // followed link. The directory is unique per run and emptied first; this is the half that does not trust it.
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

/// Step 3: the two images, built from the Dockerfiles embedded in this binary (HS-048, ADR-0057).
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
    let proxy = Expectation::of_digest(ImageKind::Proxy, config, &digest);
    let agent = Expectation::of_digest(ImageKind::Agent, config, &digest);
    // Proxy first, which is also the build order: it is the smaller image and the one step 4 needs, so a machine
    // that runs out of room or of the build bound has what its proxies run before the larger one.
    let mut pending: Vec<(ImageKind, String)> = Vec::new();
    for (kind, expectation) in [(ImageKind::Proxy, &proxy), (ImageKind::Agent, &agent)] {
        // The reason is the read's own sentence, so the plan says *why* a build is coming rather than only that
        // one is.
        match images::read(machine, kind, expectation) {
            Status::Current => {}
            Status::Missing(reason) | Status::Stale(reason) => pending.push((kind, reason)),
        }
    }
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
            sentence: format!("{}{remark}", counted(&reasons)),
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
    match images::ensure(machine, &binary, &digest, config, &pending) {
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
    if network && egress && key {
        return Line {
            step: DeployStep::Proxies,
            word: Word::Satisfied,
            sentence: format!(
                "{} is there and {} and {} are running on it",
                docker::EGRESS_NETWORK,
                container::PROXY_NAME,
                key::KEY_PROXY_NAME
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
            container::PROXY_NAME,
            "the only route out of a session's container",
        ),
        (
            key,
            key::KEY_PROXY_NAME,
            "the only thing that holds a real provider key",
        ),
    ] {
        if !running {
            pending.push(format!(
                "replace {name}, {what}, detached with `--restart unless-stopped`"
            ));
        }
    }
    let sentence = counted(&pending);
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

/// Step 5: the systemd unit, rendered from this binary's own path (spec item 10 step 5, HS-049).
///
/// The bytes are the deliverable, which is why the rendering is a public function of exactly two facts — where the
/// binary is, and whether this is a privileged install — and the read compares the file with it byte for byte. The
/// path is [`std::env::current_exe`] and not the literal `holosmith`: this binary does not install itself, and an
/// `ExecStart` resolved through systemd's `PATH` would be a coin flip between the artifact the operator just built
/// and whatever else answers to the name.
///
/// `User=` and `Group=` are the two lines `--root` omits rather than blanks: a unit in an install tree is not one
/// systemd can load, so naming a service user there would describe a machine that does not exist.
pub fn unit_text(exe: &Path, privileged: bool) -> String {
    let identity = if privileged {
        format!("User={SERVICE_USER}\nGroup={SERVICE_USER}\n")
    } else {
        String::new()
    };
    format!(
        "[Unit]\n\
         Description=Holosmith server\n\
         After=network-online.target docker.service\n\
         Wants=network-online.target\n\
         Requires=docker.service\n\
         \n\
         [Service]\n\
         Type=simple\n\
         {identity}\
         ExecStartPre={exe} doctor --quick\n\
         ExecStart={exe} serve\n\
         Restart=on-failure\n\
         RestartSec=5s\n\
         KillMode=mixed\n\
         TimeoutStopSec=30s\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        exe = exe.display()
    )
}

/// Step 5's read and apply: the file's bytes, and on a system-path install the two `systemctl` verbs that make
/// systemd load it.
fn service(target: &Target, tools: &Tools, dry_run: bool) -> Line {
    let staging = staging_dir();
    let line = service_line(target, tools, dry_run, &staging);
    let _ = std::fs::remove_dir_all(&staging);
    line
}

fn service_line(target: &Target, tools: &Tools, dry_run: bool, staging: &Path) -> Line {
    let exe = match std::env::current_exe() {
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
    };
    let path = target.unit_path();
    let bytes = unit_text(&exe, target.privileged());
    // The unit's directory first: `install -m` does not make one, and under `--root` this step is the only thing
    // that ever asks for `/etc/systemd/system`.
    let directory = Item {
        path: path
            .parent()
            .map_or_else(|| PathBuf::from(UNIT_DIR), Path::to_path_buf),
        mode: 0o755,
        kind: Kind::Directory,
    };
    let unit = Item {
        path: path.clone(),
        mode: 0o644,
        kind: Kind::Rendered(bytes.clone().into_bytes()),
    };
    let mut files = Vec::new();
    for item in [&directory, &unit] {
        match read(item) {
            Ok(Some(action)) => files.push(action),
            Ok(None) => {}
            Err(sentence) => return failed(DeployStep::Service, sentence),
        }
    }
    // The machine half. `is-enabled` is one word and one exit code — the question "would a boot start this?" — which
    // is why it is what `satisfied` is compared with rather than a parse of some other command's prose.
    let enabled = if target.privileged() {
        match ask(
            &tools.systemctl,
            &["is-enabled", UNIT_NAME],
            "step 5 asks systemd whether the unit is enabled before it enables it",
        ) {
            Ok(answer) => answer.is_some(),
            Err(sentence) => return failed(DeployStep::Service, sentence),
        }
    } else {
        false
    };
    if files.is_empty() && enabled == target.privileged() {
        let sentence = if target.privileged() {
            format!(
                "{} holds this binary's unit ({} bytes), and `systemctl is-enabled {UNIT_NAME}` answers enabled, so \
                 systemd loads it at boot",
                path.display(),
                bytes.len()
            )
        } else {
            format!(
                "{} holds this binary's unit ({} bytes){ROOT_UNIT_REMARK}",
                path.display(),
                bytes.len()
            )
        };
        return Line {
            step: DeployStep::Service,
            word: Word::Satisfied,
            sentence,
        };
    }
    // The machine half is planned whenever the step applies at all — a *changed* unit has to be re-read before it can
    // be started, so `daemon-reload` is not only for the first install.
    let mut plan: Vec<String> = files.iter().map(Action::describe).collect();
    if target.privileged() {
        plan.push("systemctl daemon-reload".to_string());
        plan.push(format!("systemctl enable --now {UNIT_NAME}"));
    }
    if dry_run {
        return Line {
            step: DeployStep::Service,
            word: Word::Needs,
            sentence: counted(&plan),
        };
    }
    for action in &files {
        if let Err(sentence) = apply(action, target, tools, staging) {
            return failed(DeployStep::Service, sentence);
        }
    }
    if target.privileged() {
        // `daemon-reload` first: `enable --now` on a unit systemd has not re-read would start the previous
        // rendering, which is exactly the version an upgrade is replacing.
        if let Err(sentence) = child(
            tools,
            target,
            &tools.systemctl,
            vec!["daemon-reload".to_string()],
        ) {
            return failed(DeployStep::Service, sentence);
        }
        if let Err(sentence) = child(
            tools,
            target,
            &tools.systemctl,
            vec![
                "enable".to_string(),
                "--now".to_string(),
                UNIT_NAME.to_string(),
            ],
        ) {
            return failed(DeployStep::Service, sentence);
        }
    }
    Line {
        step: DeployStep::Service,
        word: Word::Changed,
        sentence: counted(&plan),
    }
}

/// The control origin's local target: what `tailscale serve` proxies :443 to.
pub fn control_target(config: &Config) -> String {
    format!("http://127.0.0.1:{}", config.control_listen.port())
}

/// The preview origin's local target: what `tailscale serve` proxies :8444 to.
pub fn preview_target(config: &Config) -> String {
    format!("http://127.0.0.1:{}", config.preview_listen.port())
}

/// Step 6: `tailscale serve` for the two origins (spec item 10 step 6, HS-049).
///
/// The read is Tailscale's own description of what it serves, not a file this deploy wrote: `serve status --json`
/// answers one `Web` entry per host:port with the `Proxy` behind `/`, so "is :443 the control origin" is a field
/// comparison. That also makes a second deploy honestly `satisfied`, and it means a Tailscale that cannot be asked
/// is a **failure** rather than a pass — the deploy cannot configure what it cannot read.
///
/// Neither child is `sudo`-prefixed: `tailscale serve` writes to the daemon, not to a root path. `--root` changes
/// nothing here either, because the origins are machine facts — the same asymmetry the docker socket has.
fn origins(config: &Config, tools: &Tools, dry_run: bool) -> Line {
    let status = match execute(
        &tools.sudo,
        false,
        &tools.doctor.tailscale,
        vec![
            "serve".to_string(),
            "status".to_string(),
            "--json".to_string(),
        ],
    ) {
        Ok(output) => output,
        Err(sentence) => {
            return failed(
                DeployStep::Origins,
                format!(
                    "{sentence}: this deploy reads what it configures, so a Tailscale that cannot be asked is a \
                     step that cannot be answered"
                ),
            );
        }
    };
    let served = match served(&String::from_utf8_lossy(&status.stdout)) {
        Ok(served) => served,
        Err(sentence) => return failed(DeployStep::Origins, sentence),
    };
    let wanted = [
        (CONTROL_HTTPS, control_target(config)),
        (PREVIEW_HTTPS, preview_target(config)),
    ];
    let missing: Vec<&(u16, String)> = wanted
        .iter()
        .filter(|(port, target)| {
            !served
                .iter()
                .any(|(served_port, proxy)| served_port == port && proxy == target)
        })
        .collect();
    if missing.is_empty() {
        return Line {
            step: DeployStep::Origins,
            word: Word::Satisfied,
            sentence: format!(
                "`tailscale serve status --json` answers both origins: {} → {} and {} → {}",
                CONTROL_HTTPS, wanted[0].1, PREVIEW_HTTPS, wanted[1].1
            ),
        };
    }
    let plan: Vec<String> = missing
        .iter()
        .map(|(port, target)| format!("tailscale serve --bg --yes --https={port} {target}"))
        .collect();
    if dry_run {
        return Line {
            step: DeployStep::Origins,
            word: Word::Needs,
            sentence: counted(&plan),
        };
    }
    // One child per missing origin, in the order the origins are read: a failure names the port it was configuring
    // and stops, so the report never claims an origin that was not asked for.
    for (port, target) in &missing {
        if let Err(sentence) = execute(
            &tools.sudo,
            false,
            &tools.doctor.tailscale,
            vec![
                "serve".to_string(),
                "--bg".to_string(),
                "--yes".to_string(),
                format!("--https={port}"),
                target.clone(),
            ],
        ) {
            return failed(DeployStep::Origins, sentence);
        }
    }
    Line {
        step: DeployStep::Origins,
        word: Word::Changed,
        sentence: counted(&plan),
    }
}

/// The `Proxy` behind `/` for every `Web` entry of `tailscale serve status --json`, as (port, target).
///
/// The shape is `ipn.ServeConfig`: `Web` is keyed by `host:port` and each entry's `Handlers["/"]` carries the
/// `Proxy` this machine already serves there. An entry without one, or a key without a port, is not an origin this
/// step configured — skipped rather than guessed at.
fn served(json: &str) -> Result<Vec<(u16, String)>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|error| {
        format!(
            "`tailscale serve status --json` printed something that is not JSON ({error}): this deploy reads what \
             it configures, so it can only compare what it can parse"
        )
    })?;
    let mut served = Vec::new();
    let Some(web) = value.get("Web").and_then(serde_json::Value::as_object) else {
        return Ok(served);
    };
    for (host_port, entry) in web {
        let Some((_, port)) = host_port.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        let proxy = entry
            .get("Handlers")
            .and_then(|handlers| handlers.get("/"))
            .and_then(|root| root.get("Proxy"))
            .and_then(serde_json::Value::as_str);
        if let Some(proxy) = proxy {
            served.push((port, proxy.to_string()));
        }
    }
    Ok(served)
}

/// Step 7: wait for `/api/health`, then ask `doctor --full` (spec item 10 step 7, HS-049).
///
/// The read is the same two questions the apply ends on — the endpoint answers **and** no finding fails — so a
/// `satisfied` line here is "this machine is serving, measured", not "the unit was installed". Anything short of
/// that goes to `needs`, because an apply is what can change it (it waits, and a start is what the wait is for).
///
/// The apply's failure is `doctor`'s own sentence, verbatim: step 1's rule, one level later — a deploy, `serve`'s
/// refusal and `doctor`'s line cannot say different things about one machine.
fn health(config: &Config, tools: &Tools, machine: &dyn Machine, dry_run: bool) -> Line {
    let answering = doctor::health_answers(config.control_listen);
    let measured = doctor::run(config, Depth::Full, &tools.doctor, machine);
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
            sentence: counted(&plan),
        };
    }
    if !wait_for_health(config.control_listen, tools.health_wait) {
        return failed(
            DeployStep::Health,
            format!(
                "the control origin ({}) did not answer `/api/health` within {} s: `journalctl -u {UNIT_NAME} -n \
                 50` names why the unit did not come up",
                config.control_listen,
                tools.health_wait.as_secs()
            ),
        );
    }
    let measured = doctor::run(config, Depth::Full, &tools.doctor, machine);
    if let Some(finding) = measured.failures().next() {
        return failed(DeployStep::Health, finding.sentence.clone());
    }
    Line {
        step: DeployStep::Health,
        word: Word::Changed,
        sentence: counted(&plan),
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
