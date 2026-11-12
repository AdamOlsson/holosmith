//! `doctor`: whether this machine is fit to run sessions (HS-045, spec item 10 §1 and §7, V2).
//!
//! The command is two depths of **one list**, because that is how the spec uses it: `doctor --quick` is what
//! `server deploy` step 1 runs and what the unit file runs as `ExecStartPre` (item 10 §5), and `doctor --full`
//! is item 10 §7 — run *after* the deploy, once the machine is already serving — which adds the egress
//! self-test (whose bare probe **is** the network-off probe, ADR-0044/ADR-0046), the two proxies' state and the
//! agent image's pi version.
//!
//! Three rules hold the design together:
//!
//! - **The preflight's three checks are reused, not re-implemented.** They are [`crate::preflight`]'s report,
//!   printed here; `serve` refuses on the same sentences (ADR-0028).
//! - **Only a failure changes the exit code.** An [`Outcome::Unknown`] is "this check did not produce a verdict
//!   about whether `serve` may run here", and its sentence says what was missing and what does need it. That is
//!   ADR-0044's rule for the self-test, applied to a machine with no systemd and no Tailscale daemon — where
//!   `serve` by hand is a supported way to run (ADR-0054).
//! - **`doctor` starts nothing that outlives it.** The self-test removes its own network and probe container,
//!   the image question is a `docker run --rm --network none`, and the proxies are *read*, not started. There is
//!   no unit file to write: `ExecStartPre=holosmith doctor --quick` is HS-049's line, and this story's contract
//!   is the exit code and one actionable line per failure.
//!
//! The one check that is a measurement rather than a question is [`merge`] — V2's "`git merge-tree --write-tree`
//! exits 0 for a clean merge and 1 with paths for a conflict" — and it runs on a throwaway repository under
//! `std::env::temp_dir()`, which is the only way to answer it without owning a project.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agent;
use crate::config::{Config, Role};
use crate::container::PROXY_ALIAS;
use crate::db;
use crate::docker::Docker;
use crate::images::{Expectation, Kind};
use crate::key::KEY_PROXY_ALIAS;
use crate::preflight::{self, Check, Finding, Outcome};
use crate::proxies;
use crate::release::{self, Release};
use crate::schema::{self, Verdict};
use crate::selftest;

/// Where systemd says it is running: a directory, not a process table, because that is the fact a script can
/// read without a subprocess (and `/proc/1/comm` can be `bwrap` or `tini` for reasons that are not "no systemd").
pub const SYSTEMD_DIR: &str = "/run/systemd/system";

/// The three files the merge trial writes, so the cleanup is a directory and not a list.
const MERGE_FILE: &str = "base.txt";
const MERGE_ADDED: &str = "added.txt";

/// How much of the report the operator asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// The list `serve` refuses on and `ExecStartPre` runs: no container, no daemon beyond `_ping`.
    Quick,
    /// Everything `Quick` has, plus the self-test, the proxies and the image's pi version.
    Full,
}

impl Depth {
    /// The word the report's header prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Full => "full",
        }
    }
}

impl std::fmt::Display for Depth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The external programs and paths the checks read, as a seam.
///
/// [`preflight::Tools`] is embedded rather than copied, because the preflight's three checks are the same three
/// `serve` runs: one list of shims, one behaviour. The two additions are doctor's own question — `tailscale`
/// is a program, and systemd is a directory — and both are a `PathBuf` for `preflight::Tools`'s reason: a test
/// points them at what it wrote, because this crate denies `unsafe` and so cannot set `PATH`.
///
/// The last two are the install tree's two machine-wide paths, and they are a seam for a different reason: the
/// tree `perms` reads is the tree the *caller* installed. `server deploy --root DIR` writes `<DIR>/etc/…` and
/// `<DIR>/run/…`, so a check that spelled the system paths itself would compare the host's real install against a
/// target's data root and hand the operator a `chown` on a file the run never wrote (HS-049's review, SPEC-1).
#[derive(Debug, Clone)]
pub struct Tools {
    pub preflight: preflight::Tools,
    pub tailscale: PathBuf,
    pub systemd: PathBuf,
    /// The config file the install wrote: [`crate::config::DEFAULT_CONFIG_PATH`] on a machine.
    pub config_path: PathBuf,
    /// The admin socket's directory: [`crate::deploy::ADMIN_DIR`] on a machine.
    pub admin_dir: PathBuf,
    /// The release tree [`crate::release::ROOT`] on a machine (HS-080), for the release finding.
    pub release_root: PathBuf,
    /// The unit file [`crate::deploy::UNIT_DIR`]/[`crate::deploy::UNIT_NAME`] on a machine: where the release
    /// finding reads the `ExecStart=` that says what systemd runs.
    pub unit_path: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            preflight: preflight::Tools::default(),
            tailscale: PathBuf::from("tailscale"),
            systemd: PathBuf::from(SYSTEMD_DIR),
            config_path: PathBuf::from(crate::config::DEFAULT_CONFIG_PATH),
            admin_dir: PathBuf::from(crate::deploy::ADMIN_DIR),
            release_root: PathBuf::from(crate::release::ROOT),
            unit_path: PathBuf::from(crate::deploy::UNIT_DIR).join(crate::deploy::UNIT_NAME),
        }
    }
}

/// The machine questions `--full` asks that need a daemon, as a seam — the [`preflight::Tools`] idea applied to
/// the three things a test cannot own.
///
/// [`Docker`] is the only production implementation. The self-test joins it rather than being re-run: it already
/// takes a `Runner`, and `Docker` implements that too, so `doctor` calls the shipped entry point.
pub trait Machine {
    /// Whether a container by this name is running (`Docker::running`).
    fn running(&self, name: &str) -> bool;

    /// The pi version an image runs (`agent::version_of`): the image's own `pi --version` in a `--rm` container
    /// with no network.
    fn image_pi(&self, image: &str) -> Result<String, String>;

    /// The `org.holosmith.*` labels an image carries: `Ok(Some(map))` when it is there, `Ok(None)` when it is not.
    ///
    /// The same read `server deploy`'s step 3 makes, reached through [`crate::images::Images`]'s `Docker` impl
    /// rather than through a second copy of the `docker image inspect` call — one reader, so the label step's
    /// skip rule and this check cannot disagree about what an image says.
    fn image_labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String>;

    /// The network self-test, whose bare probe is the network-off probe (ADR-0044/ADR-0046).
    ///
    /// The names come from the run's own config ([`selftest::Names`]), because a smoke run's probes and proxies carry
    /// its prefix (HS-077) and a machine's own do not.
    fn selftest(&self, names: &selftest::Names, image: &str) -> selftest::Outcome;
}

