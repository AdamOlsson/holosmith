//! `holosmith install`: one command from nothing to a serving machine.
//!
//! The deploy is the steps; this is the verb that finishes them. `server deploy` and `hub deploy` write the tree, the
//! config and the unit and then **print** the two `systemctl --user` commands that make the unit live
//! (`deploy::unit_commands`) — an install is the same run with those commands run, `loginctl enable-linger` set so
//! the daemon starts at boot rather than while a session is open, and the `/api/health` wait repeated once the unit
//! is up. That is the whole difference between the two verbs, and it is why nothing here is a seventh step:
//! [`deploy::run_as`] does every write.
//!
//! What it does, in order, with no prompt and no `sudo` of its own (a command that needs root is printed):
//!
//! 1. **The daemon's group, before anything is written.** An account outside it cannot open the daemon's socket, so
//!    the machine could not run a session — that is a refusal with the one `usermod`, not a step that fails halfway.
//! 2. **The deploy for this machine's own half**: `hub` installs the hub's, `server` and `both` the server's, printed
//!    verbatim.
//! 3. **The unit**, through this account's own manager: `--user daemon-reload` and `--user enable --now`.
//! 4. **Linger**, so that manager starts the daemon at boot.
//! 5. **The wait**: `/api/health` answering is what "installed" means.
//!
//! Steps 3 and 4 are skipped, and nothing is run, for a tree that manager does not load — `--root`/`--config` can name
//! one, and `systemctl --user` resolves a *name* in this account's search path whatever the flags say. Both lines then
//! carry the operator's own commands instead ([`Tools::manager_unit`], ADR-0091 D4).
//!
//! # The deploy's health line, and this command's
//!
//! The deploy's last step asks whether the machine serves, and on a first install the honest answer is **no**: step 5
//! has written the unit and nothing has started it, which is this command's step 3. So the deploy pass is handed a
//! one-second wait ([`PASS_HEALTH_WAIT`]) instead of the configured deadline — that step's question cannot change its
//! answer however long it waits — its `failed` line is printed as it stands, and the run continues. The wait after
//! `enable --now` is this command's verdict, and the last word on the machine. A report that failed anywhere else
//! stops the run: the tree, the images and the proxies are what the rest of the install stands on.

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use crate::config::{Config, LoadError, Role};
use crate::deploy::{self, DeployStep, Kind, Root, UNIT_NAME, Word};
use crate::docker::Docker;
use crate::progress::{Announce, Progress, Silent};
use crate::remote;

/// What the command line asked for: the config every command reads, the install tree, whether to apply anything, and
/// which of the two renderings to print.
#[derive(Debug)]
pub struct InstallArgs {
    pub config: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,
}

/// The wait the **deploy pass's** last step is given, instead of the configured deadline.
///
/// That step waits for `/api/health` because a deploy does not start the unit; an install starts it one step later,
/// so nothing the step could wait for would change its answer. A second is what the one machine it *can* answer for
/// needs — a re-run whose unit is already serving answers on the first ask without sleeping at all — and it keeps
/// the step's own sentence ("for up to 1 s") one a reader can act on, which "0 s" is not.
const PASS_HEALTH_WAIT: Duration = Duration::from_secs(1);

/// The programs an install runs, as a seam — the same shape and the same reason as [`deploy::Tools`].
///
/// [`deploy::Tools`] is embedded rather than copied: the deploy pass does every write, so it is the same programs,
/// and `systemctl` is already one of its fields. The additions are `loginctl`, `systemctl`'s sibling for the linger
/// flag, and [`Tools::manager_unit`], which is a machine fact rather than a program.
#[derive(Debug, Clone)]
pub struct Tools {
    pub deploy: deploy::Tools,
    /// `loginctl`: `show-user --property=Linger` reads the flag and `enable-linger` sets it.
    pub loginctl: PathBuf,
    /// The unit the account's own user manager resolves — the file `systemctl --user` reaches, whatever tree this run
    /// was pointed at.
    ///
    /// ADR-0088's rule, which `uninstall` already follows: `systemctl --user` takes a **name** and looks it up in the
    /// manager's search path, which is `<home>/.config/systemd/user` for the account that runs it and nothing a flag
    /// can move. A tree under `target/` or `/tmp` therefore has a unit no manager ever reads, and enabling that name
    /// would command the *account's* unit — a service this run was never asked about.
    pub manager_unit: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            deploy: deploy::Tools::default(),
            loginctl: PathBuf::from("loginctl"),
            manager_unit: Root::resolve(None, None).unit(),
        }
    }
}

