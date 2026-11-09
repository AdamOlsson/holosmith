# The hub's CLI is a client of its own routes, and a deploy installs one half of a machine

HS-072 is the spec's third way into machinery this build already had: the deploy's seven steps and the host
registry's rules. The acceptance is five bullets — a hub deploy that runs steps 1 (without the Docker and git
checks), 2 (without the docker group), 5, 6 and 7 and builds nothing; `hub host add/list/remove` with `POST
/api/hosts`' own checks; `--dry-run`; `server deploy` then `hub deploy` yielding `both` **with the API unchanged**;
and `status` reporting the health of every registered machine.

Two decisions carry the story — one door for a mutation, and one role per half — and both are the kind that a later
story would undo by writing a second copy.

## D1 — `hub host` calls this machine's own routes, and nothing else calls the registry

The CLI is a **client of `POST`/`GET`/`DELETE /api/hosts`**, not a second writer, not a third door, and not a
reimplementation of the rules. `src/hosts/client.rs` is a hand-written HTTP/1.1 client in `http::health::ask`'s
shape (one `TcpStream`, one bounded read, no client crate — ADR-0001), and the traffic is:

| question | what the client does |
| --- | --- |
| `add <name> <url>` | `POST /api/hosts` with `{"name","url"}` |
| `list` | `GET /api/hosts` |
| `remove <name> [--force]` | `DELETE /api/hosts/<name>[?force=true]` |

Three alternatives were refused, each for its own reason.

**In-process** — open `app.db` and call `hosts::register` + `database.hosts().upsert` — would be a **second writer of
one SQLite file beside a running `serve`**, which is the whole thing the `Arc<Mutex<Database>>` of ADR-0012/0013
avoids in-process and cannot avoid across processes; and it would have to re-derive the route's own order (probe off
the lock, then upsert) and the local-row special case. Two copies of one rule is what this story exists to prevent.

**The admin socket** (`drain`) is one line protocol for one purpose (ADR-0051/ADR-0060); a second verb would make it
a general RPC door by accident.

**A local pre-check of the name and the URL** would be a second spelling of `name_problem`/`origin`, and the
acceptance's word is *same*: a rule with two implementations has two answers the first time one is edited.

So the rules, their codes and their sentences stay exactly where they are, and the CLI's job is the request and the
answer.

## D2 — the client dials `control_listen` and sends the origin's own authority and text

The CLI runs **on** the machine, so it dials the listener (`control_listen`, loopback, plain HTTP) rather than the
origin: the hop `tailscale serve` would make is one it can skip, and an `https` origin would mean an `s_client`
child, a trust seam and a DNS lookup for a request that never leaves the box. An unspecified listener address is
folded to loopback, and a `:0` listener is **refused with a sentence** rather than guessed at — a port the kernel
gives is one this process cannot know.

The head is the four guards' worth of headers, in the shape the listener sees after `tailscale serve` has proxied a
browser's call:

- `Host: <control_origin's authority>`, which is the allow-list's question (and the reason it is the *origin's* host
  rather than the dialled address: a tailnet name is what `allowed_hosts` carries, and a browser on the machine sends
  the same);
- `Origin: <control_origin>` **verbatim** — ADR-0074's landmine, because the guard compares text and a row registered
  as `http://localhost:<port>` against an origin of `http://127.0.0.1:<port>` passes the probe and then refuses every
  mutating call;
- `Content-Type: application/json` on every non-`GET`, **even with no body**, and `X-Holosmith-Client: 1` — the two
  `bad_client` reads;
- `Connection: close` and an explicit `Content-Length`, so the read ends at the body.

The connection arrives from `127.0.0.1`, which is ADR-0075's **front door** and costs no whois; on a deployed machine
the `Owner` check passes because there is no `Tailscale-User-Login` header (a direct call, not one through `serve`).

**A machine that is not serving is a sentence, not an invented answer.** Nothing answering is `Unreachable` with the
address, the connect error and the command that starts a server; a body that is not ADR-0008's envelope is
`Unreachable` too (something else holds the port), because a refusal has to be *something the registry said*.

**`list` shares the door.** One door, one answer, one sentence for "nothing is serving" — the alternative (a
read-only `db::Reader` for `list` beside an API for the other two) is the two-path shape this decision exists to
avoid, and `status` is the command that reads the table with no server up.

## D3 — one sentence per refusal, and it is the API's

