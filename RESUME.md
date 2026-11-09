# RESUME — one issue per session (next: PER-103 / HS-071, the Computers screen)

You are the **orchestrator for implementing Holosmith**. `PROMPT.md` is the standing operating procedure and
still governs the cycle; this file is the state of the world, and it adds one rule of its own:

**This session picks up exactly one issue — the one named in §1.** Take it through the whole cycle (§5):
plan, Linear, implement, gates, demo, review, fix, ship, **write the state back** into this file for the next
agent (§5, step 10), then **kill every child you started** and stop (§5, step 11). Do not start a second issue
however much budget is left: a fresh session with a fresh file is how this run continues, and one honest stop
beats two half-finished stories.

The operator is not present. **Never ask a question.** Decide canonically — `HolosmithSpec.md` →
`GLOSSARY.md` → `docs/adr/` → `docs/stories/` → the existing code — record the decision in the Linear issue
and the commit message, and continue.

Read in this order, and no further: **this file**; the issue's slice in `docs/stories/`; the spec items §1
names (and the V-rows); the ADR index (one line per decision, `docs/stories/README.md`), then only the ADRs
§1 names; then `docs/notes.md` — the **cross-cutting** section first, then the sections §6 of *this* file
names for this issue. `docs/notes.md` is the durable prose record (the build's shape, the event contract, the
landmines). Anything about a *finished* story is in Linear, `docs/review/PER-<n>.md` and `git log` — not here.

---

## 0. What this project is

`HolosmithSpec.md` is authoritative. A single-operator system that runs coding agents in containers on
machines you own, driven from a phone. The work is **80 stories** (`HS-001` … `HS-080`) in `docs/stories/`,
one Linear issue each, in five phases:

| Phase | Stories | File |
| --- | --- | --- |
| 0 — walking skeleton | HS-001 … HS-006 | `docs/stories/phase-0-walking-skeleton.md` |
| 1 — session lifecycle (stub backend) | HS-007 … HS-023 | `docs/stories/phase-1-session-lifecycle-stub.md` |
| 1b — Docker backend | HS-024 … HS-046 | `docs/stories/phase-1b-docker-backend.md` |
| 2 — deployable MVP | HS-047 … HS-067 | `docs/stories/phase-2-deployable-mvp.md` |
| 3 — multiple machines | HS-068 … HS-074 | `docs/stories/phase-3-multiple-machines.md` |
| 4 — self-development | HS-075 … HS-080 | `docs/stories/phase-4-self-development.md` |

`AGENTS.md` carries the repository rules, including that **Linear scope is exactly one project**: `Holosmith`
(`P-PER-2`, team `Personal`/`PER`). `VulkanRenderAPI` belongs to another repository — never read, create,
comment on, or close anything there.

**Branch `master`** (this repo has **no `main`**; do not rename it). Every commit is pushed; pushing is a
sandbox-specific incantation (§2).

---

## 1. The issue this session picks up: `PER-103` — HS-071, the Computers screen

**State when this file was written** (HEAD at `a624406`, **tree clean** apart from the operator's own
`orchestrate.sh`/`log.txt`): **71 of 80 stories Done, 9 Backlog**, all in project `Holosmith`. `PER-103` is
`Backlog`, it is the next in `HS-###` order, and its blocker (HS-070) is `Done` — so it is eligible. Check Linear's
`Blocked by` list rather than this sentence if it turns out to be a stop. **It is the first *page* story of phase 3**:
its deliverable is a screen, and no gate may open a browser (§3) — so the shape of its gate is the first decision,
not the last.

The last shipped story was `PER-102` (HS-070, machine-to-machine trust, **ADR-0075**) — `allowed_hubs` has its reader:
one `trust::Trust::admit` asked by the edge of both listeners, the identity being the tailnet node's short name from a
`tailscale whois --json <ip>` child, with `127.0.0.1`/`::1` as the front door and an empty list as the documented
ACLs-only fallback. Its §1 material is now Linear, `docs/review/PER-102.md`, `git log` and §9. What it leaves behind
that this story stands on:

- **`GET /api/hosts` is the only list of machines there is**, and the row's shape is the struct's order:
  `{name, url, preview, reach, version, last_error, last_checked_at}`, by name. `POST /api/hosts` upserts on the name
  and `DELETE /api/hosts/{name}[?force=true]` refuses a **local** row that has live sessions (`409 host_busy`); the
  codes are `bad_host_name`, `bad_host_url`, `not_holosmith`, `host_busy`. The probe runs at boot and at registration
  only, so a row's `reach`/`version`/`preview` are as old as the boot (ADR-0074 D5).
