# Holosmith

A single-operator system that runs coding agents in containers on machines you own, and lets you drive them from a phone.

## Language

### Machines and roles

**Machine**:
One computer running the `holosmith` binary.
_Avoid_: Node, box, instance

**Preflight**:
The checks a machine must pass before it can run a **session**: the Docker daemon answering `_ping` on
`docker_host`, git 2.38 or newer, and `min_free_gb` free on the data root. Each answers a **finding**, and its
failures are sentences that name the fix, printed by `serve` at startup (which then refuses) and by `doctor`.
A check that cannot run — an `ssh://` daemon, a missing `git` — is a complaint rather than a pass. **Amended by
ADR-0044:** the network **self-test** is a fourth check with a different rule — "could not run" is one `warn`
and the boot continues, because it needs a container rather than a command. **Amended by ADR-0054:** a finding
is one value with two arms, so the preflight reports passes as well as failures and `serve` prints the failing
projection of it. **Amended by ADR-0068:** the disk check's *measurement* moved to `disk` and two more
readers use it — the create and the resume, which answer `507 disk_floor` — while the four sentences here stay
the boot's and `doctor`'s, because an operator who taps *create* is not reading a startup check.
Its machine names are `preflight::{check, report, check_with, report_with}`, `Finding`,
`Outcome`, `Complaint` and `Check`.
_Avoid_: Sanity check, health check, smoke test (a **preflight** is not `doctor`, which is the command that
prints one)

**Role**:
Which parts of the system a machine runs: `hub`, `server`, or `both`.
_Avoid_: Mode, type

**Server**:
A machine that runs sessions. Has Docker, git and a projects root.
_Avoid_: Worker, agent host, runner

**Service user**:
The `holosmith` system user a deployed machine runs `serve` as: `useradd --system --groups docker --home-dir <data
root> --shell /usr/sbin/nologin holosmith` on a privileged **server** install, and the owner of the install tree after
step 2's `chown`. A **hub** deploy creates the same user and joins it to **no group** — a hub drives no daemon — and
nothing in the build ever removes a membership, so a `both` machine that ran `server deploy` first keeps the docker
group and a later hub install says the user *exists and joins no group* rather than claiming it is in none. Under
`--root` there is none: the tree belongs to the invoking user, and the step's line says so. Its machine names are
`deploy::SERVICE_USER` and `deploy::SERVICE_GROUP`.
_Avoid_: Service account, holosmith user, deploy user (and a `--root` install has no service user at all)

**Hub**:
The single entry point you open in a browser. Forwards requests and previews to servers. Runs no sessions.
_Avoid_: Controller, orchestrator, gateway

**Hub deploy**:
`holosmith hub deploy`: the deploy's steps **1, 2, 5, 6 and 7**, with 3 and 4 `skipped` — a hub builds no images and
starts no proxies, because it runs no containers — and step 2's service user joining no group. It is one **deploy
half** of the same seven-step command, so it has `--root` and `--dry-run` with the same meanings a server's has, and
re-running it is the whole recovery procedure. Its machine names are `deploy::{Kind::Hub, run_as, union}` and
`commands::hub::run_deploy`.
_Avoid_: Hub install, hub setup, `server deploy --hub`

**Deploy half**:
Which part of a machine one `deploy` run installs: the **server** half (all seven steps, the daemon's group, both
proxies) or the **hub** half (steps 1, 2, 5, 6 and 7, no group, no proxy). A run **unions** its half with the role the
config already carries and writes the union into the `config.toml` step 2 installs, so `server deploy` then `hub
deploy` on one machine — in either order — leaves a machine whose config says `both`; nothing is refused over a role.
Steps 1 and 7 ask their checks about **the half this run installs** rather than the union, which is why a hub deploy
asks no Docker and no git question even on a `both` machine, while the unit step follows the **union** because a unit
describes the machine that will boot. Its machine names are `deploy::{Kind, union, run_as}`.
_Avoid_: Partial deploy, role upgrade, half-install (a **Role** is what a machine holds; a deploy half is what one run
adds to it)