`hostFailure`'s rule (ADR-0076 D6) applies to the CLI unchanged, which is what makes "the CLI cannot fork it" true:
the envelope's `message` is printed **verbatim**, and the `code` stays in front for exactly the codes that are not
about a value the operator just typed — everything but `bad_host_name`, `bad_host_url` and `not_holosmith`. So a bad
name prints one sentence and a busy row prints `host_busy: "thisbox" is this machine, and it has 1 live session; …`.

`Failure` is the client's two arms and they are ADR-0008's own split: `Refused {code, message}` is the registry
answering about this request, `Unreachable(sentence)` is this machine's problem — which no code can describe, because
no route produced it.

`--force` sends the API's `?force=true`; the sentence that earns it is the API's, and the flag the operator typed is
the same decision in the CLI's own vocabulary. **That `?force=true` reads as a URL in a terminal is recorded rather
than papered over**: changing the message would change what the Computers screen shows and what `case_hosts` asserts,
and a message that named a flag in its caller's vocabulary would need the API to know its caller. It is a
follow-up, not a defect of this story.

## D4 — a deploy installs **one half** of a machine, and the two halves union

`deploy::refusal` is **deleted**. A role is not a reason to refuse a deploy: the spec's Roles table says running both
commands makes a machine `both`, so `server deploy` on a hub and `hub deploy` on a server are ordinary runs.

`deploy::Kind { Server, Hub }` carries three answers that are deliberately not one:

- `installs()` — the role this run **adds**;
- `union(found)` — the role the machine **ends with** (`server`+`hub` is `both`, either order; `both` stays `both`);
- `group()` and `skip(step)` — the two places a hub's half is not a server's.

`run_as` builds **two** configs from the one it was handed: the *installed* config (the union in its role, which step
2 renders into `config.toml`, so the file that boots next says `both`) and the *checked* config (the half this run
installs, which steps 1 and 7 hand to `doctor`). The split is the story's sharpest edge:

- **The union is why acceptance bullet 4 needs no by-hand edit.** The second deploy reads the role the first one
  wrote, and a machine that ran `server deploy` then `hub deploy` boots as `both`.
- **The checked role is why acceptance bullet 1 is true unconditionally.** A `hub deploy` asks **no Docker and no git
  question even on a machine that is now `both`**, which is what "runs steps 1 (without the Docker and git checks)"
  says. The cost is recorded rather than hidden: the *other* half's health is that deploy's business, and `serve` on a
  `both` machine refuses at its own boot when its preflight fails.
- **Step 5's unit follows the union, not the half.** The unit describes the machine that will boot: a `hub`-only
  machine's `holosmith.service` drops `Requires=docker.service` (and its `After=` clause), because a unit that
  required a daemon it does not have would never start; a `both` machine keeps it even when the run that wrote the
  file was a `hub deploy`. `Description=Holosmith <role>` — `Holosmith server` is the same bytes as before for a
  server.
- **Step 2's group follows the half.** A hub's `useradd` has no `--groups`, there is no `usermod`, and the `id -nG`
  question is not asked. Nothing ever *removes* a membership, so a `both` machine that ran `server deploy` first
  keeps the docker group when a later `hub deploy` reads it — which is why a hub's satisfied line says the user
  **exists and joins no group** rather than that it is in none.

`deploy::run` keeps its signature and *is* `Kind::Server`; `run_as` is the entry point the command uses. That is not
indirection for its own sake — `tests/deploy.rs`'s two dozen `deploy::run` calls mean the seven-step run, and making
each of them pass a variant would say nothing.

The command's tail gained two facts: `deploy: role: <union>`, with the role the config said in brackets when this run
changed it (acceptance bullet 4 says the machine is `both`; a run that reaches the union has to say so rather than
leave the operator to open the file), and next steps that follow the role — `key set` is a server's (a hub has no key
proxy) and `hub host add` is a hub's (a server registers *into* a hub and cannot reach its registry), so a hub-only
machine is never told to run a command whose only answer is "there is no key proxy here".

## D5 — a hub's five steps, and the two skips are `skipped` with a reason

All seven steps are still evaluated and printed, in the spec's order and with the spec's numbers, so a reader diffs a
server run and a hub run line for line and the gate's `1..7` order assertion stays true for both. `Images` and
`Proxies` answer `Word::Skipped` — the word's **first producer since HS-049** — and each line names what a hub has no
work for rather than saying nothing was needed.

