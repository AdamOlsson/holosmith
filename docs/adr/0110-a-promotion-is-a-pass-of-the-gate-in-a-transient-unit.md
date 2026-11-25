# A promotion is a pass of the gate in a transient unit, and the tailnet is the authority that presses it

The web app can create a session, prompt it, merge its branch and publish it, and it can do nothing else that touches
the machine: no route rebuilds, promotes or restarts anything, and `POST /api/sessions/{id}/merge` is the furthest any
request goes with the host's own git — it moves a target ref and stops there. That is a decision, not an omission, and
it is recorded twice: `crates/holosmith/src/drain.rs`'s "Why a socket and not an endpoint", and ADR-0060 D2. Both say
the same thing in the same shape — the machine's restart authority is reachable through a file and not a route, so a
phone that can reach the control origin still cannot use it.

This ADR takes the opposite decision for promotion, and it is not an argument that the drain was wrong. The drain is
one bit on `AppState` and one policy that stops idle sessions; a promotion writes a release directory, switches
`current`, builds the two product images, restarts the unit through `systemctl` and puts the previous release back if
the new one does not answer. Putting the drain on the wire would be a smaller act than what ADR-0060 refused; putting
a promotion there is a larger one — so it is a decision before it is code, and this is the decision.

Five decisions, each with the alternative it refused: **anyone the tailnet admits may press** (D1); **a passing
verification is the only requirement, in place of `--acknowledge-review`** (D2); **the candidate is a clean `master`
of the source tree, and nothing else** (D3); **the job is a transient user unit, not a child of `serve`** (D4); and
**the gate runs in the running release's agent image** (D5). One consequence is not mechanical and has its own
section: verification is a **correctness** gate, not a safety gate, because it runs the candidate's own code with the
operator's authority.