impl Machine for Docker {
    fn running(&self, name: &str) -> bool {
        Docker::running(self, name)
    }

    fn image_pi(&self, image: &str) -> Result<String, String> {
        agent::version_of(self, image)
    }

    fn image_labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        <Docker as crate::images::Images>::labels(self, image)
    }

    fn selftest(&self, names: &selftest::Names, image: &str) -> selftest::Outcome {
        selftest::run(self, names, image)
    }
}

/// Every check's answer, in the order `doctor` prints them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub depth: Depth,
    pub findings: Vec<Finding>,
}

impl Report {
    /// Whether anything failed, which is exactly what the exit code says.
    pub fn failed(&self) -> bool {
        self.findings.iter().any(Finding::is_failure)
    }

    /// The failures, in order — the "one actionable line per failure" half of the acceptance, for a caller that
    /// wants the sentences without the passes.
    pub fn failures(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|finding| finding.is_failure())
    }

    /// The whole report as the operator reads it: the depth, one line per check, and the tally.
    pub fn render(&self) -> String {
        let mut text = format!("doctor: {}\n", self.depth);
        for finding in &self.findings {
            // A `String` cannot fail to be written to, which is why this is not a `?`.
            let _ = writeln!(text, "{finding}");
        }
        let count = |outcome: Outcome| {
            self.findings
                .iter()
                .filter(|finding| finding.outcome == outcome)
                .count()
        };
        let _ = writeln!(
            text,
            "doctor: {} findings: {} pass, {} fail, {} unknown",
            self.findings.len(),
            count(Outcome::Pass),
            self.failures().count(),
            count(Outcome::Unknown)
        );
        text
    }
}

/// Run the checks `depth` names, in the order the report prints them.
///
/// The order is the operator's: the machine's init, then the three `serve` refuses on (with V2's merge trial
/// beside git), then disk, Tailscale, the ports, the database, the release tree and the installed paths' modes;
/// `--full` appends the container questions. A **hub** is not asked the Docker or git questions at all — item 10 §6's
/// `hub deploy` runs step 1 "without the Docker and git checks", because a hub runs no sessions — while disk,
/// Tailscale, ports, the database, the release tree and the permissions still belong to it (a hub has a data root and
/// a release tree too).
pub fn run(config: &Config, depth: Depth, tools: &Tools, machine: &dyn Machine) -> Report {
    let mut findings = vec![systemd(&tools.systemd)];
    let mut docker_ok = false;
    for finding in preflight::report_with(config, &tools.preflight) {
        match finding.check {
            Check::Docker | Check::Git if config.role == Role::Hub => continue,
            Check::Docker => {
                docker_ok = finding.outcome == Outcome::Pass;
                findings.push(finding);
            }
            Check::Git => {
                let git_ok = finding.outcome == Outcome::Pass;
                findings.push(finding);
                findings.push(merge(&tools.preflight.git, git_ok));
            }
            _ => findings.push(finding),
        }
    }
    findings.push(tailscale(&tools.tailscale));
    // Item 10's two moments: `--quick` runs before a start, when a free port is the pass, and `--full` runs
    // after one, when the control port is held by this machine's own `serve` — so "serving" is what a port in
    // use means there, and it is measured once, by the health endpoint, for both ports.
    let serving = (depth == Depth::Full).then(|| health_answers(config.control_listen));
    findings.extend(ports(config, serving));
    findings.push(database(config));
    findings.push(release_tree(&tools.release_root, &tools.unit_path));
    findings.push(perms(config, tools));
    if depth == Depth::Full {
        // The notification channel is the first `--full` question, and it is **not** asked by `--quick`: `--quick` is
        // the unit's `ExecStartPre` (item 10 §5), and a machine whose notification channel is missing must still boot.
        // See [`openssl`].
        findings.push(openssl(&tools.preflight.openssl));
        let image = config.agent_image.as_str();
        // Why the container questions have no verdict, in the words the report can stand behind: a `hub` is not
        // asked them at all (item 10 §6), and a server whose daemon did not answer is a different fact. Saying
        // "the docker check above failed" on a hub would name a line the report does not have.
        let because = if config.role == Role::Hub {
            "a hub runs no sessions, so item 10 §6 asks it no Docker question"
        } else {
            "the docker check above failed"
        };
        let isolation = selftest::Names::of(config);
        findings.push(network(machine, &isolation, image, docker_ok, because));
        // The two proxies by the names this run's config composes, so a smoke run's `doctor` looks at its own
        // (`smoke_hs_egress`, HS-077) rather than at the machine's.
        let names = proxies::names(config);
        findings.push(proxy(machine, &names[0], PROXY_ALIAS, docker_ok, because));
        findings.push(proxy(
            machine,
            &names[1],
            KEY_PROXY_ALIAS,
            docker_ok,
            because,
        ));
        findings.push(image_check(config, machine, image, docker_ok, because));
    }
    Report { depth, findings }
}

/// Whether `openssl` runs, which a Web Push notification needs and nothing else in this build's request path does
/// (HS-066, ADR-0071's D9).
///
/// **`--full` only**, and that is a decision rather than an ordering accident: `doctor --quick` is the unit's
/// `ExecStartPre` (item 10 §5), so a check there refuses a boot. A missing notification channel is not a machine that
/// cannot run a session — the SSE stream still reports every turn — so refusing a boot over it would trade the whole
/// product for one feature. `--full` is item 10 §7's post-deploy moment, which is where "is everything an operator
/// will expect actually there" belongs.
///
/// A `Pass` with the version line, because the version is what tells an operator which `openssl` their `PATH`
/// resolves: `s_client`'s behaviour and the two `ecparam`/`pkeyutl` commands depend on it, and the machine's own
/// answer is the only honest evidence.
fn openssl(program: &Path) -> Finding {
    let output = match Command::new(program).arg("version").output() {
        Ok(output) => output,
        Err(error) => {
            return Finding::fail(
                Check::Openssl,
                format!(
                    "`{} version` is not runnable ({error}): Web Push notifications are signed and delivered \
                     by `openssl`, so none will be sent until it is installed",
                    program.display()
                ),
            );
        }
    };
    let said = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        let refused = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let refused = if refused.is_empty() { said } else { refused };
        return Finding::fail(
            Check::Openssl,
            format!(
                "`{} version` answered a failure ({refused}): Web Push notifications will not be sent until \
                 it runs",
                program.display()
            ),
        );
    }
    Finding::pass(
        Check::Openssl,
        if said.is_empty() {
            format!("`{} version` runs", program.display())
        } else {
            said
        },
    )
}

