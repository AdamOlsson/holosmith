# The startup reconcile is one awaited pass, and a row owns its names

HS-044 is spec items 6, 7 and 8's crash sentence, and V9 is the measurement:

> Startup reconcile uses `docker ps -a --filter name=s_`, removes any leftover container, then restarts
> sessions that were live. … At startup, any row still in `starting` is either resumed or set to `error`,
> and orphaned networks, worktrees and containers are removed. … a running turn is recorded as `interrupted`.
> (V9) `kill -9` of the server mid-turn, then restart: the session returns to idle with no duplicate or missing
> seq and no container name conflict.

The state machine already had the trigger — `Trigger::CrashRecovery`, `{starting, idle, running} → starting`,
written by HS-012 and called by nobody — so this story is the pass that drives it, and the decisions below are
about what the pass may touch.

## A `kill -9` observes nothing, which is why a pass is needed at all

The container *is* the agent: an attached `docker run` whose stdio is pi (ADR-0039). When the server dies the
CLI dies with it, so the container keeps running with nobody talking to it, no `ExitWatcher` records an exit,
and the row keeps saying `running`. The same is true of a provision in flight: the row stays `starting`. A
`docker events` watcher would observe the container's death and nothing else — not the row, not the ordering,
not the stale network — and ADR-0039 deliberately has no second watcher. So the recovery is a **startup pass
over rows and names**, not a fifth observer.

## One new module, awaited before the first request is answered

`src/reconcile.rs` owns the pass, for ADR-0023/ADR-0029's reason: the module that owns the names owns the
questions about them, and `Docker` is the only thing that can read `ps`. `serve` builds it through
`AppState::reconciler()` (like `sweeper()`/`compactor()`) and calls `run().await` between "the state exists"
and "the listener answers".

**That `await` is a measurement, not a preference.** The first draft spawned the pass as a task — the shape
the sweeper and the compactor have — and the story's own demo caught what that costs: a create accepted in the
first milliseconds of a boot inserts its row and makes its worktree while the sweep still holds the row
snapshot it took at boot, so the sweep takes the new worktree and the create's container step fails with "the
worktree cannot be mounted: … is not there". Waiting instead costs two bounded daemon questions and one `git`
per project, and the listener has already bound, so a client waits rather than being told the wrong thing. The
*restarts* the pass starts are `Provisioner::spawn` tasks, so provisioning still happens off the serving path.

It is also awaited **before either policy is spawned**, and that second reason is the review's: the idle-stop
sweeper's first pass is immediate and its candidates are `idle` rows, so it could stop a row the pass is about to
bring back — the crash transition would then be refused as illegal and the row would end `stopped`, which is not
"restart sessions that were live". With the pass first, every live row is `starting` before the sweeper has a
candidate.

The order inside the pass is item 6's and item 7's, and each step is load-bearing:

1. **The crash transition**, for every row that was live (`starting`, `idle`, `running`), before anything else.
   A live row is `starting` before the sweeper's first tick can stop it, and before a restart puts a new
   container on its worktree.
2. **The names no row owns**, before any restart, so a stranded pi stops before a new one mounts the same
   worktree.
3. **The restarts**, one `Provisioner::spawn` per live row.

## Which rows come back, and which are left alone

| Row at the crash | What happens | Why |
| --- | --- | --- |
| `running` | `CrashRecovery` → `starting` with reason `interrupted`, then a re-provision whose *settle* also carries `interrupted` | spec item 8's "a running turn is recorded as `interrupted`", and the row the operator comes back to reads `idle · interrupted` |
| `idle` | `CrashRecovery` → `starting` (no reason), then a re-provision | the container died with the attached CLI; nobody else can bring the agent back |
| `starting` | no transition, just a re-provision | it is already where a provision begins; item 7's "either resumed or set to `error`" is the provisioner's own failure path |
| `stopped`, `error` | untouched | nothing was running; `POST /resume` is the way back (ADR-0022) |
| `deleting` | untouched, and its container and network are not swept | a `deleting` row is the operator's request to retry (ADR-0023) |