/// What one of install's own lines is about, in the order this command does them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    /// The unit, enabled through the account's own manager.
    Unit,
    /// The linger flag, which is what makes the unit a boot service.
    Linger,
    /// The `/api/health` wait, which is the install's own verdict.
    Health,
}

impl What {
    /// The word the line prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Linger => "linger",
            Self::Health => "health",
        }
    }
}

impl fmt::Display for What {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One of install's own lines: what it was about, the deploy's word for what the read found, and the sentence.
///
/// The deploy's words, not a second vocabulary: `satisfied`, `needs`, `changed` and `failed` mean here what they mean
/// there — and `needs` is "the remaining action is the operator's", which is exactly a refused `loginctl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub what: What,
    pub word: Word,
    pub sentence: String,
    /// The one command that puts a failed line right, when this command knows a better answer than re-running it: the
    /// deploy's field for the deploy's lines ([`deploy::Line::fix`]), filled here by the command layer with the run's
    /// own resume command when the line named none.
    pub fix: Option<String>,
}

impl fmt::Display for Line {
    /// `install: unit: changed: …` — the deploy's three colon-separated fields, with `install` where the step's
    /// number would be: these are not the spec's six steps, and numbering them 7–9 would say they were.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "install: {}: {}: {}",
            self.what, self.word, self.sentence
        )
    }
}

/// What an install did, as the operator reads it.
///
/// Returned rather than printed so that a test asserts the words instead of a captured stream, which is
/// [`deploy::Report`]'s shape and the reason it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The deploy's six steps, rendered by [`deploy::Report`]: this command runs the deploy rather than repeating its
    /// work, so the steps read here exactly as `server deploy` prints them.
    pub deploy: deploy::Report,
    /// install's own lines, after the deploy's block. A failed one stops the run, so the last line is the failure.
    pub lines: Vec<Line>,
    /// The control origin this machine serves, and the one next step the half this run installed earns — the two
    /// lines the command prints after the steps, and only when nothing failed.
    pub url: String,
    pub next: String,
}

impl Report {
    /// Whether anything failed, which is exactly the exit code: one of install's own lines, or a deploy step that is
    /// **not** the deploy's health line.
    ///
    /// That line is tolerated on purpose (see the module docs): on a first install it answers `no` because nothing has
    /// started the unit yet, which is the very step install does next, so the deploy's block prints it as it stands —
    /// its tally stays honest — while the verdict is install's own wait after `enable --now`. Counting it here would
    /// make a fresh machine exit 1 and never print the URL it just earned, and would make a second run on a serving
    /// machine exit 1 whenever `doctor --full` has one failing finding.
    pub fn failed(&self) -> bool {
        self.deploy
            .failed
            .is_some_and(|step| step != DeployStep::Health)
            || self.lines.iter().any(|line| line.word == Word::Failed)
    }

    /// Put `fallback` on whichever line decides this run failed, when that line named no repair of its own: the run's
    /// own resume command (`install::resume`), which is the command that just failed.
    ///
    /// **The deploy's health line is skipped.** It may be `failed` on a run that succeeded — on a first install it
    /// answers "not serving yet" because nothing has started the unit, which is the very next thing this command does
    /// (ADR-0091 D2) — and a `fix:` on a line the run recovered from would offer the operator a command for a
    /// problem they do not have.
    pub fn fill_fix(&mut self, fallback: &str) {
        if self
            .deploy
            .failed
            .is_some_and(|step| step != DeployStep::Health)
        {
            self.deploy.fill_fix(fallback);
            return;
        }
        if let Some(line) = self.lines.iter_mut().find(|line| line.word == Word::Failed)
            && line.fix.is_none()
        {
            line.fix = Some(fallback.to_string());
        }
    }