/// Whether this machine has systemd, where "not there" is a fact rather than a failure.
///
/// `/run/systemd/system` exists exactly while systemd is the init, and its absence is the supported way to run
/// `serve` by hand (docs/build.md says so). A `server deploy` (HS-049) is where systemd becomes a prerequisite,
/// and that step is where the refusal belongs — refusing here would make `doctor` unrunnable on the machine that
/// runs no systemd and serves perfectly well.
fn systemd(dir: &Path) -> Finding {
    if dir.is_dir() {
        return Finding::pass(
            Check::Systemd,
            format!(
                "{} is there, so systemd is this machine's init",
                dir.display()
            ),
        );
    }
    Finding::unknown(
        Check::Systemd,
        format!(
            "{} is not there, so this machine has no systemd: `serve` by hand is supported and needs none, and \
             `server deploy` (HS-049) is where systemd becomes a prerequisite",
            dir.display()
        ),
    )
}

/// Whether Tailscale is installed, its daemon answers, and the node is logged in.
///
/// Three outcomes and a real failing arm: a daemon that answers "logged out" is a machine an operator can fix
/// with one command, and it is measured rather than inferred. A daemon that does not answer is *unknown*, not a
/// failure — a machine reached only on the LAN does not need Tailscale (ADR-0054).
fn tailscale(tailscale: &Path) -> Finding {
    let output = match Command::new(tailscale).args(["status", "--json"]).output() {
        Ok(output) => output,
        Err(error) => {
            return Finding::unknown(
                Check::Tailscale,
                format!(
                    "the tailscale CLI is not runnable ({error}): a machine reached only on the LAN does not need \
                     it, and a hub's phone access and `tailscale serve` do (spec items 10 §6 and 11)"
                ),
            );
        }
    };
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let said = if said.is_empty() {
            format!("exit {}", output.status.code().unwrap_or(-1))
        } else {
            said
        };
        return Finding::unknown(
            Check::Tailscale,
            format!(
                "the tailscale daemon did not answer ({said}): `serve` by hand does not need it, and a hub's \
                 phone access does — start it with `sudo systemctl start tailscaled` when this machine is to be \
                 reached off-LAN"
            ),
        );
    }
    match backend_state(&String::from_utf8_lossy(&output.stdout)).as_deref() {
        Some("Running") => Finding::pass(
            Check::Tailscale,
            "`tailscale status` reports a logged-in node",
        ),
        Some(state) => Finding::fail(
            Check::Tailscale,
            format!(
                "Tailscale is {state} and this node is not logged in: run `tailscale up` on this machine, which is \
                 what serves the control origin (spec item 10 §6) and lets a phone reach it (item 11)"
            ),
        ),
        None => Finding::unknown(
            Check::Tailscale,
            "`tailscale status --json` printed something without a node state, so whether this machine is logged \
             in was not measured: run `tailscale status` by hand",
        ),
    }
}

/// The node state out of `tailscale status --json`, or `None` when the answer is not that shape.
///
/// JSON rather than the human text: the state is a field with a contract, and `tailscale status`'s prose is not
/// (ADR-0035's "a template asks for the field rather than parsing a sentence", applied to a status).
fn backend_state(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.get("BackendState")?.as_str().map(str::to_string)
}

/// The two listeners `serve` binds: free before a start, or held by this machine's own server after one.
///
/// `serving` is `None` for `--quick` and `Some(health)` for `--full`; see [`run`] for why the moment decides what
/// "in use" means.
fn ports(config: &Config, serving: Option<bool>) -> Vec<Finding> {
    vec![
        port(config.control_listen, "control", serving),
        port(config.preview_listen, "preview", serving),
    ]
}