The reason is written twice, and by the same function: the transition into `starting` records the crash, and
the settle records it again so the row and its last event agree. The second is what keeps `idle · interrupted`
on the row after the re-provision — without it the row would end plain `idle` and the page would have nothing
to show. `Transitioner::apply` remains the only writer; `Provisioner` gained
`with_settle_reason(Option<&str>)` so the reconcile states the reason instead of a second writer existing. A
recovery that *fails* carries the failure's own sentence instead: `error · the worktree step: …` is more useful
than `interrupted`.

The row half runs for **every backend**, the name half only for Docker. A `running` row is a lie about the
process whichever backend wrote it (the stub's turn is in-process and died with it), and it is what makes the
row half measurable on a hub — `tests/reconcile.rs` and the gate's `case_reconcile` need no daemon. Containers,
networks and worktrees exist only in a Docker run (ADR-0015), and a stub boot must not touch `projects_root`.

## What a row owns, and where the line is

The rule is one sentence: **a row owns its names; the sweep takes only what no row owns — except a container,
which goes unless its row is a delete in progress.**

- **Containers** (`s_<id>`): removed unless `<id>` is a `deleting` row. Every other container is stale (no row,
  or a row that is not live) or about to be replaced by a restart — and a stranded pi must stop *before* the new
  one starts, so a live row's container is in the sweep too. A delete's container is its own to remove.
- **Networks** (`agents_<id>`): removed only when **no row** has the id. A live row's network is reused by the
  restart and holds the proxies' endpoints; a stopped row's is what its resume reuses; a `deleting` row's is the
  delete's.
- **Worktree directories** (`<projects>/.worktrees/<id>`): removed only when **no row** has the id, and **never
  a branch or a ref** (ADR-0052). A registered directory goes through `git worktree remove --force`, which takes
  the registration too; an unregistered one (a killed start, before git ever heard of it) goes through the
  filesystem. `Worktree::remove` is told `Retained::Unknown`, which keeps the branch: "there is no row to ask"
  must never mean "delete".
- A name that is not the convention — `hs_egress`, `hs_key`, an operator's own container — is not the build's.
  The id must be 16 lowercase hex characters (`sessions::is_id`, ADR-0011), and Docker's `name=` filter is only
  a cheap pre-selection because it matches substrings.

## Docker's answer is read with a template, never parsed

`docker ps -a --filter name=s_ --format '{{.Names}}'` and `docker network ls --filter name=agents_ --format
'{{.Name}}'`, one name per line (ADR-0035's rule: a template asks for the field, so no sentence is parsed). The
calls are **bounded** (`Docker::bounded`), and a listing that does not finish is one `warn`, not a refused boot:
the reconcile is on the startup path, and a daemon that answers the preflight and then wedges must cost a bound,
not the control UI. A network removal gained the same bound (`remove_network_bounded`).

## One consequence on HS-060's rule: compaction may fold a `starting` row

A crash recovery parks every live row in `starting` for as long as its re-provision takes, so the compaction
task's first pass (which is immediate at startup, ADR-0032) now meets rows in a state it used to exclude
(`foldable` was `idle`, `stopped`, `error`). The gate's own `case_compaction` caught it: a restarted machine with
a day-old log folded nothing until the next 60-second tick. `foldable` now includes `starting`, which is ADR-0032's
argument applied to one more state — a provisioned row has no turn running (a prompt needs `idle`) and no removal
in flight, the pass holds the database mutex the transitioner takes, and the fold only touches message rows older
than the window. The alternative — waiting a tick — would make a restart slower to fold than a fresh boot.

## What this deliberately does not do

- **No synthesized `turn.end`.** Item 8's own sentence makes `interrupted` a reason on a `session.status` event;
  the transcript's turn boundaries belong to the pump (ADR-0040), and a second producer is a second contract.
- **No `docker events` watcher** (ADR-0039).
- **No delete finishing.** A `deleting` row, and everything it owns, is left exactly as the crash left it.
- **No branch or ref is ever deleted here** (ADR-0052).

## Idempotence, in `Worktree::ensure`'s sense

A second pass leaves the same names and the same idle rows: everything it removed is gone, and everything it
kept is kept for the same reason. It is **not** a no-op — the live rows are restarted again, which is what a
reconcile is. That is why `doctor` (HS-045) and HS-046's verification run may call it, and why the fact is
recorded here rather than discovered there.