    /// The report as an operator reads a run that worked: one line per step, the mark for its word, and the detail
    /// only where there is something to know or to run ([`deploy::Report::render`]).
    ///
    /// install's own three lines are **unnumbered** — they are not the spec's six steps, and the verbose report says the
    /// same with `install:` where a number would be — and they render the same way, so one run has one shape.
    pub fn render(&self) -> String {
        let mut text = self.deploy.render();
        for line in &self.lines {
            let _ = writeln!(text, "{}", line.terse());
        }
        text
    }

    /// The report as any earlier release printed it: the deploy's block — its header, its step lines and its tally —
    /// and then install's own lines, one per line under a blank line of their own.
    pub fn render_verbose(&self) -> String {
        let mut text = self.deploy.render_verbose();
        if self.lines.is_empty() {
            return text;
        }
        text.push('\n');
        for line in &self.lines {
            // A `String` cannot fail to be written to, which is why this is not a `?`.
            let _ = writeln!(text, "{line}");
        }
        text
    }
}

impl Line {
    /// One terse line: `unit ✓`, unnumbered.
    ///
    /// A `changed` line of install's own stops at the mark, where a deploy step's adds a parenthetical: these three
    /// sentences narrate what this run just did (`… ran: the manager has loaded the unit this run wrote`), so their
    /// first clause is the whole sentence and a parenthetical of it would be longer than the verbose line it replaced.
    /// What a reader needs from them is the word — and a `needs` or a `failed` one keeps everything, because what it
    /// carries is the operator's.
    fn terse(&self) -> String {
        let mut text = format!("{} {}", self.what, self.word.mark());
        if self.word != Word::Changed {
            deploy::terse_detail(&mut text, self.word, &self.sentence, self.fix.as_deref());
        }
        text
    }
}

/// The command `main` dispatches to: the config every command reads, this machine's tree, and the run itself.
pub fn run(args: InstallArgs) -> ExitCode {
    let loaded = match Config::load(args.config.clone()) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: install: {error}");
            return ExitCode::FAILURE;
        }
    };
    let root = Root::resolve(args.root.clone(), args.config.clone());
    // The one config the deploy pass sees: the two roots moved under the root, and every machine fact untouched — the
    // contract `server deploy` hands its steps (`deploy::run_as`).
    let deployed = root.config(&loaded.config);
    let tools = Tools::default();
    let machine = Docker::new(deployed.docker_host.clone());
    // Two sinks, one of them silent: a plan has nothing to wait for, so `--dry-run` says nothing while it works,
    // and a real run tells its reader what this machine is about to do. **Only `install` announces in this change**:
    // `server deploy` and `hub deploy` keep the silent default, because their report is the whole of what they print
    // and an operator runs them on a machine that is already there (ADR-0092).
    let mut quiet = Silent;
    let mut shown = Announce;
    let progress: &mut dyn Progress = if args.dry_run { &mut quiet } else { &mut shown };
    let mut report = match execute(&deployed, &root, &tools, &machine, args.dry_run, progress) {
        Ok(report) => report,
        Err(refusal) => {
            eprintln!("holosmith: install: {refusal}");
            return ExitCode::FAILURE;
        }
    };
    if report.failed() {
        report.fill_fix(&resume_command(&args));
    }
    // A plan says once, on its own line, that it is a plan: it is read as a result otherwise, which is the misreading
    // this line exists to prevent. A real run prints no header in the terse shape.
    if args.dry_run && !args.verbose {
        println!("install: dry run — nothing is written");
    }
    print!(
        "{}",
        match args.verbose {
            true => report.render_verbose(),
            false => report.render(),
        }
    );
    if report.failed() {
        // The verbose shape ends a failed run with the resume command, as it always has; the terse one carries it in
        // the failed step's `fix:` line, and printing it twice would be one fact in two places.
        if args.verbose {
            println!("{}", resume(&args));
        }
        return ExitCode::FAILURE;
    }
    match args.verbose {
        true => {
            println!("install: url: {}", report.url);
            println!("install: next: {}", report.next);
        }
        false => {
            println!("url: {}", report.url);
            println!("next: {}", report.next);
        }
    }
    ExitCode::SUCCESS
}