/// One listener's answer.
fn port(listen: SocketAddr, label: &str, serving: Option<bool>) -> Finding {
    if listen.port() == 0 {
        return Finding::pass(
            Check::Ports,
            format!(
                "{label} {listen} asks the kernel for a port, so there is nothing here to hold"
            ),
        );
    }
    match TcpListener::bind(listen) {
        Ok(listener) => {
            drop(listener);
            Finding::pass(Check::Ports, format!("{label} {listen} is free"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => match serving {
            // Item 10 §7's moment: the deploy has just waited for `/api/health`, so an occupied port is the
            // machine serving rather than somebody else's process.
            Some(true) => Finding::pass(
                Check::Ports,
                format!(
                    "{label} {listen} is held by this machine's own `serve` (`GET /api/health` answers), which is \
                     what a serving machine looks like"
                ),
            ),
            _ => Finding::fail(
                Check::Ports,
                format!(
                    "{label} {listen} is in use ({error}): stop whatever holds it — `ss -ltnp` names the process — \
                     or move this listener to another port in the config file"
                ),
            ),
        },
        Err(error) => Finding::fail(
            Check::Ports,
            format!(
                "{label} {listen} cannot be bound ({error}): check that the address is this machine's and that \
                 the user may bind it, or change the listener in the config file"
            ),
        ),
    }
}

/// Whether `GET /api/health` answers 200 on `listen` — the one question that tells "this machine is serving"
/// from "something else holds the port".
///
/// Public because it has two callers and must answer both the same way: this module's port check, and `server
/// deploy`'s step 7, which waits on it before it calls a unit's start successful (HS-049). A second copy of the
/// request would be a second meaning of "serving".
///
/// **A wrapper, not a request.** Since HS-050 the request itself is [`crate::http::health::ask`], because
/// `holosmith status` reads the same payload for the same reason and two hand-written probes would be two
/// meanings of "this machine is answering".
pub fn health_answers(listen: SocketAddr) -> bool {
    crate::http::health::ask(listen).is_ok()
}

/// The database's schema against this binary's, read **without** migrating it (ADR-0047).
///
/// `serve` needs no restore after a rollback, so it warns on a database a newer release wrote and carries on;
/// `doctor` is the guard that runs before a start, so it refuses. A machine that has never served has no file and
/// that is a pass — a first boot is not a problem to fix.
fn database(config: &Config) -> Finding {
    let path = config.data_root.join(db::DATABASE_FILE);
    match db::recorded_without_migrating(&path) {
        Ok(None) => Finding::pass(
            Check::Database,
            format!(
                "there is no database at {} yet, and `serve` creates and migrates one at startup",
                path.display()
            ),
        ),
        Ok(Some(recorded)) => match schema::verdict(recorded, schema::SCHEMA_VERSION) {
            // The one sentence, from the one comparison: `serve` warns with this string and `doctor` refuses with
            // it (HS-075, ADR-0047).
            Verdict::Newer => Finding::fail(
                Check::Database,
                Verdict::Newer.sentence(recorded, schema::SCHEMA_VERSION),
            ),
            verdict => Finding::pass(
                Check::Database,
                verdict.sentence(recorded, schema::SCHEMA_VERSION),
            ),
        },
        Err(error) => Finding::fail(
            Check::Database,
            format!(
                "the database at {} could not be read without migrating it ({error}): check that this user can \
                 read the data root, because `serve` cannot open it either",
                path.display()
            ),
        ),
    }
}

/// The release tree: what `current` resolves to, and whether the unit can execute through it (HS-080, spec item 10).
///
/// One function with both arms, like every check here, and the arms are worth a table because the verdict is not "is
/// the tree tidy" but "can systemd start this machine":
///
/// | state | outcome |
/// | --- | --- |
/// | `current` resolves to `releases/<tag>/holosmith` | **pass**, naming the tag and the resolved path |
/// | it does not resolve, and the unit's `ExecStart` names this tree's `current/holosmith` | **fail** — systemd cannot exec what the link names |
/// | it does not resolve, and the unit runs something else | **pass** — a machine that has not promoted yet |
/// | it does not resolve, and there is no unit to read | **unknown** — no fact about what systemd would run |
///
/// The failure is a `--quick` failure on purpose: `--quick` is the unit's `ExecStartPre`, and a `current` the unit
/// executes through that resolves to nothing is the boot that pre-line exists to protect — it has already happened,
/// and the sentence names the hand fix. A machine whose unit names a plain binary is *not* that machine: `server
/// deploy` installs a binary and writes `ExecStart=` to it, and the first promotion is what points the unit through
/// `current`, so a broken link there is a tree nobody switched to rather than a machine that cannot start.
///
/// The two paths are [`Tools`]'s, not the machine's, for `perms`'s reason: a `--root` run reads its own install tree.
fn release_tree(root: &Path, unit_path: &Path) -> Finding {
    let tree = Release::at(root);
    let link = tree.link();
    let named = link.as_deref().and_then(|link| tree.tag_of(link));
    let unit = std::fs::read_to_string(unit_path);
    let exe = unit.as_deref().ok().and_then(release::unit_executable);
    let through = exe.as_ref() == Some(&tree.current().join(release::BINARY));

    // The pass every healthy machine answers: the link resolves, so say to what — the tag and the path, because the
    // tag is what a `version` inside it prints and the path is what `systemctl status` would show. The tuple is why
    // there is no `unwrap_or_default`: `live()` is `Some` only when the link named one of this tree's releases, so a
    // resolvable tree always has a tag to name.
    if let (Some(live), Some(tag)) = (tree.live(), named.clone()) {
        let resolved = std::fs::canonicalize(&live).unwrap_or(live);
        let sentence = match &exe {
            Some(exe) if through => format!(
                "{} -> releases/{tag}, and the unit's ExecStart runs {}: the release systemd starts is the one \
                 `current` names",
                tree.current().display(),
                resolved.display()
            ),
            Some(exe) => format!(
                "{} -> releases/{tag} ({}); the unit's ExecStart names {}, so a switch takes effect when the unit is \
                 written again",
                tree.current().display(),
                resolved.display(),
                exe.display()
            ),
            None => format!(
                "{} -> releases/{tag} ({})",
                tree.current().display(),
                resolved.display()
            ),
        };
        return Finding::pass(Check::Release, sentence);
    }

    // Everything below is a tree that does not resolve, so the sentence has to say which way it does not — and
    // whether `current` is a plain entry, because a *directory* named `current` is not replaced by `ln -sfn`.
    let entry = std::fs::symlink_metadata(tree.current());
    let plain = link.is_none() && entry.is_ok();
    let state = if link.is_none() {
        match &entry {
            Ok(meta) if !meta.file_type().is_symlink() => format!(
                "{} is a {} and not a symlink, and the layout is `releases/<tag>` with `current` a symlink",
                tree.current().display(),
                if meta.is_dir() { "directory" } else { "file" }
            ),
            _ => format!("{} does not exist", tree.current().display()),
        }
    } else {
        match &named {
            Some(tag) => format!(
                "{} names releases/{tag}, and {} is not there",
                tree.current().display(),
                tree.binary(tag).display()
            ),
            None => format!(
                "{} names {}, which is not one of this tree's releases (a link here is `releases/<tag>`)",
                tree.current().display(),
                link.as_deref()
                    .map(|link| link.display().to_string())
                    .unwrap_or_default()
            ),
        }
    };

    if through {
        // The remedy names a release that exists, so an operator can copy it out of the sentence; a plain entry has to
        // be removed first, because `ln -sfn` refuses a directory; and a tree with nothing to point at needs a deploy,
        // which is the only command that writes the unit and the binary.
        let fix = match tree.history().first() {
            Some(newest) if plain => format!(
                "Replace it — `rm -rf {} && ln -sfn releases/{newest} {}`",
                tree.current().display(),
                tree.current().display()
            ),
            Some(newest) => format!(
                "Point it at a release that exists — `ln -sfn releases/{newest} {}`",
                tree.current().display()
            ),
            None => "There is no release to point it at, so `holosmith server deploy` is what restores this machine's \
                     boot"
                .to_string(),
        };
        return Finding::fail(
            Check::Release,
            format!(
                "{state}, and the unit's ExecStart ({}) names it: systemd cannot start this machine. {fix}",
                tree.current().join(release::BINARY).display()
            ),
        );
    }
    match &exe {
        Some(exe) => Finding::pass(
            Check::Release,
            format!(
                "{state}; the unit's ExecStart names {}, so this machine does not execute through `current` — a \
                 promotion re-points it",
                exe.display()
            ),
        ),
        None => Finding::unknown(
            Check::Release,
            format!(
                "{state}, and {} names no `ExecStart=` ({}): there is no unit file to say what systemd would run, so \
                 nothing here can be called live — `holosmith server deploy` writes the unit and a promotion writes \
                 the tree",
                unit_path.display(),
                unit.as_ref()
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "no such key".to_string())
            ),
        ),
    }
}

/// The installed paths' modes and owners, against the one reference a machine has: its own data root's owner
/// (spec item 10 step 2's modes, HS-049's "a wrong mode fails `doctor`").
///
/// One check rather than one per path: a [`Finding`] is one check and one sentence, and a tree whose modes are
/// wrong is usually wrong in one way an operator fixes with one `chmod`. The reference is deliberately **not** a
/// uid from `getuid` and not the service user's name — the deploy writes the tree as whoever it ran as and then
/// chowns it, so "every installed path belongs to the same user as the data root" is the invariant that holds on
/// every machine, `--root` included, without this module knowing what a service user is.
///
/// The tree is the caller's, not the machine's: the config file and the admin directory come from [`Tools`], so a
/// `--root` deploy's `doctor --full` reads the tree it just wrote rather than the host's real install.
///
/// The limit is recorded rather than hidden: this asserts consistency and the modes the deploy writes, not "the
/// right user owns it" — the latter is what the deploy's `chown` writes and what the unit's `User=` names.
fn perms(config: &Config, tools: &Tools) -> Finding {
    let data = &config.data_root;
    let owner = match std::fs::metadata(data) {
        Ok(metadata) => metadata.uid(),
        // A machine that has not deployed has no install tree, and that is not a wrong mode: it is the third
        // verdict, which is what keeps `doctor` runnable on a host that serves by hand.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Finding::unknown(
                Check::Perms,
                format!(
                    "{} is not there yet: a machine that has not run `server deploy` has no install tree to check",
                    data.display()
                ),
            );
        }
        Err(error) => {
            return Finding::fail(
                Check::Perms,
                format!(
                    "{} could not be read ({error}): the data root's owner is what every other installed path is \
                     compared with",
                    data.display()
                ),
            );
        }
    };
    let secrets = data.join(crate::key::SECRETS_DIR);
    let tree = [
        (data.clone(), 0o755),
        (secrets.clone(), 0o700),
        (secrets.join(crate::key::KEYS_FILE), 0o600),
        (crate::vapid::store(data), 0o600),
        (tools.admin_dir.clone(), 0o700),
        (tools.config_path.clone(), 0o644),
    ];
    let mut offenders = Vec::new();
    let mut absent = Vec::new();
    for (path, mode) in tree {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                absent.push(format!("{} {mode:04o}", path.display()));
                continue;
            }
            Err(error) => {
                return Finding::fail(
                    Check::Perms,
                    format!(
                        "{} could not be read ({error}): its mode and its owner are what this check is for",
                        path.display()
                    ),
                );
            }
        };
        let found = metadata.permissions().mode() & 0o777;
        if found != mode {
            offenders.push(format!(
                "{} is {found:04o} where `server deploy` writes {mode:04o}: chmod {mode:04o} {}",
                path.display(),
                path.display()
            ));
        }
        if metadata.uid() != owner {
            offenders.push(format!(
                "{} is owned by {} where the data root's owner is {owner}: chown {owner}: {}",
                path.display(),
                metadata.uid(),
                path.display()
            ));
        }
    }
    if let Some(first) = offenders.first() {
        let mut sentence = first.clone();
        if offenders.len() > 1 {
            sentence.push_str(&format!("; also: {}", offenders[1..].join("; ")));
        }
        return Finding::fail(Check::Perms, sentence);
    }
    let mut sentence = format!(
        "the installed paths have the modes `server deploy` writes and belong to the data root's owner ({owner})"
    );
    if !absent.is_empty() {
        sentence.push_str(&format!("; not there yet: {}", absent.join(", ")));
    }
    Finding::pass(Check::Perms, sentence)
}

