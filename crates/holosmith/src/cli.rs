//! The command line. Phase 1 adds `doctor`, `key set` and the rest of `config`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::runtime::Backend;

#[derive(Debug, Parser)]
#[command(name = "holosmith", version, about = "Run Holosmith")]
pub struct Cli {
    /// Path to the config file (default: ~/.config/holosmith.toml, in the home of the account this runs as).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run the control listener in the foreground until SIGTERM or SIGINT.
    Serve {
        /// Port for the control listener; the control origin's port follows it.
        #[arg(long, value_name = "PORT")]
        port: Option<u16>,
        /// Data root for this run, instead of the one in the config file.
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
        /// Directory the admin socket (`holosmith drain`) lives in, instead of `/run/holosmith`.
        #[arg(long, value_name = "PATH")]
        admin_dir: Option<PathBuf>,
        /// Which backend runs sessions.
        #[arg(long, value_enum, value_name = "BACKEND")]
        backend: Option<Backend>,
    },
    /// Version, git tag, pinned pi version, schema version.
    Version {
        /// Print one line of JSON instead of `key value` lines.
        #[arg(long)]
        json: bool,
    },
    /// Check, print or edit the machine's config file.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Run one of the proxies, in the foreground. Used inside containers.
    Proxy {
        #[command(subcommand)]
        which: ProxyAction,
    },
    /// Whether this machine is fit to run sessions.
    Doctor {
        /// The list `serve` refuses on, with no container: what the unit file runs as `ExecStartPre`.
        /// This is the default when neither flag is given.
        #[arg(long, conflicts_with = "full")]
        quick: bool,
        /// Everything `--quick` has, plus the network self-test, the proxies and the agent image's pi version.
        #[arg(long)]
        full: bool,
        /// The root to examine instead of `HOLOSMITH_ROOT`'s — the directory the data root, the release tree and
        /// the unit live under. The config is `--config`'s, because it is not part of the root.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Store, list or forget this machine's model provider keys and tool keys.
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    /// Update this machine: the whole gate as the verification, then the promotion, as one command.
    ///
    /// The candidate is the `master` of the clean checkout `[update] source` names. The verification is that tree's
    /// own `scripts/verify-in-container.sh`, run in the image the running release deployed, which builds the artifact
    /// and the candidate's own product images; a green gate is the acknowledgement, so there is no
    /// `--acknowledge-review` here. The promotion is then `upgrade`'s pipeline run on the artifact the gate left, one
    /// printed line per step. A gate that fails asks the machine nothing — no smoke run, no drain, no backup.
    ///
    /// Exit 0 only when the candidate is live, 1 for every other ending, and `--report` writes one JSON object saying
    /// which: the candidate, the gate's command and exit code, the artifact and its digest, each step's ending and
    /// the final state. It is written for every ending, because a job's caller reads that file and not a terminal.
    Update {
        /// The file the run's report is written to, as one JSON object, whatever the ending. Absent means nothing is
        /// written.
        #[arg(long, value_name = "PATH")]
        report: Option<PathBuf>,
        /// How long the drain may wait for the turns that are running (`10m`, `500ms`).
        #[arg(long, value_name = "DURATION", default_value = "10m", value_parser = parse_wait)]
        wait: std::time::Duration,
        /// Promote into this root instead of `HOLOSMITH_ROOT`'s.
        ///
        /// The same meaning as `upgrade --root`: the releases and `current` live under it.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Promote a new version: the whole pipeline, one printed line per step.
    ///
    /// The review gate acknowledged, the host smoke test, the images when they changed, the drain, a database
    /// backup, the release directory, the `current` symlink, `systemctl --user restart` and a 20-second health poll
    /// that puts the previous release back when the candidate does not answer. Every refusal leaves the machine as it
    /// found it, and says so — except that a failure after the drain leaves it draining, which the sentence names.
    ///
    /// The promotion on its own: `holosmith update` is the same pipeline with the gate in front of it and no
    /// acknowledgement to give.
    Upgrade {
        /// The candidate: the file a promotion is about to make live.
        #[arg(value_name = "BINARY")]
        binary: PathBuf,
        /// Item 12's diff review, acknowledged. A promotion without it is refused: that review is the real gate.
        #[arg(long)]
        acknowledge_review: bool,
        /// How long the drain may wait for the turns that are running (`10m`, `500ms`).
        #[arg(long, value_name = "DURATION", default_value = "10m", value_parser = parse_wait)]
        wait: std::time::Duration,
        /// Promote into this root instead of `HOLOSMITH_ROOT`'s.
        ///
        /// The same meaning as `server deploy --root`: the releases and `current` live under it. The restart goes
        /// through the operator's own `systemctl --user`, so it is the same user manager whichever root this run
        /// writes — the unit beside the config, not a file under the root.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Go back one release: switch `current` to the release before the live one, drain, restart and verify.
    ///
    /// The emergency brake for a promotion that passed its health check but turned out to be wrong. It drains first,
    /// switches `current` **back one release in the tree's own order**, restarts the unit and polls `/api/health` for
    /// a process younger than that restart. There is no review acknowledgement: the review is a pre-promotion
    /// gate, and a rollback is the command that undoes a promotion. The database is never restored — the migration
    /// rule (expand/contract) is what makes switching back safe.
    Rollback {
        /// The release to switch to, by tag. Absent means the release the live one replaced.
        #[arg(long, value_name = "TAG")]
        to: Option<String>,
        /// How long the drain may wait for the turns that are running (`10m`, `500ms`).
        #[arg(long, value_name = "DURATION", default_value = "10m", value_parser = parse_wait)]
        wait: std::time::Duration,
        /// Roll back into this root instead of `HOLOSMITH_ROOT`'s.
        ///
        /// The same meaning as `upgrade --root`: the releases and `current` live under it.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Container helpers, used inside a session's container.
    Container {
        #[command(subcommand)]
        action: ContainerAction,
    },
    /// Run the smoke run against a candidate binary: a promotion's go/no-go (ADR-0081).
    ///
    /// The promotion's go/no-go: it starts the binary named by `--binary` as a `serve` with a smoke config — its own
    /// port, its own temporary roots, `smoke_` in front of every container and network it names — creates a session,
    /// runs one turn through a container whose `pi` replays a recorded turn, asserts the frames, deletes it, and
    /// answers 0 when the candidate passed, 1 when a measurement failed and 2 when the run could not happen at all.
    Smoke {
        /// The binary to test. Absent means this executable.
        #[arg(long, value_name = "PATH")]
        binary: Option<PathBuf>,
        /// The directory the smoke run's config, data root and scratch repository live under. Absent means
        /// `<data_root>/smoke`, which is the one directory the config already guarantees the daemon can see.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// What this machine is and what it is doing: role, version, uptime, session counts, the proxies, and the
    /// health of the machines registered with it.
    Status,
    /// The server's log from journald, or one session's log (`-f` follows either).
    Logs {
        /// Keep printing what arrives.
        #[arg(short = 'f', long)]
        follow: bool,
        /// One session's `<data>/sessions/<id>/session.log` instead of the unit's journald log.
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// Remove the unit, the proxies and the machine's networks, keeping the data.
    Uninstall {
        /// Also remove the data root: the database, every session's log and `secrets/`.
        #[arg(long)]
        purge: bool,
        /// Override the refusal to purge while sessions are live. Only meaningful with `--purge`.
        #[arg(long, requires = "purge")]
        force: bool,
        /// Tear down this install tree instead of the system paths, as the invoking user (no sudo).
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Refuse new work and wait for running turns, over the local admin socket.
    ///
    /// One connection, one local unix socket, never the network: the machine answers this from
    /// `/run/holosmith/drain.sock` and nowhere else, and a request that reaches the control origin cannot make it
    /// happen. The exit code is 0 when the machine is quiet and 1 when the wait ran out with a turn still
    /// running — in which case the drain stays on rather than pretending it finished.
    Drain {
        /// How long to wait for the turns that are running (`10m`, `500ms`).
        #[arg(long, value_name = "DURATION", default_value = "10m", value_parser = parse_wait)]
        wait: std::time::Duration,
        /// Directory the admin socket lives in, instead of `/run/holosmith`.
        #[arg(long, value_name = "PATH")]
        admin_dir: Option<PathBuf>,
    },
    /// Install this machine's own half and start it: one command from nothing to a serving machine.
    ///
    /// It checks the daemon's group first, runs the deploy for the half this machine's config asks for (`hub`
    /// installs the hub's, `server` and `both` the server's), enables the unit through this account's own systemd
    /// manager, sets linger so the daemon starts at boot, and waits for `/api/health`. It runs no `sudo` and asks
    /// no questions: anything that needs root is printed for you to run. Re-running is safe and changes nothing.
    Install {
        /// Print what each step would do and change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print every step's full sentence, the install tree and the tally, as earlier releases did.
        #[arg(long)]
        verbose: bool,
        /// Install into this directory instead of `HOLOSMITH_ROOT`'s (default `~/.local/share/holosmith`).
        ///
        /// The root is one directory: the data root, the release tree and the projects. It moves the state and
        /// only the state — the config is `--config`'s, `~/.config/holosmith.toml` by default.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
    },
    /// Install and start a server on this machine.
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    /// Install and start a hub on this machine, and manage the machines it forwards to.
    Hub {
        #[command(subcommand)]
        action: HubAction,
    },
    /// Build a project's image, or the base image, without waiting for a session to need it.
    ///
    /// A project's sessions run `hs-<project>:<digest>`, a content address over its `docker/Dockerfile` and the base
    /// image's tag, and the first session that needs one builds it — under a lock, so two creates do not run one
    /// build. This verb is that build on its own: a fresh machine should not pay six minutes of it inside somebody's
    /// first turn.
    Image {
        #[command(subcommand)]
        action: ImageAction,
    },
    /// Add a repository to the projects the sessions are created against, or print the ones this machine has.
    ///
    /// A project is a repository directly inside the projects root, and the name a session asks for is its
    /// directory name: `add` derives that name from the URL it clones and prints it, so the printed word is
    /// exactly what the new-session form takes. What it cloned is put through the same refusals a create
    /// makes, and a clone one of them refuses is removed again, so the corrected URL can be added under the
    /// same name.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
}

/// What `server` does.
#[derive(Debug, Subcommand)]
pub enum ServerAction {
    /// Run the deploy's seven steps in order, each checking the current state first. Re-running is safe.
    ///
    /// On a machine whose config says `hub` this installs the **server half** and leaves the config `both`: a role
    /// is not a reason to refuse a deploy.
    Deploy {
        /// Print each step's plan and change nothing. A broken machine still exits non-zero.
        #[arg(long)]
        dry_run: bool,
        /// Print every step's full sentence, the install tree and the tally, as earlier releases did.
        ///
        /// Without it a report is one line per step: the mark for its word, and the commands a step leaves to you.
        #[arg(long)]
        verbose: bool,
        /// Install into this directory instead of `HOLOSMITH_ROOT`'s (default `~/.local/share/holosmith`).
        ///
        /// The root is one directory: the data root, the release tree and the projects. It moves the state and
        /// only the state — the config is `--config`'s, `~/.config/holosmith.toml` by default.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Install on another machine: copy this binary there over ssh, verify the copy, and run the same deploy
        /// there as the login's own account, because a machine is the account that runs it (the deploy escalates
        /// nothing).
        #[arg(long, value_name = "USER@HOST")]
        ssh: Option<String>,
        /// Send this file instead of the binary this command is running. It must be built for the target's
        /// architecture: a mismatch is refused before anything is copied, and this flag is how a matching build is
        /// supplied.
        #[arg(long, requires = "ssh", value_name = "PATH")]
        binary: Option<PathBuf>,
    },
}

/// What `hub` does: install this machine as a hub, or manage the registry it forwards with.
#[derive(Debug, Subcommand)]
pub enum HubAction {
    /// Steps 1, 2, 5, 6 and 7 of the deploy, each checking the current state first. No image is built and no proxy is
    /// started, because a hub runs no containers. Re-running is safe.
    ///
    /// On a machine whose config says `server` this installs the **hub half** and leaves the config `both`.
    Deploy {
        /// Print each step's plan and change nothing. A broken machine still exits non-zero.
        #[arg(long)]
        dry_run: bool,
        /// Print every step's full sentence, the install tree and the tally, as earlier releases did.
        ///
        /// Without it a report is one line per step: the mark for its word, and the commands a step leaves to you.
        #[arg(long)]
        verbose: bool,
        /// Install into this root instead of `HOLOSMITH_ROOT`'s (default `~/.local/share/holosmith`).
        ///
        /// The root is one directory: the data root, the release tree and the projects. It moves the state and
        /// only the state — the config is `--config`'s, `~/.config/holosmith.toml` by default. The deploy never
        /// escalates, so what it needs is a root this user can write, which is the tree a test or a by-hand
        /// install owns.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Install on another machine: copy this binary there over ssh, verify the copy, and run the same deploy
        /// there as the login's own account, because a machine is the account that runs it (the deploy escalates
        /// nothing).
        #[arg(long, value_name = "USER@HOST")]
        ssh: Option<String>,
        /// Send this file instead of the binary this command is running. It must be built for the target's
        /// architecture: a mismatch is refused before anything is copied, and this flag is how a matching build is
        /// supplied.
        #[arg(long, requires = "ssh", value_name = "PATH")]
        binary: Option<PathBuf>,
    },
    /// Manage this machine's host registry — the machines it forwards to — with the API's own checks.
    Host {
        #[command(subcommand)]
        action: HostAction,
    },
}

/// The registry's three commands. Each one is a call to **this machine's own** `POST`/`GET`/`DELETE /api/hosts`, so
/// the rules, their codes and their sentences are the route's and never a second copy of them.
#[derive(Debug, Subcommand)]
pub enum HostAction {
    /// Register one machine, or update the row it already has (a name is a row's identity).
    Add {
        /// The name the row takes: one path segment — lowercase letters, digits and `-`.
        name: String,
        /// The machine's control origin: `http(s)`, a MagicDNS name or an address in 100.64.0.0/10, and a port.
        url: String,
    },
    /// Print every row, by name.
    List,
    /// Forget one machine. A row that names this machine is refused while it has live sessions.
    Remove {
        /// The row's name.
        name: String,
        /// Remove the row even when it is this machine and it has live sessions (the API's `?force=true`).
        #[arg(long)]
        force: bool,
    },
}

/// What `image` does: build what a session's `docker run` would otherwise have to build itself.
#[derive(Debug, Subcommand)]
pub enum ImageAction {
    /// Build one project's image from its own recipe, and the base image it builds `FROM` first.
    ///
    /// Nothing is rebuilt when the tag is already there: the tag is a digest over the recipe and the base image's
    /// tag, so "it exists" is the whole currency question. Re-running is therefore free, and `--force` is the one way
    /// to rebuild a tag the digest says is current — a recipe whose `COPY`'d file moved is stale with the same tag.
    ///
    /// The build has the internet, and the session it produces does not: the daemon builds on its own bridge, and
    /// the container joins an internal network. A recipe that runs `apt-get` works here and nowhere else.
    ///
    /// Exit 1 is a build that failed, with the tail of what the daemon said on stderr and the path of the whole log
    /// beside it. Exit 4 is a build whose pinned fetch did not answer — musl.cc unreachable while the image builds —
    /// which is this machine's network rather than the recipe, and the same code `server deploy`'s image step answers.
    Build {
        /// The project whose image to build. Absent with `--base`.
        #[arg(
            value_name = "PROJECT",
            required_unless_present = "base",
            conflicts_with = "base"
        )]
        project: Option<String>,
        /// Build the base image instead — `holosmith-base:<version>`, from the recipe embedded in this binary.
        #[arg(long)]
        base: bool,
        /// Rebuild even when the tag is already there.
        #[arg(long)]
        force: bool,
    },
}

/// What `project` does: put a repository where the sessions can be created against it, or say which ones are
/// there.
#[derive(Debug, Subcommand)]
pub enum ProjectAction {
    /// Clone a repository into the projects root and print the name a session is created against.
    ///
    /// The name is the URL's last path segment, less a trailing `/` and a trailing `.git`, so it is the
    /// directory's name and not something to remember. It goes to stdout alone, which is what makes
    /// `name=$(holosmith project add <url>)` work; what the clone is doing goes to stderr.
    Add {
        /// The repository to clone: any address `git clone` accepts, a path or a URL.
        url: String,
    },
    /// Print every project's name, one per line.
    ///
    /// Only the directories a create would mount: one whose repository it would refuse (no `.git`, submodules,
    /// `extensions.worktreeConfig`) is not a project a session can run in, so it is not printed.
    List,
}

/// The proxies a container runs.
#[derive(Debug, Subcommand)]
pub enum ProxyAction {
    /// The egress proxy: the only route out of a session container.
    Egress {
        /// Address to listen on.
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// A domain a tunnel may target. Repeatable; absent means the config's `[egress] allow`.
        #[arg(long, value_name = "DOMAIN")]
        allow: Vec<String>,
    },
    /// The key proxy: validates a session's dummy key and swaps in the real provider key.
    Key {
        /// Address to listen on.
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// The key file. Absent means the config's `<data>/secrets/keys.toml`.
        #[arg(long, value_name = "PATH")]
        keys: Option<PathBuf>,
    },
    /// The MCP proxy: validates a session's dummy key and swaps in a tool's real key for the host it names.
    Mcp {
        /// Address to listen on.
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// The key file. Absent means the config's `<data>/secrets/keys.toml`.
        #[arg(long, value_name = "PATH")]
        keys: Option<PathBuf>,
    },
}

/// The container helpers.
#[derive(Debug, Subcommand)]
pub enum ContainerAction {
    /// Save the worktree's uncommitted work at `refs/holosmith/wip/<HS_SESSION_ID>`.
    Snapshot,
    /// Be pi, for the smoke run: replay the committed turn on stdin/stdout.
    ///
    /// The smoke image's `/usr/local/bin/pi` is a two-line wrapper around this, so a session's container runs this
    /// binary under pi's own name and `agent::COLUMN`'s launch line, version check and record framing are the real
    /// ones. Every argument after the subcommand is pi's and is **ignored** — `--mode rpc`, `--no-session`,
    /// `--approve`, `--model` and `--thinking` are the launch's, and a fake that refused them would be a fake nobody
    /// can start the way the build starts pi. `--version` answers the pinned version, which is what
    /// `agent::version_of` asks the image.
    FakePi {
        /// pi's own flags, accepted and ignored; `--version` is the one that is read.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "PI_ARGS"
        )]
        pi_args: Vec<String>,
    },
}

/// The four key subcommands.
#[derive(Debug, Subcommand)]
pub enum KeyAction {
    /// Read one provider's or one tool's key from stdin and store it, mode 0600.
    ///
    /// A terminal is asked for the key and its echo is off, so Enter finishes it; a pipe is read to the end, which
    /// is the form a script and `printf '…' | holosmith key set <id>` use. A model provider's key is stored under
    /// `[providers]`, where the key proxy swaps it in; a tool's key under `[tools]`, where `key revoke` owns it.
    Set {
        /// The provider id (`opencode-go`, `anthropic`, …) or the tool id (`linear`).
        id: String,
    },
    /// Print the providers and tools that have a key. Never the values.
    List,
    /// Remove one model provider's key; a tool's is `key revoke`.
    Remove {
        /// The provider id (`opencode-go`, `anthropic`, …).
        provider: String,
    },
    /// Forget one tool's key. A tool with no key is not an error, so a second revoke exits 0.
    Revoke {
        /// The tool id (`linear`).
        tool: String,
    },
}

/// The three config subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Report every problem in the config file.
    Validate,
    /// Print the resolved config, defaults and all, as TOML.
    Show,
    /// Open the config file in $VISUAL or $EDITOR, then validate what was saved.
    Edit,
}

/// `--wait 10m`: a span of time in the config file's own grammar (`10s`, `1500ms`, `2m`).
///
/// One parser for one idea. The config's [`crate::config::Duration`] already refuses a bare number (its unit
/// would be a guess) and a zero (a wait that waits for nothing is a mistake, not a value), and a second grammar
/// on the command line would be a second set of the same mistakes. `clap` turns the `Err` into a usage error
/// before anything is loaded, which is where a typo in a flag belongs.
fn parse_wait(text: &str) -> Result<std::time::Duration, String> {
    text.parse::<crate::config::Duration>()
        .map(std::time::Duration::from)
}