/// [`run`] with the config already loaded and the machine handed in — the seam `tests/install.rs` drives, the shape
/// [`deploy::run_as`] and [`crate::uninstall::run`] already have.
///
/// **`Err` is the one refusal** — this account is not in the daemon's group — and nothing has been written when it is
/// answered: the read is the first thing this function does. A failed *step* is in the report instead, because every
/// step reads before it applies and so re-running is a failed step's recovery, while a refusal would be refused
/// again ([`crate::uninstall::run`] draws the same line).
pub fn execute(
    config: &Config,
    root: &Root,
    tools: &Tools,
    machine: &dyn deploy::Machine,
    dry_run: bool,
    progress: &mut dyn Progress,
) -> Result<Report, String> {
    let kind = match config.role {
        Role::Hub => Kind::Hub,
        Role::Server | Role::Both => Kind::Server,
    };
    // The daemon's-group question, and the account it is about: `Kind::group` answers `None` for a half that opens
    // no socket, where the only fact this command needs is the account's name (for the linger read).
    let group = kind.group(config.docker_host.as_deref());
    let account = group.map(|group| deploy::read_account(&tools.deploy, group));
    // A read this run cannot make is not a refusal: nothing has been written, and the deploy pass's step 2 is the
    // sentence about an `id` that cannot say who is running (ADR-0090's rule about an unread uid, one fact over).
    let name = match &account {
        Some(Ok(account)) => Some(account.name.clone()),
        Some(Err(_)) => None,
        None => deploy::read_name(&tools.deploy).ok(),
    };
    if let (Some(Ok(account)), Some(group)) = (&account, group)
        && !account.may_open(group)
    {
        return Err(not_in_group(group));
    }

    let pass = deploy::Tools {
        health_wait: PASS_HEALTH_WAIT,
        ..tools.deploy.clone()
    };
    let report = deploy::run_as_with(config, root, &pass, machine, dry_run, kind, progress);
    let url = config.control_origin.as_str().to_string();
    // **The deploy's tail cut to one.** `deploy` prints every next step its half earns, and a `both` machine's
    // `server deploy` prints two (`key set`, then the hub's `hub host add`); this command prints one, because the story
    // it implements promises "the one next step". So on a `both` machine the hub's line is not repeated here — the run
    // that installed that half is where it was printed, and `deploy` is still the verb with the whole list.
    let next = super::deploy::next_steps(kind.installs())
        .first()
        .cloned()
        .unwrap_or_default();
    let stopped = |deploy: deploy::Report| Report {
        deploy,
        lines: Vec::new(),
        url: url.clone(),
        next: next.clone(),
    };
    // The deploy's health step is the one failure this command does not take as its own (see the module docs); any
    // other one means the machine has not earned the rest.
    if report.failed.is_some_and(|step| step != DeployStep::Health) {
        return Ok(stopped(report));
    }
    // Whether this run put new unit bytes on disk, which is part of the unit's read: the manager's cached copy is
    // then older than the file, so `daemon-reload` is what makes it read what is there (`needs` is a dry run's
    // "would write").
    let written = report.lines.iter().any(|line| {
        line.step == DeployStep::Service && matches!(line.word, Word::Changed | Word::Needs)
    });
    let mut lines = Vec::new();
    // One at a time, and each begin line printed **before** the work it announces: an array of three lines is
    // evaluated whole, which would put a 60-second wait's line after the wait and make a failed unit's step wait for a
    // poll it never needs (`A failed one stops the run` below).
    for what in [What::Unit, What::Linger, What::Health] {
        progress.begin(what.as_str(), &beginning(what, config, tools));
        let line = match what {
            What::Unit => unit(root, tools, written, dry_run),
            What::Linger => linger(root, tools, name.as_deref(), dry_run),
            What::Health => health(config, tools, dry_run),
        };
        let failed = line.word == Word::Failed;
        lines.push(line);
        if failed {
            break;
        }
    }
    Ok(Report {
        deploy: report,
        lines,
        url,
        next,
    })
}