/// V2: `git merge-tree --write-tree` answers 0 for a clean merge and 1 with the path for a conflict.
///
/// The trial is a real repository — one commit, two branches that diverge on one file, and a third commit on the
/// other side that does not — because the only honest answer to "does this git do it" is to ask it. It lives in
/// `std::env::temp_dir()` and is removed whatever happened: `doctor` leaves no state behind (ADR-0054).
///
/// **The trial-merge fallback is not implemented**, and this is the decision ADR-0054 records rather than a gap:
/// the git check refuses anything older than [`preflight::MIN_GIT`], which is the release `--write-tree` arrived
/// in, so a git that lacks it never reaches this function at all — [`merge`]'s `git_ok` arm is `unknown`, and the
/// machine has already failed on `git`. The spec's "if the git version lacks it" therefore cannot happen on a
/// machine `doctor` would pass, which is the same shape as ADR-0035's unneeded V1 fallback.
fn merge(git: &Path, git_ok: bool) -> Finding {
    if !git_ok {
        return Finding::unknown(
            Check::Merge,
            format!(
                "not measured: the git check above failed, so this git's `merge-tree --write-tree` was not tried \
                 — it is the {}.{} release that arrived in, and the trial-merge fallback is not implemented \
                 because a git older than that cannot run item 12's mergeability check at all (ADR-0054)",
                preflight::MIN_GIT.0,
                preflight::MIN_GIT.1
            ),
        );
    }
    let scratch = std::env::temp_dir().join(format!("holosmith-doctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let trial = trial(git, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    match trial {
        Ok(None) => Finding::pass(
            Check::Merge,
            "`git merge-tree --write-tree` answered 0 for a clean merge and 1 with the path for a conflict",
        ),
        Ok(Some(said)) => Finding::fail(
            Check::Merge,
            format!(
                "`git merge-tree --write-tree` does not behave as item 12 needs: {said} — upgrade git, or install a \
                 build that has merge-tree"
            ),
        ),
        Err(said) => Finding::unknown(
            Check::Merge,
            format!(
                "the merge trial could not run in {} ({said}): this is not a verdict about mergeability, and \
                 `git` needs a writable temporary directory",
                scratch.display()
            ),
        ),
    }
}

/// The two measurements, as `Ok(None)` when they behave, `Ok(Some(sentence))` when they answered something else,
/// and `Err(sentence)` when the trial could not run at all.
fn trial(git: &Path, dir: &Path) -> Result<Option<String>, String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    // `init -b` needs 2.28, which the version check above has already established.
    git_ok(
        git,
        dir,
        &with_identity(&["init", "--quiet", "-b", "main", "."]),
    )?;
    write(dir, MERGE_FILE, "base\n")?;
    git_ok(git, dir, &with_identity(&["add", MERGE_FILE]))?;
    git_ok(
        git,
        dir,
        &with_identity(&["commit", "--quiet", "-m", "base"]),
    )?;
    // Two branches that change the same line, which is the conflict; and a commit on `main` that changes nothing
    // the other branch touched, which is the clean merge.
    git_ok(
        git,
        dir,
        &with_identity(&["checkout", "--quiet", "-b", "left"]),
    )?;
    write(dir, MERGE_FILE, "left\n")?;
    git_ok(
        git,
        dir,
        &with_identity(&["commit", "--quiet", "-a", "-m", "left"]),
    )?;
    git_ok(git, dir, &with_identity(&["checkout", "--quiet", "main"]))?;
    git_ok(
        git,
        dir,
        &with_identity(&["checkout", "--quiet", "-b", "right"]),
    )?;
    write(dir, MERGE_FILE, "right\n")?;
    git_ok(
        git,
        dir,
        &with_identity(&["commit", "--quiet", "-a", "-m", "right"]),
    )?;
    git_ok(git, dir, &with_identity(&["checkout", "--quiet", "main"]))?;
    write(dir, MERGE_ADDED, "added\n")?;
    git_ok(git, dir, &with_identity(&["add", MERGE_ADDED]))?;
    git_ok(
        git,
        dir,
        &with_identity(&["commit", "--quiet", "-m", "added"]),
    )?;

    let clean = git_output(
        git,
        dir,
        &with_identity(&["merge-tree", "--write-tree", "main", "left"]),
    )?;
    let conflict = git_output(
        git,
        dir,
        &with_identity(&["merge-tree", "--write-tree", "left", "right"]),
    )?;
    let clean_code = clean.status.code().unwrap_or(-1);
    let conflict_code = conflict.status.code().unwrap_or(-1);
    let clean_out = String::from_utf8_lossy(&clean.stdout);
    let conflict_out = String::from_utf8_lossy(&conflict.stdout);
    if clean_code != 0 {
        return Ok(Some(format!(
            "a merge that does not conflict answered {clean_code} ({})",
            tail(&clean_out)
        )));
    }
    if clean_out.trim().lines().count() != 1 {
        return Ok(Some(format!(
            "a clean merge printed {} lines where the tree's id is one: {}",
            clean_out.trim().lines().count(),
            tail(&clean_out)
        )));
    }
    if conflict_code != 1 || !conflict_out.contains(MERGE_FILE) {
        return Ok(Some(format!(
            "a conflict answered {conflict_code} with {:?} where 1 and the path {MERGE_FILE} were expected",
            tail(&conflict_out)
        )));
    }
    Ok(None)
}