**Host registry**:
Every entry point a machine may send a request to: one row per host, keyed by the **name** the operator gives it, which
is the row's identity — so registering a machine twice replaces its row rather than adding a second. A row holds the
**origin** it answers on (scheme, host and port; never a path), its **reach** (`reachable` or `unreachable`), the
`version` the last answer reported, the sentence a failed probe gave (`last_error`), when anything last asked
(`last_checked_at`), and where its **preview** listener answers (`preview`, an origin the probe reads out of that
machine's own health answer and no caller may supply). A machine's own row is the one whose origin is its **control
origin**; every other row is another machine's. Its table is `hosts`, its routes are `GET`/`POST /api/hosts` and
`DELETE /api/hosts/{name}`, and its machine names are `hosts::{Host, Reach, register, merge, probe}`.
_Avoid_: Host list, node table, inventory

**Hosts seed**:
An optional file, named by `hosts_seed` and copied to every machine, that seeds the host registry so the lists stay
aligned: `[[hosts]]` tables of `name` and `url`, read at startup and merged so the file wins on a name while a row it
does not mention survives. It is **not** filtered by the registration's host rule — it is on the machine's own disk
and as trusted as the config — where `POST /api/hosts` is, because that one exists for values arriving over a
network. A missing file is one warning and no rows; a malformed one merges nothing at all.
_Avoid_: Config sync, inventory

**Host probe**:
The one `GET /api/health` a machine makes to a registry row: `openssl s_client` for an `https` origin, a plain socket
for an `http` one, one deadline for the whole exchange, and **never a redirect followed** — an answer that is not a
Holosmith health payload is a refusal rather than a host that is merely away. Its machine names are
`hosts::probe::{ask, Outcome, PROBE_TIMEOUT}`; it happens at registration and at startup, and there is no timer that
repeats it.
_Avoid_: Health check, ping, uptime check (a **self-test** is this machine testing its own isolation, and a
**preflight** is a check that must pass before a session starts)

**Machine trust**:
The rule that decides which machines may drive this one: a call is served only when the **address** its connection came
from is the LocalAPI's own answer about a machine listed in `allowed_hubs` — never a header the caller supplies. The
identity is the node's **short machine name** (`hub` out of `hub.tail1a2b.ts.net`), because that is the name the
registry keys rows by; the tailnet **login** is the owner and is identical on every node, so it is deliberately not the
comparison. Three arms decide it: an **empty `allowed_hubs`** verifies nobody (Tailscale's ACLs are then the only
control — the spec's documented fallback, and the default), a peer at `127.0.0.1`/`::1` is this machine's **front
door** (what `tailscale serve` connects from) and costs no whois, and every other address must whois to a listed node.
An identity that cannot be read is a **refusal**, because an unread identity is not an identity. **What it cannot see,
it does not claim:** `tailscale serve` (deploy step 6) dials the origin's local target, so a call published that way
arrives from the front door and the rule there is the tailnet's ACLs plus **Owner** — the whois check governs a machine
whose control origin is its listener's own tailnet-reachable address (ADR-0075 D10). Its machine names are
`trust::{Trust, admit, Admitted, Refusal, whois, identity, front_door}`, `Tools::tailscale`, and the shell word
`tailscale whois --json <ip>`.
_Avoid_: Authentication, authorization, ACL (an ACL is the tailnet's, and it is what applies when the list is empty), hub
check

**Reach**:
Whether a host answered its own health payload the last time anything asked: `reachable` or `unreachable`. A host that
does not answer is **unreachable with the sentence it failed with**, and the row stays — an unreachable row is a fact
about a machine, not a reason to forget it. The probe asks `GET /api/health` once, over `openssl s_client` for
`https` and a plain socket for `http`, and never follows a redirect: an answer that is not a Holosmith health payload
is a refusal (`not_holosmith`), not an unreachable machine.
_Avoid_: Status (a session has a state), health, uptime, online

**Local**:
The host name a server uses for its own sessions — and, as a **reserved name**, one no host registry row may take,
because a registered host called `local` would make every forwarded path ambiguous. A **local row** is a different
thing with the same word: the registry row whose origin is this machine's control origin, which is the one row a
delete refuses while this machine has live sessions.
_Avoid_: Self, here, thisbox

**Forwarded request**:
One API call a **hub** relays to the machine that owns the session: `/api/hosts/<name>/<rest>` reaches that machine's
own `/api/<rest>`, and the answer — status, headers and body — is the peer's, not the hub's. The hub **authors** the
call it makes (`Host` and `Origin` are the peer's own origin, `X-Holosmith-Client: 1`, the caller's `Content-Type`,
`Idempotency-Key` and `Last-Event-ID` verbatim) and it is answered only when the machine's role forwards at all. It is
bounded at the reply's **head** — 5 s, or 60 s for a merge, a publish and a delete of one session — never on the body,
because a stream has no end. A path naming `local` is a `308` to `/api/<rest>` rather than a hop, and `/api/hosts/…`
is never relayed: one hop is the whole promise. Its machine names are `forward::{request_head, Reply, Framing,
BodyStream, Link, TIMEOUT, SLOW_TIMEOUT, Forwarding}`, `http::forward::respond`.
_Avoid_: Proxy request, tunnel (a **preview** is the byte tunnel; this one is parsed and re-framed)

**Preview forwarding**:
One **preview** request a hub relays to another machine's preview listener (`/<name>/<id>/<rest>` → that machine's
`/local/<id>/<rest>`), **including WebSocket upgrades**, as bytes: the hub does not read what a dev server emits, so a
chunked body, an event stream and a `101` cannot be mangled. The peer's own preview origin — its `preview_origin`,
learned from its health answer and stored on its registry row — is what the forwarded `Host` and `Origin` say, and the
`Origin` matters twice: the peer's listener accepts an upgrade only from the origin it serves. A row with no preview
origin refuses the hop with a sentence naming the probe rather than dialling a guess.
_Avoid_: Preview proxy (the proxy is the container on the session network), tunnel routing

### Configuration

**Config**:
The single TOML file of machine settings that the binary reads at startup: role, addresses, roots, limits and allow-lists. It never holds a credential.
_Avoid_: Settings, preferences, env

**Config source**:
Where the running config came from: the config file, or the built-in defaults when no file exists at the path that was tried. `config show` prints it on its second line.
_Avoid_: Config origin, mode, fallback

**Data root**:
The directory holding everything the binary writes: the database, session directories, backups and the secrets file. Its machine name is `data_root`.
_Avoid_: Data dir, state dir, home

**Projects root**:
The directory holding the git repositories that sessions are created against, deliberately outside the data root — the two compared lexically, with `.` and `..` folded — so that no path this file can spell puts a repository inside the data. A symlink that points back into the data root is the operator's responsibility. Its machine name is `projects_root`.
_Avoid_: Project root, workspace, repos dir

**Secrets file**:
The file at `<data>/secrets/keys.toml`, mode 0600, holding the provider API keys that the key proxy uses
(`[providers]`) and the per-session dummy keys it validates (`[sessions]`). The config loader never reads it;
`hs_key` is the only container that mounts anything of it, and what it mounts is the **directory**, because the
file is written by rename and a file mount would pin the old inode (ADR-0050).
_Avoid_: Keys config, credentials file

**VAPID keys**:
The machine's Web Push key pair at `<data>/secrets/vapid.toml`, mode 0600 inside the 0700 secrets directory: two
unpadded base64url strings, the 65-byte uncompressed P-256 point with its leading `0x04` (87 characters) and the
32-byte private scalar (43). `server deploy` step 2 writes it **once** and never regenerates it — absent is the
only state that generates, and a present file that is not a pair is a failed step, because regenerating would stop
every push subscription made against the old pair — and it holds no configuration: a JWT's `sub` claim is the
consumer's (HS-066). Its machine names are `vapid::Pair` and `vapid::VAPID_FILE`.
_Avoid_: VAPID credentials, push keys (and not the **secrets file**, which is the provider-key store `key set` owns
and no deploy generates)

### Sessions and turns

**Project**:
A git repository on a server that sessions are created against.
_Avoid_: Repo, workspace

**Session**:
One unit of agent work: one worktree, one container, one agent process, one id. The id is 16 lowercase hex characters generated by the database, and it is what the branch `agent/<id>`, the container `s_<id>`, `<projects>/.worktrees/<id>` and `<data>/sessions/<id>/` are all built from.
_Avoid_: Task, job, run, conversation

**Create**:
One `POST /api/sessions` request: it validates the body, writes a `starting` row and answers 202 with it. Provisioning happens after the answer, never inside it.
_Avoid_: Start, launch, new session, create session

**Idempotency key**:
The `Idempotency-Key` header a create carries, stored verbatim in the unique column `client_key`: two creates with one key are one session, and the repeat is answered with the same one.
_Avoid_: Client key, request id, dedupe token, idempotency token

**Sessions screen**:
The page the operator works from, at `/`: the machine's health, the sessions held **by each machine** with each one's
state and last activity, and — where this machine runs sessions — the form that creates one. Its groups are one
answer per machine (`GET /api/sessions` for this one, `GET /api/hosts/<name>/sessions` for every **host registry**
row), so a session's owner is the heading above it, and a machine that answered nothing is a group whose body is the
refusal's sentence — never an empty list. A machine whose **role** forwards nothing has one group, its own, and one
sentence saying so. Its states arrive as `session.status` frames, so the page holds no state of its own.
_Avoid_: Dashboard, home page, console, session list page

**Session screen**:
The page for one session: its state badge and reason, its transcript, the composer, the **Changes sheet** and the
delete affordance. Its path names the **machine that owns the session** — `/sessions/<id>` for this one and
`/hosts/<host>/sessions/<id>` for another's — so a reload lands on the same machine's transcript and every request the
screen makes is made against that name. Its first answer is `GET /api/sessions/:id` (through the hub's prefix when the
session is a peer's) and everything after that arrives on the session's stream; the path is read from the address, not
from a router.
_Avoid_: Detail page, viewer, session viewer

**Computers screen**:
The page that lists the machines, at `/computers`: one card per machine with its name, origin, **role**, version,
**reach**, when anything last asked, and its live and stopped session counts; the form that registers a machine
(`POST /api/hosts`, whose refusal is shown as the API's own sentence — without its code for `bad_host_name`,
`bad_host_url` and `not_holosmith`, because those are sentences about a value the operator typed) and the remove
affordance (`?force=true` for a `409 host_busy`). Nothing on it is stored: for every machine but this one, role,
version and uptime come from one forwarded `GET /api/hosts/<name>/health` and the counts from one forwarded
`/sessions`, folded by `status` — `live` is `starting`/`running`/`idle` and `stopped` is every other state. This
machine's own card is drawn from the health answer the page was served by and one local `/sessions`. A machine that
does not answer is a card with the refusal's sentence and a `—` count, never a zero; a version that differs from this
build's is one sentence, never a refusal; and where this machine's **role** forwards nothing, every other card says
that instead of asking. This machine is always the first card, and its own start form appears only where `role` is not
`hub`. Its
rules are `web/src/transcript.ts`'s functions — among them `apiPath`, `hostRef`, `sessionCounts`, `skewNote`,
`reachText`, `lastCheckText`, `hostFailure` and `createOffered` — with tests in `web/test/transcript.test.ts`, and its
DOM is `main.ts`'s `computersSection`.
_Avoid_: Machines page, hosts page, fleet view, nodes

**Composer**:
The session screen's box of guidance and the four actions it offers: prompt, steer, abort and resume. Which of the four exist is the state's (`affordances`): prompt on `idle`, steer and abort on `running`, resume on `stopped` and `error`, and none on `starting` or `deleting`.
_Avoid_: Input bar, message box, chat box, prompt bar

**Turn**:
One prompt-to-settled cycle inside a session.
_Avoid_: Request, message, exchange

**Turn runner**:
The background work one prompt becomes: it touches the session's last activity, asks the backend to run
the turn, and then takes the session out of `running` — `agent_settled` on a normal finish,
`unexpected_exit` on a failed one. Its machine name is the `Turn` type in `crates/holosmith/src/turn.rs`.
The backend emits the turn's events; only the runner and the state function move the session's state.
_Avoid_: Turn loop, task, executor, agent loop

**Turn boundary**:
The pair of events that brackets a turn in a transcript: `turn.start` when it begins and `turn.end` when it ends. The event kinds are their machine names, and a turn's number on screen is counted from the `turn.start` events a client has seen.
_Avoid_: Turn marker, divider, separator

**Prompt**:
Starting a turn.
_Avoid_: Send, message, ask

**Steer**:
Adding guidance to a turn that is already running. A steer changes no state: it is recorded as a `turn.steer` frame carrying the guidance, and the block a transcript draws for it is the frame's own — a steer has no message id, because no later frame continues it.
_Avoid_: Interrupt, inject, redirect

**Abort**:
Ending a turn early, on request or by timeout. The work is the abort ladder's.
_Avoid_: Cancel, kill, stop

**Abort ladder**:
The two-rung routine that ends a turn which will not end itself: rung 1 clears the agent's queue and then asks it to abort; rung 2 is reached when the row is still `running` after the grace, and kills the container and records the reason. Its machine names are the `Ladder` type and its two entry points: `Ladder::climb` (the rungs and then the way back, so rung 2 resumes the session at once) and `Ladder::end_turn` (the rungs alone, which is what a delete climbs, because resuming a session that is being removed would provision a container in order to delete it). The reason rung 2 records is the caller's — `forced_abort` for an abort, `deleted` for a delete, and the turn timeout's, the token budget's and the **session disk cap**'s own words (`turn_timeout`, `budget_exceeded`, `disk_quota`).
_Avoid_: Kill path, escape hatch, force stop, cancel chain

**Rung**:
One numbered step of the abort ladder, as spec item 16 numbers them.
_Avoid_: Stage, level, attempt

**Turn timeout**:
The deadline on a single turn: `turn_timeout_min` (45 by default, and `0` is the off switch), after which the
turn is ended through the **abort ladder** with reason `turn_timeout`. It is the turn's own, not the
session's: the clock starts when the turn does and dies with it, so a settled turn has no clock to reset and
a steer cannot extend one. The deadline is the turn runner's (`Turn::run`), because the row keeps no
per-turn timestamp — `last_active_at` moves on a steer — and both of its endings record the reason: the
runner's `running → idle` when the agent answers rung 1, and the ladder's `running → stopped` when it does
not. Its machine names are `Turn`'s `timeout`, `TURN_TIMEOUT` and the ladder's `climb`.
_Avoid_: Timeout, watchdog, expiry, kill timer

**Grace**:
The span rung 1 gets before rung 2 fires: the config key `abort_grace`, ten seconds unless the file says otherwise. It is the ladder's only number, and `0s` is refused because a ladder with no grace is rung 2 with extra steps.
_Avoid_: Timeout, patience, deadline

**Agent settled**:
The moment the agent has finished everything for the current turn and is waiting for input.
_Avoid_: Idle, done, finished

**State**:
One of `starting`, `idle`, `running`, `stopped`, `error`, `deleting`. Its machine names are the `sessions.status` column and the `Status` type. The legal moves between states are the transition table, and no production code changes a state outside the one function that applies a row of it — a create's first state is the table's own `none` row, read rather than spelled again.
_Avoid_: Status code, phase

**Trigger**:
The cause of one state change, one per row of the transition table: `create_accepted`, `pi_ready`, `provisioning_failed`, `prompt`, `agent_settled`, `abort_completed`, `abort_ladder`, `evict`, `idle_stop`, `drain`, `resume`, `unexpected_exit`, `pi_exit`, `delete`, `crash_recovery`, `disk_quota`, `expire`. Its machine name is the `Trigger` type.
_Avoid_: Event, action, cause, command

**Transition**:
One legal state change: a from, a trigger and a to, held as data in the transition table. Its machine name is `Transition`, and the one function that applies one is `Transitioner::apply`, which persists it and then emits its `session.status` event.
_Avoid_: Move, step, change, migration

**Stopped**:
A session whose container is gone but whose history, worktree and events are kept, and which can be resumed. Reached from `idle` by `evict`, `idle_stop`, `drain` and `disk_quota`, from a killed `running` turn by the abort ladder, and from any live state by a **clean exit** — the container's own exit code `0` with no **expected stop** recorded, which is the agent finishing rather than failing; never a dead end, because resume always accepts it. A delete can leave one on its way past: a running turn it had to stop (reason `deleted`) and then would not remove, which is a `stopped` session with nothing wrong with it.
_Avoid_: Ended, closed, paused, evicted

**Expected stop**:
The row's record that a stop of this session's container was the server's own: `sessions.expected_stop`, written by the one stop routine before every `docker stop -t 10` and cleared by a new container's provisioning (and by the exit classification that consumes it). It is what tells an eviction or an idle stop apart from a crash, and it is deliberately **not** part of the session object the API answers. Its machine name is `Stopper`, the one place the flag and the stop live together.
_Avoid_: Planned stop, graceful stop, stop flag, stopping flag

**Exit classification**:
The decision spec item 6 makes about a container that stopped: with the **expected stop** recorded it was our stop, so the session stays `stopped`; without it, exit code `0` is the agent finishing (`stopped`, reason `pi_exit`) and anything else is a failure (`error`, reason = the last five lines of stderr, which the badge shows as its last line). It is a pure function (`exit::classify`) plus the one entry point that applies its answer (`Exits::record`), which also appends what the container printed to the **session log**. `unexpected_exit` is the failure arm's trigger, `pi_exit` the clean one's. Its observer is HS-031's attached stdio: a detached `--rm` container leaves nothing to inspect.
_Avoid_: Exit code, exit status, death, crash detection

**Resume**:
Bringing a stopped (or failed) session back under the same id: the transition table's `resume` row takes it to `starting`, and provisioning walks it to `idle`. It is the operator's action on a `stopped` or `error` session, and the page performs it under a request that was refused with `session_stopped` or `session_error`.
_Avoid_: Restart, reopen, revive, unpause, wake

**Live session**:
A session in `starting`, `idle` or `running` — the only states the cap counts. `stopped`, `error` and `deleting` are not live, so a machine may hold any number of them.
_Avoid_: Active session, running session, open session

**Cap**:
`max_live_sessions`: how many live sessions a machine allows at once, and never fewer than one. A create at the cap stops the least recently active `idle` session and takes its slot, and answers 409 `cap_reached` only when nothing is idle to stop; a resume at the cap refuses (ADR-0022).
_Avoid_: Limit, quota, maximum sessions

**Disk floor**:
`min_free_gb` (default 20, and `0` means **no floor**, ADR-0068): the free space the data root's filesystem must
have before a machine starts work. One measurement — `df -Pk`, its `Available` column in `disk::available_gb`,
the comparison in `disk::verdict` — is read by three places: `serve`'s **preflight**, which refuses the boot;
`doctor --quick`, which prints the same finding; and the two request paths that *start* work,
`POST /api/sessions` and `POST /api/sessions/{id}/resume`, which answer `507 disk_floor` below it with a
sentence naming both numbers and the fix. Only a run that provisions containers is measured at all
(`disk::guarded`: Docker, and not a `hub`) — so on a stub run `doctor` still reports the `disk` line and the
request paths do not ask, which is the same split as a stub `serve` booting on a full disk — and `0` means
*nothing is measured*, not "measured and passed", so a floor of `0` needs no `df`. The refusal is asked before
the project's refusals and before the cap, and only starts are refused: nothing on a running turn's path reads
the disk. Its machine names are `disk::{measure, available_gb, verdict, guarded}`, `http::disk::refusal`,
`DISK_FLOOR` and `ApiError::disk_floor`.
_Avoid_: Disk quota, storage limit, free-space check (a **disk floor** is the machine's; a **session disk cap**
is one session's)

**Session disk cap**:
`session_disk_soft_gb` (default 10, and `0` means **off**): how large one session's worktree may grow before
the machine stops it, so one bad build cannot starve the others. A **watchdog** (`quota::Quota`, spawned by
`serve` beside the **sweeper**) measures every `running` and `idle` session's worktree with `du -sk` on a
five-minute tick — the interval is a constant, because it *is* the policy — and when the size is over the cap
it ends the turn (the **abort ladder**'s second rung, with the reason `disk_quota`), moves the row to
`stopped` (never `error`), stops the container through the **expected stop**, and writes one `error` event with
`code: "disk_quota"`, the measured size and the cap. The stop is **resumable after cleanup**: nothing is
deleted, and the operator raises the cap for a large Rust project rather than fighting it, because `target/`
and `node_modules/` are inside the worktree and therefore counted. Caches are per session (decision D2, spec
item 14): nothing a session mounts is shared except the project's `.git`, so a first build is slow and nothing
else pays. Its machine names are `quota::{Quota, DISK_QUOTA, watchable, measure, over}`, the trigger
`Trigger::DiskQuota`, the event kind `events::ERROR` and the reason `DISK_QUOTA`.
_Avoid_: Disk quota (the code's word, not the concept), storage limit, per-session filesystem, hard cap

**Evict**:
Stopping the least recently active `idle` session so a create can take its slot: the row becomes `stopped` with reason `evicted` and the container is stopped, while history, worktree and events stay. "Least recently active" is `last_active_at`, and a `starting` or `running` session is never a candidate — it is live and its turn is nobody's to end. Its machine names are `Trigger::Evict`, `SessionRepository::oldest_idle` and the reason `EVICTED`.
_Avoid_: Kill, delete, prune

**Idle stop**:
Stopping an `idle` session whose `last_active_at` is older than `idle_stop_min`, so a machine the operator
forgot about is not a bill. The row becomes `stopped` with reason `idle_stop`, `stopped_at` moves with it,
and the slot is free at once; the container is stopped after the row, and history, worktree and events stay,
so a resume continues the session. Only `idle` is a candidate — a `starting` or `running` session is working,
and `error` and `deleting` belong to flows of their own — and `0` is the key's off switch. Its machine names
are `Sweeper`, `Trigger::IdleStop` and the reason `IDLE_STOP`.
_Avoid_: Reap, timeout, expiry

**Sweeper**:
The background task that applies the **idle stop**: on a fixed tick it reads every session that has been
`idle` for longer than `idle_stop_min`, moves each one to `stopped` with reason `idle_stop`, and hands the
container's `stop` to a task of its own. It is a policy rather than an endpoint — nothing a client sends
starts it — and a process has one, spawned by `serve` unless the key is `0`. Its machine names are
`Sweeper` and `Sweeper::sweep` (one pass, which a test awaits).
_Avoid_: Reaper, janitor, cron, garbage collector

**Startup reconcile**:
The pass `serve` makes before the listener answers anything, putting right what a `kill -9` left behind: every
row that was live when the process died (`starting`, `idle`, `running`) is moved to `starting` through the
table's `crash_recovery` row and provisioned again — a row whose turn was in flight carries the reason
**`interrupted`**, so it comes back `idle · interrupted` — and every name no row owns is removed (a leftover
container unless its row is a delete in progress, an `agents_*` network and a `.worktrees/<id>` directory only
when no row has the id). It never deletes a branch or a ref, and it never finishes a `deleting` row's teardown.
A second pass leaves the same names and the same rows; it restarts the live sessions again, which is what a
reconcile is. Its machine names are `Reconciler`, `Reconciler::run`, the reason `INTERRUPTED` and the trigger
`Trigger::CrashRecovery`.
_Avoid_: Recovery job, crash handler, cleanup, janitor

**Stopped-session expiry**:
The daily policy that removes a session nobody came back to, so the operator does not prune by hand: a row
`stopped` for longer than `stopped_ttl_days` (default 7) is handed to the **Delete flow**, which snapshots it
first and keeps its **Kept work** — which is why an expiry is a delete and never a purge. A **WIP snapshot** that
fails skips the row instead of forcing it (a policy may not discard work), and nothing about the row moves, so
the same row is a candidate tomorrow: the retry is the candidate set. An `error` row is never a candidate, and
neither is one already `deleting`. It is item 15's `stopped_at` clock that decides, and the **Sessions screen**
shows the days a stopped row has left from that column plus the machine's window. Its machine names are
`expiry::Expiry` (and `Expiry::sweep`, one pass a test awaits), `expiry::TICK`, `expiry::cutoff`,
`Trigger::Expire`, `SessionRepository::expired_before` and `sessions::complete_deletion` (the **Delete flow**'s
second half, which the operator's `DELETE` runs too).
_Avoid_: Prune, reap, garbage collection, cron, auto-cleanup

**Delete flow**:
The one way a session is removed, and the only thing here that destroys work. `DELETE
/api/sessions/:id` ends a running turn first, takes a **WIP snapshot**, moves the row to `deleting` and
answers 202; a background task then revokes the session's key, removes what the backend owns, and
removes the row and its event log. A snapshot that fails answers 409 **unsaved work** unless the
request carried `?force=true`, and the session is left where the delete found it — except that a
running turn has already been ended by then, which is the order the spec asks for and the price of
learning that the work could not be saved. Repeating the request while the row exists re-runs the
removal; once the row is gone the answer is 404. It has two **halves and two callers**: the request half
(`sessions::deletion`) reads the row, answers a `deleting` one by re-running the removal and ends a running
turn, and the second half (`sessions::complete_deletion`) is the snapshot, the keep rules, the transition and
the spawned task. A `DELETE` is not its only entry — a **Merge** with
`delete_session` runs the same request half with `force` false and the target it just landed in, so the branch
is not **Kept work**, and a **Stopped-session expiry** runs the same second half with `Trigger::Expire` and
`force` false, which is what "using the delete flow" means.
Its machine names are `Deleter` (and the task's `Deleter::remove`), `Trigger::Delete` and the state `deleting`.
_Avoid_: Purge, drop, cleanup, destruction

**Kept work**:
What a **Delete flow** leaves in the project repository, because deleting a session must not mean losing
its work: branch `agent/<id>` when its **Ahead** is greater than zero and it is not merged into a **Target
branch**, and `refs/holosmith/wip/<id>` when its tree differs from the branch's. The delete's answer names
them (`kept`), and recovery is `git branch rescue refs/holosmith/wip/<id>`. A repository that cannot answer
keeps the branch — "cannot tell" is never "delete" — and claims nothing. One request adds a fact: a **Merge**
whose `delete_session` is true passes the commit it measured (`landed`), and a branch whose tip **is** that
commit is not kept, because the work is in the target whatever `git merge-base --is-ancestor` says about a
`squash`. A branch that has moved since is the ordinary question's, so a commit the merge never saw is never
deleted. Its machine names are `worktree::Retained`, `Worktree::retained` and the `landed` parameter that
travels from `sessions::deletion` through `Backends::retained`.
_Avoid_: Preserved work, residue, leftovers, orphan branch

**Unsaved work**:
A session's uncommitted changes that no **WIP snapshot** could record, which is what makes a delete
answer 409 `unsaved_work` instead of removing the session. `?force=true` is the operator saying the
work may go anyway; it forgives the failure, it does not skip the attempt.
_Avoid_: Dirty worktree, lost changes, uncommitted state

**Drain**:
Refusing new work and waiting for running turns to finish, so the machine can restart. `holosmith drain [--wait 10m]`
asks for one over the **admin socket** and is answered with one word — `drained`, `expired` or `refused` — which is
the exit code (0 only for `drained`). It is the process's, not the database's: `draining` is a bit the restart
clears, so there is nothing to un-drain. Its wait is the `running` rows, its stop is every `idle` row with reason
`server_restart` through **expected stop**, and it is **not** a shutdown — `serve` keeps answering reads, and
`SIGTERM` is still what ends the process.
_Avoid_: Shutdown, quiesce, pause

**Provisioner**:
The background work that turns an accepted session into a running one, reporting progress by step.
_Avoid_: Launcher, bootstrapper

**Step**:
One of the four parts of provisioning, in order: `worktree`, `network`, `container`, `pi`. Its
machine name is the `Step` type, and its word is what a `session.status` event carries in `step`. A
step event reports that the step is now running. The event that ends a *successful* provisioning
carries `null`, because no step is running then; the event that ends a failed one names the step
that failed.
_Avoid_: Phase, stage, task, milestone

**Backend**:
What performs provisioning and the session lifecycle: the stub backend, or the Docker backend. Its
machine name is the `SessionBackend` trait; `runtime::Backend` is only which backend this run chose
and is not a backend itself.
_Avoid_: Provider, driver, engine, runtime

**Stub backend**:
A session backend that produces sessions, turns and events without Docker, git or a real agent.
_Avoid_: Mock, test mode, dry run

**Fake pi**:
A stand-in agent process inside a container that replays a recorded session.
_Avoid_: Mock pi, test agent

**Agent process**:
The `pi` process a session's container runs, in RPC mode with the container's stdio attached: its stdout
carries the protocol's records, its stdin takes commands, and its exit code and stderr are what the
**exit classifier** reads. One process per launch; a resume starts a new one, and the **launch generation**
is what tells an old process's exit from the current one's. Its machine names are `agent::Agent` and
`ProcessEnd`.
_Avoid_: Container (the container is what runs it, and a stopped session has neither), worker, child

**Agent directory**:
`<data>/sessions/<id>/pi-home`: the directory the agent process reads its configuration from, as
`PI_CODING_AGENT_DIR` names it. It is a **mount** of the session's container, at its own host path, and it
is seeded once from a template with the session's model and thinking level and no secrets.
_Avoid_: Pi home, config dir, profile

**Ready handshake**:
The `get_state` command a launch sends and the `response` record that repeats its `id`, which is what makes
a `starting` session `idle`. Not a delay: a process that never answers fails the **step** that asked.
_Avoid_: Health check, warmup, ping

**Launch generation**:
The number that says which launch of a session's container is current. Every launch bumps it, the backend's
registry holds it beside the **agent process**, and an exit whose generation is no longer the current one is
not news: nothing is recorded, because a newer container has replaced the one that ended. Its machine name is
`Launched::generation`, and what it answers is `ProcessEnd::NotCurrent`.
_Avoid_: Incarnation, attempt, epoch, version

**Launch line**:
### Events and history

**Event**:
One durable record in a session's stream, identified by a strictly increasing seq. Stored in the `events` table and written only by the event writer.
_Avoid_: Log line, message, item

**Seq**:
The per-session sequence number of an event, assigned by the event writer from the session's `last_seq` counter and never reused — except by a **message snapshot**, which takes the highest seq of the delta rows it removed, so gaps are legal exactly where compaction removed rows.
_Avoid_: Offset, id, index

**Session log**:
`<data>/sessions/<id>/session.log`: the append-only file that holds what a session's process printed on
stderr — the kept tail of it, at most `agent::STDERR_TAIL`, drained from the pipe but not stored whole — with
no timestamps, plus the server's own **launch line**. It is the complete
record, where the row's `status_reason` keeps only the last few lines of a failure; spec item 15's "stderr
only" is what keeps a **tool's** output out of it — that goes to a file of its own. Its machine names are
`exit::LOG_FILE` and `exit::session_dir`. The badge a row shows is not this file: a multi-line reason is
rendered as its last line, clipped, by `transcript.ts`'s `stateText`. `holosmith logs --session ID` is its reader
(and `-f` follows it), which is why the command needs no `tail`.
_Avoid_: Container log, pi log, stderr log, transcript

**Event writer**:
The one thread that assigns each event's seq, commits a batch of events in one transaction, and only then broadcasts it. Its two handles are `EventWriter` (a producer appends) and `EventStream` (a subscriber reads committed events).
_Avoid_: Logger, event loop, queue

**Stream**:
The HTTP response at `GET /api/sessions/:id/events`: one session's committed events as server-sent events, replayed from the client's last-seen seq and then delivered live. Its SSE `id` field is the seq. Not the `EventStream` handle, which is the in-process subscriber the event writer broadcasts to.
_Avoid_: Event stream, connection, feed, socket

**Last-Event-ID**:
The request header a reconnecting client sends: the last SSE `id` it received, which is a seq. The replay starts at that seq plus one; absent or unusable, it starts at the newest `history_window` messages — a **Truncated history** — or at the beginning when the log fits the window.
_Avoid_: Cursor, offset, resume token, since

**Stream registry**:
The per-session record of open streams that `AppState` carries: how many are open (HS-066 reads it) and one close signal each (HS-019 ends them by deleting a session).
_Avoid_: Subscriber list, presence, connection pool

**Keep-alive comment**:
The `:` line axum's keep-alive writes on a stream that has been silent for 15 s, so an intermediary cannot idle-close it. Its machine name is `KEEP_ALIVE_INTERVAL`, re-exported from `http`; nothing writes it by hand.
_Avoid_: Heartbeat, ping, keepalive event

**Delta**:
An incremental piece of model output that is appended to a message.
_Avoid_: Chunk, token, fragment

**Turn error**:
One event saying something a turn could not do: `{"reason", "method"}`, and the transcript's fifth kind.
It has **no message id** — one frame, one block, nothing continues it, exactly a **turn steer** frame's
shape — and its one producer today is an extension's dialog, which a session cannot answer and therefore
cancels: the `reason` is the sentence the screen shows and the `method` is pi's own name for what was
asked. Its machine name is `turn.error` (`TURN_ERROR`) and `rpc::Cancel::frame`.
_Avoid_: Error event, failure, warning, exception

**Coalescing**:
Merging the consecutive deltas of one message and kind inside one event-writer commit into a single row with one seq, so a streaming turn does not make the log grow one row per token: `events::coalesce`, applied to the writer's batch before it becomes rows. The merged row keeps the first event's `created_at`, and the transcript cannot tell it from the deltas it replaced.
_Avoid_: Batching, debounce, dedupe, aggregation

**Tool call**:
One call the agent makes to a tool inside a turn, reported as two events: `tool.start`, which carries the
tool's name and its arguments, and `tool.end`, which carries its output. A call is keyed by its
`tool_id` and is its own block in the transcript, never a message block.
_Avoid_: Tool use, function call, invocation, action

**Usage**:
The token counts a message ends with: `in`, the tokens the message consumed, and `out`, the tokens it
produced. They arrive on `message.end` as `{"usage": {"in": …, "out": …}}`, and the screen shows them as
one `in N · out N` line inside the message's block. They are added to the session's **spend** by the event
writer, inside the transaction that assigns the event its seq, and a `message.end` that carries none is a
message whose cost is unknown — never a free one. Drawing the totals on the session screen is HS-034's.
_Avoid_: Cost, tokens, billing, price

**Spend**:
What a session has cost so far: its two `usage` columns summed, `usage_in + usage_out`, which is the one
number the **token budget** compares with its ceiling. It accumulates over the session's life — every
`message.end` adds its own counts — and a resume does not reset it, because the spec's ceiling is per
session and a resumed session is the same work. Its machine name is `SessionRepository::spend`, and the add
is `SessionRepository::add_usage`, whose one caller is the event writer's batch.
_Avoid_: Total cost, running total, counter, budget used

**Token budget**:
The ceiling `max_session_tokens` puts on a session's **spend**, so one runaway loop cannot drain the quota.
At or over it — reached, not overshot — the turn runner ends the turn through the shared ladder with reason
`budget_exceeded`, from a task of its own; a turn that settles between two polls instead records the reason
on its own ending, and the next turn trips on its first poll. Unset is unlimited, and it is the one key in
item 13's table whose absence is the off switch, where the timers use `0` instead. A backend that reports no
**usage** leaves the budget with nothing to count, so what it counts is what was *reported*, and the
session screen says so in a sentence rather than showing a zero. Its machine names are
`max_session_tokens`, `Turn::over_budget` and the reason `BUDGET_EXCEEDED`.
_Avoid_: Limit, quota, cap (the cap is `max_live_sessions`), ceiling

**Delta kind**:
Which of a message's two kinds of output a delta belongs to: `text`, the answer, or `thinking`, the agent's reasoning, which the screen renders apart from it. Its machine name is `kind` in a `message.delta` payload; it is not an event kind, because one message holds both kinds. On screen, each kind has its own node inside the message's block, and that node is never called anything else.
_Avoid_: Channel, stream, type, part

**Message id**:
The identifier a delta and a snapshot name, and the key a transcript holds one block under. Minted by the producer, unique within a session; its machine name is `message_id` in the payload. pi's messages carry no id, so the **mapping** mints one per assistant message: `msg-<the process's start instant>-<a process-wide counter>`, the stub's shape for the stub's reason — a restarted process that began again at `msg-1` would fold a new turn's deltas into a previous turn's block against a durable log.
_Avoid_: Message key, uuid, index

**Record**:
One JSON object pi printed on its stdout in RPC mode, framed by a lone LF (`docs/rpc.md`), before it means anything to this build: a `response` to a command, a session event, an extension UI request, or the process's own bookkeeping. A record is not an **event** — it becomes at most one frame, or none — and the word is deliberately not "message", which is pi's and the transcript's. The reader that splits them is `agent::read_records`; `agent::Received` is what a turn is handed.
_Avoid_: Line, message, frame, event

**Mapping**:
The one translation from a **record** to a **frame** (an event's kind and payload): `src/rpc.rs`'s `Mapping::map`, one record in and at most one frame out, plus the two facts a turn's stream needs (which message id is being streamed, how many records it declined). Pure — no clock, no database, no agent — so a fixture recorded from a real turn replays through the code a container drives. `rpc::pump` is the loop that appends what it maps, and a record the mapping has no frame for is counted and dropped, never fatal.
_Avoid_: Adapter, translator, decoder, serializer

**Message snapshot**:
One event that replaces the deltas of an old message with its whole state: `{"message_id", "text", "thinking"}`. It is the transcript's only replacing frame — a client applies it by message id, so receiving one after partial deltas is safe — and it takes the highest seq among the delta rows it replaces, which is the one place a seq is reused. Its machine name is `MESSAGE_SNAPSHOT` and its event kind is `message.snapshot`.
_Avoid_: Compaction record, rollup, summary

**Compaction**:
Replacing the delta rows of messages completed more than a day ago with one **message snapshot** each. A message is eligible when it has a `message.end` older than the 24-hour window — the spec's number, and deliberately not a config key — still has delta rows and has no snapshot yet; the fold is per session and inside one transaction, it never runs for a session whose turn is running, and `message.end`, tool frames and turn boundaries are never removed. Its machine names are `compactor::Compactor` (with `sweep` and `fold`) and `EventRepository::compact`.
_Avoid_: Cleanup, pruning, archiving, summarising

**Message**:
A completed unit of conversation output (text plus thinking) that deltas accumulate into.
_Avoid_: Bubble, response

**Transcript**:
The session screen's ordered output: one block per message id, with a marker per turn boundary. Its machine name is the `Transcript` model in `web/src/transcript.ts`, which folds stream frames into the changes the page applies and is tested without a DOM.
_Avoid_: Log view, output, console, chat

**History**:
The events of a session that a client can read back, in pages.
_Avoid_: Log, archive, transcript

**Truncated history**:
A replay that does not reach the beginning of the log: the stream sends the newest `history_window` messages and announces where it started with a `history.truncated` frame carrying `before_seq` and `turns_before`. The frames below that cursor are what `GET /api/sessions/:id/events/history?before=` answers, one page per "Load earlier" click. It is a property of one replay, never of the log — the rows are all still there.
_Avoid_: Partial log, clipped history, head cut

### Work and results

**Worktree**:
The git worktree that holds a session's files, on its own branch: `<projects>/.worktrees/<id>`, on branch `agent/<id>`, made from the project repository's `HEAD`. Its machine name is `Worktree` in `crates/holosmith/src/worktree.rs`, which is also where the creating, counting and removing live.
_Avoid_: Checkout, copy, clone

**Start lock**:
The per-session mutex a container start holds, so two starts of one session cannot interleave `rm -f` with `run` and make one of them fail with "the name is already in use". It is `DockerBackend`'s, and it is `tokio`'s because it is held across an `await`.
_Avoid_: Create lock, session mutex, start guard

**Session container**:
The container one session runs in: the name `s_<id>`, the image `agent_image` names, and the mounts the session needs. Its machine name is `SessionContainer` in `crates/holosmith/src/container.rs`.
_Avoid_: Agent container, sandbox, box

**Mounts**:
The four bind mounts of spec item 1 that make a session's files reachable: the worktree at `/work` (writable), the project repository's `.git` at its own host path (writable, because `objects`, `refs`, `logs` and `worktrees/<id>` are what a commit writes), and `.git/hooks` and `.git/config` stacked on top of it **read-only**, because each is a way a repository could run a program on the host. Each keeps its host path inside the container except `/work`, because a worktree's `.git` file holds an absolute `gitdir:`. Its machine name is `Mounts` in `crates/holosmith/src/container.rs`.
_Avoid_: Volumes, binds, mounts list

**Refused project**:
A project repository the machine will not mount into a container, answered by the create (and by a resume) with one sentence and one of three codes: `git_not_directory` (a `.git` that is not a directory, or no checkout at all), `submodules_present` (`.gitmodules` or `.git/modules`), `worktree_config_enabled` (`extensions.worktreeConfig`). Its machine name is `Refusal` and `worktree::refuse`, asked through `Backends::refuse`; nothing is written for it, and a stub run refuses nothing because it mounts no repository.
_Avoid_: Invalid project, bad repo, unsafe project

**Session branch**:
The branch `agent/<id>` that a session commits to, in the project repository.
_Avoid_: Agent branch, feature branch

**Base commit**:
The commit a session branch started from. "Ahead" is measured from here.
_Avoid_: Parent, origin, fork point

**Ahead**:
The number of commits a session branch has that its base commit does not: `git rev-list --count <base_commit>..agent/<id>`. It is computed when it is asked for, and never stored.
_Avoid_: Distance, progress, delta

**Behind**:
The number of commits a **target branch** has that a session branch does not: `git rev-list --count agent/<id>..<target>`. The other half of **Ahead**, and computed rather than stored. It is `null` (not `0`) when the repository has no target, because there is then no branch to be behind.
_Avoid_: Lag, drift, gap

**Target branch**:
The branch a session's work is merged into: the first **existing** branch of the configured `[merge] targets`, in that order (default `main`, then `master`), which is the one the UI preselects. A name the repository does not have is not a target — for the read *and* for the merge (`422 bad_target`), which are one rule rather than two. A **Merge** may write only a branch in that list, and where it writes it depends on which checkout holds it (see **Fast-forward**).
_Avoid_: Main, trunk, destination

**WIP snapshot**:
A commit of a session's uncommitted work at `refs/holosmith/wip/<id>`, stored by a **temporary index**
(`GIT_INDEX_FILE`) so the branch, the working tree and the session's own index are never touched. The
identity is `holosmith <holosmith@localhost>`; a tree equal to `HEAD` removes the ref instead of writing a
commit that says nothing. It runs on every `agent_settled` (`Turn::finished`), before every stop
(`Stopper::stop`) and before a delete. Its machine names are `snapshot::take` and
`holosmith container snapshot`.
_Avoid_: Stash, backup, autosave, checkpoint

**Project backup**:
One git bundle of a project repository — every branch and tag, `--all` — taken at most once per calendar day (UTC) when `project_backup` is on: `<data>/backups/<project>-<YYYY-MM-DD>.bundle`. It is written aside, verified with `git bundle verify`, then renamed into place, and the last seven days per project are kept; the create takes it before the session can touch the repository, and a failure is a log line that does not block the session. Its machine names are `backup::Backups` and `Backups::take`.
_Avoid_: Snapshot (that is the message snapshot and the WIP snapshot), archive, dump, checkpoint

**DB backup**:
A copy of the database file taken before a promotion.
_Avoid_: Snapshot

**Tool image**:
One image a tool's result carried, stored as its own file beside the call's text:
`<data>/sessions/<id>/tool/<tool_id>-<block index>.<ext>`. It is named by `tool.end`'s `images` entries
(`index`, `mime`, `output_ref`) and read through `GET /api/sessions/{id}/tool-image/{tool_id}/{index}`,
which serves the bytes with the type the *file* has. The bytes are never in an event payload, for the
reason a **tool call**'s output is a file, and the `index` is the block's position in pi's
`result.content`. The text of the same result keeps `[image]`, so a result with no store still says one
was there, and the transcript draws **one link per picture** rather than the picture itself.
_Avoid_: Attachment, picture, screenshot, image blob

**Changes view**:
The answer to `GET /api/sessions/{id}/changes`: what a session's branch changed against its **base commit** and whether it would merge into its **target branch**. Eleven keys — `branch`, `base_commit`, `head`, `target`, `ahead`, `behind`, `dirty`, `mergeable`, `files[]` (`path`, `status`, `additions`, `deletions`), `touches_ci` — plus the additive `mergeable_reason`, and since HS-058 the additive `choices` (`targets[]` = the configured **targets** that exist as branches, in the config's order; `remotes[]` = `[publish] remotes` verbatim) that the **Changes sheet**'s two pickers read. It is a **host** read (never a container), it writes no ref and no working tree (`merge-tree --write-tree` writes an unreferenced tree object, which is its documented cost), and `dirty` uses the same definition of work as the **WIP snapshot**. Its machine names are `changes::Changes::read`, `changes::Choices` and `http::changes`; its second route is the **Unified diff**.
_Avoid_: Diff viewer, explorer, file tree

**Changes sheet**:
The **Session screen**'s folded section for reviewing and landing a session's branch (spec item 12's UI): a *Changes* toggle, the file list from the **Changes view** with each file's **Unified diff** behind a tap, the ahead and behind counts, the `touches_ci` warning, the target and **Merge strategy** pickers with the Merge button, the Publish button when the machine names a remote, and rule 3's conflict state with Resolve with agent. Its disabled reasons are the **Merge** route's own order — a `running` turn, then no target, then the session's uncommitted work — and its two prompts are ADR-0065 D4's and ADR-0064 D10's, so the shortcut and the refusal it pre-empts describe one next step. Its rules are `web/src/transcript.ts`'s functions — among them `mergeBlocked`, `countsLine`, `fileLine`, `ciWarning`, `publishVisible`, `publishQuestion`, `conflictLine`, `mergedNotice` and `publishedNotice` — with tests in `web/test/transcript.test.ts`, and its DOM is `main.ts`'s `changesSection`; no gate case may need a browser, so the gate measures that the artifact ships the sheet (ADR-0067).
_Avoid_: Changes panel, diff screens, review page, drawer

**Unified diff**:
The answer to `GET /api/sessions/{id}/changes/diff?path=`: `git diff <base_commit> <head>` as `text/plain`, optionally narrowed to one path. It is the **Changes view**'s second route, on the same repository, the same two-dot basis and the same four `409 no_worktree` refusals; `path` must be a member of `files[]` (anything else is a `404`, so a traversal is impossible by construction rather than by cleaning up), binaries are summary lines rather than bytes, and it is **commits only** — the working tree is the **WIP snapshot**'s reader. The body is capped at 256 KB (`changes::DIFF_CAP`) with a marker line naming what git printed, and the child is bounded (`changes::COUNT_MAX`) rather than buffered. Its machine names are `changes::diff`, `changes::Diff` and `http::changes::respond_diff`.
_Avoid_: Patch endpoint, file history, blame, diff viewer

**Mergeability**:
Whether a session branch would merge into its target without a conflict, asked with `git merge-tree --write-tree` (git 2.38+), which writes a tree object and touches no working tree. Three answers, not two: `true` clean, `false` a conflict (with the paths in `mergeable_reason`), and `null` with a sentence when the question could not be asked (no target, a git too old). It is not the delete's question — **Ahead** and the keep-rules ask "can I lose this branch?", which is a different question with a different function.
_Avoid_: Conflicts, rebase, apply

**Merge**:
Landing a session branch on a **Target branch**, on the server, never in a container. `POST /api/sessions/{id}/merge` with `{target, strategy?, message?, delete_session?}`; the answer is `{merged_commit, target, strategy}` and nothing about the session row changes, because a merge is neither a transition nor an event — except that `delete_session: true` runs the **Delete flow** afterwards, and then the row is `deleting` before the answer. Item 12's rules; all six are built. **Rule 1**'s two guards are the route's and refuse a `running` turn (`409 session_running`) and the session's own uncommitted work (`409 dirty_worktree`, the definition of work the **Changes view** calls `dirty`); every other row state is merged. **Rule 5** is the Delete flow entered with the fact that this request landed the branch. **Rule 6**'s 60-second timeout belongs to the hub's forwarding client (HS-069), which this build does not have. Its machine names are `merge::merge`, `merge::Merged`, `http::merge` and `sessions::deletion`.
_Avoid_: Apply, integrate, land

**Merge strategy**:
Which of item 12's three shapes a **Merge** takes, as a type (`merge::Strategy`) because the strategies differ in *which commands run*: **Fast-forward** (`ff-only`, the default), `merge` (a commit with two parents, made in the **Temporary merge worktree**) and `squash` (one commit whose only parent is the old target and whose tree is the branch's). A fourth word is `422 bad_strategy`; an absent `strategy` is the default. The answer's `strategy` is the one that **ran**. A `merge` or a `squash` does not ask the fast-forward question — a target that has moved on is the case a real merge exists for.
_Avoid_: Mode, flavour, kind

**Temporary merge worktree**:
The detached worktree rule 2 makes a landing happen in: `<data>/merge/<id>`, at the target's commit, created with `git worktree add --detach`, removed afterwards **even on failure** — and removed *before* the target moves, because the move is what fails last. It exists so that a repository's working tree is never half-merged, and every command run in it carries `-c core.hooksPath=/dev/null` (hooks live in the **common** git directory, so a project's hooks are reachable from it) and holosmith's own identity. A leftover directory from a killed run is **removed, never refused**. Its machine names are `merge::MERGE_DIR` and the private `land`/`apply`/`clear`/`remove`.
_Avoid_: Scratch tree, merge checkout, sandbox

**Fast-forward** (`ff-only`):
The default **Merge strategy**, and the only one that needs no **Temporary merge worktree** and writes no commit: the target is moved to the session branch's tip, and refused with `409 not_fast_forward` if the target has commits the branch does not. The check is `git merge-base --is-ancestor refs/heads/<target> refs/heads/agent/<id>` — an exit code that *is* the answer — and where the write lands is decided by **which checkout holds the target**: nowhere → `git update-ref refs/heads/<target> <new> <old>` (a compare-and-swap, so a target that moved is the same refusal), the project's own checkout → `git merge --ff-only <new>` there (`409 target_dirty` when local work would be overwritten), anywhere else → `409 target_checked_out`, because in practice that is a session's worktree with a container working in it. All three strategies move the target **this** way: a `merge` commit descends from the target, so its landing is a fast-forward of it too, and there is one way to move a ref. Every `git merge` it runs carries `-c core.hooksPath=/dev/null`, so a project's hooks never run on an agent's behalf. One per project at a time (`merge::Merges`), on the host.
_Avoid_: Rebase, cherry-pick, reset, squash

**Conflict**:
A landing git could not reconcile, which is rule 3's `409 conflict`: the merge is aborted — the **Temporary merge worktree**'s removal *is* the abort — nothing changes in the project repository, and the paths travel in the envelope's additive `conflicts` key (the only error response with a payload besides `code` and `message`). The paths are read from the **index** (`git ls-files --unmerged -z`), never from a diff, so a repository's `diff.external` or a textconv driver cannot answer the question. The Changes sheet's Resolve-with-agent affordance (HS-058) prompts the session to merge the target into its branch and retry.
_Avoid_: Clash, unmerged state, half-merge

**Publish**:
Pushing a session's branch to a configured remote, on the server, as **the host's own git**. `POST /api/sessions/{id}/publish` with `{remote, branch?, confirm:true}`; the answer is `{remote, branch, commit}`, and `commit` is the branch's tip **read before** the push. `branch` names the **destination** ref — the ref pushed from is always the session branch, so the refspec is `refs/heads/agent/<id>:refs/heads/<branch>` — and an absent `branch` means both are `agent/<id>`. The push runs in the project repository with the server user's environment inherited, so the host's credential store is the only credential there is; containers never hold one. It carries `-c core.hooksPath=/dev/null`, `--no-follow-tags` and `--porcelain`, and it **never force-pushes**: the refspec has no `+`, and a remote that refused is `409 push_rejected` rather than a retry. An empty `publish.remotes` disables the endpoint (`403 publish_disabled`). Its machine names are `publish::publish`, `publish::Pushed`, `http::publish` and `config::Publish`.
_Avoid_: Deploy, release, ship, push (a push is git's verb; the route is a **Publish**)

**Protected name**:
A destination a **Publish** never writes, whatever the config says: `main`, `master`, and anything under `release/` — one constant (`publish::PROTECTED`) and one two-arm predicate, so `release-notes` is an ordinary branch name and `release/1.0` is not. `422 protected_branch`. It is not the same list as a **Target branch**: a machine may merge into `trunk` and must still never push `main`, which is why a destination naming a configured target is its own refusal (`422 target_not_pushable`, item 12's "push it from the host").
_Avoid_: Forbidden branch, reserved ref

**`touches_ci`**:
Whether a session's diff changes CI configuration or a hook directory, which warrants a warning before publish. One constant (`changes::CI_PATHS`) and one predicate (`changes::touches_ci`, which the Changes sheet's publish warning reads — the **Publish** route itself reads none of it, because a route that carried a flag it never computed would be claiming a diff it did not look at): directory prefixes (`.github/`, `.gitlab/`, `.circleci/`, `.buildkite/`, `.woodpecker/`, `.husky/`, `.githooks/`) and file names matched on the last path segment (`.gitlab-ci.yml`, `.travis.yml`, `Jenkinsfile`, …). An **added** file counts like a modified one; `.git/hooks` is not in the set because a diff can never contain it.
_Avoid_: CI flag, risk flag

### Network and credentials

**Egress network**:
The machine's own, **not** internal Docker network — `egress` — that the proxies are attached to, so a
container's only way out is through something this build controls. It is created once at startup on a Docker
run, and its absence is a `warn` rather than a refusal because a session's own network step already fails with
a message about itself. Its machine name is `docker::EGRESS_NETWORK`.
_Avoid_: Proxy network, outbound network, dmz

**Self-test**:
The startup measurement of a machine's isolation: one probe container on a temporary internal network tries to
reach the internet, the host, and the egress proxy, and the verdict is the probe's exit code. `Passed` (nothing
reached, a `debug` line), `Unknown` (the probe could not run: one `warn`, the boot continues) and `Failed`
(something was reached: the boot **refuses**, through a `preflight::Complaint`). A *refused* connection counts as
reached — a reset proves the host answered — which is the distinction the test rests on. The proxy target is
what makes **the off case** measured rather than assumed: `egress-proxy` is the proxy's network **alias**, so on a
network nothing is joined to it does not resolve, and a proxy that answers there is a finding of its own (it means
a session that asked for no internet access would reach one anyway). The probe runs **twice on that network** —
once bare, once with the proxy joined — because a silence is only evidence if the alias answers the moment
something is attached; the second run is a `debug`/`warn` line and never a verdict, since a proxy that will not
join fails a download rather than a session. Its machine names are `selftest::run`, `selftest::Outcome`,
`selftest::PROXY_TARGET`, `selftest::KEY_PROXY_TARGET`, `selftest::pair`, `selftest::key_pair` and
`templates/selftest-probe.js`.
_Avoid_: Health check, smoke test, probe

**Key proxy**:
A **mode of this binary** (`holosmith proxy key`), run as the `hs_key` container on the `egress` network and
joined to every session network under the alias `key-proxy`. Pi is configured with
`http://key-proxy:8080/<provider>` and a per-session **dummy key**; the proxy validates that dummy against
`<data>/secrets/keys.toml`, swaps in the provider's real key in the header that provider uses, and forwards the
request over TLS. The provider is selected by the first path segment from a fixed table (`key::PROVIDERS`), so
a request cannot name its own host or port. It logs one line per request with the session, the provider, the
host and the bytes. Its machine names are `keyproxy::Route`, `container::KeyProxy`,
`key::KEY_PROXY_NAME`, `key::KEY_PROXY_ALIAS` and `key::KEY_PROXY_PORT`.
_Avoid_: Auth proxy, gateway, sidecar

**Dummy key**:
The per-session throwaway credential the agent holds instead of a real key. It is minted by
`key::Store::dummy_for`, stored in `<data>/secrets/keys.toml`'s `[sessions]` table, written into the session's
`pi-home/models.json` as a literal `apiKey`, and revoked when the session is deleted. It has no spending power
outside the key proxy, which is the point: the real key never enters a session mount, an environment variable or
the agent's directory.
_Avoid_: Session key, placeholder, fake token

**Egress proxy**:
A **mode of this binary** (`holosmith proxy egress`), run as the `hs_egress` container on the `egress` network
and joined to each session network that allowed internet access. It is the only route from a session network
to the internet: `CONNECT` to port 443 only, allow-listed names only, and never a destination that resolves
into the machine. It logs one line per answered request with the session, the host and the bytes. Its machine
names are `egress::Rules`, `container::EgressProxy`, `container::PROXY_NAME` and `container::PROXY_ALIAS`.
_Avoid_: Firewall, forward proxy, gateway

**Allow-list**:
The set of **exact** hostnames the egress proxy will tunnel to (`[egress] allow`). A subdomain is a second
entry, because a suffix match is the wildcard `config validate` refuses. It **should** never contain a provider
host — a model call goes through the key proxy — and nothing checks that for an operator: what holds the two
apart is that the shipped list is empty, that provider calls are kept off this route by `NO_PROXY=key-proxy`, and
that a review of an `[egress] allow` change is a review of where a session may send data.
_Avoid_: Whitelist, permitted hosts, domain filter

**Proxy username**:
The session id a request carries as its `Proxy-Authorization` username — and therefore the session id in the
container's `HTTPS_PROXY` (`http://<id>@egress-proxy:3128`). It is attribution, not authentication: the
password is empty on purpose, because nothing checks it and a secret in an environment variable is what
spec item 2 forbids. `egress-proxy` is the proxy's network **alias**, not its container name.
_Avoid_: Proxy auth, proxy token, credentials

**Session network**:
The private internal Docker network one session's container joins: `agents_<id>`, created `--internal` so that
nothing on it has a route out — no internet, and no way to another network's gateway. Nothing else joins it.
Its machine names are `container::network_name` (the `agents_` prefix, spec item 2) and
`docker::create_internal_network`. **Measured**: an internal network still *reports* an IPAM gateway, and that
gateway answers from inside it (ADR-0044) — so "no route out" is the property, not "no gateway"; the proxies of
HS-037/039 are what a container's only way out runs through.
_Avoid_: Agent network, container network, sandbox network, bridge, net

**Internet access**:
The per-session choice of whether the machine's egress proxy is joined to the session's own network
(`sessions.network`; `network_default` is only the create form's starting state, never a body's default). **Off by
default.** It is **topology, not configuration**: a session's container carries the same `HTTPS_PROXY` either way,
and what changes is whether anything answers on that address — so "off" means nothing outside the machine is
reachable. The **boot** measures the proxy's half of that (a network nothing is joined to reaches nothing, and
joining the proxy is what changes it) and `case_internet_toggle` measures a **session's** (the row's choice, the two
networks it produces, and the same choice after a resume). Its machine names are `sessions::NewSession::network`,
`DockerBackend::apply_proxy` and `selftest::PROXY_TARGET`; the phrase an operator reads is `transcript.ts`'s
`networkText`.
_Avoid_: Network flag, online mode

**Control listener**:
The HTTP listener that serves the app and the API.
_Avoid_: API port, main port

**Preview listener**:
A second HTTP listener that serves only session previews, on its own origin.
_Avoid_: Preview port, proxy port

**Control origin**:
The origin of the control listener, spelled the way a browser spells it: scheme and host, plus a port only when it is not the scheme's default (`Origin::with_port`).
_Avoid_: App URL

**Preview origin**:
The origin of the preview listener. Never the same as the control origin.
_Avoid_: Preview URL

**App shell**:
The page the control listener serves at `/`: the app's name, version, role and uptime, read from the health endpoint. It holds no sessions yet.
_Avoid_: Home page, landing page, index, SPA

**Health endpoint**:
`GET /api/health` on the control listener: name, version, role and uptime of the running binary, and the one endpoint no role check applies to. Never the same thing as `holosmith status`, which also asks other machines.
_Avoid_: Status, ping, heartbeat, healthcheck

**Guard**:
One of the four checks the control listener makes on a request that is not a health probe: the Host allow-list, the control origin, the JSON client header, and the owner. A route cannot skip them.
_Avoid_: Filter, middleware, auth

### Operations

**Status command**:
`holosmith status`: one `key value` line per fact about this machine — its role, its version, the running
server's uptime, one count per session status, whether each of the two proxies is running, and the last health of
every machine registered with it. Each value has three arms, `measured`, `unknown: sentence` and `fail: sentence`,
and only `fail` changes the exit code. Its machine name is `status::Report`. Never the same thing as the health
endpoint: the endpoint answers about one running binary, and this command **reads** the host registry for the others
— `machines <n>` and one `machine <name>` line per row, through a read-only open of the table, so no server has to be
running and no probe is made. Those facts are the last probe's, which is as old as the boot (ADR-0074 D5), and
`checked <n>s ago` is the sentence that says so; `hosts::facts` is the one function this command and `holosmith hub
host list` share, so a machine reads the same in both.
_Avoid_: Health, healthcheck, ping, machine state

**Uptime**:
How long the **running** server has been up, in seconds, as its own health payload reports it (`uptime_s`, from
`Runtime`). It is a fact about a process rather than about a machine: a stopped server has no uptime, a rebooted
machine starts a new one, and `holosmith status` prints `unknown:` with a sentence when nothing answers rather than
guessing from `/proc` or from systemd.

**Uninstall**:
`holosmith uninstall [--purge]`: the inverse of `server deploy`. It removes the unit, the two proxies and the
machine's `egress` network, and **keeps** the data root, the projects and every session's own container and network;
`--purge` also removes the data root (and the containers and networks the rows named), and refuses while a session
is live unless `--force`. Its lines carry four words of its own — `removed`, `kept`, `nothing`, `failed` — not the
deploy's five. Its machine name is `uninstall::Report`.
_Avoid_: Teardown, remove, destroy, deploy --reverse

**Version**:
The semantic version of the `holosmith` binary itself, such as `0.1.0`. It is not a Release and never a synonym for one.
_Avoid_: Release, build, build number

**Git tag**:
The tag the binary was built from, or the short commit sha when no tag exists.
_Avoid_: Commit, release version, build id

**Pinned pi version**:
The one pi version that the build, `doctor` and the agent image all read, kept in `pins/pi.txt` and nowhere else. Its machine name is `pi_version`, in the binary's output and in any later config key.
_Avoid_: Pi version, agent version, latest pi

**Migration**:
One numbered SQL file that changes the database schema, embedded in the binary and applied in order at startup. Forward-only: an applied migration is never edited and never runs twice.
_Avoid_: Patch, upgrade script, version script

**Contract migration**:
A migration that drops or renames something an earlier release still uses. It ships one release after the migration that replaced it, and only in a file marked as a contract migration, so a release that only expands can never contain one by accident.
_Avoid_: Cleanup script, destructive migration, breaking migration

**Schema version**:
The number of the newest database migration the binary embeds, which `version` reports and the database records.
The two can disagree, and `schema::verdict` is the one function that says what that means: a **recorded** number
below the binary's is migrated on open, and one above it is a database a newer release wrote — `serve` continues
(a rollback must not need a restore) and `doctor` refuses, and `Verdict::sentence` is the words both use.
_Avoid_: DB version, migration count, schema revision

**Previous schema**:
The schema the **previous release** shipped, kept as a real SQL dump in `docs/schema/previous.sql` and compared
with this build's as a subset by `tests/schema_previous.rs`: every table and column it names must still exist, so a
drop or a rename fails and an addition does not. It is not a migration and does not live under `migrations/`, and
refreshing it (`scripts/schema-snapshot.sh`) is a **release step**, so its diff is the record of what a release gave
up.
_Avoid_: Old schema, baseline schema, schema history

**Release**:
One built version of the binary, in its own directory, with the `current` symlink pointing at the live one.
_Avoid_: Version, build, deploy

**Promotion**:
Moving a candidate release to live: smoke test, drain, backup, switch, verify, or roll back.
_Avoid_: Deploy, upgrade, rollout

**Rollback**:
Pointing `current` back at the previous release.
_Avoid_: Revert, downgrade

**Remote deploy**:
A deploy that runs on **another machine**: `holosmith server deploy --ssh user@host` (or `hub deploy`) sends the
binary this command is running — or the file `--binary` names — to the target over ssh, verifies the copy's SHA-256
on that side, and runs the same command there, so the seven steps, the `kind` and the flags are the target's own and
nothing about *what* is installed is decided here. The copy lands at
`$HOME/.cache/holosmith/deploy-<first 12 hex of the digest>`; the architecture is compared **before** it is sent (the
file's ELF header against the target's `uname -m`), and a run that fails before the fourth child has executed
nothing on the target. It is not a second kind of deploy: `--binary` names the file to send, never a second binary to
run. Its machine names are `remote::Remote`, `remote::Plan`, `Binary`, `Sent`, `Machine` and `Tools`.
_Avoid_: Remote install, push, upload, scp

**Deploy step**:
One of the seven parts of a `deploy` run — `holosmith server deploy` or `holosmith hub deploy` — in the spec's
order: `preflight`, `files`, `images`,
`proxies`, `service`, `origins`, `health`. A deploy step is a **state read** and an **apply**: the read says
whether the step is already satisfied and what is missing, the apply does the missing part, and `--dry-run`
prints the read alone. Each one reports one of five words — **satisfied**, **needs** (a dry run's plan),
**changed**, **skipped** (carrying the reason, and never a pass) and **failed** (which stops the run and makes the
resume command the last line). `skipped` has had two reasons in its life and both are "no body to run here": a story
that still owned the body (nothing since HS-049) and — since HS-072 — a **deploy half** with no work for the step
(a hub's `images` and `proxies`), which is why a hub run's tally reads `5 satisfied, 0 changed, 2 skipped, 0 failed`.
Its machine names are `deploy::DeployStep` and `DeployStep::ALL`, and `--root` is the install tree the step's paths
are built under. It is not `Step`.
_Avoid_: Install step, deploy phase, stage, deployment

**Image build**:
The two images `server deploy`'s step 3 builds on this machine, from the Dockerfiles embedded in the binary:
`holosmith-proxy:<version>`, the artifact on `alpine` plus openssl, which both proxies run; and
`agent-image:<version>`, the artifact plus pi at the pinned version, git and the configured toolchains, which a
session's container runs. It is a **build**, not a pull: nothing in this phase pushes or fetches an image, and
the context is an archive holding the Dockerfile and the running binary. Its machine names are
`images::Kind`, `images::ensure` and `images::proxy_image`/`images::agent_image`.
_Avoid_: Container image, docker image, image pull, base image

**Image label**:
One of the five `org.holosmith.*` values a built image carries — version, git tag, pinned pi, the SHA-256 of the
binary it was built from, and (on the agent image only) the toolchain list — written by the Dockerfile from
build arguments and read back by `server deploy`'s step 3 skip rule and by `doctor`'s `image` finding. The
digest is what makes a **rebuilt** artifact a rebuild: `git describe` is the same for two builds of the same
commit, dirty tree included. An image with no Holosmith labels is one another tool built (`pi-agent:local`),
which is a supported machine rather than a mismatch. Its machine names are `images::LABEL_VERSION`,
`LABEL_GIT_TAG`, `LABEL_PI`, `LABEL_BINARY` and `LABEL_TOOLCHAINS`.
_Avoid_: Tag, annotation, metadata, image version

**Doctor**:
The preflight and self-test command that decides whether a machine may run sessions: `doctor --quick` is the
list `serve` refuses on and the unit file's `ExecStartPre`, and `doctor --full` adds the network self-test,
the two proxies and the agent image's pi version. Its exit code is non-zero iff a **finding** failed. Its
machine names are `doctor::run`, `Depth`, `Tools`, `Machine` and `Report`; the network self-test keeps its own
`selftest::Outcome`, which is the report's entry for that check and not a second vocabulary.
_Avoid_: Healthcheck, verify

**Finding**:
One check's answer: the check's word, whether it **passed**, **failed** or is **unknown**, and the sentence the
operator reads. Only a failure changes `doctor`'s exit code; an unknown is "this check produced no verdict about
whether `serve` may run here", and its sentence says what was missing and what does need it.
_Avoid_: Result, verdict (`Verdict` is the schema's word), status

**Admin socket**:
The local unix socket used for commands that must never be reachable over HTTP: `<admin dir>/drain.sock`, 0600 in a
0700 directory whose own protection is the whole authorisation. Its one verb is the **drain**.
_Avoid_: Control socket, IPC

**Verification table**:
The eleven checks V1–V11 in `HolosmithSpec.md` that phase 1 must run on a real Docker host, each with the condition
that makes it a pass, the fallback to take if it fails, and a **result cell** (the table's own `Result` column)
recording the verdict, the automation that produced it (a `case_*` in `scripts/verify.sh`'s `serve_cases`, a
`tests/*.rs`, a `scripts/*.sh`, or `docs/build.md`'s by-hand procedure) and, on a failure, the fallback taken. It
is the record: there is no second document, `case_verification_table` fails the gate when a row rots, and V11 stays
deferred to phase 3. A result cell is not a `Finding`: a Finding is one of `doctor`'s checks, while a cell is a
verification row's verdict *and* the automation that stands behind it.
_Avoid_: Vertical-slice table, checklist, test matrix, verification report, phase-1 report

### Notifications

**Web Push subscription**:
One browser's registration, stored in the `push_subscriptions` table as `endpoint`, `p256dh`, `auth` and
`created_at`: the endpoint it receives on, the two keys RFC 8291 encrypts to, and when this machine first heard from
it. It is the **browser's** material and not the machine's — `vapid.toml`'s pair signs, a subscription's keys are
sealed to — so it is rows rather than a file. Machine-wide, not per session: item 11's payload carries the session,
and a subscription that named one would be a registration no browser makes. Its machine name is
`push::repository::Stored`.
_Avoid_: Push target, device, client, subscriber

**Push endpoint**:
The capability URL a subscription's row is keyed by — `https://host[:port]/…` on a push service. Its **origin** is
the VAPID token's `aud`, and the URL itself never appears in a log line: anybody holding it can send to that browser.
_Avoid_: Push URL, push channel, subscription URL

**Notification**:
The message a subscription earns when a turn ends (spec item 11): a VAPID-signed, sealed body whose plaintext is
exactly `{host, session_id, kind}`. `host` is **Local**, `session_id` is the row's id and `kind` is one of four
words — `error`, `turn_timeout`, `budget_exceeded` and `turn_finished` — the first three of which are words this
build already uses for the same fact. It is not sent while an SSE stream for that session is open, and the envelope
never carries a message's content. Its machine names are `push::Pusher::announce` and `push::Watcher::observe`
for the sender, and `web/src/sw.ts`'s `push` handler for the four sentences a phone shows.
_Avoid_: Push notification (the *module* is `push`; the thing a phone shows is a Notification), alert, message

**Application server key**:
The Web Push pair's **public** half, base64url, 87 characters — the `k=` of every VAPID token this machine signs and
the value a browser hands to `pushManager.subscribe`. It is public by design; the private half never leaves
`<data>/secrets/vapid.toml`. HS-067 serves it as `GET /api/health`'s sixth key (`application_server_key`), read at
the moment of asking, `null` on a machine with no pair; `vapid::public_key` is the reader.
_Avoid_: VAPID public key, push key (a **Push subscription**'s `p256dh` is the browser's, not this)

**Installed app**:
Holosmith running from a home-screen icon rather than a browser tab: `display: standalone` from the **app manifest**,
or iOS's own `navigator.standalone`. It is not a second build — the same shell, served from the same origin — and
iOS delivers a **Notification** only to it, which is why the **Settings screen** says so.
_Avoid_: Native app, wrapper, home-screen shortcut

**App manifest**:
`/manifest.webmanifest`, shipped in the app and served as `application/manifest+json`: name, `start_url` and `scope`
`/`, `display: standalone`, the theme and background colours, and the **app icons**. Its shape is a node test's
(`web/test/push.test.ts`) and its served bytes are `case_pwa`'s.
_Avoid_: PWA config, web app config

**App icons**:
The four checked-in PNGs the app ships — 192 and 512 `any`, 512 `maskable`, and 180 for iOS's `apple-touch-icon`
link — because iOS will not take an SVG home-screen icon and Chrome will not offer an install without both `any`
sizes. Generated once, outside the build, and committed as bytes.
_Avoid_: Favicon (that is the separate SVG the tab shows), logo

**Service worker**:
The one script the app registers at `/sw.js` (scope `/`), built from `web/src/sw.ts` so its rules — the four
`kind` sentences, the sixteen-hex id rule, the flattened subscription body — are `web/src/push.ts`'s, typechecked and
node-tested like every other sentence. It handles `push`, `notificationclick` and `pushsubscriptionchange`, takes
over at once, and has **no `fetch` handler**: it caches nothing, because a cache is a second source of truth for a
page whose every value came from a response or a frame.
_Avoid_: Worker thread, background page, PWA script

**Settings screen**:
The app's third screen, at `/settings`: this machine's **Web Push subscription** state (one row, because there is one
control origin), the one control the browser's state allows, and the iOS home-screen sentence. It is a path of its
own rather than a section of the **Sessions screen** because a subscription belongs to the machine — and a machine
just installed has no sessions. What it *says* is `web/src/push.ts`'s and has a node test per sentence — the state
sentence and its detail, the button's label, the per-machine row, the two install sentences and the four a
notification draws; what the page adds (the heading, the busy label and the two refusals it makes about its own
failed request) is the DOM's, like every other affordance's.
_Avoid_: Preferences, config screen (the **Config** is the machine's TOML file, and this screen edits nothing in it)