/// The refusal: this account cannot open the daemon's socket, so the machine could not run a session.
///
/// The command is the sentence's **last line**, unadorned, because it is the thing to paste — and `$USER` rather than
/// the account's name (which the deploy's own step-2 remark spells) because it is meant for the shell the operator is
/// sitting in, where `$USER` is that account and a name typed into a copy could be edited into a different one.
/// Nothing has been written when this is answered, which the sentence says.
fn not_in_group(group: &str) -> String {
    format!(
        "this account is not in the {group} group, and the daemon's socket is owned by `root:{group}` with mode \
         0660, so the `serve` this command would install could not run a session; a group reaches a login only at \
         the next one, so log out and back in after this, and nothing has been written yet:\n\
         sudo usermod -aG {group} $USER"
    )
}

/// Whether this tree's unit is the one the account's manager loads.
///
/// The one fact both of install's manager lines turn on: `systemctl --user` resolves the **name** [`UNIT_NAME`] in its
/// own search path, so a tree under `target/` or one `--config`/`--root` moved has a unit no manager reads, and
/// commanding that name would command a service this run was never asked about (ADR-0088, ADR-0091 D4).
fn own_unit(root: &Root, tools: &Tools) -> bool {
    root.unit() == tools.manager_unit
}

/// The unit, through the account's own manager — the two commands the deploy prints and never runs.
///
/// Three things decide the word, and the first is not a program's:
///
/// - **The manager must be this tree's.** `systemctl --user` resolves the *name* [`UNIT_NAME`] in its own search path,
///   so a tree under `target/`, or one `--config` moved, has a unit no manager reads; commanding that name would
///   command the account's own unit. Such a tree is told which two commands are still the operator's
///   ([`Tools::manager_unit`], ADR-0088).
/// - **The read is two questions**, because they are two facts: `is-enabled` says the unit comes back at the next
///   boot and `is-active` says it is running now. A unit that is enabled but stopped is a machine that will serve
///   after a start, not one that serves.
/// - **`written` overrides both.** When the deploy pass wrote the unit's bytes, the manager holds the older copy, and
///   `daemon-reload` is the one thing that closes that gap — so that run cannot be `satisfied` however the two
///   questions answer.
fn unit(root: &Root, tools: &Tools, written: bool, dry_run: bool) -> Line {
    let path = root.unit();
    if !own_unit(root, tools) {
        return Line {
            what: What::Unit,
            word: Word::Needs,
            sentence: format!(
                "{} is this tree's unit and not the one this account's manager loads ({}), so nothing was enabled: \
                 `systemctl --user` resolves the name {UNIT_NAME} in that manager's own search path, and this run does \
                 not command a service it was not asked about. Still yours to run: {}, on a machine whose config is \
                 {}",
                path.display(),
                tools.manager_unit.display(),
                deploy::unit_commands_printed(&tools.deploy.systemctl).join("; "),
                root.config_file().display()
            ),
            fix: None,
        };
    }
    let (enabled, active) = match manager_state(tools) {
        Ok(state) => state,
        Err(sentence) => return failed(What::Unit, sentence),
    };
    // The one state in which `enable --now` starts nothing: the manager is already running this unit, so the process
    // serving is older than whatever `written` put on disk.
    let already_running = enabled && active;
    if already_running && !written {
        return Line {
            what: What::Unit,
            word: Word::Satisfied,
            sentence: format!(
                "`systemctl --user is-enabled` answers `enabled` and `is-active` answers `active`, so {UNIT_NAME} \
                 starts at boot and is running"
            ),
            fix: None,
        };
    }
    let plan = deploy::unit_commands_printed(&tools.deploy.systemctl)
        .iter()
        .map(|command| format!("`{command}`"))
        .collect::<Vec<String>>()
        .join(" and ");
    if dry_run {
        return Line {
            what: What::Unit,
            word: Word::Needs,
            sentence: match already_running {
                true => format!(
                    "would run {plan}: the manager is running this unit from the bytes it read before this run \
                     would write new ones"
                ),
                false => format!(
                    "would run {plan}: the manager has not loaded this unit, or has not started it"
                ),
            },
            fix: None,
        };
    }
    for command in deploy::unit_commands() {
        if let Err(sentence) = ran(
            &tools.deploy.systemctl,
            command,
            "the unit is written, and these are the two commands that make the account's manager load and start it",
        ) {
            return failed(What::Unit, sentence);
        }
    }
    Line {
        what: What::Unit,
        // `enable --now` starts a unit that is not running and leaves one that is alone, so "started it" is only true
        // when the manager answered `inactive` a moment ago. A re-run that rewrote the unit's bytes with the previous
        // process still serving is `already_running`, and what it needs is the restart — which is why that arm is
        // `needs` and not `changed`: `needs` is "the remaining action is the operator's", a `changed` line of
        // install's own stops at the mark in the terse report, and a command may not stop at it.
        word: match already_running {
            true => Word::Needs,
            false => Word::Changed,
        },
        sentence: match already_running {
            true => format!(
                "{plan} ran: the manager has loaded the unit this run wrote, and {UNIT_NAME} was already running — the \
                 process serving predates these bytes, so `systemctl --user restart {UNIT_NAME}` is what starts them"
            ),
            false => {
                format!("{plan} ran: the manager has loaded the unit this run wrote and started it")
            }
        },
        fix: None,
    }
}