/// The trial's git flags: an identity (a machine may have none configured), no hooks (a global `core.hooksPath` is
/// somebody else's code, and the trial commits) and no signing (a machine that signs every commit by default would
/// otherwise turn "this git can merge" into "this machine has a GPG agent running").
const IDENTITY: [&str; 10] = [
    "-c",
    "user.name=holosmith",
    "-c",
    "user.email=doctor@holosmith.invalid",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "tag.gpgsign=false",
];

/// [`IDENTITY`] followed by one git command.
fn with_identity<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    let mut all: Vec<&'a str> = IDENTITY.to_vec();
    all.extend_from_slice(rest);
    all
}

/// One `git` invocation inside the trial: `git -C <dir> <arguments>`.
fn git_output(git: &Path, dir: &Path, arguments: &[&str]) -> Result<std::process::Output, String> {
    Command::new(git)
        .arg("-C")
        .arg(dir)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| format!("`git {}` could not be run: {error}", arguments.join(" ")))
}

/// The same, insisting on a zero exit and naming git's own answer when it is not.
fn git_ok(git: &Path, dir: &Path, arguments: &[&str]) -> Result<(), String> {
    let output = git_output(git, dir, arguments)?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "`git {}` answered {} ({})",
        arguments.join(" "),
        output.status.code().unwrap_or(-1),
        tail(&String::from_utf8_lossy(&output.stderr))
    ))
}

/// A few words of a git answer, for a sentence: one line, bounded.
fn tail(text: &str) -> String {
    let said = text.trim();
    let first = said.lines().next().unwrap_or_default();
    if first.chars().count() > 200 {
        first.chars().take(200).collect()
    } else {
        first.to_string()
    }
}

/// A file the trial needs.
fn write(dir: &Path, name: &str, body: &str) -> Result<(), String> {
    std::fs::write(dir.join(name), body)
        .map_err(|error| format!("{name} could not be written: {error}"))
}

/// The machine's network isolation, measured by the self-test (ADR-0044) — whose **bare probe is the
/// network-off probe**: a container on a temporary internal network that reaches neither the internet, nor the
/// host, nor the egress proxy's alias. The proxy-joined pass is [`selftest::pair`], a `debug`/`warn` line rather
/// than a verdict, which is why one outcome speaks for both halves here.
fn network(
    machine: &dyn Machine,
    names: &selftest::Names,
    image: &str,
    docker_ok: bool,
    because: &str,
) -> Finding {
    if !docker_ok {
        return Finding::unknown(
            Check::Network,
            not_measured(
                "the isolation and the network-off probe need a container",
                because,
            ),
        );
    }
    match machine.selftest(names, image) {
        selftest::Outcome::Passed => Finding::pass(
            Check::Network,
            "a probe container on a temporary internal network reached none of the ways out, and the network it \
             used was removed",
        ),
        selftest::Outcome::Failed(sentence) => Finding::fail(Check::Network, sentence),
        selftest::Outcome::Unknown(sentence) => Finding::unknown(Check::Network, sentence),
    }
}

/// Whether one of the machine's proxies is up. Read, never started: `serve`'s boot owns them (ADR-0045/0048).
fn proxy(
    machine: &dyn Machine,
    name: &str,
    alias: &str,
    docker_ok: bool,
    because: &str,
) -> Finding {
    if !docker_ok {
        return Finding::unknown(
            Check::Proxy,
            not_measured(
                &format!("whether {name} is running needs the daemon"),
                because,
            ),
        );
    }
    if machine.running(name) {
        return Finding::pass(
            Check::Proxy,
            format!("{name} is running on the `egress` network, resolved by sessions as `{alias}`"),
        );
    }
    Finding::fail(
        Check::Proxy,
        format!(
            "{name} is not running: the machine's `serve` starts it on every Docker boot (spec item 10 §4), so \
             restart the server — or `docker rm -f {name}` and let the next boot create it"
        ),
    )
}