`Word::Skipped`'s doc comment changes with it: it was "another story owns this step's body" and is now "this run's
role has no work for this step". The tally counts skips apart from satisfied and changed, which is the property that
makes the word worth having: a hub run's tally reads `5 satisfied, 0 changed, 2 skipped, 0 failed`, and no reader can
mistake two unbuilt images for two successes.

## D6 — `status` reads the registry table

`machines <n>` (a measured count, so a machine with no rows still prints a fact) and one `machine <name>` line per
row, through a new `db::Reader::hosts()` — the read-only open of `recorded_without_migrating`'s precedent (HS-045,
HS-050), so `status` needs no server, no lock and no second writer.

The row **is** the answer to the acceptance's last bullet. `reach`, `version`, `last_error` and `last_checked_at` are
the last probe's facts (ADR-0073), which is as old as the boot (ADR-0074 D5) — and `checked <n>s ago` is the sentence
that says so, so the line cannot be read as a live health check this command did not make. Asking each machine live
was refused: it is a hop per row, with N network timeouts and no bound of its own, and it is what the Computers screen
does because a page can afford it and a registry row cannot.

`hosts::facts(host, now)` is one function with two callers — `status` and `hub host list` print the same words for a
machine — and `now` is a parameter for `clock`'s own reason. `NO_REGISTRY` is **deleted** with its test: "no hub has
registered a machine with this one, and the registry that would hold one is phase 3 (HS-068)" was false twice over.

A failed read is one `machines fail:` line and **no** row lines, which is the session counts' rule one table over: an
empty list over a read that failed would be a claim about a machine nobody could look at.

## D7 — what the gate measures, and what it cannot

`case_hub_deploy` is **the one case in the run that needs no daemon, no image and no `openssl`** — the story's own
headline ("on a Docker-less machine") as a property of the harness. python3 is needed only to pick ports
(`free_port`) and to keep the busy-row arm's seed addressable, and a missing python3 is the case's only `SKIP` line,
with `tests/deploy.rs` and `tests/hub_cli.rs` named as where every arm lives.

It measures, in the order a failure reads best: the dry run's seven lines, four `needs` and two `skipped` with their
reasons, and an install root that does not appear; the `--root` run's steps 2–6 with their modes, the written
`role = "hub"`, a unit **without** `docker.service`, and a second run of five `satisfied` and two `skipped`; the
union on a **server** config — `role = "both"`, a unit that keeps the daemon, the role line, and a stub boot of that
tree answering `"role":"both"` with `/api/hosts` and `/api/sessions` still there (bullet 4's "the API is
unchanged"); `add`/`list`/`remove` through the client against that machine's own listener, including both
registration sentences with no code in front, the `host_busy` sentence with its code and its `force=true`, and the
`--force` removal that leaves the session standing; and `status`'s `machines 1` / `machine attic …` / `machines 0`.

Two things it does not do, deliberately. It does **not** assert that no image tag exists, because a tag from an
earlier case is not this case's fact — the claim that a hub deploy builds nothing is `tests/deploy.rs`'s, where the
`Fake` records every build and every daemon call and the test asserts both lists empty. And it does **not** reach a
real tailnet, a real unit or a second machine: step 6's origins exist only in the shim's answer, systemd is absent,
and `--ssh` is HS-073's.

The `409 host_busy` arm is the one that needed a trick, and it is the seed's: `POST /api/hosts` refuses loopback
(`host_problem`) while `hosts.toml` is not filtered by that rule (ADR-0073 D9), so the row that names this machine
can only be a seeded one — and a seed needs the port before the boot, which is why the case pins it
(`case_hosts`' idiom) instead of taking the kernel's.

## D8 — what carries forward

`GLOSSARY.md` gains **Hub deploy** and **Deploy half**, and amends **Service user** (a hub's joins no group) and
**Step**/`skipped` (a role's word, not only a story's). HS-073 (`--ssh`) inherits the `Kind`-shaped step plan: a
remote run reuses `run_as` unchanged and changes only how it reaches the machine. HS-078/079 (`upgrade`/`rollback`)
re-run a deploy's steps and must take the same `Kind`, or an upgrade of a hub-only machine would try to build images.
HS-074 owns cross-machine sessions and the notification deep link.

The follow-up this story leaves: `?force=true` inside `host_busy`'s message is the API's vocabulary in a terminal,
and a sentence that named the caller's flag would need the API to know who is calling it.