- **A row has no `role`, no uptime and no session counts** — and HS-071's acceptance names role, version,
  reachability, last-seen and live/stopped counts. The *health payload* carries `role`, `uptime_s` and
  `preview_origin`, and **the forwarding route already reaches a peer's health answer**: `GET /api/hosts/<name>/health`
  is a `GET` (so the hub's guards pass it) forwarded to the peer's `/api/health` (which is guard-exempt). That is one
  hop per host for role + version + uptime, today, with no migration and no new route. The counts are the same shape:
  `GET /api/hosts/<name>/sessions` answers `{sessions:[…], network_default, stopped_ttl_days}` and each row carries
  `status`, so `live` (running/idle/starting) and `stopped` are a fold over that answer.
- **The page has no host awareness at all**, which ADR-0074 D10 recorded as *the* limitation HS-071 closes:
  `web/src/main.ts` fetches `/api/sessions` and `/api/sessions/<id>`, builds the preview link from **this machine's**
  `health.preview_origin` (`transcript.ts`'s `previewUrl`/`previewOffered`), and every affordance spells its own path
  (prompt, abort, steer, resume, delete, the SSE stream, the Changes sheet's four reads, tool output, tool images).
  HS-071 is therefore mostly **`apiPath(host, rest)`** — `/api/<rest>` for `local`, `/api/hosts/<host>/<rest>`
  otherwise — plus threading the host through `StreamTarget` and every request, and the preview link needs the
  **row's** `preview` origin rather than the health payload's.
- **The machine's own role is already on the wire**: `/api/health`'s `role`, which `main.ts` already prints in the
  header. "A hub-only machine shows no new-session affordance for itself" needs no new field and no probe.
- **`web/`'s rules are pure functions with node tests** (`transcript.test.ts` 72, `push.test.ts` 32): every sentence,
  every count and every URL HS-071 adds is a pure function there first, and `main.ts` keeps only the DOM (HS-058 and
  HS-067's rule). A gate case can only assert the **served bundle's** ids and sentences (`case_pwa`'s precedent).
- **No ADR number taken: 0076 is next.** No migration is needed unless role or counts are stored rather than read.

### 1.1 The slice (verbatim from `docs/stories/phase-3-multiple-machines.md`; Linear's body is the same text)

> ### HS-071 — Hub UI: Computers screen
>
> **As** the operator **I want** to see my machines and their state **so that** I know where work is running.
>
> - **Size:** M · **Blocked by:** HS-070 · **Spec:** Roles, item 4
> - **Demo:** the Computers screen lists two machines with health, version, live session counts and last-seen times,
>   and can add and remove one.
>
> **Acceptance**
> - [ ] Lists name, URL, role, version, reachability, last health check and live/stopped session counts.
> - [ ] Add and remove are available with the same validation as `POST /api/hosts`, showing the specific rejection
>       reason.
> - [ ] Version skew between machines is visible, since it affects which API a call can use.
> - [ ] Sessions screen groups by machine and shows which machine owns each session.
> - [ ] A hub-only machine shows no "new session" affordance for itself.

### 1.2 The spec it closes

`HolosmithSpec.md`'s **Roles** section and item 4's host-registration paragraph, verbatim:

> API paths are unchanged on every role: a hub serves `/api/hosts*`, a server serves the session API.

> Host registration (`POST /api/hosts`): the URL must be http(s), the host a MagicDNS name or an address in
> 100.64.0.0/10, the port in an allowed range; redirects are not followed; and the `/api/health` reply must carry the
> Holosmith name and version fields.

The neighbours: **HS-072** (`PER-104`) is `hub deploy` and `hub host add/list/remove`, which must perform *the same
checks as `POST /api/hosts`* — a validation rule the screen shows and the CLI repeats is one rule, so where the
sentence lives matters; **HS-073** (`PER-105`) is `--ssh` deploys; **HS-074** (`PER-106`) is cross-machine sessions **and
notifications**, which owns `{host, session_id, kind}` payloads and therefore the host-aware deep link this story's
`apiPath` makes possible.

### 1.3 What already exists (read this before planning)

- **`src/hosts/`** — `Host` (the row), `Reach`, `register`/`accept`/`merge_outcome`/`probe_row`/`probe_one`/`merge`,
  `name_problem`/`origin`/`host_problem`/`port_problem`, `is_local`, `LOCAL`, and `seed.rs`. **The validation
  sentences already exist** and are what the screen must show unchanged: `http::hosts`' three refusals are
  `bad_host_name`, `bad_host_url`, `not_holosmith` (ADR-0073).
- **`src/http/hosts.rs`** — the registry's three routes and the probe-then-store order (`spawn_blocking` **before** the
  lock).
- **`src/http/forward.rs` + `src/forward.rs`** — `/api/hosts/<name>/<rest>` forwards one call to the peer that owns it
  (ADR-0074): the response is the peer's, byte for byte; `local` is a `308`; `hosts/…` is `403 not_forwarded`; the
  bound is 5 s (60 s for `merge`, `publish`, a `DELETE` of one session).
- **`src/preview.rs`** — `Route::path`/`slashed_path` spell `/<host>/<id>/…`, and `HOST` is `hosts::LOCAL`: the
  preview URL a row already supports.
- **`web/src/main.ts`** — the three screens (`/`, `/sessions/<id>`, `/settings`), `StreamTarget`'s one `showState`, and
  the settings screen's push state; `web/src/transcript.ts` — the renderer's rules; `web/src/push.ts` — the sentences
  and `navLink`.
- **`case_pwa`, `case_hosts`, `case_forward`, `case_machine_trust`** — the four cases closest to this story, and the
  only precedent for "a gate may not run a browser" that is not a promise.

### 1.4 What does not exist

No Computers screen, no host-aware page (no `apiPath`), no per-host role/uptime/counts reader, no version-skew rule, no
ADR 0076, and no `case_*` that asserts a screen's *bundle* for a screen other than Settings and the session list.

### 1.5 What the plan must decide (and the arms each one has)

1. **Where role and uptime come from.** (a) One forwarded `GET /api/hosts/<name>/health` per host on the page's own
   call — the route exists today, needs no migration, and costs one hop per machine inside the 5 s bound; (b) a new
   `role` column fed by the boot's probe (migration 0008, `SCHEMA_VERSION` 8) — fresher only at boot, and a row that
   lies about a machine's role after a redeploy; (c) both, the column cached and the hop read when the operator opens
   the screen. Say which, and what an unreachable machine shows.
2. **Where session counts come from.** (a) `GET /api/hosts/<name>/sessions` per host, folded by `status` on the page
   (the hub has none of its own, and its own role is `hub`); (b) a new hub-side aggregate route (a second reader of
   every machine, and a route with an N-hop bound — item 3's promise is "a remote machine going down mid-stream
   produces a clear client-visible error, not a hang", which an aggregate would have to answer for ×N); (c) nothing new:
   the rows the hub already forwards. Pick one and say what a *stopped* count means (the peer's `stopped_ttl_days`
   window, not the hub's).
3. **`apiPath(host, rest)` and everything it touches.** Which requests move (the list, one row, the stream,
   prompt/abort/steer/resume/delete, the four Changes reads, tool output and images, merge, publish), where the host
   travels (`StreamTarget`, the sheet, the delete affordance), and what the page does with a host it did not come from
   (`/api/hosts/<name>/sessions/<id>` is already the peer's own path). ADR-0074 D10 named this function; the story's
   weight is in it.
4. **Grouping and ownership.** "Sessions screen groups by machine and shows which machine owns each session." (a) One
   list per machine, each from its own hop, rendered as sections with the machine's name as the heading; (b) a single
   fan-in list with a `host` key added by the page (`{host, …row}` — the row is the peer's, so it cannot be edited
   server-side, ADR-0074 D10); (c) a `host` query on the list route. Say which, and what a machine that cannot be
   reached shows in the list (`{host, kind:"unreachable"}`? the `push::Payload` precedent).
5. **Version skew.** The row's `version` is what the peer reported at boot; this build's is `version::VersionInfo`'s.
   (a) A sentence when they differ (`machine trusts peers`? no — the sentence is about *API skew*: a call the older
   peer does not know is a `404`), plus the row's version always shown; (b) a warning only; (c) refuse forwarding to a
   skewed peer — no: ADR-0074 never gates on version. Also: a mixed-version fleet is `tests/`-measurable by writing a
   row whose `version` is `"0.0.1"`.
6. **Add and remove from the page.** The three refusals are the API's, and the screen must show *the sentence*: `POST
   /api/hosts` bodies (`{name, url}`), `DELETE` with `?force=true` when `host_busy` names live sessions. (a) The
   screen prints the envelope's `message` verbatim (the project refusal precedent: `transcript.ts`'s `createFailure`);
   (b) a new pure function mapping codes to sentences — no: the API's sentence is the one the CLI will reuse in
   HS-072.
7. **The gate's shape.** No browser. (a) A `case_*` that boots a hub and a peer, asserts `GET /api/hosts`'s rows,
   asserts `GET /api/hosts/<peer>/health` and `/sessions` answer through the hop (that is the data the screen draws),
   and greps the served bundle for the screen's new ids and sentences; (b) the same plus `web/test/`'s pure functions
   (which is where each sentence is actually pinned); (c) a by-hand browser check, which the gate may not do. Say what
   the case leaves behind and what it would still measure if a sentence were deleted.

### 1.6 The questions the plan must answer (before code)

1. Which reader feeds role/uptime, and is a migration in scope?
2. What exactly does "counts" mean, and whose window is a `stopped` count?
3. What does `apiPath` change, and what is the host's route through `StreamTarget`?
4. How is the list grouped, and what does an unreachable machine render as?
5. What is the version-skew rule, in one sentence?
6. What does the gate measure without a browser, and which `web/test/` functions carry the sentences?

### 1.7 Acceptance, restated as checks

- *Lists name, URL, role, version, reachability, last health check and counts* — one row per machine from
  `GET /api/hosts`, the rest from the two forwarded reads; a `web/test/` function for each rendered field's rule.
- *Add and remove with the same validation, showing the specific reason* — the three refusals' `message` rendered
  verbatim; `host_busy` offering the forced retry; a `case_*` assertion on the API and on the bundle's ids.
- *Version skew is visible* — a row whose `version` differs renders the sentence; measured with a hand-written row.
- *Sessions grouped by machine, each naming its owner* — the group heading and the row's machine; the peer's own rows,
  not a re-serialisation.
- *A hub-only machine shows no new-session affordance for itself* — from `/api/health`'s `role`, with a `web/test/`
  function (and `case_pwa`-style bundle grep), not a new route.
- Plus the usual gates, a `case_*` that needs no browser, `GLOSSARY.md` (a **Computers** term?), and an ADR (**0076**).

### 1.8 Non-goals

`hub deploy`/`hub host` (HS-072) — though the validation rule must not fork; `--ssh` (HS-073); cross-machine
notifications and the `{host, session_id, kind}` payload (HS-074); a per-host role column unless §1.5's arm (b) is
chosen; any change to `POST /api/hosts`' rules.

### 1.9 The demo

A hub booted with a `hosts.toml` naming one reachable peer and one that is not there, and the screen opened in the
browser this machine has (Playwright's Chromium, `docs/notes.md`'s fact: launch with `--no-sandbox`, navigate with
`domcontentloaded`): the two machines with role, version, reachability, last-seen and counts; the unavailable one
degraded with a sentence rather than an empty list; a version-skew sentence on a row the demo writes; a session created
on the peer and **visible through the hub grouped under it**; and the hub-only affordance rule. Say plainly what was
not measured: no real tailnet, and the skew is a written row rather than a second build.

### 1.10 What the next agent needs back

When you write the state back (§5, step 10): §1 becomes the **next** issue — by `PROMPT.md`'s ranking that is
`PER-104` (HS-072, `hub deploy` and `hub host`) *if* HS-071 ships and its blocker (HS-071) is `Done`, and `PER-103`
again if it does not; check Linear's `Blocked by` list. Refresh the counts, the ADR number you took (**0076** is next),
the last gate run's own numbers (and `web/`'s test count), and replace §9's row with this story's. Anything you learned
that is **not** state and **not** an ADR goes into `docs/notes.md` under a new `### Landmines: …` heading, never into
§1. Then kill what you started (§5, step 11).

## 2. Environment (already repaired — do not redo)

Everything below lives **outside the repo** and survives. If this is a fresh container, redo it exactly
as written.

```bash
. ~/.pi/toolchains/env.sh      # ALWAYS source this before cargo/rustc
```

It sets `RUSTUP_HOME`, `CARGO_HOME` and `PATH` to:

- rustup 1.99.1, minimal profile + clippy/rustfmt, targets `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl`;
- musl.cc cross toolchains for both arches (`x86_64-linux-musl-gcc`, `aarch64-linux-musl-gcc`);
- `~/.pi/toolchains/bin/cc` → `/usr/bin/gcc` (the `cc` crate needs a `cc` on PATH);
- `~/.pi/toolchains/bin/qemu-aarch64-static` (7.2.0) — an **optional dev tool** so the aarch64 artifact
  can actually be *run* here. No repo script may download it; `scripts/check-static.sh` prints one
  `SKIP` line when it is absent and still passes.

Why outside the repo: `/` is a 3.4 GB tmpfs and `~/.pi` is the only large writable ext4 mount.

**Available:** node 24.22.2 / npm 11.19.1 with the npm registry reachable (node 24 strips TypeScript
types natively, which is what `npm test` relies on); `curl`; `python3` (with stdlib `sqlite3` and
`http.client`); `sed`, `grep`, `tr`, `cut`, `cmp`, `find`, `timeout`, `git`, `du` (GNU coreutils 9.4, which
HS-063 used); **Playwright's Chromium**
(`~/.cache/ms-playwright`, bound read-only by `bwrap.sh`) and the `playwright` npm package at
`~/.cache/hs-browser/pw/node_modules`. Two quirks, both hit: ESM **ignores `NODE_PATH`**, so a script
imports the package by its own path — `…/playwright/index.mjs` (the `.js` entry is CJS and its default
export does not have `chromium` at the top level); and Chromium must be launched with
`--no-sandbox`, because its own namespace sandbox cannot nest inside ours. A third, learned in HS-020:
**`networkidle` never settles** on the sessions list, because a live row holds an `EventSource` open —
navigate with `domcontentloaded` and wait for the rows.
**Available since HS-024: a real Docker daemon.** `docker version` shows a 29.9.2 server, `/var/run/docker.sock` exists,
`docker run` works and images pull over the network. `bwrap.sh` binds the host's socket at
`/var/run/docker.sock` (`DOCKER=()` … `--bind "$sock" /var/run/docker.sock`), so a `docker` inside the
sandbox drives the real daemon — that is host root by another name and it is granted deliberately; a
machine without a socket simply gets no binds. **`pi-agent:local` is loaded and is the image every Docker
story uses**: its pi version equals `pins/pi.txt` exactly (`agent::version_check` asserts it at launch), git
2.39.5, node 24, default user `node` (uid **1000**, the host uid). Everything the Docker chain needs is here,
including network access for `alpine:3` and friends. **A preview demo needs `node` inside the container** —
this image has it, and so must any image `agent_image` points at (HS-036 learned that the hard way: a missing
node is exit 127, not a test failure). **A `serve` boot starts `hs_egress` and `hs_key` and they outlive
it**, so a gate run or a demo leaves both running: remove them (`docker rm -f hs_egress hs_key`) when you want
`tests/container.rs`'s egress-proxy test to run its *real* path, which it skips while a proxy is running. Since
HS-048 both proxies run `holosmith-proxy:<version>`, built by `server deploy`'s step 3, so a boot on a machine
that has never deployed warns once and starts no proxy — and the gate's `case_server_images` is what leaves that
image behind for every later case.
The machine's own `egress` network must exist for that test (`docker network create egress` is the call a boot
makes) — a cleanup removed it once and the test failed on setup at HS-043's baseline.

**pi's own RPC documentation is on this machine**:
`~/.pi/agent/install/releases/1.5.0/node_modules/@earendil-works/pi-coding-agent/docs/rpc.md` (how to start
it, the JSONL framing rules — one complete JSON object per line, split on LF only, never a reader that also
splits on U+2028), `rpc-commands.md` (the `prompt`/`steer`/`abort`/`get_state` commands and the `disposition`
field), `json.md` (the event stream: `agent_end`, `agent_settled`, tool and message events),
`rpc-extension-ui.md` (the UI requests `pump` auto-cancels) and `environment-variables.md`. The binary in
the image is `/usr/local/bin/pi`, its version already equals `pins/pi.txt`, and `pi --mode rpc --no-session`
is the launch `agent::COMMAND` builds. `pi --help` in the sandbox is the same version's, so an experiment
is free; nothing in the repo may hard-code the path to those documents.

**Not available, and not to be invented:** no `sudo`, no `apt`, no `gh`, no ssh key (the agent has one,
§pushing), **no systemd** (`/proc/1/comm` is `bwrap`; `/run/systemd/system` does not exist) and **no Tailscale
daemon** — the `tailscale` CLI *is* at `/usr/bin/tailscale`, but `tailscale status` answers
`failed to connect to local tailscaled`, which is the fact `doctor` renders as `unknown` rather than failing on.
There is a **browser** (`docs/notes.md`, fact 3), but no *gate* may need one.
**No `awk`** and **no `shellcheck`** on this machine — do not write a script or a gate that needs
either.

### Linear access

There are no Linear tools in the tool list. `list_issues` / `save_issue` / `save_comment` / `get_issue`
are reachable through the same HTTP MCP endpoint `scripts/import-linear.py` uses, so drive it from a
wrapper. Recreate `/tmp/lin.py` if it is gone (`/tmp` is ephemeral):

```python
#!/usr/bin/env python3
# Thin CLI over the Linear MCP server, reusing scripts/import-linear.py plumbing.
#   python3 /tmp/lin.py issues            # id, title, status
#   python3 /tmp/lin.py issue PER-48      # full issue JSON
#   python3 /tmp/lin.py comment PER-48 FILE
#   python3 /tmp/lin.py state PER-48 "In Progress"
#   python3 /tmp/lin.py states            # the team's states
#   python3 /tmp/lin.py call get_issue '{"id":"PER-48"}'
import importlib.util, json, sys
REPO = '/home/adamolsson/projects/holosmith'
spec = importlib.util.spec_from_file_location('imp', REPO + '/scripts/import-linear.py')
imp = importlib.util.module_from_spec(spec); spec.loader.exec_module(imp)
PROJECT = 'Holosmith'
a = sys.argv[1:]
if a[0] == 'issues':
    for i in imp.call('list_issues', {'project': PROJECT, 'limit': 250})['issues']:
        print(i['id'], i['status'], i['title'][:70])
elif a[0] == 'issue':
    print(json.dumps(imp.call('get_issue', {'id': a[1]}), indent=2))
elif a[0] == 'comment':
    print(imp.call('save_comment', {'issueId': a[1], 'body': open(a[2]).read()})['id'])
elif a[0] == 'state':
    imp.call('save_issue', {'id': a[1], 'project': PROJECT, 'state': a[2]})
    print(imp.call('get_issue', {'id': a[1]})['status'])
elif a[0] == 'states':
    print(json.dumps(imp.call('list_issue_statuses', {'team': 'PER'}), indent=2))
elif a[0] == 'call':
    print(json.dumps(imp.call(a[1], json.loads(a[2])), indent=2))
```

Always run it with `PYTHONDONTWRITEBYTECODE=1` and `rm -rf scripts/__pycache__` afterwards, or you will
leave an untracked `scripts/__pycache__/` behind. `list_issues` on this project returns **80 issues**
(not 250); `grep -v ' Done '` is the quickest way to the unfinished ones.

**Two API traps, both hit already:**

- `save_issue` takes `state` (the **name**, e.g. `"In Progress"`). It rejects `stateId`.
- `get_issue` takes only `id`. Passing `project` is a validation error.

The team's states are `In Review`, `Backlog`, `Canceled`, `Duplicate`, `Done`, `Todo`, `In Progress`.
Never invent a state name.

**Token expiry is routine, not a blocker.** When a call prints `The Linear access token has expired.
Re-authenticate 'linear' in pi.`, refresh it in place — the store keeps a refresh token and a client id:

```python
import json, os, time, urllib.request, urllib.parse
p = os.path.expanduser('~/.pi/agent/mcp-auth.json'); key = 'mcp__linear|https://mcp.linear.app/mcp'
d = json.load(open(p)); e = d[key]
body = urllib.parse.urlencode({'grant_type': 'refresh_token',
                               'refresh_token': e['tokens']['refresh_token'],
                               'client_id': e['clientInformation']['client_id']}).encode()
req = urllib.request.Request(e['discovery']['authorizationServerMetadata']['token_endpoint'],
                            data=body, method='POST',
                            headers={'Content-Type': 'application/x-www-form-urlencoded'})
new = json.loads(urllib.request.urlopen(req, timeout=60).read())
e['tokens'] = {**e['tokens'], **new}
e['tokensExpireAt'] = int((time.time() + int(new.get('expires_in', 3600))) * 1000)
json.dump(d, open(p, 'w'), indent=2)
```

If that fails, **stop and report** — that is a `PROMPT.md` stop condition.

---

### Pushing

**A session whose sandbox was started without the agent has no way to push at all, and that is a report, not a
workaround.** In PER-101's session `/run/ssh-agent.sock` did not exist, `SSH_AUTH_SOCK` was unset, `ssh-add -L`
answered `Could not open a connection to your authentication agent`, and `~/.ssh` held only `config` and
`known_hosts` — no key of any kind. The commits stayed local and the ship note says so in as many words; a later
session pushes them. **Never fake a push, and never invent a key**: the next agent's first check is
`ssh-add -L` (or `ls /run/ssh-agent.sock`), and if that is empty, the honest end of the session is a clean tree, green
gates and a Linear comment naming the unpushed commits.

**When the agent is there, pushing works — but the sandbox's own `~/.ssh/config` gets in the way.** `origin` is `git@github.com:AdamOlsson/holosmith.git`; `bwrap.sh` binds the host's
`SSH_AUTH_SOCK` at `/run/ssh-agent.sock` (the private key never enters the sandbox), so a push signs on
the host. The bound `~/.ssh/config` has a `Host github.com` block with `IdentityFile ~/.ssh/github`, and
that file is **not** in the sandbox, so a plain `git push` fails with
`no such identity: /home/adamolsson/.ssh/github: No such file or directory` / `Permission denied
(publickey)`. It is intermittent — it worked once and failed the next commit — so use the override every
time:

```bash
GIT_SSH_COMMAND='ssh -F /dev/null -o IdentityAgent=/run/ssh-agent.sock \
  -o StrictHostKeyChecking=accept-new -o User=git' git push origin master
```

`-F /dev/null` is the load-bearing part: it stops ssh reading the sandbox's `~/.ssh/config` at all, which
is what names the missing key. (A plain `git push` worked once and then stopped; `-c
core.sshCommand='ssh -o IdentityAgent=…'` was not enough either — the config still won.) `ssh -T
git@github.com` with those options answering `Hi AdamOlsson!` is the way to check the agent before
blaming the remote.

**The reliable form offers ONE key.** The agent holds three, and *which* GitHub accepts changes between
sessions — in this session it was the **third**, offered by `sed -n '3p'`, after the first two were rejected
(`ls-remote` succeeded with it once and failed the next time, so retry the loop rather than trusting one
probe). Loop over the three, offer one at a time, and push as soon as one works:

```bash
for i in 1 2 3; do ssh-add -L | sed -n "${i}p" > /tmp/k.pub
  GIT_SSH_COMMAND="ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityFile=/tmp/k.pub \
    -o IdentityAgent=/run/ssh-agent.sock -o StrictHostKeyChecking=accept-new -o User=git" \
    git push origin master && break; done
```

(The older single-key form, for reference:)

```bash
ssh-add -L | sed -n '2p' > /tmp/pushkey.pub
GIT_SSH_COMMAND='ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityFile=/tmp/pushkey.pub \
  -o IdentityAgent=/run/ssh-agent.sock -o StrictHostKeyChecking=accept-new -o User=git' \
  git push origin master
```

(`-i` names a `.pub` that is in the agent: ssh offers exactly that one and signs with the agent's copy.)
`git ls-remote` with the same options is the cheap way to tell "pushed" from "the agent refused again".
Push after every commit that matters, and if a push fails, report it — do not fake one.

---

## 3. The gates

`. ~/.pi/toolchains/env.sh` first. Then:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                          # lib + every test binary; the run prints the counts
cargo test --test schema_previous   # the `migrations` phase, run on its own
cd web && npm test                  # the transcript's rules, node's own runner
cd web && npx tsc --noEmit          # not a gate phase, but a web edit that does not typecheck is a bug
scripts/verify.sh                   # THE one-command gate since HS-006
```

`scripts/verify.sh` runs seven phases, cheapest first: `fmt`, `clippy`, `test`, **`migrations`**
(`cargo test --test schema_previous` — the expand/contract rule, HS-075), `build` (`scripts/build.sh`, which
itself runs `build-web.sh` — `npm test` then Vite — and `check-static.sh` last), `embed`
(`scripts/check-embed.sh`), then `serve` — the **shipped musl artifact** booted on `--port 0` with a temp data
dir, running its cases plus a CORS sweep, then torn down with `SIGTERM` and an asserted exit 0. **The run
prints its own case and assertion counts — quote those, never a number copied out of this file.** Since HS-046
it also prints one `ok <case>` line per case that measured something: a case that printed `SKIP` withholds that
line and the run ends with `SKIP n of N cases measured nothing`, so `grep -c '^ok case_'` is the number that
proved something and a skip can never read as a pass. It takes ~13–14 min (both musl targets, the Docker cases,
`case_idle_stop`'s real minute, `case_disk_quota`'s two boots, `case_preview`'s container and
`case_doctor_full`'s self-test). `--no-build` skips `build` and `embed`; `--keep` keeps the work dir.

Some cases bring their own boot, and they are the ones a new story is most likely to collide with:

| Case | What it needs |
| --- | --- |
| `case_token_budget`, `case_usage_survives_resume`, `case_compaction`, `case_history`, `case_backup`, `case_worktree`, `case_worktree_refusal`, `case_agent_launch`, `case_docker_resume`, `case_docker_turn`, `case_internet_toggle`, `case_doctor_full` | its own `serve`, and (the last five) the daemon and `agent_image` |
| `case_changes`, `case_changes_diff`, `case_merge`, `case_merge_strategies`, `case_merge_guards`, `case_publish` | `python3` for a `_ping`-only docker socket, a real repository under their own `projects_root`, and (for the three merge cases) their own **data root**, because `<data>/merge` is what they measure. `case_merge_guards` also writes one column (`sessions.status`, with python3's `sqlite3`) because a gate may not run a turn. `case_publish` also makes a **bare repository** under `$WORK` as `origin`, boots **three** times (the ordinary run, one with git's `GIT_CONFIG_*` naming a second `pushurl` repository, one with `remotes = []`), and needs no daemon. No daemon. |
| `case_verification_table` | no server, no daemon, no image: it reads `HolosmithSpec.md`'s eleven `V<n>` rows (and runs first in `serve_cases`, so a rotted table fails before the Docker cases) |
| `case_docker_preflight`, `case_doctor_quick`, `case_disk_floor` | no shared server (`case_doctor_quick` and `case_disk_floor` build their own `_ping` socket and a `PATH` stand-in — `df` for the floor — so they need `python3` and nothing else; `case_disk_floor` also boots its own Docker run and puts the shared boot back) |
| `case_disk_quota` | its **own** Docker boot and `agent_image` (the worktree must be real: a stub row has none), and a `du` stand-in on `PATH` that records its argv and answers an over-cap size; it restarts that boot so the watchdog's first (post-reconcile) pass measures, then puts the shared boot back — one `SKIP` line without the daemon or the image |
| `case_container_mounts` | no server *and* no daemon — one printed `SKIP` line when there is neither a daemon nor an image |
| `case_exit_classification` | `python3` (it reads a row's `expected_stop` out of `app.db`), skips without it |
| `case_egress_proxy` | **no** server, daemon, internet or key: it starts the artifact as `proxy egress` on a loopback port and asks it four questions over a socket |
| `case_schema_newer` | no daemon; boots the artifact over a schema-99 database |
| `case_key_proxy_request` | a stand-in `openssl` on `PATH`; asserts the bytes the key proxy really sends |
| `case_server_deploy`, `case_server_service` | two halves each: an **offline** one (`python3` for a `_ping` socket / a `/api/health` responder) and an **online** one (the daemon and an image) that drives a `--root` deploy with `deploy_machine_shims`' `tailscale` on `PATH`; they remove `hs_egress`/`hs_key` before and after, and `case_server_images` must have run first |
| `case_preview` | **its own boot** (stub backend, own data root, `preview_listen = "127.0.0.1:0"`) and, for its second half, the daemon and `agent_image` — it makes its own `s_<id>` container on an `--internal` `agents_<id>` and prints one `SKIP` line without them; the first half always runs and puts the shared boot back |
| `case_pwa` | **nothing** — no daemon, no image, no `openssl`, no `node`, no `python3`: its own stub boot and its own data root with a hand-written VAPID pair written into `secrets/`, then the health answer's sixth key compared with that string and `null` after the file goes. It has **no `SKIP` arm at all**, on purpose. It restores the shared boot (`stop_serve`/`start_serve`), so it sits right after `case_push` |
| `case_push` | **no daemon, no image**: its own stub boot, a `node:https` receiver on `127.0.0.1:0` (so `node`), a CA and a `localhost` certificate plus the machine's VAPID pair (so `openssl`), and `python3` for the SEC1 walk and `app.db`. It exports `SSL_CERT_FILE` around `start_serve` and unsets it at once — the sender passes no `-CAfile` on purpose (ADR-0071), which is what lets a private CA stand in front of it. It holds an SSE stream open **across** a turn (a raw `/dev/tcp` descriptor, because `fetch_stream` closes its socket and releases the guard) |
| `case_forward` | **two boots of its own** (a `role = "server"` and a `role = "hub"` with a `hosts.toml` naming it — `start_peer`/`stop_peer`, its own admin dir, `forward_fetch <port>`), a `python3` silent peer for the `504`, and — for the preview arms only — the daemon and `agent_image` (one `SKIP` line without them; the API half needs no daemon). It puts the shared boot back |
| `case_machine_trust` | **its own boot, twice** (an empty `allowed_hubs`, then `["workshop"]`), a `tailscale` shim first on `PATH` (the `deploy_machine_shims` shape, paths through the environment), and `python3` for a client **bound to `127.0.0.2`** — the only way to be a peer on one box. No daemon, no image. It prints one `SKIP` line without `python3` and restores `PATH` and the shared boot |
| `case_hosts` | **its own stub boot on a pinned control port** (`start_serve <config> <data> <listen>` — a seed row naming *this* machine has to spell the port the boot will use), three `python3` **TLS** stand-ins on loopback (a healthy payload, a `302` that records each request, a payload with no `name`), a CA + `localhost` certificate with a `DNS:` SAN (so `openssl`), and `SSL_CERT_FILE` exported around both `start_serve` calls — the probe passes no `-CAfile` on purpose (ADR-0071/0073), which is what lets a private CA stand in front of it. It restores the shared boot |

A story is not done until the gates are green on a clean tree, and then you run the story's own demo
yourself and paste its output.

**Every story adds at least one `case_*`** — the rule is in bold in `docs/build.md` and the reviewer enforces
it. Two shapes to avoid: a case that *needs* a session created two cases earlier, and a case whose `SKIP`
would also fire when the feature it checks is deleted (§6).

**Run one build at a time.** A `scripts/build.sh` or `scripts/verify.sh` in the same tree while another
one runs can make `check-static.sh`'s banner wait flake under load (it happened once during HS-013: the
gate reported `FAIL build/failed` while the same command passed a minute later, alone). Serialise them.

**Rebuild after committing.** `git describe` is baked into the artifact and `check-static.sh` fails on a
stale `dist/`, so a commit without a rebuild leaves the gate red. Commit, then `scripts/verify.sh`, then
the demo you paste.

**Rebuild after editing `web/`, before any demo.** The artifact embeds `web/dist`, so a demo run against
an artifact built before the page changed is a demo of the *old* page — and it will look like your fix
did nothing. HS-020 lost a demo cycle exactly this way: the fix to `main.ts` was invisible until
`scripts/build.sh` ran again. `scripts/verify.sh` rebuilds, so "gate green, then demo" is the safe order.

---


## 4. The repository as it stands

```text
Cargo.toml                  workspace: member crates/holosmith, edition 2024, one release profile
Cargo.lock                  COMMITTED, registry-only. check-static.sh fails on a git+ source.
.cargo/config.toml          musl linkers + relocation-model=static (non-PIE) for both targets
pins/pi.txt                 THE ONLY copy of the pinned pi version. Never repeat the literal anywhere.
crates/holosmith/
  build.rs                  git tag (env → git describe → unknown), HOLOSMITH_TARGET, rerun-if-changed
  build_support/web_embed.rs  embed-vs-placeholder decision + placeholder marker
  migrations/0001_initial.sql   schema_version
  migrations/0002_sessions.sql  sessions + two indexes (schema 2)
  migrations/0003_events.sql    events + sessions.last_seq (schema 3)
  migrations/0004_expected_stop.sql  sessions.expected_stop (schema 4, HS-030)
  migrations/0005_push_subscriptions.sql  push_subscriptions (schema 5, HS-066)
  migrations/0006_hosts.sql     hosts (schema 6, HS-068)
  migrations/0007_hosts_preview.sql  hosts.preview (schema 7, HS-069)
  src/main.rs               dispatcher only
  src/cli.rs                clap: version, config {validate,show,edit}, serve [--port|--data-dir|--backend],
                            proxy {egress --listen --allow}
  src/commands/             version.rs, config.rs, serve.rs, proxy.rs, doctor.rs, key.rs, container.rs,
                            deploy.rs (the seven steps' command: the config, the tail, the resume command),
                            drain.rs (`holosmith drain [--wait]`: a std UnixStream client, and the exit code
                            is the server's terminal word — 0 only for `drained`)
  src/config/               mod.rs (typed Config, KeySpec walk, 33 keys), schema.rs, problem.rs,
                            render.rs, duration.rs, editor.rs
  src/backend/              mod.rs (Step, SessionBackend, `ProcessEnd`/`ExitWatcher`/`supervise`,
                            Backends::for_run(backend, ContainerSettings),
                            BackendError, TurnEnd, Provisioned), stub.rs (the scripted turn;
                            `ignoring_abort`, `failing_at`, `failing_snapshot`), docker.rs (all four steps and
                            a turn are real: `start_container` = `rm -f` then an attached `run`; the
                            per-session start lock, which the **network** step takes too since HS-037;
                            `stop` = `docker stop --time 10`; `resume` = network + proxy + container;
                            `apply_proxy` joins the egress proxy to the session network only when the row's
                            `network` flag is set, and `remove_network` leaves it before the `rm`)
  src/provisioner.rs        the four-step background task and its five session.status events; stops at
                            a step boundary when the row leaves `starting`; after the pi step it spawns the
                            task that awaits the backend's `ExitWatcher` and calls `Exits::record` (HS-031);
                            `with_settle_reason` so a startup recovery's `starting -> idle` carries
                            `interrupted` (HS-044)
  src/reconcile.rs          the startup pass (HS-044, ADR-0053): `recoveries` (the live rows), the pure
                            `containers_to_remove`/`networks_to_remove`/`orphan_worktrees`, `sweep_worktrees`
                            (git's `worktree remove` for a registered orphan, `remove_dir_all` for an
                            unregistered one, never a branch or a ref), `Reconciler::{new, run}` and
                            `INTERRUPTED`; awaited by `serve` before the first request and before either
                            policy starts
  src/turn.rs               the turn runner (HS-015) and the turn timeout (HS-022): touch → the
                            backend's turn → agent_settled + touch, abort_completed (or a `debug` line if
                            the ladder got there first), or unexpected_exit with the reason. Under a
                            deadline the ladder is *spawned* and the turn keeps being awaited;
                            `TURN_TIMEOUT`, `turn::timeout(minutes)`. `TurnEnd` is the backend's answer
  src/ladder.rs             the abort ladder (HS-016): clear_queue → abort → wait out abort_grace →
                            `running → stopped` with the caller's reason → stop. `climb` adds the
                            resume (the abort); `end_turn` returns `Ending::{Asked,Forced}` and does
                            not (the delete, the timeout and the budget). `FORCED_ABORT`
  src/trust.rs              machine-to-machine trust (HS-070, ADR-0075): `Trust` (`hubs`, `program`, `of(config, &Tools)`,
                            `guarded`, `admit(Option<IpAddr>) -> Result<Admitted, Refusal>`), `Admitted`
                            (`Unchecked`/`FrontDoor`/`Node{peer, name}`, each with the one `debug` line an accept logs),
                            `Refusal` (`NoPeer`/`NotListed`/`Unverifiable`, each with `status`/`code`/`sentence`/`log` —
                            the code is `http::error`'s constant, the sentence names the peer and the node and never a
                            configured entry), `front_door` (`127.0.0.1`/`::1` **exactly**, v4-mapped folded),
                            `whois(program, peer) -> Result<String, String>` (one `tailscale whois --json <ip>` child,
                            `try_wait` against `WHOIS_TIMEOUT` = 3 s, pipes read after it exits — `hosts::probe`'s
                            shape), `identity(answer)` (`Node.Name`'s first label, lowercased; `None` for anything else,
                            including a login-only answer), `PROGRAM` ("tailscale")
  src/sweeper.rs            the idle stop's task (HS-021): `Sweeper::sweep` (one pass, public for tests),
                            `run` (the tick loop), `spawn`; `IDLE_STOP`, `TICK` (5 s, a constant)
  src/stop.rs               the one way a container is stopped (HS-030): `Stopper { backend, database }`,
                            `stop` writes `sessions.expected_stop` and *then* calls the backend's `stop`.
                            The sweeper, the create's eviction and the ladder's forced rung all use it, so
                            spec item 6's "before every `docker stop -t 10`" has one home
  src/tool.rs               a tool call's output as a file (HS-033, ADR-0041) and its images as files beside it
                            (HS-035, ADR-0043): `Store::new/store/read/path`, `store_images`/`image`/`read_image`,
                            `preview`/`truncate`/`valid_id`/`floor_boundary`, `decode_base64`, `CAP`/`IMAGE_CAP`
                            (5 MiB), `PREVIEW_MAX`/`ARGUMENTS_MAX` (2000), `MAX_ID` (128), `truncated_marker`,
                            `Stored { preview, reference }`, `Image`/`StoredImage`, `sweep`
  src/exit.rs               exit classification (HS-030, ADR-0038): `Exit::{Expected,Clean,Failed}`, the
                            lifted `SessionLog` (HS-031: the backend appends the launch line), `Exits::for_database`,
                            pure `classify(expected_stop, code, stderr)`, `PI_EXIT`, `STDERR_LINES` (5),
                            `HEADLINE`-free `session_dir`/`LOG_FILE`, and `Exits::record(session, code,
                            stderr)` which takes the flag, applies `PiExit` or `UnexpectedExit`, and appends
                            the stderr to `<data>/sessions/<id>/session.log`. Its caller is the task the
                            provisioner spawns after the pi step (ADR-0039)
  src/drain.rs              the drain (HS-051, ADR-0060): `Control` (the process's `Arc<AtomicBool>` — never a
                            row), `Request` (`drain <millis>`, one line), `Drain` (set the flag, poll
                            `counts_by_status` for `running` every 100 ms, then `Trigger::Drain` +
                            `Stopper` per idle row), `Ending` (`drained`/`expired`/`refused`, and the exit
                            code), `LineSink`, `listen`/`prepare`/`spawn`/`answer` (the unix socket: 0600 in a
                            0700 directory, a stale socket replaced, a live one refused, a regular file named
                            and left alone), `SERVER_RESTART` ("server_restart"), `SOCKET_NAME`, `POLL`,
                            `REQUEST_TIMEOUT`, `MAX_REQUEST`
  src/deleter.rs            the delete flow's task (HS-019): revoke_key (HS-039's dummy) →
                            `backend.delete` → remove the row *and its event log* in one
                            transaction → close the session's streams. `DELETED` is the rung-2 word
  src/agent.rs              the agent process (HS-031, ADR-0039) and its record seam (HS-032): `Agent::launch`
                            (piped stdio, an LF-only JSONL reader that routes `response` records by command id
                            and **publishes** the rest, stderr into a 64 KiB tail, the exit sent after the last
                            record), `records()`/`Slots`/`Received`/`AgentRecords`/`RECORDS_BUFFER` (a bounded
                            channel the reader *waits* on), `request`, `ready`, `Ended`, `AgentError`,
                            `HOME`/`SETTINGS`/`TEMPLATE`/`ENVIRONMENT`/`COMMAND`,
                            `home`/`ensure_home`/`seed`/`launch_line`, `version_of`/`version_check`
                            /`version_sentence`
  templates/pi-home/settings.json  the seed's template, embedded with `include_str!` (no secrets)
  images/{proxy,agent}.Dockerfile  the two recipes `server deploy`'s step 3 builds from (HS-048, ADR-0057),
                            `include_str!`ed by `src/images.rs`; markers greped out of the artifact by
                            `scripts/check-static.sh`
  src/images.rs             the image builds (HS-048, ADR-0057): the two names, `Kind`, the five `LABEL_*`,
                            `Expectation` (`of`/`of_digest`/`mismatches`/`unbuilt`/`build_args`), `digest`/
                            `own_digest` (a hand-written SHA-256), `archive` (ustar with uid/gid/mtime zero),
                            `build_arguments`, `failure`, `Status`/`read`, `architecture`, `ensure`, the
                            `Images` seam
  src/container.rs          the session's container (HS-027/028/029, ADR-0035/0036/0037): `KeyProxy::mount` is
                            the **directory** `<data>/secrets` since HS-040 (a file mount pins the inode the store
                            renames away); `Mounts`
                            (spec item 1's four, host path == container path except `/work`),
                            `Mount::argument`/`Mounts::check` (refuses a missing or wrong-kind source
                            *before* `docker run`), `Limits`, `ContainerSettings` (`of(config)`, `at(root)`,
                            `docker()`), `SessionContainer::run_args` (the whole `docker run` vector:
                            `-i --rm --init --name s_<id> --network agents_<id>`, the hardening, the
                            three limits, `-w /work`, `-e HS_SESSION_ID`, pi's five variables and
                            `HTTPS_PROXY`), `network_name`, `Unmountable`; and since HS-037 the machine's
                            proxy: `EgressProxy` (`of(&Config)`, `run_args`), `PROXY_NAME` (`hs_egress`),
                            `PROXY_ALIAS` (`egress-proxy`), `PROXY_ENV`, `proxy_url(id)`,
                            `PROXY_START_TIMEOUT` (90 s); and since HS-039 the key proxy:
                            `KeyProxy` (`of(&Config)`, `run_args`), which mounts the key file read-only
                            and runs `proxy key` on `hs_key`
  src/push/                 the Web Push backend (HS-066, ADR-0071): mod.rs (the four kind words —
                            `turn_finished` is its own, `error`/`turn_timeout`/`budget_exceeded` are words the build
                            already had —, `LONG_TURN` (20 s, a constant), `Watcher` (the trigger table as one pure
                            function over frame, arrival and threshold), `Pusher` (`new`/`with_threshold`/
                            `with_deliver`/`spawn`/`run`/`announce`/`threshold`), the `Deliver` trait (which takes the
                            send's scratch directory), `Payload` = `{host, session_id, kind}` with `host` =
                            `preview::HOST`, and `Watcher::forget_all` for a lagged broadcast), repository.rs
                            (every `push_subscriptions` statement + the `sql::guard` walk), crypto.rs
                            (HMAC-SHA-256 on `images::sha256`, HKDF, AES-128 encryption, GCM's GHASH — pinned by RFC
                            4231, RFC 5869, FIPS 197 and NIST SP 800-38D, plus a `cfg(test)` `open`), encrypt.rs
                            (RFC 8291: the `pkeyutl -derive` child, the three HKDF chains, the 86-octet header and the
                            `aes128gcm` body, with §5's example byte for byte), jwt.rs (the ES256 VAPID token via
                            `dgst -sha256 -sign`, the `sec1`/`spki` DER writers, `raw_signature`, `origin`, `verify`
                            for tests, and `Temp`: a `0700` directory under `<data>/push/tmp/` with `0600` files,
                            removed on drop), send.rs (`request`'s byte-exact head, `status_of`, `Disposition`,
                            `DELIVER_TIMEOUT` = 60 s, and `SslClient`: one `s_client` child whose streams go to files
                            and whose wait is `try_wait` against that deadline), `OPENSSL`, `SUB`
  src/vapid.rs              the machine's Web Push key pair (HS-049, ADR-0058): `VAPID_FILE` (`vapid.toml`),
                            `Pair`, `store`, `render`, `parse`, `is_pair`, `from_der` (the SEC1 walk), `base64url`/
                            `unbase64url`; unit tests for the DER vector, RFC 4648's vectors and the three refusals
  src/key.rs                the provider/store half of HS-039 (ADR-0048), whose table gains `opencode-go` for
                            HS-040 (host `opencode.ai`, `/inference/go/anthropic`, `x-api-key`; ADR-0050):
                            `Provider`, `PROVIDERS`,
                            `provider`, `Store` (`from_data_root`, `ensure_file`, `load`, `save`,
                            `set_provider`, `remove_provider`, `providers`, `dummy_for`, `revoke`,
                            `session_of`), `Secrets`, `models`, `KEY_PROXY_ALIAS`, `KEY_PROXY_PORT`,
                            `KEY_PROXY_NAME`, `contains_real_key`
  src/keyproxy.rs           the key proxy itself (HS-039, ADR-0048): `Route::parse`, `Head`/`parse`,
                            `upstream` (header swap **and the body**: it returns head+body as one value since
                            HS-040, whose V3 run measured a `Content-Length` with no bytes behind it),
                            `serve`/`handle`, `forward` (openssl s_client, counted streaming), `Record`,
                            `MAX_BODY`, `MAX_CONNECTIONS`, `CA_FILE`
  src/snapshot.rs           the WIP snapshot (HS-042, ADR-0049): `take` (temporary index, read-tree,
                            add -A, write-tree, commit-tree, update-ref), `Taken::{Written, Removed}`,
                            `SnapshotError`, `REF_PREFIX`, `MESSAGE`, `IDENTITY`, `reference`
  src/commands/key.rs       `holosmith key set|list|remove` (HS-039): stdin-only set, providers only in
                            list, remove
  src/commands/container.rs `holosmith container snapshot` (HS-042): `/work` + `HS_SESSION_ID` + a
                            temporary index
  src/docker.rs             the `docker` CLI as a program (HS-028): `Docker{host}`, `run`/`succeeds`,
                            `ensure_network`/`gateway` and `EGRESS_NETWORK` (HS-036), `command_line` (the one
                            call whose stdio a caller keeps: the launch),
                            `create_internal_network`/`remove_network`, `remove_container`/`stop_container`
                            (`STOP_SECONDS` = 10), `inspect`, `DockerError`; and since HS-037 the bounded
                            family for the startup path — `bounded` (the primitive), `attempt` (the probe),
                            `run_detached(args, bound)`, `removed_bounded`, `remove_container_bounded`,
                            `remove_network_bounded`, `running`, `join_network`/`leave_network` (alias,
                            disconnect-first, force), and since HS-044 the two `--format` listings
                            `container_names`/`network_names`
  src/compactor.rs          compaction's task (HS-060, ADR-0032; `foldable` learns `starting` in HS-044,
                            because a recovery parks every live row there): `SNAPSHOT_AFTER` (24 h, a constant),
                            `TICK` (60 s), `MESSAGES_PER_PASS` (500), `Compactor::{new, with_window,
                            spawn, run, sweep, fold}`. A pass at startup, then a tick; the per-session
                            step holds the database mutex, so a running turn is skipped by the re-read
  src/backup.rs             the project backup (HS-065, ADR-0034): `Backups::{new, take, prune}`,
                            `BackupError`, `KEEP` (7), `BACKUPS` ("backups"), and the private `day`/
                            `civil_from_days` (UTC, integer-only, no dependency). Sync and std-only, and
                            it logs nothing — the create logs its error
  src/hosts/                the host registry (HS-068, ADR-0073): mod.rs (`Host` {name, url: `config::Origin`,
                            reach, version, last_error, last_checked_at}, `Reach`, `Refusal`, `name_problem`,
                            `origin`, `host_problem`, `port_problem` (a port that is not a number is *not* an
                            allowed port), `is_local`, `MIN_PORT` (1024 + each scheme's default), `MAX_NAME`,
                            `LOCAL` ("local", reserved), `register`/`accept`/`merge_outcome`/`probe_row`/`probe_one`,
                            `merge` (read the seed, merge, probe **every** row, write once) and `Merge`),
                            repository.rs (every `hosts` statement + the `sql::guard` walk),
                            seed.rs (`[[hosts]]` tables, a strict reader that refuses the whole file on one bad
                            line), probe.rs (`ask` → `Outcome::{Healthy, Unreachable, Refused}`, one `s_client`
                            child for `https` or one plain socket for `http`, **one** deadline for the whole
                            exchange, `interpret` pure, `PROBE_TIMEOUT` 3 s)
  src/http/hosts.rs         the registry's three routes (HS-068): `GET /api/hosts` (`{"hosts":[…]}` by name),
                            `POST /api/hosts` (upsert on the name, `201` + the row), `DELETE /api/hosts/{name}
                            [?force=true]`; codes `bad_host_name`, `bad_host_url`, `not_holosmith`, `host_busy`;
                            the probe runs in `spawn_blocking` **before** the lock is taken
  src/http/push.rs          the two routes of item 11 (HS-066): `POST /api/push/subscribe` (204, an upsert on the
                            endpoint) and `DELETE /api/push/subscribe?endpoint=` (204/404/422), the codes
                            `bad_endpoint` and `bad_subscription_key`, and the query read through `changes::decode`
  src/forward.rs            the hub's client (HS-069, ADR-0074): `Forwarding` (the 5 s/60 s pair, `timeout` by method
                            and path), `TIMEOUT`/`SLOW_TIMEOUT`/`CONNECT_TIMEOUT`/`MAX_HEAD`, `authority`, `raw_rest`
                            (the client's own bytes, never a decoded capture — the review's blocker), `Request`/
                            `request_head` (the head the hub authors: the peer's `Host`/`Origin`, the caller's
                            `Content-Type`/`Idempotency-Key`/`Last-Event-ID`, `Expect` and `tailscale-user-login`
                            dropped), `Reply`/`parse_reply`/`Framing`/`keep_header`, `Link` (plain socket or one
                            `s_client` child with `kill_on_drop`, `AsyncRead + AsyncWrite`), `Failure`, and
                            `BodyStream` — the streaming chunked decoder, which errors rather than ending early
  src/http/forward.rs       one handler for five registrations of `/api/hosts/{name}/{*rest}` (HS-069): `local` → a
                            bodyless `308`, `hosts/…` → `403 not_forwarded`, the row → the peer's origin, the body
                            buffered with `Content-Length` recomputed, the peer's status and headers copied (a
                            `Content-Length` only when it *is* the framing), and a 5 s/60 s bound on the reply's head
  src/http/tool_output.rs   one tool's stored output (HS-033): 404 for an unknown session, an id that cannot be
                            a filename and a missing file; `text/plain; charset=utf-8` otherwise
  src/rpc.rs                the mapping (HS-032, ADR-0040; HS-033's tool pair in ADR-0041; HS-034's usage
                            arithmetic in ADR-0042; HS-035's images and dialogs in ADR-0043):
                            `Mapping::{new, map, skipped, unknown}` (one pi record → at most one frame,
                            `Mapped::{Nothing, Frame, Settled, Tool, Cancel}`), `pump(records, mapping, events,
                            store, agent)` (turn.start → frames → turn.end, ending on `agent_settled`), the
                            minted message id, `declined`/`unknown` counters, `ToolEnd`/`result_images`,
                            `Cancel`/`cancelled_reason`, the two additive keys on `message.end` and the two on
                            `tool.end`
  src/http/tool_image.rs    one image a tool returned (HS-035): 404 for an unknown session, an id that cannot
                            be a filename, a non-numeric index and a missing file; the file's own type otherwise
  src/runtime.rs            runtime::Backend {Stub,Docker} — which backend this run uses
  src/http/                 mod.rs, app.rs (Routes/App seam, AppState + provisioner()/turner()/
                            transitioner()/ladder()/deleter()/sweeper()/compactor()/reconciler()/
                            stopper()/quota()/expiry()/pusher(), edge layer,
                            is_guard_exempt), error.rs
                            (the one envelope — `{code, message}`, plus `conflicts` when and only when a
                            merge conflicted — every code, ApiError::not_in_state / bad_prompt /
                            bad_steer / unsaved_work / unknown_session / cap_without_idle /
                            git_not_directory / submodules_present / worktree_config_enabled /
                            bad_target / bad_strategy / not_fast_forward / target_dirty /
                            target_checked_out / conflict / dirty_worktree / disk_floor — `unsupported`
                            retired with its last field in HS-056 — and since HS-062 `ErrorBody.message` is a
                            `Cow<'static, str>`, so a refusal can carry a measurement;
                            `http/disk.rs` is the floor's request-path half: `refusal(AppState) ->
                            Option<ApiError>`, answered before the project's refusals and the cap on both of
                            the two starts),
                            health.rs, guard.rs (the four checks), sessions.rs (POST+GET
                            /api/sessions, GET /api/sessions/{id}, POST …/prompt, …/abort, …/steer,
                            …/resume, DELETE /api/sessions/{id}[?force=true], TURN_STEER, EVICTED,
                            `Slot` + `slot_for` + `evict_one`, `apply_trigger`, the project's
                            refusals: `refuse_project` + `refusal_error`, and since HS-056 `deletion` —
                            the delete flow's request half, `delete()`'s body and the merge's rule 5 —
                            with `retained_for(state, session, landed)` and its `pub(crate)` answer
                            `DeleteAnswer`/`Kept`, and since HS-064 `complete_deletion(state, current,
                            trigger, force, landed)` — the flow's **second half** (snapshot → keep-rule →
                            transition → `Deleter::spawn`), re-exported from `http/mod.rs` so
                            `expiry.rs` runs one code path with the operator's `DELETE`), streams.rs (the
                            SSE handler)
  src/sessions/             mod.rs (Status, Session, NewSession, RepoError, `is_id`), repository.rs (every
                            `sessions` statement + its SQL-location guard; includes touch,
                            `oldest_idle`, `last_seq` (HS-044's reader), and `remove` — the row and its log in
                            one transaction),
                            state.rs (the transition table, Transitioner::apply / apply_logged, the
                            set_status guard), activity.rs (the one `touch`: lock, warn, carry on)
  src/events/               mod.rs (Event, NewEvent, EventWriter, EventStream, and since HS-059
                            `coalesce` + its private `parse_delta`/`Delta`, which HS-060's fold reads
                            too; and since HS-063 the `ERROR` kind — `"error"`, the per-session disk
                            cap's `{code, message, size_kib, limit_gb}`), writer.rs (`collect` →
                            `coalesce` → `commit`; the failure log names appended events *and* rows),
                            repository.rs (`insert_batch` drops an event whose session has gone;
                            `delete_session_log`; `compact`, the fold into one snapshot; since HS-061
                            `truncation_point`/`turns_before`/`read_before`; and `usage`/`message_id`
                            — the three payload shapes it parses, ADR-0032), stream.rs (replay→live,
                            lag, `Item::Truncated`/`HISTORY_TRUNCATED`, the cut in the read step),
                            registry.rs (open-stream counts)
  src/worktree.rs           the session's worktree (HS-025, ADR-0029), and since HS-044 `registrations`
                            reads git's own `worktree list --porcelain` for the startup sweep:
                            `Worktree::of` (the naming rule; it
                            carries `id` since HS-043), `ensure` (create or adopt, `Ensured { base_commit,
                            created }`), `ahead` (`rev-list --count`), `remove(&Retained)` (registered? →
                            remove → branch −D **only when not kept** → prune), and `GitError`; since
                            HS-026 the refusal reads (ADR-0030): `refuse`, `Refusal`, `repository_of`,
                            `CONFIG_READ` (`--local --type=bool`); and since HS-043 the keep-rule
                            (ADR-0052): `retained(base, targets, landed)` → `Retained::{Known { branch, wip },
                            Unknown}` (`merges_into` per existing target, `differing_ref` by tree, and since
                            HS-056 a **tip comparison** against `landed` — the commit a merge measured — so a
                            branch that has moved since is kept; ADR-0065 D6) and `Retained::{keeps_branch,
                            recovery}`; and since HS-053 `git_error_from`, the same
                            sentence for a caller that read git's streams itself (`changes::run_patch`); and
                            since HS-054 `checkouts` → `Checkout { path, branch, prunable }` with
                            `parse_checkouts` (the four block shapes git prints) and `registrations` delegating.
                            Sync and std-only; the backend calls it on `spawn_blocking`
  src/publish.rs            the publish (HS-057, ADR-0066): `PROTECTED` (main, master, release/), `listed`,
                            `protected` (the two arms), `destination` (ours -> theirs), `Request`, `Pushed`
                            (`{remote, branch, commit}`), `PublishError` + `sentence()`, `push_arguments`
                            (`-c core.hooksPath=/dev/null push --porcelain --no-follow-tags`), `rejected_reason`
                            (the porcelain `!` flag), `push_command` (the host's env, `GIT_TERMINAL_PROMPT=0`),
                            `remote_exists` (`remote get-url`, exit 2), `publish`. Sync and std-only
  src/http/publish.rs       one handler: `POST /api/sessions/{id}/publish` — body → **disabled** (403, before the
                            row) → `confirm` → remote listed → `branch`'s alphabet → protected → the target →
                            the row (404) → `base_commit` → `changes::basis` + the push on `spawn_blocking`
  src/changes.rs            the changes view's summary (HS-052, ADR-0061): `Changes::read(&Worktree,
                            base_commit, targets, remotes)` — the *ordered* refusal (`NoRepository` → `Missing` →
                            `Unregistered` → `NoBranch`, then `Git`) and five host reads: `ahead`
                            (reused from `Worktree`), `behind` (`rev-list --count agent/<id>..<target>`),
                            `dirty` (`status --porcelain --untracked-files=normal --no-optional-locks` in the
                            worktree), `files` (private: `diff --numstat -z` + `diff --name-status -z`, joined,
                            with `--no-ext-diff --no-textconv --no-renames`, a binary file's counts `None`),
                            and `merge_trial` (`merge-tree --write-tree`, `Merge::{Clean,Conflicted,Unasked}`;
                            `merge_arguments` pins the two-ref order). Also `FileChange`, `Changes` (eleven
                            keys + `mergeable_reason` + `choices`), `Choices` (`targets` = the configured
                            targets that exist, in the config's order, from `existing_targets` — the walk
                            `first_target` used to make and now returns; `remotes` = the config's list
                            verbatim — HS-058, ADR-0067), `ChangesError` + `sentence()`, `CI_PATHS`/`touches_ci`
                            (public for HS-058; the two arms are exclusive). Since HS-053 the *diff* too
                            (ADR-0062): `basis` (the four refusals + the tip, shared by both readers), `diff`
                            → `Diff { body: Vec<u8>, truncated, total, utf8 }` and `DiffError { Changes,
                            NotChanged }`, `patch_arguments` (`--literal-pathspecs`, `DIFF_FLAGS`,
                            `--no-color`, `--`), `run_patch` (`Patch`: 64 KiB chunks, keep `DIFF_CAP` = 256 KiB,
                            count to `COUNT_MAX` = 8 MiB then kill, stderr on its own thread), `assemble`
                            (line boundary, then a UTF-8 boundary only when the cut was mid-character) and
                            `truncated_marker`. Sync and std-only
  src/http/changes.rs       the two handlers: the summary (HS-052) and the diff (HS-053) — lock → row → the
                            row's `base_commit` or `409` → `Worktree::of` → `spawn_blocking` → answer, with
                            `ApiError::no_worktree`'s four sentences and `internal(..)` for a repository git
                            cannot read; the diff answers `text/plain` (`charset=utf-8` only for real UTF-8
                            bytes) or a `404 not_found` for a `path` outside `files[]`, and `path_of`/`decode`
                            read the query (percent-decoded, invalid escapes literal)
  src/merge.rs              the merge (HS-054, HS-055; ADR-0063/0064): the first writer of the project
                            repository. `merge(&Worktree, &Request, &[String], &Merges, data_root)` → `Merged
                            { merged_commit, target, strategy }` or `MergeError { Changes, Unlisted, NoTarget,
                            NotFastForward, TargetDirty, CheckedOutElsewhere, Conflict { paths }, Temporary
                            { path, detail } }` + `sentence()`; `Request { target, strategy, message }`,
                            `Strategy::{FastForward, Merge, Squash}` (`parse`/`as_str`, private `landing()`),
                            `listed` (the config predicate, asked by the handler and again by the module),
                            `target_head`, `is_ancestor` (`merge-base --is-ancestor`, 0/1 as the answer),
                            `checkout_of` (from `worktree::checkouts`, skipping prunable), `is_the_repository`,
                            `Site::{Ref, Checkout}`, `update_ref` (the compare-and-swap;
                            `update_ref_arguments` pinned by a unit test), `fast_forward` (runs git, then
                            classifies a failure by re-measuring the ancestry and `changes::dirty` — never by
                            parsing English), `fast_forward_arguments`/`hooks_flag`/`landing_prefix`
                            (`core.hooksPath=/dev/null` **and** `snapshot::IDENTITY`), `worktree_add_arguments`,
                            `merge_arguments` (`--no-ff -m …`), `squash_arguments`, `commit_arguments`,
                            `message_for` (`Merge`/`Squash agent/<id> into <target>` — git's own default is
                            `… into HEAD` in a detached tree), `land`/`apply` (the temporary worktree, the
                            strategy's commands, the teardown *before* the move), `run_in` (classifies a non-zero
                            exit by reading the index), `unmerged`/`conflict_paths` (`ls-files --unmerged -z`),
                            `nothing_staged` (`diff-index --cached --quiet`), `head_of`,
                            `clear`/`add`/`remove` (`<data>/merge/<id>`, a stale path removed rather than refused,
                            an unconditional `worktree prune`), `MERGE_DIR`, `Merges` (one `std` mutex per
                            repository path, held inside `spawn_blocking`), `FF_ONLY`. Sync and std-only
  src/http/merge.rs         one handler: `POST /api/sessions/{id}/merge` — body → **target listed** (422) → row
                            (404) → **rule 1a** the row's `status` (409 `session_running`, the only state
                            refused) → `base_commit` (409 `no_worktree`, this route's sentences) → **rule 1b**
                            `changes::basis` then `changes::dirty` in the session's own worktree (409
                            `no_worktree` or 409 `dirty_worktree`), both guards before the blocking task →
                            `spawn_blocking(merge)` → the three keys, and **rule 5** `sessions::deletion(…,
                            Some(tip))` when `delete_session` is true and the row is not already `deleting`
                            (ADR-0065); `parse_body` → `Parsed { request: merge::Request, delete_session }` (the
                            four keys, `unknown_field`, `bad_strategy` for a fourth word, `bad_body` for a
                            non-string or empty `message` or a non-boolean `delete_session`), `refusal_of` (logs
                            `sentence()`, then answers — including `ApiError::conflict(paths)`),
                            `changes_refusal` (the same `no_worktree` code, this request's four messages,
                            `internal("merge", ..)` for git)
  src/egress.rs             the egress proxy (HS-037, ADR-0045): `Request::parse`/`username`, `Rules`
                            (`new`/`allow`/`permits`/`target`), `Target`, `Refusal` (status, word, sentence),
                            `address_problem`/`embedded_v4` (the ranges, IPv4-mapped, 6to4/NAT64/compatible),
                            `Record` (`unreadable`/`tunnel`/`log`), `serve`/`handle`/`read_head`/`resolve`/
                            `connect`/`establish`/`respond`, `pump`/`pump_both` (counted both ways),
                            `PORT`/`TLS_PORT`/`DEFAULT_LISTEN`/`MAX_HEAD`/`HEAD_TIMEOUT`/`ACCEPT_BACKOFF`
  src/selftest.rs           the network self-test (HS-036, ADR-0044; HS-038's pair, ADR-0046; HS-039's
                            inverse pair, ADR-0048): `Runner` (the seam, now with `proxy`/`join_proxy`/
                            `leave_proxy`/`key_proxy`/`join_key_proxy`/`leave_key_proxy`), `Report`,
                            `Outcome::{Passed, Failed, Unknown}`, `run`/`run_with`/`decide`/`leaked`, `pair`,
                            `key_pair`, `NETWORK`, `PUBLIC`, `PROXY_TARGET` (`egress-proxy:3128`),
                            `KEY_PROXY_TARGET` (`key-proxy:8080`), `HOST_PORT`,
                            `REACHED` (9), `PROBE_TIMEOUT`, `PROBE` (`include_str!` of
                            `templates/selftest-probe.js`)
  src/preflight.rs          the three startup checks and their sentences (HS-024, ADR-0028), and since HS-045
                            the vocabulary `doctor` prints too: `Finding { check, outcome, sentence }`,
                            `Outcome { Pass, Fail, Unknown }`, `report_with` (each check has one function with
                            both arms) and `check`/`check_with` = its failures via `Complaint::of` (the failing
                            projection, same `String`); `Tools { git, df, du }` is the tests' shim seam
                            (`du` since HS-063), `Check`
                            is the **one** word table (docker, git, disk, network, systemd, merge, tailscale,
                            ports, database, proxy, image), `socket_sentence`, `MIN_GIT` (2.38). Since HS-062
                            `disk` is a *caller*: the measurement itself lives in `src/disk.rs`
  src/quota.rs              the per-session disk soft cap (HS-063, ADR-0069): `DISK_QUOTA` (the reason, the
                            trigger's word and the event's code — one spelling), `DU`, `TICK` (5 min),
                            `SETTLE`/`SETTLE_POLL` (the first pass waits for the boot's re-provisioning),
                            `DuError { Unrunnable, Refused, Unprintable }`, `measure(program, path) ->
                            Result<u64, DuError>` (`du -sk --`, blocking), `kibibytes`, `over(size_kib,
                            limit_gb)` (strict, in `u128`), `watchable(&[Session])` (running/idle with a
                            worktree), `Quota::{new, spawn, run, settle, sweep, stop, stop_idle, announce,
                            report}`; `tests/quota.rs` drives both ladder rungs, the under-the-cap arm, an
                            unmeasureable worktree, the argument vector and the wire
  src/expiry.rs             the stopped-session expiry (HS-064, ADR-0070): `TICK` (24 h, asserted), the pure
                            `cutoff(now, ttl_days)` (saturating twice), `Expiry { state: AppState, ttl_days }`
                            — it holds the *state* because it calls the delete flow's own half — and
                            `Expiry::{new, spawn, run, sweep, candidates, expire, row}`. The first pass is
                            immediate (the reconcile only touches live rows) and there is **no off switch**
                            (`stopped_ttl_days` already refuses `0`). It calls `http::complete_deletion` with
                            `Trigger::Expire` and `force: false`, logs a skip at `info` and any other ending at
                            `warn`, and emits nothing of its own. Its `#[cfg(test)]` interleaving test is where
                            the pass's status re-read is isolated (it drives the *table*, because
                            `no_status_write_outside_the_one_function` walks `src/**`)
  src/disk.rs               the free-space floor's one measurement (HS-062, ADR-0068): `DF` (the program's
                            name, once), `DiskError { Missing, Refused, Unrunnable, Unprintable }` + its
                            `Display`, `Verdict { Enough, Short }`, `available_gb(df_output)` (field 4 of line
                            2, in whole GiB), `verdict(available, min)`, `measure(df, root) -> Result<u64,
                            DiskError>` (blocking, `df -Pk`), `guarded(backend, role)` = Docker && not Hub,
                            and the source-walk guard that `disk::measure` is named outside this file only in
                            `preflight.rs` and `http/disk.rs`; `min_free_gb = 0` is *no floor* (nothing is
                            measured)
  src/doctor.rs             `holosmith doctor`'s list (HS-045, ADR-0054): `Depth {Quick, Full}`,
                            `Tools { preflight, tailscale, systemd, config_path, admin_dir }` (the last two are the
                            install tree's machine-wide paths, so `perms` reads the *caller's* tree — HS-049),
                            `perms` (the installed paths' modes and owners, the data root's owner as the reference),
                            the `Machine` seam (`running`,
                            `image_pi`, `selftest`, implemented by `Docker`), `run(config, depth, tools,
                            machine) -> Report`, `Report::{failed, failures, render}`; the checks it owns —
                            systemd (`/run/systemd/system`), the merge trial (a throwaway repo:
                            clean 0, conflict 1 with the path), Tailscale (`status --json`'s `BackendState`),
                            the two ports (free before a start, held by this machine's own `/api/health`
                            after one), the database (`db::recorded_without_migrating` + `schema::verdict`),
                            the self-test, `hs_egress`/`hs_key`, the image's pi, and since HS-066 `openssl`
                            (`Check::Openssl`, **`--full` only** — `--quick` is the unit's `ExecStartPre`, so a
                            missing notification channel must not refuse a boot); `SYSTEMD_DIR`, `PROBE_TIMEOUT`
  src/commands/doctor.rs    the subcommand: config, `doctor::run`, print, exit 1 iff a finding failed
  src/db.rs                 pragmas + read-back, apply_pending (which switches on `schema::verdict`),
                            `recorded_without_migrating` (SQLITE_OPEN_READ_ONLY: `doctor`'s reader, which must
                            not migrate), Database::sessions()/events()/push()
  src/sql.rs                the one SQL statement scanner + the shared source walk (cfg(test) guard)
  src/schema.rs             SCHEMA_VERSION (= 6), include_dir! embed, check_embed, contract_violations,
                            and since HS-075 `Verdict {Same, Older, Newer}`, `verdict` and `Verdict::sentence`
                            (the sentence the boot warns with and `doctor` refuses with, ADR-0047)
  docs/schema/previous.sql   the previous release's schema as a dump (HS-075, ADR-0047): compared with this
                            build's by `tests/schema_previous.rs` as a subset; NOT a migration, and it must
                            never move under `migrations/`
  scripts/schema-snapshot.sh  the release step that regenerates it (`<data root>/app.db`, read-only open)
  src/{serve,web,logging,clock,version,pins}.rs
  tests/                    trust (HS-070: the decision table over shims — every arm, the identity parser's
                            shapes, the child's argv, the deadline, "the sentence names no entry" — and the wire:
                            a real router driven from `127.0.0.2` through `ConnectInfo`, and the preview listener
                            over its own socket), forward (HS-069: the head the hub authors, the raw tail and its refused control byte,
                            the reply's framing and a whole chunked/length body, the five `308`-and-codes arms, a
                            peer that is gone (`502`) and one that never answers (`504`, against a shortened seam),
                            and the preview hop against a stand-in peer — the peer receives `/local/<id>/…`, `Host`
                            and `Origin` its own, an upgrade's origin rewritten),
                            hosts (HS-068: the rules over the wire, the merge's two boots, the probe's arms
                            against real listeners, the local row's 409/forced delete, the no-seed re-probe, and
                            `accept` over a real refused outcome), version_cli, config_cli, http_contract,
                            serve_cli, db_migrations, exit, agent,
                            push (HS-066: the rows and both routes over a real database, the trigger table, the three
                            triggers through the real Pusher with a recorder, the stream-open pair on the real
                            registry, the disposition table, the payload's three keys, a token signed by a real
                            `ecparam` pair and verified by a second `openssl` child, and the containers holding
                            nothing; since HS-067 the health answer's sixth key is the pair's `public_key` byte
                            for byte, and `null` once the file is gone),
                            disk (HS-062: a `df` stand-in the test rewrites — the boot's check and a real
                            `POST /api/sessions` read one number, 507 on create and on resume with the code and
                            both numbers, no row and no worktree, a replay answering its own row, a create at
                            the cap refusing *without* evicting, a turn that settles while the floor is
                            crossed, `min_free_gb = 0` needing no `df` at all, and a stub run not measured;
                            a `tokio::sync::Mutex` serialises the bodies because the shims fork),
                            expiry (HS-064: the pass over a backdated row, the stub's `Snapshot`-before-
                            `Delete` order, a failed snapshot skipping *and* staying a candidate, the
                            `error` row, the six reasons into `stopped` with the resume's clear, the
                            window from the config, and the resumed row),
                            quota (HS-063: both ladder rungs — a stub told to ignore `abort` so the grace is
                            spent, and a turn that ends itself while the ladder waits —, the worktree exactly
                            at its cap, a `du` that cannot run, `session_disk_soft_gb = 0`, the argument
                            vector through a recording shim, and the event at the wire; one test uses the
                            machine's own `du` over a tree with `target/` and `node_modules/`),
                            changes (HS-052: the summary over a real repository in a `TempDir` — the two
                            counts against git's own arithmetic, the two-dot file list, a rename, a binary
                            file, an ignored and an untracked file, `touches_ci`, a conflict that moves no
                            working tree, every refusal arm at the module *and* at the wire, the JSON nulls,
                            and the manifest's `ReadOnly`; since HS-053 the diff — the body against the test's
                            own `git diff`, the binary summary with no NUL, the cap and the marker, the four
                            `path` refusals, an empty branch, a name that looks like a glob, and three
                            repository-config tests that drop each flag), rpc (the fixture replays and the pump),
                            tool_output
                            (the endpoint and the store's rules at the wire), key_cli, key_proxy, snapshot,
                            fixtures/pi-turn-{text,thinking,
                            tool,image,extension-ui}.jsonl, pin_single_source, guards, session_repository,
                            sessions_api, drain (the socket, the wait, the stop and the refusals), drain_cli
                            (the printed lines and the exit code against a hand-written socket), events,
                            streams, provisioning, state_machine, session_list, session_screen,
                            turns, abort, steer, resume, delete, eviction, idle_stop, turn_timeout,
                            deploy (the seven steps over shims), images (the embedded consts, the ustar, the
                            failure sentence, and a daemon-gated build of a test tag), doctor,
                            token_budget, preflight, doctor (every check's arms over shims, a fake `Machine`,
                            a real bound port and a real health listener, a real database in each schema state,
                            and a real git for the merge trial), backend_contract, worktree, reconcile (the row half and
                            the worktree sweep, no daemon), compaction,
                            history, backup, schema_previous (the subset test + the comparison function +
                            the verdict's sentences), merge (the fast-forward, both landing strategies, the
                            conflict's paths, the temporary worktree's lifecycle, the keep-rule after each
                            strategy, the identity and message of the commit this build writes — all over a
                            real repository, plus the wire group; since HS-056 rule 1's two guards with a
                            per-status negative half, the `dirty_worktree` and ignored-file pair, the
                            basis-before-dirty order, `delete_session` as far as a stub backend can carry it,
                            and the landed-tip rule's two halves), container (V1 + the container's flags + stop/remove/resume +
                            eight concurrent starts; **needs a daemon and `$HOLOSMITH_TEST_IMAGE`,
                            default `pi-agent:local`, and skips otherwise**), support/mod.rs
                            (the shared harness, which also has `repository`/`git`/`head`/
                            `branch_exists`/`worktrees` helpers for a real git repo in a `TempDir`)
web/                        Vite 7 + TypeScript, three screens (`/`, `/sessions/<id>`, `/settings`)
                            package-lock COMMITTED; main.ts is the DOM, transcript.ts the renderer's
                            rules (frame parsing, block model, affordances, `stoppedNotice` /
                            `stoppedDetail`, `budgetNotice`, `createFailure`, `parseTruncated`,
                            `setTurnBase`, `historyPath`, since HS-030 `stateText`/`headline` — a
                            multi-line reason renders as its last line, clipped at `REASON_MAX` — and since
                            HS-043 `deletedNotice`/`Kept`, `UNSAVED_WORK_NOTICE`, `FORCED_DELETE_LABEL`;
                            since HS-058 the Changes sheet's rules — `fileLine`/`countsText`/`statusWord`,
                            `countsLine`, `ciWarning`, `mergeBlocked`, `targetOptions`/`preselectedTarget`,
                            `publishVisible`/`publishQuestion` (the CI warning is the confirm paragraph's
                            second line), `conflictLine`, `mergedNotice`/`publishedNotice`, `diffPath`,
                            `sheetFailure`, `NOTHING_COMMITTED`, `COMMIT_ASKED`/`RESOLVE_ASKED`,
                            `STRATEGIES`, `COMMIT_PROMPT`/`RESOLVE_PROMPT`;
                            `main.ts`'s `deleteSection` is the delete affordance, `changesSection` the
                            sheet, and `StreamTarget` carries one `showState` that patches badge, preview,
                            composer, the delete affordance and the sheet; since HS-067 `push.ts` (the
                            notification sentences, the id→path rule, `pushState`'s five arms, the two install
                            sentences, `navLink`, `subscriptionBody`) with `test/push.test.ts`, and
                            `sw.ts` — the service worker, bundled by `vite.sw.config.ts` into `web/dist/sw.js` —
                            plus `public/manifest.webmanifest` and four PNGs under `public/icons/`)
web/test/transcript.test.ts 72 tests over synthetic frames, node's own runner (`npm test`)
scripts/build-web.sh        npm ci + npm test + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/,
                            atomic copy, then check-static.sh. Uses `cargo build --locked`.
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html, marker
                            gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
scripts/verify.sh           the one-command gate (see §3), including the two stand-ins HS-049 added:
                            `deploy_machine_shims` (a `tailscale` that records argv and answers `serve status
                            --json` from the pairs it was asked for) and `health_responder` (a `python3` that
                            answers `/api/health` for a whole case)
scripts/import-linear.py    the MCP plumbing /tmp/lin.py reuses
docs/adr/0001…0075          read the recent ones before planning anything (0075 is machine trust, whose D10 is
                            the topology limit this story's UI inherits; 0076 is the next free number)
docs/build.md               prerequisites, gates, verify.sh, guards, migrations, placeholders
docs/review/PER-33…PER-102, PER-107  each a verbatim review + a `## Resolution` table
                            (`PER-102` is this build's newest; `PER-101` the one before it)
docs/stories/               the 80 story slices — the dependency order and definition of done
GLOSSARY.md                 the domain vocabulary. Check it BEFORE inventing a term.
```

### What exists and what does not

- **Working:** `version` (text + `--json`), `config {validate,show,edit}`, `serve` (control listener,
  embedded app, `/api/health`, `POST`/`GET /api/sessions`, `GET /api/sessions/{id}`,
  `POST /api/sessions/{id}/{prompt,abort,steer,resume}`, `DELETE /api/sessions/{id}[?force=true]`,
  `GET /api/sessions/{id}/events`, `…/events/history`, `…/tool-output/{tool_id}`,
  `…/tool-image/{tool_id}/{index}`, `…/changes`, `…/changes/diff[?path=]`, `POST …/merge` with all three of
  item 12's strategies, rule 1's two guards and rule 5's `delete_session`, `POST …/publish` with the config's
  remotes, the protected names, the merge target and a `confirm` that is required), `POST`/`DELETE
  /api/push/subscribe` (which store a browser's subscription and nothing a phone shows), `GET`/`POST
  /api/hosts` and `DELETE /api/hosts/{name}[?force=true]` (the host registry, which probes a host's own
  `/api/health` and marks it when it cannot), SQLite with **six** migrations, the guards, the event writer (which coalesces a commit's deltas), the SSE stream, the stub
  backend and its four-step provisioner, the
  enforced state machine, the abort ladder, the delete flow, the cap with eviction, the create's and the
  resume's three project refusals, the compaction task (which folds a day-old message into one
  `message.snapshot`), the Sessions screen with its create form (which prints a project
  refusal's sentence alone) and the "nothing was deleted" notice, the session screen at
  `/sessions/<id>` with a live transcript and a four-action composer.
- **The host registry exists (HS-068), the hub forwards through it (HS-069) and it is trusted by address (HS-070).**
  `hosts` is one row per machine
  (`name` the identity, `url` an **origin**, `reach`, `version`, `last_error`, `last_checked_at`), read from the
  table and seeded from `hosts.toml` at every boot — the file wins on a name, a row it does not mention survives,
  and **every** row is re-probed whether or not a file is named. A row whose origin is this machine's
  `control_origin` is **local** (`hosts::is_local`) and is the one whose delete is refused while this machine has
  live sessions; a local row is stored `reachable` with this build's version and never probed (the merge runs
  before the listener binds — and under `--port 0` the origin's port is re-derived after the merge, so the shortcut
  cannot fire on such a boot; ADR-0073 D7). The probe is the build's second outbound client: **one `s_client`
  child** for `https` (no `-CAfile`, so `SSL_CERT_FILE` is the trust seam) or one plain socket for `http`, one
  deadline for the whole exchange, name resolution the platform's, and an answer that is not a Holosmith health
  payload refused with `422 not_holosmith` rather than marked unreachable. **Two readers send**: `http::forward` dials
  the row's `url` for `/api/hosts/<name>/<rest>`, and `preview` dials its `preview` origin for `/<name>/<id>/…`; a row
  carries **no `role`, no uptime and no session counts**, which is HS-071's to read (one forwarded `GET
  /api/hosts/<name>/health` and one `/sessions` per machine work today). **Machine trust is the fifth check on the
  control listener and the first on the preview listener** (`trust::Trust::admit`, ADR-0075): a machine with a
  non-empty `allowed_hubs` verifies the caller's address against the tailnet's own answer, `127.0.0.1`/`::1` is the
  front door (`tailscale serve`'s), and an empty list verifies nobody — the documented ACLs-only fallback.
- **The agent is real (HS-031) and so is a turn (HS-032 – HS-034).** A Docker create provisions the worktree,
  the network, the attached container running pi in RPC mode, and pi's ready handshake, and ends **`idle`**;
  the agent's directory and the launch line are under the data root, the exit the attached stdio observes is
  classified, and a resume replaces the container and reaches `idle` again. A **prompt** reaches pi, its
  records become the transcript's frames, a tool call's output is filed, and the session's spend is counted —
  with one gap a real container still shows: pi refuses a `prompt` before a stream exists, because a session's
  network is `--internal`, its pi-home has only a dummy key, and the gate's machine stores no provider key, so
  `case_docker_turn` asserts pi's own refusal sentence and the streaming evidence is the committed fixtures —
  while HS-040's recorded run is the evidence that a real turn works end to end through the key proxy (text and
  thinking deltas, a tool round trip, usage), because the gate may not hold a provider key.
- **The egress proxy exists and is the only route out (HS-037).** `holosmith proxy egress` is a mode of the
  artifact; a Docker boot starts `hs_egress` on `egress` (bounded, `warn` on failure) and the network step
  joins it to a session's network under the alias `egress-proxy` **only when the row's `network` flag is set**,
  so `HTTPS_PROXY` is the same string either way and the difference is topology. HS-038 measured that (the boot's
  bare/joined pair and `case_internet_toggle`) and left the **copy** under the checkbox to whoever next owns the
  create form, because a new element is a UI change in a phase that forbids one (ADR-0046).
- **The migration rule has two halves and both are enforced (HS-075).** `schema::contract_violations` refuses an
  unmarked destructive statement in a migration (`cargo test`), and `tests/schema_previous.rs` refuses a drop or a
  rename against `docs/schema/previous.sql` — the gate calls the second the `migrations` **phase**. Refreshing
  that snapshot is a **release step** (`scripts/schema-snapshot.sh`), not a story's, and a contract migration
  changes it in the same commit. `schema::verdict` is the sentence `serve` warns with on a newer database and
  `doctor` refuses with (ADR-0047) — kept by HS-045 without migrating the file.
- **The startup reconcile landed (HS-044).** `serve` awaits `Reconciler::run` right after the state exists and
  before the sweeper and the compactor are spawned: every live row goes to `starting` (with `interrupted` when a
  turn was in flight), the names no row owns are swept (a container unless its row is a `deleting` delete in
  progress; a network and a worktree directory only when no row has the id; never a branch or a ref), and the
  sessions are provisioned again. **What is left of the network half:** nothing but the phase-1 verification
  run (HS-046), which is this handoff's §1 — `doctor` was HS-045. The delete's keep-rules (HS-043) are done and ADR-0052: `agent/<id>` survives a
  delete when it is ahead of its base and in no target, `refs/holosmith/wip/<id>` survives when its tree
  differs from the branch, the 202 carries `kept` (one additive key on the flattened row) and is the same
  decision the spawned removal applies, and the session screen has the **delete affordance it did not have**
  — a Delete button, a confirm step and the forced retry a `409 unsaved_work` earns. **V9 passes** (HS-044);
  every row of the spec's verification table now carries a verdict and the automation that produced it — HS-046
  filled V1, V2, V6, V8 and V10, and **HS-070 filled V11** (`pass, with the fallback applied`, ADR-0075 D10). `doctor --full` (HS-045) is what makes V2 — and V10's build, and
  the self-test — repeatable rather than a ceremony. The preview listener (HS-041) is
  done: the binary's third byte tunnel, `/‹host›/‹id›/*` with the prefix stripped and an upgrade's `Origin`
  checked, and **V7 passes** (the host reaches a container on an
  `--internal` network, measured directly and through the artifact). The key proxy (HS-039) and its
  **verification** (HS-040 — V3, V4 and V5 pass) and the WIP snapshot (HS-042) are done, and the spec's
  verification table has a **Result** column that HS-046 finishes filling.
- **Phase 1 is closed by the verification run (HS-046).** `HolosmithSpec.md`'s **Vertical slice verification**
  table is the record: every `V1`–`V10` row carries a verdict and the automation that produced it (a `case_*`, a
  `tests/*.rs`, a `scripts/*.sh`, or `docs/build.md`'s by-hand section), including V11 since HS-070. V3, V4 and V5 were **not** re-measured by that story — no provider key exists here and no gate may
  hold one — so they quote HS-040's by-hand run and say so. `case_verification_table` is what keeps the table
  honest (ADR-0055), and the gate now prints one `ok <case>` line per case that measured something, withholding
  it for a case that printed `SKIP`.
- **`doctor` exists (HS-045).** `holosmith doctor [--quick|--full]` prints one line per check —
  `check: pass|fail|unknown: sentence` — after a `doctor: <depth>` header and a tally, and exits 1 iff a finding
  failed. `--quick` is the list `serve` refuses on (same `Finding`/`Complaint`, so the sentence is byte-identical)
  plus systemd, the merge trial, Tailscale, the two listeners and the database's schema; `--full` adds the
  network self-test (whose bare probe *is* the network-off probe), `hs_egress`/`hs_key` and the image's pi. On
  this machine `systemd` and `tailscale` are `unknown` (no systemd; no `tailscaled` answering), which is the
  designed outcome, not a failure. Ports are two moments: free before a start, held by this machine's own
  `/api/health` after one. It starts nothing that outlives it. `ExecStartPre=holosmith doctor --quick` is
  **HS-049's** line to write.
- **The log is now read in two shapes and written by two producers of two kinds.** The stream replays a
  *window* (`history_window`, default 50) and says so with `history.truncated`; `GET …/events/history`
  answers the frames below that cursor. `events/stream.rs`'s `REPLAY_LIMIT` is still `u32::MAX` — it is
  "everything from the cut", and a second window there would be an invisible truncation. The project
  backup is a second *writer* of files under the data root (`<data>/backups/`), not of the database.
- **A stop is now recorded as the server's own, and a clean exit has its own state row — observed.**
  `Stopper` writes `expected_stop` before the `docker stop`, and `Exits::record` turns an exit into `stopped`
  (expected), `stopped · pi_exit` (code 0, no flag) or `error` + the last stderr lines. Since HS-031 its
  caller is the task the provisioner spawns after a successful pi step, fed by the backend's `ExitWatcher`
  (ADR-0039). Do not "fix" anything by adding a `docker events` watcher: the attached process *is* the
  observation.
- **Five policies can now stop something and they must not be confused:** the cap evicts (`evicted`,
  inside a create), the sweeper stops a quiet one (`idle_stop`, on its own tick), the turn timeout ends a
  *turn* (`turn_timeout`, from the runner), the token budget ends a *turn* (`budget_exceeded`, also from
  the runner, on one `fired` shared with the timeout so only one climb happens) and the drain stops every
  `idle` row an operator asked for (`server_restart`, over the admin socket). All of them go through
  `Transitioner::apply`, and the row moves before the container in every one of them.
- **`runtime::Backend`** has `Stub` and `Docker`; `serve --backend` and the config key `backend` select
  it. `DockerBackend::new(ContainerSettings)` provisions all four steps, runs a turn's five operations and
  implements the WIP snapshot. The default backend is **`docker`** (ADR-0001: the stub is opt-in), so a
  default build provisions a session to `error` with a reason rather than hanging in `starting`, and
  `scripts/verify.sh` boots with `backend = "stub"` in its config (so does `scripts/check-static.sh`'s
  empty-root run, because a Docker run refuses at startup on a machine that fails the preflight, HS-024, and
  that sandbox has no daemon, no `git` and no `df`). (Since HS-026 a create for a project that is not a plain
  checkout is refused earlier, with a 422; the demo trick HS-019 used still works for a project that *is*
  one — which is also how HS-019 provoked a real failed snapshot from the shipped artifact.)
- **`AppState`** is `{ config, runtime, database: Arc<std::sync::Mutex<Database>>, events: EventWriter,
  streams: StreamRegistry, backend: Backends, drain: Control, merges: Merges, tools: preflight::Tools }` — the
  last three since HS-051, HS-054 and HS-062 — with **thirteen** wiring helpers — `provisioner()`,
  `turner()`, `transitioner()`, `ladder()`, `deleter()`, `sweeper()` (an `Option`, `None` when
  `idle_stop_min` is 0), `compactor()` (always `Some`), `reconciler()` (HS-044), `stopper()` (HS-030),
  `quota()` (an `Option`, `None` when `session_disk_soft_gb` is 0, HS-063), `expiry()` (always present,
  HS-064), `drainer()` (HS-051) and `pusher()` (always present, HS-066). The mutex is an explicit
  stopgap (ADR-0012/0013; `docs/notes.md`'s *Data and state* landmines).
- **`sessions::state::Transitioner::apply`** is the only way a session's state changes; `apply_logged`
  is the runners' wrapper (log, never return). A provisioning step report is `from == to` with `step`
  set and is *not* a table row.
- **The create is one lock scope plus a loop.** `create_locked` answers `Slot::{Inserted, Existing,
  Full}`; a `Full` makes `slot_for` evict one idle session (through `apply`, which takes the lock
  itself) and try again. Idempotency is checked first, so a retry never evicts anyone. `evict_one`
  moves the row **before** it spawns the backend's `stop`, because the row is what frees the slot.
- **`config`** has (`32` keys now) `agent_image` (default `agent-image:<binary version>`; this machine's
  demos and the gate set `pi-agent:local`), `container_cpus` (2), `container_memory_mb` (4096),
  `container_pids` (512) — all three **refuse `0`** — plus `abort_grace` (default `10s`, **used** by the ladder), `idle_stop_min`,
  `stopped_ttl_days`, `min_free_gb`, `turn_timeout_min`, `max_session_tokens` and the rest — check
  `config show` before adding a key. The delete flow and **compaction** have **no** config key of their
  own (the 24-hour window is a constant), and `max_live_sessions` (default 3, ≥ 1) is the cap.
- **`POST /resume`** takes the transition, then spawns the provisioner, and **touches `last_active_at`**
  (a resumed session must not be the eviction's first candidate — which is now a real policy, not a
  future one). The cap is checked **before** the transition, and the resume answers `409 cap_reached`
  rather than evicting (ADR-0022, ADR-0024). `SessionBackend::resume` has had a body since HS-029 and is how
  a `stopped` row comes back.

---

## 5. The cycle — one issue, then write the state back

Everything is **sequential: one writer at a time**, never two children editing files. Review children are
read-only and may run in parallel.

1. **Confirm the issue.** Read §1's slice, the Linear body (`python3 /tmp/lin.py issue <IDENT>`, §2) and the
   spec items. **Check the issue's title against the story you mean** — the Linear number is not derivable
   from the `HS-` number (HS-059 is `PER-91`, not `PER-93`; a session wrote `Closes PER-93` for it and had to
   record the correction in three places). Note `HEAD` — that SHA is the review's fixed point.
2. **Plan.** For an **L** story, give a fresh `delegate` child a grilling brief
   (`~/.agents/skills/grilling/SKILL.md` + `domain-modeling`): it puts **one** round of questions to *you*
   via `contact_supervisor`, and you answer with `subagent_supervisor({action:"reply", …})`. For **S/M**
   (HS-043 is M), write the plan yourself — it is faster and the analysis is usually already done. Either
   way: state the environment facts, name the files to read, list the decisions already settled, and
   **pre-answer the obvious round-2 questions**. A planner may write only `GLOSSARY.md` and a new
   `docs/adr/*.md`.
3. **Record in Linear before any code exists**: `save_comment` with the plan shape, a decisions→precedent
   table, the acceptance restated as testable checks, the non-goals, the docs touched, the demo, and the
   follow-ups; then `state` the issue `In Progress` and re-read it to confirm.
4. **Implement.** Two proven patterns: **S/M in-session** (most stories since HS-016), or **M/L with a lot
   of new surface** handed to a fresh `worker` child with a *written* brief — the plan plus the "how", the
   ground rules (no commit, no Linear, no new dependency, `env.sh` first, don't edit `docs/stories/**`), the
   exact gate commands, and "budget discipline: keep the tree compiling; if you run long, stop green and
   report which steps remain". Twice a child found a genuine contradiction in the brief — verify its claims
   rather than trusting them.
5. **Verify yourself.** Never trust a child's pasted output: run the gates and the demo. This has caught
   something every time (a wrong count, a demo-revealed message bug, a panic path, an assertion that passed
   over an already-stopped row).
6. **Review** with a fresh `reviewer` child running `~/.agents/skills/code-review/SKILL.md`, read-only,
   against `<fixed point>...HEAD` plus untracked files, spec axis sourced from the Linear body + story slice
   + spec items. `git add -A` first and write `git diff --cached <fp>` to
   `/tmp/holosmith/reviews/PER-<n>-diff.txt` so new files appear. **The reviewer has no shell and no write
   tool**: check its claims, and *you* save the review verbatim to `docs/review/PER-<n>.md`. **Commit before
   launching it** — a child reading a tree that moves under it stalls.
7. **Fix** in the same session (or with a fresh `worker`), triaging every finding as fixed or
   dismissed-with-citation; a "won't fix" without a citation is not acceptable. Re-run the gates. Append a
   `## Resolution` table to the review file and commit it with the fixes. If a claim cannot be dismissed,
   mutate the code and watch the guarding test go red before believing it.
8. **Ship**: focused conventional commits that name the issue, the last one ending `Closes <IDENT>` (§1's
   identifier). Commit messages are prose about *why*, not a restatement of the diff — look at the last few
   for tone. Rebuild after committing (§3). Then `save_comment` with what shipped, the gates, the verbatim
   demo output, the acceptance item by item and the follow-ups; set the issue `Done`; re-read it to confirm.
9. **Push**, with the sandbox's single-key loop (§2). If the push fails, report it — never fake one.
10. **Write the state back.** Rewrite §1 of this file for the next issue (§1.10's shape), refresh
    the counts, the ADR number, the last gate run's numbers and §9's row, then commit that with the message
    `Refresh the handoff after <IDENT>`. Set §1's replacement by `PROMPT.md`'s ranking (Linear priority →
    `HS-###` order → anything already `In Progress` → skip an issue whose `Blocked by` stories are not `Done`).
11. **Kill every child you started, then stop.** This is the last thing a session does, and it is not
    optional: a child still running holds the working tree, a port, a data root or a Docker network, and the
    next agent then finds a tree that moves under it, a port that is taken, or a `case_*` that fails for a
    reason that belongs to *your* session. Nothing may outlive the session that started it.
    - **Subagent children:** `subagent({action:"list"})` and `subagent({action:"stop", id})` on every run
      that is still live — the planner, every `worker`, every `reviewer`, and one you left `interrupted`. A
      run that already answered needs nothing; a run that is *listed as running* is the one that matters.
      `stop` is not resumable, which is exactly right at the end of a session: a half-finished brief is
      recoverable from Linear and `docs/review/`, a child that keeps editing the tree is not.
    - **Processes:** every demo's `serve`, every backgrounded `timeout` wrapper, `scripts/verify.sh` if you
      interrupted it, and any `node`/`python3` client you left running. **Score the pids when you start them
      and kill those pids** — `pgrep -f 'holosmith serve'` matches unrelated processes (a landmine, in
      `docs/notes.md`), so a pattern-based kill is both too broad and unreliable.
    - **Docker:** `docker rm -f` every container a demo or a test made (`s_*`, `hs_*` names of yours) and
      `docker network rm` the `agents_*` network it made; leave the machine's own `hs_egress`/`hs_key`
      alone if `serve` is not yours to stop, and never remove a container you did not create.
    - **Check it, do not assume it:** `subagent({action:"list"})` shows nothing running, the pids you scored
      are gone, `docker ps` shows none of your containers, and `git status --short` is clean apart from the
      operator's own `log.txt`/`orchestrate.sh`. Then say in one line what you killed, and stop.

Working artifacts (`/tmp` is ephemeral; the durable record is Linear + `docs/review/` + `git log`):

```text
/tmp/holosmith/plans/PER-<n>.md            the plan
/tmp/holosmith/plans/PER-<n>-linear.md     the Linear comment body
/tmp/holosmith/plans/PER-<n>-impl-brief.md the implementer's brief
/tmp/holosmith/plans/PER-<n>-ship.md       the closing Linear comment body
/tmp/holosmith/reviews/PER-<n>-diff.txt    the diff artifact for the reviewer
/tmp/holosmith/reviews/PER-<n>-review-brief.md
/tmp/holosmith/reviews/PER-<n>-review-verbatim.md
/tmp/holosmith/demo-per-<n>.sh|.py         the demo, with its client half
/tmp/holosmith/web-check*.mjs              the Chromium half, when the story has a screen
```

---

## 6. Which notes bear on this issue

The catalogue lives in **`docs/notes.md`** — read the **cross-cutting** sections before you start anything,
then the sections below. It is a reference: everything there is a measurement or a review finding.

| Read | Why it bears on HS-071's Computers screen |
| --- | --- |
| Cross-cutting: *HTTP and the envelope*, then *Data and state*, *Config and docs* | **HTTP and the envelope** is what a screen draws from: ADR-0008's one envelope and the rule that a refusal's `message` is the sentence a client shows, the four guards, `is_guard_exempt`'s one entry (the health read a per-host role needs is exempt, which is why a forwarded health call works), and the `Routes`/`App` seam with its single `.layer(`. **Data and state**: a session's `status` is the fold the counts are made of, and the stopped window is the **peer's** `stopped_ttl_days`, not the hub's. **Config and docs**: a new key moves the golden file, `KNOWN_KEYS` and `config show`'s prose; an ADR means a line in `docs/stories/README.md`. |
| *Landmines: the installable app (HS-067)* and *the Changes sheet (HS-058)* | **This is the story's own section.** A page that must show something new: every sentence and every rule is a pure `transcript.ts`/`push.ts` function with a node test, `main.ts` keeps nothing but the DOM, `StreamTarget`'s one `showState` patches the badge, the preview link, the composer, the delete affordance and the sheet — and a gate case can only assert the served **bundle**'s ids and sentences, because no gate may run a browser. HS-058's `apiPath`-shaped lesson is here too: a route the page spells is a string a test pins. |
| *Landmines: the hub's forwarding (HS-069)* and *machine trust (HS-070)* | The two hops this screen reads through: `GET /api/hosts/<name>/<rest>` relays the peer's own answer **byte for byte** (so a row's fields are the peer's struct order, not this page's invention), `local` is a `308`, `hosts/…` is `403 not_forwarded`, the bound is 5 s per hop (60 s for `merge`, `publish`, a `DELETE` of one session) — so N hosts is N hops and an unreachable one must degrade rather than hang — and ADR-0075 **D10** is why a hub's forwarded call arrives at a peer as a front-door caller. |
| *Landmines: the host registry and its probe (HS-068)* | The row this screen lists: its shape and order, `is_local` (a row naming this machine is stored without a round trip), `reach`/`version`/`preview` being as old as the boot (ADR-0074 D5), the seed's asymmetry, and the probe's measured landmines — **one** deadline, `SSL_CERT_FILE` as the only trust seam, the certificate's `DNS:` SAN. The three registration refusals are the sentences the Add form must show unchanged. |
| *Landmines: the preview listener (HS-041)* | Where a preview link points and why it is a **second origin**: `previewUrl`/`previewOffered` in `transcript.ts`, `preview::Route::path`, and the row's `preview` (measured, never configured) — a page that built the link from *this* machine's health origin already exists and is the thing to change for a remote session. |
| *Reviews* (`docs/review/PER-102.md`, `PER-101.md`, `PER-100.md`, `PER-98`) | Four reviews' worth of the lessons that recur: a gate claim that asserts nothing, a **forged-header arm that could not fail** because it forged a value the implementation would refuse anyway (PER-102 S2), a check whose claim outran its topology (PER-102 S1, ADR-0075 D10), and a background path with no deadline. |

Three rules worth repeating here because they are the most expensive to relearn: **a test that cannot fail is worse
than no test** (mutate the behaviour and watch it go red); **a gate case that SKIPs on the bug it exists to catch is
this project's P1**; and **after any scripted Markdown edit, read the changed lines** — three such edits have cost a
review finding each. One more, new here: **`case_push`'s stream arm holds a raw `/dev/tcp` descriptor**, because
`fetch_stream` closes its socket — a case that copies the helper for a "keep it open" arm measures nothing.

## 7. Decisions taken on the operator's behalf (do not silently undo)

Almost every hard-to-reverse decision is an ADR (`docs/adr/0001` … `0075`, one line each in
`docs/stories/README.md`): read the recent ones before planning, and **amend rather than rewrite** — an ADR
a later story changes gets a "Superseded by ADR-00xx" note (0012/0013, and 0026 amended 0020). The **ADR index in
`docs/stories/README.md` is the one part of that directory a story edits** — it is the record of the decisions,
not a story slice, and HS-054 found ADR-0062 had never been given its line.

The handful that are **not** in an ADR, and that a fresh session is most likely to undo by accident:

- the default branch stays **`master`** — this repo has no `main`, and renaming it would be an invented
  change;
- **never fake a push**: report the failure instead (§2's loop is how a push actually works);
- **`docs/stories/**` is not edited** — it is the source the Linear issues were imported from; a contract
  that needs changing goes in an ADR;
- the default backend is **`docker`** with the stub opt-in (ADR-0001), so every test and script that boots
  `serve` says which backend it means;
- **S/M stories are implemented in-session; M/L ones go to a worker child with a written brief** (§5);
- the **Linear issue number is not derivable from the story number** — read the title before writing
  `Closes <IDENT>`;
- **one issue per session, then write the state back and stop** (this file's own rule, §5 steps 10–11);
- **nothing a session starts outlives it** — every subagent run, every demo server and every container it
  made is stopped before it stops (§5, step 11). A leftover child is not harmless: it keeps editing a tree
  the next agent has to trust.

---

## 8. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                                  # clean apart from log.txt/orchestrate.sh
. ~/.pi/toolchains/env.sh                           # ALWAYS before cargo/rustc
python3 /tmp/lin.py issues | grep -v ' Done ' | head # recreate /tmp/lin.py first (§2)
python3 /tmp/lin.py issue PER-103                   # scoped, Backlog, the issue §1 names
docker version                                      # the daemon must answer, or a Docker check cannot be measured
docker image inspect pi-agent:local                 # the image the Docker stories run
docker network ls | grep -w egress                  # the proxy test's premise (§2); `grep -x` never matches
docker rm -f hs_egress hs_key 2>/dev/null           # so tests/container.rs runs its real path
scripts/verify.sh                                   # baseline green, ~13–14 min (it prints its own counts)
```

If the baseline is red before you touch anything, that is the first thing you fix — and say so in Linear.

Then start §5 on **§1's issue** (`PER-103`, HS-071) and stop after step 11.

Before you begin, check that the last session left nothing behind — `subagent({action:"list"})` should show
no run in progress, `docker ps` none of its containers, and `git status --short` should look like the line
above. If it does not, that is the first thing you fix, and say so in Linear: a tree another session is still
editing is a stop-the-line error, not an inconvenience.

---

## 9. Reporting

The durable record is **Linear** (plan, demo output and ship note per issue), **`docs/review/PER-<n>.md`**
(the review verbatim and its `## Resolution` table) and **`git log`**. Keep a short ledger in the
conversation while the session runs (issue → plan → implement → review → fix → ship) so a restart can see it
from those three; do not carry old sessions' ledgers in this file.

If you stop before the issue ships, say so explicitly, name the next action, and leave the tree clean and the
gates green — **a half-finished story is worse than an honest stop**.

**The last shipped story** (`PER-102`, HS-070, ADR-0075; commits `fa0bf32`, `e087358`, `027f036`, `a624406`): phase
3's third story — machine-to-machine trust. Four things are worth carrying: **one decision, asked at the edge of both
listeners** (`trust::Trust::admit` in `http::app::edge` after the four browser checks and in `preview::handle` after
`parse_head`), because two copies of a trust rule is the failure mode and ADR-0051 already gives the two listeners two
vocabularies; **the identity is the tailnet node's short machine name and never the login**, because the login is the
owner and is identical on every node — comparing it would admit the stray node the story exists to refuse; **an
identity that cannot be read refuses** rather than admits, and the spec's ACLs-only fallback is spelled by an **empty
list**, not by a broken daemon; and **the front door is `127.0.0.1`/`::1` exactly**, because `tailscale serve` connects
from there — which the review then turned into the story's most important paragraph: `tailscale serve` dials the
origin's *local target*, so on a deployed machine every caller including the hub arrives as the front door, the whois
check bites only where a machine's control origin is its listener's own tailnet-reachable address, and a
serve-published machine rests on the tailnet's ACLs plus item 4's `owner` rule. That is **ADR-0075 D10**, it is why
V11's cell reads `pass, with the fallback applied`, and a machine-identity path behind serve is a named follow-up
(HS-072's shape). The gate: **92 cases, 1823 assertions**, no `SKIP`; `cargo test` **1254** (62 binaries); `web/`
**104**. The review found **one blocker** (that topology, recorded and narrowed rather than papered over), **one
major** (the forged-header arm could not fail, now forging the *listed* name) and nine findings in all, every one
fixed or recorded in `docs/review/PER-102.md`. Two measurements this run made rather than assumed: `cat f 2>/dev/null >&2`
sends stdout to `/dev/null` too (the log line's `detail` was empty until the shim was fixed), and a `tracing` field
written without `%` is Debug-quoted, so `reason=` arrived as `reason="…"` and the gate's own grep would not have matched.

**Two things this build does not do, recorded rather than discovered:** a resumed session's pi has **no memory** of
earlier turns (`--no-session`; the transcript is Holosmith's log), and a notification's payload is deliberately
content-free, so the browser has four `kind` words and a session id and nothing else to draw with — the four
sentences are `web/src/push.ts`'s, and the worker never asks for more.