**What has landed since, and what has not.** The candidate resolver (`candidate::resolve`, `[update] source`), the
launcher (`update::start`, the transient user unit), the verb (`holosmith update`, which is one pass of the gate and
then `promote`'s pipeline, with the free-space check in front of it and one JSON report behind it), the digest line
in `scripts/build.sh`, and the three routes — `POST /api/update`, `GET /api/update`, which answers the report file and
the log after the restart the job performs, and `GET /api/update/candidate`, which says what a press would make live
before there is a press — are in the tree, and so are the **page and the button**: the app's **Service** screen at
`/service`, which draws the live version, whether the machine is draining, `master`'s commit and tag, one button with
the wait it starts stated before the press, and the job's own lines while it runs and its ending when it stops.

The screen is a screen of its own and not a card on the **Computers** screen, which owns "what is this machine
running" for every machine and this one included: a promotion's job prints for three quarters of an hour and its lines
do not fit in a card, so this machine's card gained **one sentence and a link** to the screen and nothing else. That is
the placement this ADR owes the app a sentence about, and it is the whole of what it costs the card. The candidate is a
route of its own rather than a field on `GET /api/update` because that one is polled while a job runs and the candidate
is five `git` children: a field there would make every poll five processes, and the report's own `candidate` is the
commit a run was *about*, not the commit a press would make live now.

What is still **not** in the tree is a measurement of the whole path: no promotion has been started from a browser, and
the sections below that answer questions those changes own still say so rather than pretending otherwise.

## Superseded in part: ADR-0060 D2, and ADR-0082 D2's review gate

**ADR-0060 D2** is about the drain socket, and almost all of it stands: the path, `--admin-dir`, and the three
bind-time rules are unchanged, and so are D1 and D3–D9. The half this ADR supersedes is D2's closing paragraph, which
is a general claim and not a fact about the socket:

> The socket is **0600** and the directory **0700**. There is no authentication of its own: on a deployed host only the
> service user can open the file, so being able to open it *is* the authorisation, and the API table's "not reachable
> over HTTP" is then a fact about the filesystem rather than a claim about a route.

The fact is still true of `<admin dir>/drain.sock`, and the drain stays on it. What this ADR supersedes is its use as the
answer to "who may restart this machine": a promotion is a decided route, so the authority ADR-0060 described sits on a
route as well as on this socket, and the drain stops being the machine's only machine-touching act. `drain.rs`'s header
sentence — "no route, no guard, no CORS layer, no `App` — so a phone that can reach the control origin still cannot
drain the machine" — likewise stays true of the drain and stops being true of the machine.

**ADR-0082 D2** is superseded in part for the second time. ADR-0086 removed its first half (the privileged-install
check); this ADR removes the second, which is the review gate:

> **The review gate is `--acknowledge-review`, and the pipeline cannot run without it.** … a review is a human act;
> the flag is the *record* of it, printed with the digest and the tag it acknowledges so the log says which binary was
> reviewed.

A passing verification replaces that word for a promotion the browser starts, and D2 below is the argument. ADR-0082's
D1 and D3–D13 stand, including the pipeline's order, its refusals and the switch-back that follows a failed health
poll; what changes is only *what is required before the pipeline may run*.

Both files carry the marker in the shape ADR-0088 left on ADR-0086: a `Superseded in part by ADR-<n>:` line at the top of
the superseded section, naming what moved and, after the dash, what stands. A reader who arrives at the old decision first
learns which half moved, and a reader who arrives here learns which half stands.

## Five facts, each quoted from the tree

Every decision below rests on one of these, so they are quoted rather than paraphrased. Each is a line in this tree
today; a later change that contradicts one of them makes this ADR wrong, not merely old.

**The owner check is vacuous for a caller that sends no login.** `crates/holosmith/src/http/guard.rs`, inside
`Guards::check`:

```rust
                // Direct access, not behind `tailscale serve`: there is no login to check.
```

The arm below it is `(None, _) => true`: a request with no `tailscale-user-login` header is not answered `bad_owner`
whenever `owner` is configured, and with `owner` unset the check does not run at all.

**A forwarded call is authored by the hub, so a peer cannot tell it from a call at its own origin.**
`crates/holosmith/src/trust.rs`:

> The hole it closes is the one ADR-0074 recorded rather than discovered: [`crate::forward::request_head`] authors a call
> whose `Host` and `Origin` are the peer's own and which drops `tailscale-user-login`, so a peer cannot tell a
> forwarded call from a call at its own origin — and its `owner` check is vacuous for one.

And the headers are `crates/holosmith/src/forward.rs`'s: `Host` and `Origin` are the peer's own origin, and
`X-Holosmith-Client: 1` "is this machine saying it is one". The route that relays them, in
`crates/holosmith/src/http/forward.rs`, is one wildcard path for five methods — "One wildcard route rather than a
second registration of every session route, and the peer's paths stay its own."

**The unit is a user unit, and its cgroup outlives `serve` by thirty seconds at most.** `deploy::unit_text` renders
this `[Service]` section (`{exe}` and `{config}` are its format string's placeholders):

```text
[Service]
Type=simple
UMask=0077
NoNewPrivileges=yes
LockPersonality=yes
RestrictSUIDSGID=yes
RestrictNamespaces=yes
RestrictRealtime=yes
SystemCallFilter=@system-service
ExecStartPre={exe} doctor --quick
ExecStart={exe} serve --config {config}
Restart=on-failure
RestartSec=5s
KillMode=mixed
TimeoutStopSec=30s
```

`[Install]` is `WantedBy=default.target`, so it is the operator's **user** unit (ADR-0088), not a system one.

**`scripts/build.sh` prints the digest and writes no digest file.** Its last loop prints
`<sha256>  dist/<triple>/holosmith (N bytes)` and leaves one file behind. `holosmith build` writes two: `builder::DIGEST`
is `"holosmith.sha256"`, and `write_digest` writes `format!("{built}  {BINARY}\n")` — the shape `sha256sum -c` reads.

**The gate's own container cases own the absent state of the two product tags.** `scripts/verify.sh`'s
`case_server_images` removes `holosmith-proxy:<version>` and `agent-image:<version>` at its start, asserts each is
gone, and rebuilds them by running the **candidate artifact's** `server deploy`:

> This case owns the absent state: the deploy's own proxy containers go first (a running container pins the image it
> was started from, so an untag with them up fails and the build that follows would be a no-op), then both tags are
> removed — and the removal is *asserted*, because "the image was already there" is exactly the state this case exists
> to rule out.

## D1 — Anyone the tailnet admits may press

A promotion route carries **no rule the merge route does not already have**: ADR-0010's four header checks, then
`trust::Trust::admit` where it applies, and nothing else. No login-header requirement of its own, no owner check beyond
the guard's, no separate promoter credential. The authority is `tailscale serve` and the tailnet's ACLs — exactly what
it is for merge, publish, delete and every other mutating route today.

**Rejected: a `tailscale-user-login` requirement on this one route.** The header is absent on a direct call and the
guard's own words for that arm are "Direct access, not behind `tailscale serve`: there is no login to check", so a
requirement here would be a second owner check beside the configured one, disagreeing with it in the one case the first
exists for. It would also have to answer what the login *is*: the header names the tailnet user, one operator's tailnet
has one user, so the check would admit everybody it could see and refuse only the call that arrived without the header —
the shape of a rule that looks like a boundary and is not one.

**Rejected: an owner check beyond the guard's.** The config's `owner` exists, is optional, and is already applied
where a machine wants it — and `docs/config.md` says in the same row what it is worth: "this is a check on the proxy in
front, not an alternative to tailnet ACLs". A promotion-only spelling of it would be a second identity model for one
button, and it would still not be a boundary: a hub writes a forwarded call's headers, so the check would be about
which machine relayed the button and not about who pressed it.

**Rejected: a separate promoter credential** — a token, a key in `keys.toml`, a password prompt. Every other
machine-touching act on this API is authorised by the tailnet, the credential would need a store, a rotation and an
answer to where the browser keeps it, and it would no longer be "the authority is `tailscale serve`" — it would be "the
authority is `tailscale serve` plus a shared secret from a phone". A secret a phone keeps is not a boundary; it is a
second thing to leak, on the device that is most likely to be lost.

**Recorded consequence, not a discovered one.** A hub page can promote **any registered host**. The hub's forward route
is `/api/hosts/{name}/{*rest}` — one wildcard for five methods — and `forward::request_head` writes the peer's `Host`,
the peer's `Origin` and `X-Holosmith-Client: 1`, so the peer's four checks pass for a call the browser made at the hub.
The owner check is vacuous for that call, as `trust.rs` says in as many words, and `Trust::admit` admits a listed hub
by *machine* name (or, with `allowed_hubs` empty, admits anything: the config's "ACLs only" arm). The consequence is
therefore not "a hub can promote its own machine"; it is that a page served by one machine can promote every machine
the registry holds, and the tailnet's ACLs are the whole of what stands in the way. That is the same consequence merge
already has, and it is written here because a reader will reach for the owner check and find it empty.

## D2 — A passing verification is the only requirement

`--acknowledge-review` is replaced by **the gate**: `scripts/verify.sh` — the proof, not `scripts/gate-quick.sh` — run
in a container on the machine's daemon, **full and not `--no-build`**. The reason for that last part is the whole of
the decision: the artifact the gate's `build` phase produces is the artifact that gets promoted, so a gate that skipped
the build phase would either promote nothing or promote bytes no phase measured. `scripts/verify.sh`'s own usage line
is where the flag comes from — `Usage: scripts/verify.sh [--keep] [--no-build]` — and its `build` phase is
`run_phase build scripts/build.sh` followed by `run_phase embed scripts/check-embed.sh`, with `scripts/check-static.sh`
as `scripts/build.sh`'s last step.

**Rejected: `--acknowledge-review` kept beside the gate.** Then the pipeline has two required words and the weaker one
is the product's record: a human's acknowledgement says the diff was read, and it says nothing about whether the tree
builds, whether its tests pass or whether the smoke run's candidate can serve a session. A promotion that carries a
review flag and a green gate is a promotion whose flag adds nothing but a step; one that carries the flag and no gate
is the pipeline this ADR exists to change.

**Rejected: `scripts/gate-quick.sh`.** It is the worker's gate — `cargo fmt`, `clippy`, `cargo test --lib` and only the
integration binaries a change names — and its own last line says the tree is not proven. A promotion is the tree being
proven.

**Rejected: `scripts/verify.sh --no-build`.** It skips the two artifact phases and assumes `dist/` already holds the
artifact (`SKIP build and embed: --no-build (the artifact must be in dist/ already)`). There is no artifact before a
promotion builds one, and "assume the artifact" is precisely the review-artefact mistake ADR-0082 D2 already refused.

**Rejected: a separate review artefact check** — a file naming a review, a signed tag, a marker in the release tree.
ADR-0082 D2 refused it in these words: "nothing in this build can tell a review from a file that names one, so it would
be an assertion that measured a filename". A green gate is the opposite kind of thing: it is a measurement.

**The cost, accepted.** The gate is the proof and it is also the whole wait: `AGENTS.md` measures it at "about half an
hour on this host once `target/` is warm", "nearer forty in a fresh worktree", and "34 to 43 minutes when the image
cases rebuild `agent-image`" — which is the run a promotion always gets, because `case_server_images` removes both
product tags before it does anything. So a promotion is that run plus the pipeline's own steps: **about three quarters
of an hour**, and while it runs the **log is the only detail**. `GET /api/update` answers the report's `state` —
`running`, its `since`, and the steps that have finished — and the log's last lines, so a caller can say that a run is
under way and show what the gate is printing; it cannot say how far along the run is, because the report gains a step
only when that step ends and the gate is one step. Accepted for now, and the first thing to improve later — nothing in
this decision depends on the route learning to report progress.

## D3 — The candidate is a clean `master`, and nothing else

The candidate is one commit: `git -C <source> rev-parse master`, refused unless `git status --porcelain -uall` is
empty. Nothing unmerged, nothing unstaged, no untracked file. Two reasons, and the first is the release tree's own
rule: `scripts/build.sh` bakes `git describe --tags --always --dirty` into the binary, so a dirty tree builds a release
directory whose name says `-dirty` — a name a rollback can never reproduce from a commit, because there is no commit
whose bytes are those. The second is ADR-0082's: a release directory is named after the candidate's own `git_tag`, and
a name is only a name if a commit answers to it.

`-uall` is spelled because `git status --porcelain` respects `status.showUntrackedFiles=no`, and a repository config
that hides untracked files would hide exactly the case this check exists for.

**Rejected: promoting an arbitrary ref** (`--ref`, a tag, a branch). The commit a release directory names would stop
being derivable from the tree it was built out of, and there would be no answer at all to "which commit did this
machine make live" — the question D3 answers for free.

**Rejected: promoting a worktree** — a session's `.worktrees/<id>` on `agent/<id>`. A merge is what turns one of
those branches into the target, and a promotion that read a worktree would make the live code a session's branch with
no merge and no review in between.

**Rejected: a dirty tree, with the `-dirty` tag accepted.** It is the same refusal as the first alternative seen from
the other end: the tag would say a commit and the bytes would not be that commit.

**What the check is not.** `target/`, `dist/` and `web/node_modules` are gitignored, so the gate's own writes — about
10 GB of them, see below — leave `git status --porcelain` empty. That is why the check is a **precondition of
starting** and not a promise kept over the run: it names the tree the build began from, and a file the build writes is
invisible to it either way.

**And what "clean master" does not answer.** `POST /api/sessions/{id}/merge` already moves a target ref, but nothing
resolves "which commit would a promotion make live": `[merge] targets` defaults to `["main", "master"]` per project, so
a project whose sessions merge into `main` leaves `master` where it was, and a promotion would build `master` anyway.
The route's answer is `git rev-parse master` and a release tag that comes back through `git describe`; a later change
owns the rest of that question.

## D4 — The job is a transient user unit, and not a child of `serve`

The promotion runs as a **transient user unit**: `systemd-run --user --unit=holosmith-update --collect`, output to a
file, one at a time by unit name. This is the industry shape — the job is the supervisor's sibling, not its child —
and here it is forced rather than preferred.

**The cgroup fact.** A child of `serve` lives in `holosmith.service`'s cgroup, and the unit's `[Service]` section says
what happens to that cgroup on the restart a promotion ends with (quoted above): `Type=simple`, `Restart=on-failure`,
`RestartSec=5s`, **`KillMode=mixed`** and **`TimeoutStopSec=30s`**. `KillMode=mixed` sends `SIGTERM` to the main
process only and `SIGKILL` to every other process of the unit's cgroup, and `TimeoutStopSec` is the bound on the whole
stop. So the child that was going to watch the new release and put the previous one back is killed by a signal it
cannot handle — at the latest thirty seconds into the stop, and in practice as soon as `serve` has exited — while the
health poll it would have had to outlive runs for twenty seconds. The process that performs the restart cannot be the
process that supervises it.

**Rejected: a detached container.** It survives the restart by construction, and it cannot finish the job: step 6
restarts the unit with `systemctl --user restart`, which needs the operator's **user bus** and a `systemctl` the agent
image does not ship. `crates/holosmith/images/agent.Dockerfile` says what the gate does instead — "what the gate
supplies itself — `tailscale`, `systemctl`, `journalctl` and `loginctl` stand-ins, the mounted `docker` CLI — stays out
of this recipe" — and a stand-in is what a *test* has, not what a product can ship. A container that can restart the
operator's service is a container with the user bus mounted into it, which is a bigger authority than the promotion
needs and one no other part of this system has.

**Rejected: a child of `serve`** — for the cgroup fact above, and for one more reason: a promotion that restarts the
service is a promotion whose supervisor has to be alive after the service stops. `serve` is the thing being replaced.

**Rejected: a system unit** (`systemd-run` without `--user`). It needs root, and since ADR-0088 an install is the
operator's own account and a deploy escalates nothing; a promotion that asked for root to start its own job would be the
one privileged step in a pipeline whose every other write is plain `std::fs` as the release tree's owner.

**One at a time, by unit name.** `--unit=holosmith-update` is also the lock: systemd refuses to start a second unit
under a live name, so two presses are one promotion and the second answers a refusal naming the one already running.
`--collect` is what makes the name usable again once the unit has exited — including a failed one — so the name is
transient and not a reservation.

**What the later change owns.** The route's own answer (an accepted start, the unit's name, the log's path), the
file the output goes to, and the exact `systemd-run` argv. This ADR decides the mechanism and the naming rule, because
they are what the cgroup fact forces.

## D5 — The gate runs in the running release's agent image

Candidate first, image second, and the order is the point: the *candidate's* image cannot exist before the candidate is
built, so the gate that builds it must run in an image that already does. The image is therefore the **running
release's** — `HOLOSMITH_VERIFY_IMAGE=agent-image:<this binary's version>` — and `server deploy` is what guarantees
that tag exists, since its step 3 builds both product images when their labels say they do not match.

The wrapper is `scripts/verify-in-container.sh` (ADR-0109), and this is a fact about it rather than a preference:
it already defaults to `agent-image:<the version in Cargo.toml>` and already refuses with the sentence that names the
repair when the tag is missing — "the image $image is not on this daemon: run 'holosmith server deploy' to build it, or
set HOLOSMITH_VERIFY_IMAGE to the one the gate should run in". A promotion passes the tag **explicitly** rather than
letting the default stand, because that default is read from the tree's own `Cargo.toml` — which, in the source tree a
promotion builds, names the *candidate's* version, and the candidate's image is the one thing that cannot exist yet.

Inside that container the gate does the building that matters: the `build` phase runs `scripts/build.sh`, which builds
the web app, sets `HOLOSMITH_REQUIRE_WEB=1`, builds **both** musl artifacts and ends with `scripts/check-static.sh`;
and `case_server_images` builds the candidate's own `agent-image` and `holosmith-proxy` from the **candidate's own
embedded recipes**, because the recipes are embedded in the binary and the labels come from its `version --json`. That
is the only way the candidate's image can be built correctly: the deploy that builds those tags has to be the
candidate's own binary — run by any other, the tags and the labels are that binary's release, and the candidate's image
still does not exist.

**Rejected: `holosmith-builder` and `holosmith build` on this path.** They are the operator's release path (ADR-0080,
unchanged) and they stay it: a `holosmith build` container has the source for the tree it runs `scripts/build.sh` in,
and it cannot run the *gate*, which needs the daemon, the git dirs and the sockets ADR-0109's wrapper mounts. More
directly, the candidate's image cannot exist before the candidate, so the builder path cannot produce the image the
gate is asked to run in.

## The chain, in as many words

**Verification is a correctness gate, not a safety gate.** `scripts/verify.sh` runs the **candidate's own** code:
`scripts/build.sh` from the candidate tree, the cases' own copies of the candidate's `serve`, `deploy` and `upgrade`,
and `docker build` through the machine's daemon. It runs all of it as the operator, and it needs the Docker socket —
which is host root by another name, since anything that can reach the socket can mount `/`. So "the gate passed"
proves the candidate **works**; it does not prove the candidate is **safe**, and it executes whatever the candidate
contains with the operator's authority. A tree whose `scripts/verify.sh` printed `verify: <n> assertions, ok` and then
did something else is a tree the gate would pass.

With D1 that is a chain, and it is one sentence: **anyone the tailnet admits can merge to `master`, and whoever presses
promote then runs that tree as the operator.** The decision is that this is acceptable on a **single-operator tailnet**
— one person, one set of machines, one set of credentials, and no boundary between them to defend — and that it is the
**first thing to revisit if the tailnet ever holds more than one person**. At that point the two halves of the chain
stop being one person's and the answer is not a new flag on this route: it is that both halves need the boundary, and
that is a different ADR.

## Two image facts the later change must not discover the hard way

**The gate owns the product tags, and it can leave them gone.** `case_server_images` removes
`holosmith-proxy:<version>` and `agent-image:<version>` at its start, asserts both are gone, and rebuilds them by
running the candidate artifact's own `server deploy` — so the tags a green gate leaves are labelled from the
**candidate**, which is exactly what the promotion that follows wants. A gate that dies between the two steps leaves the
machine with **neither** tag: no `agent-image:<version>` for a session to start from and no `holosmith-proxy:<version>`
for it to reach, which is a machine that cannot run a session at all. The repair is one command, `holosmith server
deploy`, and the promotion's failure sentence has to name it — the gate's own `FAIL` line names the case, not this
consequence. When the candidate and the running release share a version string, the tag being removed is the one the
gate's own container was started from; a running container holds its layers, so the run continues, and that is why this
is a fact about tags rather than about the gate.

**A candidate that changes `Cargo.lock` cannot be verified in-container at all.** The agent image carries a vendored
crate set, and it carries the **running** release's: `scripts/check-crate-set.sh` compares this tree's `Cargo.lock` with
the image's own marker and exits **3** when they differ, with

> check-crate-set: … names a different `Cargo.lock` (…) than this tree's (…), so a crate this tree needs is missing from
> the image and cargo reports it as not found in the registry index: rebuild agent-image on this host, because this is a
> stale image and not a code fix.

For a promotion that sentence is the wrong diagnosis and the right boundary. The image is not stale — it is the running
release's, and the candidate is newer — and the fix is the one `AGENTS.md` already names in three parts: a new crate is
a **host act** (manifest and lock on a host with `cargo fetch --locked`, then a rebuilt `agent-image`), because a
session's container has no registry and a promotion's gate has no way to add one. So the route must refuse a candidate
whose lock differs **before** it starts anything, in words that name the host act: the candidate's `Cargo.lock` changes
the crate set, the running release's image vendors another, and a host has to build and deploy the image from the
candidate tree before this machine can verify it. A gate that exits 3 instead would read as a broken registry — the
exact misreading `scripts/check-crate-set.sh`'s own header exists to prevent.

**Of that, one half has landed and one has not.** The gate now answers 3: `scripts/verify.sh` runs
`scripts/check-crate-set.sh` as a precondition before its first phase and exits the check's own status, so a candidate
whose lock the running release's image does not vendor reaches `holosmith update` as **3** in the run's opening
seconds rather than as a red `FAIL` — `case_agent_toolchain`'s `vendor/lock` assertion minutes in, or `could not be
found in registry index` out of the offline build well after it — and `crates/holosmith/src/pass.rs`'s
`Verdict::StaleCrateSet` turns that code into the host-act sentence above, so the misreading does not survive into the
report a caller reads. What is still owed is the *before*: nothing refuses such a candidate before the gate is spawned,
so the repair is read once the gate has started rather than instead of it.

## What a promotion costs, and what the operator has to act on

- **The run.** About three quarters of an hour, one log file, nothing else (D2). The unit is the only handle: its name,
  its state and the file its output went to.
- **The disk.** The gate writes about 10 GB into `<source>/target/` and `dist/`. ADR-0107 measures `target/` at 9.2 GB
  after `cargo build --locked`, `cargo clippy --all-targets` and `cargo test --no-run`, and at 9.6 GB after both musl
  release builds — which is where the round figure comes from, and `dist/` and `web/node_modules` are on top of it.
  The route therefore checks free space on the filesystem that holds `<source>` **before** it spawns the unit and
  refuses with a number — the space it found and the space the run needs — because a run that fills the disk takes the
  machine's database down with it. Whether that number is a config key beside `session_disk_soft_gb` or a constant is
  the later change's; the requirement is that the refusal carries the figure.
- **One missing line in `scripts/build.sh`, and it is required rather than nice.** The gate's build phase leaves
  `dist/<triple>/holosmith`, and `promote::read_candidate` reads `<binary>.sha256` beside it and refuses the candidate
  without one: "`holosmith build` writes the artifact and its digest beside each other, and a promotion reads both — a
  file with no digest is not a build this machine can promote". `scripts/build.sh` computes that digest for its printed
  line and writes no file. The later change adds the write, in `builder`'s own shape (`<digest>  holosmith` in
  `holosmith.sha256`), so that the two writers of a release directory agree about what a build is. Everything else
  `read_candidate` asks — `builder::artifact_problem`, `version --json` as one child — already answers about that
  artifact.

## What this ADR does not decide

- **The route's name, its method and its answer shape.** One mutating route under `/api/`, guarded like merge, is the
  whole of what D1 requires; a `ReadOnly` kind beside it, a status reader, and a cancel are surfaces nobody has asked
  for and each would be a decision.
- **`--acknowledge-review`'s life on the operator's own verb.** D2 replaces it as the requirement for a promotion the
  browser starts. Whether `holosmith upgrade` keeps the flag as the hand path's word, or loses it to the same gate, is
  the later change's — it is a change to a landed command's contract, and this ADR only supersedes the claim that the
  *pipeline* cannot run without a human's acknowledgement.
- **Whether the gate's `--keep` and the run's work dir are the route's to clean.** `scripts/verify.sh` already has the
  cleanup machinery and a killed run's evidence is its `work dir kept:` line (ADR-0089); a promotion that spawns it
  inherits the question and adds nothing to it.
- **Where the update log lives and how long it is kept.** A file under the data root is the shape; a rotation is the
  backups' question (ADR-0082 D9) and not this one's.

## What is not measured here

- **A real `systemd-run`, a real unit, and a real restart.** There is no systemd on this machine, so ADR-0082's limit
  applies unchanged: `systemctl` is a child the gate stands in for, and a unit booting a real release is not measured.
  What *is* measured is the argv: `tests/update_api.rs` points `deploy::Tools::systemd_run` at a recorder and asserts
  the whole command line D4 decided — `--user`, `--unit=holosmith-update`, `--collect`, both `append:` streams under
  the data root, `WorkingDirectory=` naming the candidate tree, and the job's own words after `--` — and
  `case_web_update` stands in for the launcher with a script that does what the unit would: it reads those two
  properties and starts the job in a session of its own, which is how "a promotion outlives the restart it performs"
  becomes something a run can watch rather than something this ADR asserts.
- **A promotion started from a browser, end to end.** The page is the app's, and no browser is in the gate; what the
  routes answer is measured. `tests/update_api.rs` drives the two routes that file is about over a socket — `POST
  /api/update` and the `GET /api/update` it starts a job for — covering the refusals the module asks in order (`403
  update_disabled`, the four shapes of the `404` it drives, `422 stale_candidate`, `409 update_running`), the `202` that
  starts a job, both guards, and the report a `GET` answers before and after the process that started the job is gone —
  and `case_web_update` presses the artifact itself: `POST /api/update`, the job the route starts, the restart it
  performs, `GET /api/update` answering on the other side of it, and the release **directory** the process answering
  `/api/health` runs from — `releases/old/holosmith` after the rollback and `releases/<tag>/holosmith` after the
  promotion, read from `/proc/<pid>/exe`. The third route, `GET /api/update/candidate`, is measured a layer down:
  `tests/update.rs` builds the router over the same fixture checkout and reads its `403`, its `404` and its `200` over a
  socket, which is the only place the page's `Candidate` is checked against what the machine sends. What neither does
  is run the gate that press rests on, which is the bullet below, and what no case can is show two *different*
  versions on one machine: a gate has one artifact, so both releases a run holds are that one build, and
  `/api/health`'s `version` is a compile-time constant in it — one a release build's thin LTO splits into immediates,
  so not even a byte substitution can move it. A promotion across a version boundary is an operator's act —
  the next tree is built and its artifact promoted — and not something a gate case can manufacture without a second
  build, which is the second build path `AGENTS.md` refuses.
- **The Docker behaviour behind "a running container holds its layers."** `case_server_images` untagging the image the
  gate itself runs in has not happened in a promotion yet; it happens in every in-container gate whose candidate shares
  the running release's version, and a run that finishes is what would measure it.
- **The source tree's config key.** `[update] source` names the checkout, with a validator and a row in
  `docs/config.md`, and `candidate::resolve` is its one reader — reached from three places, which is the point of the
  candidate route: the job's own first step (`pass::pass`), `GET /api/update/candidate` rendering it for the page, and
  `POST /api/update` re-resolving it to compare a press's token against `master`'s now. That transition is measured
  now, over a socket: `tests/update_api.rs` presses `master`'s commit and gets `202`, and presses another one and gets
  `422 stale_candidate` naming both — with the four shapes of the `404` a press can reach beside it, each of them the
  one `no_candidate` code the envelope carries: a path that is not there and a directory that is no repository
  (`NotARepository` twice), a `master` with no commit (`NoMaster`), and a tree holding work its commit does not
  (`Dirty`). The two shapes that file does not drive are measured elsewhere: `NoCandidate`, an unset `[update] source`,
  by `tests/update.rs`'s candidate route, and `NotOnMaster` by `tests/candidate.rs` and the resolver's unit tests,
  neither of them over a socket.
- **A pass of the gate end to end.** `holosmith update` is measured by a fixture tree whose `scripts/verify-in-container.sh`
  records its argv and answers the exit code the test chooses, so the four steps, the exit-code translation and the
  report are facts about the command — and the run that is the point of D2, a green gate on a real machine promoting the
  artifact its own build phase wrote, has not happened yet. `case_web_update` is the same shape one layer up: its
  fixture gate exits `0` and the artifact it leaves is seeded by the case, so the promotion and its failure are real
  and the three quarters of an hour are not. The gate itself is measured separately (ADR-0109).

## Consequences

- The machine gains one mutating route whose work outlives the request, and the first mechanism on the request path
  whose supervisor is `systemd` and not a request's own task. Everything it can refuse, it refuses before the job
  starts: a dirty or non-resolving `master`, a candidate whose lock the image cannot build, and a disk with no room.
- `scripts/build.sh` gains its second file, and `promote::read_candidate` becomes the reader of the gate's output
  rather than of `holosmith build`'s — one artifact layout with two writers.
- The product tags become a promotion's business during the run: a green gate leaves them the candidate's, a dead one
  leaves them absent, and `server deploy` is the repair both times.
- `GLOSSARY.md`'s **Promotion** entry said a promotion is "the review gate acknowledged (`--acknowledge-review`)" first.
  That clause is now the pass of the gate, and an **Update** entry names the verb — the entry was waiting for this change
  both because the route had not landed and because `--acknowledge-review`'s life on `holosmith upgrade` was unsettled;
  it stays, as the hand path's word, while `holosmith update` never takes it.