/// The two facts the account's manager answers about the unit: whether it comes back at the next boot, and whether it
/// is running now.
///
/// Two children rather than one parse of a multi-property `show`, because each exit status **is** the fact: `is-enabled`
/// exits 1 for `disabled` and 4 for `not-found`, and neither has to be told apart from a blank line or from a `show`
/// whose two values arrive in an order this run would then depend on.
fn manager_state(tools: &Tools) -> Result<(bool, bool), String> {
    let because = "the unit is loaded and started by this account's manager, so this run asks it before it runs \
                   anything";
    let enabled = asked(
        &tools.deploy.systemctl,
        &["--user", "is-enabled", UNIT_NAME],
        because,
    )?;
    let active = asked(
        &tools.deploy.systemctl,
        &["--user", "is-active", UNIT_NAME],
        because,
    )?;
    Ok((
        enabled.ok && enabled.out == "enabled",
        active.ok && active.out == "active",
    ))
}

/// The linger flag, which is what turns the unit into a boot service rather than one that exists while a session is
/// open (ADR-0088 D2).
///
/// **A refusal here does not fail the run, and it is the only step that may not.** Setting the flag needs no
/// privilege on a machine whose polkit allows `set-self-linger` (measured, ADR-0088), and where it does not, the
/// answer is one command the operator runs: the machine serves either way, and what is missing is that it serves
/// after a reboot as well. That is [`Word::Needs`], the deploy's word for a step whose remaining work is the
/// operator's, and the same `$USER` the refusal prints.
///
/// **A tree that is not the manager's gets no flag either** (ADR-0091 D4): the flag is what makes the unit the
/// manager loads a boot service, so on a tree no manager reads it would set a boot service for a unit this run was
/// never asked about — the one thing [`unit`] refuses to do. It is [`Word::Needs`] with the operator's own command
/// instead, and nothing is read or run.
fn linger(root: &Root, tools: &Tools, name: Option<&str>, dry_run: bool) -> Line {
    if !own_unit(root, tools) {
        return Line {
            what: What::Linger,
            word: Word::Needs,
            sentence: format!(
                "this run set no linger flag either: the flag is what makes the unit this account's manager loads a \
                 boot service, and {} is not that unit (the manager reads {}). Still yours to run, on a machine whose \
                 config is {}: {}; sudo loginctl enable-linger $USER",
                root.unit().display(),
                tools.manager_unit.display(),
                root.config_file().display(),
                deploy::unit_commands_printed(&tools.deploy.systemctl).join("; ")
            ),
            fix: None,
        };
    }
    // The flag is read from `loginctl show-user`, which names the account: this is why the run reads `id -un` even
    // for a half that asks no group question. `None` (no account name, no `loginctl`, a bus that will not answer) is
    // not "off" — it is "not known to be on", which is what makes the apply run and its own answer the line.
    let on = match name {
        Some(name) => match asked(
            &tools.loginctl,
            &["show-user", name, "--property=Linger", "--value"],
            "this run asks whether this account's linger is already on before it sets it",
        ) {
            Ok(answer) => answer.ok && answer.out.eq_ignore_ascii_case("yes"),
            Err(_) => false,
        },
        None => false,
    };
    if on {
        return Line {
            what: What::Linger,
            word: Word::Satisfied,
            sentence: "`loginctl show-user --property=Linger` answers `yes`, so the daemon starts at boot and not only \
                       while a session is open"
                .to_string(),
            fix: None,
        };
    }
    if dry_run {
        return Line {
            what: What::Linger,
            word: Word::Needs,
            sentence: "would run `loginctl enable-linger`, so the daemon starts at boot and not only while a session is \
                       open"
                .to_string(),
            fix: None,
        };
    }
    match ran(
        &tools.loginctl,
        &["enable-linger"],
        "linger is what makes a user unit start at boot, and setting it is this account's own",
    ) {
        Ok(()) => Line {
            what: What::Linger,
            word: Word::Changed,
            sentence: "`loginctl enable-linger` ran: the daemon starts at boot and not only while a session is open"
                .to_string(),
            fix: None,
        },
        Err(said) => Line {
            what: What::Linger,
            word: Word::Needs,
            sentence: format!(
                "this account's linger is off and `loginctl enable-linger` was refused ({said}), so the daemon serves \
                 only while a session is open — the machine works, and this is the one command that makes it a boot \
                 service:\nsudo loginctl enable-linger $USER"
            ),
            fix: None,
        },
    }
}