/// Whether the configured agent image runs the pi this binary is built for **and carries this binary's labels**
/// (spec's build-and-release note: "`doctor` fails if the agent image's pi differs from it").
///
/// The pi question is the same two functions HS-031 asserts with at a launch — [`agent::version_of`] and
/// [`agent::version_check`] — asked on demand, so the sentence an operator reads here is the one a session would
/// fail with. The labels are the second half the pi version cannot answer: an image can run the pinned pi and
/// still carry toolchains or a git tag that this binary did not build, which is why `server deploy`'s step 3
/// compares the same four keys (ADR-0057).
///
/// An image with **no** Holosmith labels at all is a pass, on the pi sentence plus a remark saying what could not
/// be compared: `pi-agent:local` is a supported way to run a session, and a doctor that failed on it would
/// refuse a machine the spec allows.
fn image_check(
    config: &Config,
    machine: &dyn Machine,
    image: &str,
    docker_ok: bool,
    because: &str,
) -> Finding {
    if !docker_ok {
        return Finding::unknown(
            Check::Image,
            not_measured(
                "asking the image for its pi version needs the daemon",
                because,
            ),
        );
    }
    let found = match machine.image_pi(image) {
        Ok(found) => found,
        Err(said) => {
            return Finding::fail(
                Check::Image,
                format!(
                    "the agent image {image} did not run ({said}): build it with `holosmith server deploy`'s \
                     step 3, or point `agent_image` at an image that has pi"
                ),
            );
        }
    };
    let found = match agent::version_check(image, &found) {
        Ok(found) => found,
        Err(sentence) => return Finding::fail(Check::Image, sentence),
    };
    // The expected labels include a digest of *this* binary, which is the bytes a build run from here would copy:
    // a doctor that compared the label set without one could not tell an image built from last week's artifact from
    // one built from this one (ADR-0057).
    let expected = match crate::images::own_digest() {
        Ok(digest) => Expectation::of_digest(Kind::Agent, config, &digest),
        Err(sentence) => return Finding::fail(Check::Image, sentence),
    };
    match machine.image_labels(image) {
        // The image answered a run and then was not there: the daemon is answering two questions about the same
        // image inconsistently, which is a machine problem rather than an image's verdict — and it must not read
        // as "unlabelled", which is a pass, or as "no mismatch", which would hide it.
        Ok(None) => Finding::fail(
            Check::Image,
            format!(
                "the agent image {image} ran `pi --version` and then was not there when its labels were read: the \
                 daemon is answering two questions about the same image inconsistently"
            ),
        ),
        // No Holosmith label at all is the by-hand image, and a pass: `pi-agent:local` is a supported way to run a
        // session (a machine that tagged its own image), and failing it would refuse a machine the spec allows.
        // The sentence says what it could not compare, so the pass is not read as a label comparison that agreed.
        Ok(Some(labels)) if expected.unbuilt(&labels) => Finding::pass(
            Check::Image,
            format!(
                "the agent image {image} runs pi {found}, which is the pinned one, and it carries no Holosmith \
                 labels: it was not built from this binary's Dockerfile, so there is nothing to compare"
            ),
        ),
        Ok(Some(labels)) => {
            let mismatches = expected.mismatches(&labels);
            if mismatches.is_empty() {
                return Finding::pass(
                    Check::Image,
                    format!(
                        "the agent image {image} runs pi {found} and carries this binary's labels (version {}, \
                         git tag {}, pi {}, binary sha256 {}…, toolchains {})",
                        expected.version,
                        expected.git_tag,
                        expected.pi_version,
                        &expected.binary[..12],
                        expected.toolchains.unwrap_or_default().join(" ")
                    ),
                );
            }
            Finding::fail(
                Check::Image,
                format!(
                    "the agent image {image} is not this binary's build: {} — `holosmith server deploy`'s step 3 \
                     rebuilds it, or point `agent_image` at another image",
                    mismatches.join("; ")
                ),
            )
        }
        // An image that answers a run and not an inspect is a machine problem rather than an image's verdict, and
        // it still has to cost a line: a label read that failed silently would make a mismatched image a pass.
        Err(said) => Finding::fail(
            Check::Image,
            format!(
                "the agent image {image} runs pi {found}, but its labels could not be read ({said}): building it \
                 with `holosmith server deploy`'s step 3 is the fix a label read that failed points at"
            ),
        ),
    }
}