/// The wait that makes an install done — "it is done when a user unit answers `/api/health`" (ADR-0088).
///
/// The machine answered before the deploy pass ran and may answer during it; neither is this command's verdict,
/// because the unit had not been started. This is: the same [`deploy::wait_for_health`] the deploy's last step uses,
/// with the configured deadline rather than the deploy pass's one second, and the deploy's own sentence when it runs
/// out — the journal is where a unit that did not come up explains itself.
fn health(config: &Config, tools: &Tools, dry_run: bool) -> Line {
    let listen = config.control_listen;
    let bound = tools.deploy.health_wait.as_secs();
    if dry_run {
        return Line {
            what: What::Health,
            word: Word::Needs,
            sentence: format!(
                "would wait for `/api/health` on {listen} for up to {bound} s, and print the control URL once it \
                 answers"
            ),
            fix: None,
        };
    }
    if deploy::wait_for_health(listen, tools.deploy.health_wait) {
        return Line {
            what: What::Health,
            word: Word::Satisfied,
            sentence: format!(
                "the control origin ({}) answers `GET /api/health`, so this machine is serving",
                config.control_origin.as_str()
            ),
            fix: None,
        };
    }
    failed(
        What::Health,
        format!(
            "the control origin ({listen}) did not answer `/api/health` within {bound} s of {UNIT_NAME} being \
             enabled: `journalctl --user -u {UNIT_NAME} -n 50` names why the unit did not come up"
        ),
    )
}

/// What one of install's own lines is about to do, present tense, for the begin line [`Progress::begin`] carries.
///
/// The health wait is the one that names its bound and its address, because it is the one that may take a minute with
/// nothing printed: `health_wait` is the configured deadline here, not the deploy pass's one second, so this is also
/// where a reader sees which of the two waits they are watching.
fn beginning(what: What, config: &Config, tools: &Tools) -> String {
    match what {
        What::Unit => {
            "running this account's two `systemctl --user` commands for the unit".to_string()
        }
        What::Linger => {
            "setting this account's linger flag, so the daemon starts at boot".to_string()
        }
        What::Health => format!(
            "waiting for `/api/health` on {} for up to {}",
            config.control_listen,
            bound(tools.deploy.health_wait)
        ),
    }
}

/// A wait as the one number a progress line names: seconds where there is a whole one and milliseconds below that,
/// because a bound printed as `0 s` says nothing about how long a reader is being asked to wait.
fn bound(wait: Duration) -> String {
    match wait.as_secs() {
        0 => format!("{} ms", wait.as_millis()),
        secs => format!("{secs} s"),
    }
}

/// A failed line: the deploy's word and the sentence that says what stopped the run.
fn failed(what: What, sentence: String) -> Line {
    Line {
        what,
        word: Word::Failed,
        sentence,
        fix: None,
    }
}

/// What one child answered: whether it exited zero, its trimmed stdout, and the first non-blank line of its stderr —
/// or, for a child that said nothing, its exit code.
///
/// One shape for a read and an apply, because they differ in what the caller does with the answer rather than in how
/// it is read: `systemctl --user is-enabled` answers `disabled` and exits non-zero, which is a "no" rather than a
/// failure, while `enable --now` exiting non-zero is a failed step with that line in its sentence.
struct Answer {
    ok: bool,
    out: String,
    said: String,
}

/// Run one child and answer what it said. `Err` is a program that could not be run at all — a different fact from
/// one that answered no, exactly as [`deploy::Tools`]' `id` read draws it.
fn asked(program: &Path, arguments: &[&str], because: &str) -> Result<Answer, String> {
    let rendered = format!("{} {}", program.display(), arguments.join(" "));
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("`{rendered}` could not be run ({error}): {because}"))?;
    Ok(Answer {
        ok: output.status.success(),
        out: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        said: String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map_or_else(
                || format!("exit {}", output.status.code().unwrap_or(-1)),
                str::to_string,
            ),
    })
}

/// The same child where a non-zero exit is a failure: `Ok` when it exited zero, and a sentence naming the argv it
/// was given and what it answered when it did not — `uninstall`'s shape for the same fact.
fn ran(program: &Path, arguments: &[&str], because: &str) -> Result<(), String> {
    let answer = asked(program, arguments, because)?;
    if answer.ok {
        return Ok(());
    }
    Err(format!(
        "`{} {}` failed ({})",
        program.display(),
        arguments.join(" "),
        answer.said
    ))
}

/// The resume command as a reader pastes it: this run's own flags, made absolute, quoted for a shell, and `--dry-run`
/// when this run was one.
///
/// The install is the recovery for its own failure — every step of it reads before it applies, the deploy's included
/// — so the command names this verb rather than the two deploys it runs: `server deploy` would install the same tree
/// and leave the unit printed rather than started, which is the command that just failed. It is a value and not a
/// line because it travels twice: as a failed step's `fix:` ([`Report::fill_fix`]) and as the verbose shape's last
/// line.
///
/// **Each path is quoted** ([`crate::remote::quote`]), because this value is a command line an operator runs: a plain
/// path comes back byte for byte, and one with a space in it survives the paste as the one argument it was. The
/// flags the operator did not give are not invented, so a bare resume stays bare.
fn resume_command(args: &InstallArgs) -> String {
    let mut command = String::from("holosmith install");
    if let Some(config) = &args.config {
        command.push_str(&format!(
            " --config {}",
            remote::quote(&deploy::absolute(config).display().to_string())
        ));
    }
    if let Some(root) = &args.root {
        command.push_str(&format!(
            " --root {}",
            remote::quote(&deploy::absolute(root).display().to_string())
        ));
    }
    if args.dry_run {
        command.push_str(" --dry-run");
    }
    command
}

/// [`resume_command`] as the verbose report prints it, with the field's name in front.
fn resume(args: &InstallArgs) -> String {
    format!("install: resume: {}", resume_command(args))
}