/// The sentence a container-dependent check answers with when it was not asked, and why.
fn not_measured(what: &str, because: &str) -> String {
    format!("{what}, and {because}: this is not a verdict about the machine")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    #[test]
    fn a_depth_prints_the_specs_word() {
        assert_eq!(Depth::Quick.as_str(), "quick");
        assert_eq!(Depth::Full.to_string(), "full");
    }

    #[test]
    fn the_report_prints_one_line_per_finding_and_its_tally() {
        let report = Report {
            depth: Depth::Quick,
            findings: vec![
                Finding::pass(Check::Docker, "the daemon answers"),
                Finding::fail(Check::Git, "git is old: upgrade git"),
                Finding::unknown(Check::Tailscale, "no daemon"),
            ],
        };
        let text = report.render();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "doctor: quick");
        assert_eq!(lines[1], "docker: pass: the daemon answers");
        assert_eq!(lines[2], "git: fail: git is old: upgrade git");
        assert_eq!(lines[3], "tailscale: unknown: no daemon");
        assert_eq!(lines[4], "doctor: 3 findings: 1 pass, 1 fail, 1 unknown");
        assert_eq!(lines.len(), 5);
        assert!(report.failed());
        assert_eq!(report.failures().count(), 1);
    }

    #[test]
    fn a_report_with_no_failure_exits_zero() {
        // ADR-0044's rule, as the exit code: "could not measure" is not a failure, or a machine with no
        // Tailscale could not run `doctor` at all.
        let report = Report {
            depth: Depth::Quick,
            findings: vec![
                Finding::pass(Check::Docker, "the daemon answers"),
                Finding::unknown(Check::Tailscale, "no daemon"),
            ],
        };
        assert!(!report.failed());
        assert_eq!(report.failures().count(), 0);
    }

    #[test]
    fn a_systemd_that_is_not_there_is_unknown_and_one_that_is_is_a_pass() {
        let dir =
            std::env::temp_dir().join(format!("holosmith-doctor-systemd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let missing = systemd(&dir);
        assert_eq!(missing.outcome, Outcome::Unknown);
        assert!(
            missing.sentence.contains("server deploy"),
            "{}",
            missing.sentence
        );
        std::fs::create_dir_all(&dir).expect("the scratch directory is creatable");
        let present = systemd(&dir);
        assert_eq!(present.outcome, Outcome::Pass);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tailscale_json_answer_is_read_as_its_node_state() {
        let running = r#"{"BackendState":"Running","Self":{"HostName":"h"}}"#;
        assert_eq!(backend_state(running).as_deref(), Some("Running"));
        assert_eq!(
            backend_state(r#"{"BackendState": "NeedsLogin"}"#).as_deref(),
            Some("NeedsLogin")
        );
        assert_eq!(backend_state("{}"), None);
        assert_eq!(backend_state("not json"), None);
    }

    #[test]
    fn a_free_port_is_a_pass_and_a_held_one_names_the_fix() {
        // A port the kernel gave us, held for the length of the test: the fail arm is the one `ExecStartPre`
        // cares about, and 0 is the vacuous arm this asserts is not.
        let held = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
        let listen = held.local_addr().expect("the bound address is known");
        let config = Config {
            control_listen: listen,
            preview_listen: SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
            ..Config::default()
        };
        let findings = ports(&config, None);
        assert_eq!(findings[0].outcome, Outcome::Fail);
        assert!(
            findings[0].sentence.contains("in use"),
            "{}",
            findings[0].sentence
        );
        assert!(
            findings[0].sentence.contains("ss -ltnp"),
            "the failure names no way to find the holder: {}",
            findings[0].sentence
        );
        // Port 0 is "the kernel will give me one", which is not a port that can be held.
        assert_eq!(findings[1].outcome, Outcome::Pass);
        assert!(findings[1].sentence.contains("nothing here to hold"));

        // The item 10 §7 moment: with this machine's own health answering, a port it holds is the pass — held
        // across both arms, because the only interesting question about a *held* port is who is holding it.
        assert_eq!(
            port(listen, "control", Some(true)).outcome,
            Outcome::Pass,
            "a serving machine's own port must not be a failure"
        );
        // …and a held port on a machine that is *not* serving is the failure, which is what makes the line above
        // meaningful.
        assert_eq!(port(listen, "control", Some(false)).outcome, Outcome::Fail);
        drop(held);
        // The last arm asks about a port **this test has just taken and released**, and it takes a fresh one rather
        // than `listen`: `port` binds and drops the address it is given, and a parallel test binding `:0` in that gap
        // can be handed the port we just freed — measured as `AddrInUse` in a gate run (HS-077), which is a flake in
        // this test and not a fact about the machine.
        let probe = TcpListener::bind("127.0.0.1:0").expect("a port is bindable");
        let fresh = probe.local_addr().expect("the bound address is known");
        drop(probe);
        assert_eq!(
            port(fresh, "control", None).outcome,
            Outcome::Pass,
            "the port is free again, so the last check would be vacuous if it still failed"
        );
    }

    #[test]
    fn a_port_this_machine_cannot_bind_names_the_address_and_not_a_holder() {
        // TEST-NET-1 is not an address this machine has, so the error is not `AddrInUse` and the sentence must not
        // tell the operator to stop a process (there is none).
        let finding = port("192.0.2.1:9".parse().expect("an address"), "control", None);
        assert_eq!(finding.outcome, Outcome::Fail);
        assert!(
            finding.sentence.contains("cannot be bound"),
            "{}",
            finding.sentence
        );
        assert!(
            !finding.sentence.contains("ss -ltnp"),
            "{}",
            finding.sentence
        );
    }

    #[test]
    fn a_tailscale_that_cannot_be_run_is_not_measured() {
        let missing = std::env::temp_dir().join(format!(
            "holosmith-no-such-tailscale-{}",
            std::process::id()
        ));
        let finding = tailscale(&missing);
        assert_eq!(finding.outcome, Outcome::Unknown);
        assert!(
            finding.sentence.contains("not runnable"),
            "{}",
            finding.sentence
        );
    }

    #[test]
    fn a_merge_trial_that_cannot_run_is_not_a_verdict() {
        // The scratch path is a file, so the trial cannot make its directory: "could not measure" is not "this git
        // cannot merge" (ADR-0044's rule, one check over).
        let blocker = std::env::temp_dir().join(format!("holosmith-doctor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&blocker);
        std::fs::write(&blocker, "not a directory").expect("the blocker is writable");
        let finding = merge(Path::new("git"), true);
        let _ = std::fs::remove_file(&blocker);
        assert_eq!(finding.outcome, Outcome::Unknown, "{}", finding.sentence);
        assert!(
            finding.sentence.contains("could not run"),
            "{}",
            finding.sentence
        );
    }

    #[test]
    fn health_is_measured_and_not_assumed() {
        // A listener that answers `/api/health` with the payload, one that answers 200 with something else, and one
        // that answers nothing: the three answers the `--full` moment turns on.
        use std::net::TcpListener as Listener;
        // The payload's own shape (`Health`'s six keys); the reader takes the three it needs.
        let body = r#"{"name":"holosmith","version":"0.0.0","role":"server","uptime_s":0,"preview_origin":"http://localhost:1","application_server_key":null}"#;
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let good = Listener::bind("127.0.0.1:0").expect("a port is bindable");
        let good_address = good.local_addr().expect("the address is known");
        let thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = good.accept() {
                let mut buffer = [0_u8; 512];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        assert!(health_answers(good_address));
        thread.join().expect("the answering thread finishes");

        // A 200 is not enough: a port another program holds must not read as this machine serving, or the deploy's
        // step 7 would call a unit's start successful over somebody else's server.
        let impostor = Listener::bind("127.0.0.1:0").expect("a port is bindable");
        let impostor_address = impostor.local_addr().expect("the address is known");
        let thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = impostor.accept() {
                let mut buffer = [0_u8; 512];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
            }
        });
        assert!(
            !health_answers(impostor_address),
            "a 200 with no health payload is not this machine serving"
        );
        thread.join().expect("the answering thread finishes");

        let silent = Listener::bind("127.0.0.1:0").expect("a port is bindable");
        let silent_address = silent.local_addr().expect("the address is known");
        assert!(
            !health_answers(silent_address),
            "a listener that says nothing is not a serving machine"
        );
        drop(silent);
        assert!(
            !health_answers(silent_address),
            "a port nothing listens on is not a serving machine either"
        );
    }

    #[test]
    fn a_docker_dependent_check_without_a_daemon_is_not_a_verdict() {
        // Two different reasons, and the sentence must say the right one: a server whose daemon failed, and a hub
        // that is asked no Docker question at all (item 10 §6) — the second used to name a line the report does not
        // have.
        let failed = not_measured(
            "the isolation needs a container",
            "the docker check above failed",
        );
        assert!(failed.contains("docker check"), "{failed}");
        let hub = not_measured(
            "the isolation needs a container",
            "a hub runs no sessions, so item 10 §6 asks it no Docker question",
        );
        assert!(!hub.contains("above failed"), "{hub}");
        assert!(hub.contains("hub"), "{hub}");
        // The reason this matters: `serve` may run a `stub` backend on a machine with no daemon, and `doctor`
        // must still print its list rather than claiming a leak.
        let finding = network(
            &NoDaemon,
            &selftest::Names::of(&Config::default()),
            "image",
            false,
            "the docker check above failed",
        );
        assert_eq!(finding.outcome, Outcome::Unknown);
    }

    #[test]
    fn a_missing_image_names_the_build_and_the_key() {
        let finding = image_check(
            &Config::default(),
            &NoDaemon,
            "agent-image:test",
            true,
            "the docker check above failed",
        );
        assert_eq!(finding.outcome, Outcome::Fail);
        assert!(
            finding.sentence.contains("agent-image:test"),
            "{}",
            finding.sentence
        );
        assert!(
            finding.sentence.contains("agent_image"),
            "{}",
            finding.sentence
        );
        assert!(
            finding.sentence.contains("server deploy"),
            "the failure does not name the step that builds one: {}",
            finding.sentence
        );
    }

    /// A machine whose container questions all answer "nothing there" — the daemon-less half of a machine.
    struct NoDaemon;

    impl Machine for NoDaemon {
        fn running(&self, _name: &str) -> bool {
            false
        }

        fn image_pi(&self, _image: &str) -> Result<String, String> {
            Err("no such image".to_string())
        }

        fn image_labels(&self, _image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
            Ok(None)
        }

        fn selftest(&self, _names: &selftest::Names, _image: &str) -> selftest::Outcome {
            selftest::Outcome::Unknown("no image".to_string())
        }
    }
}
