# The Computers screen reads each machine over the hop it already forwards with

HS-071 is item 4's *other* half. The registry landed in HS-068, the forwarding in HS-069, the trust in HS-070 — and
the page still knew nothing about machines at all: `web/src/main.ts` fetched `/api/sessions` and
`/api/sessions/<id>`, built the preview link from **this** machine's `health.preview_origin`, and every affordance
spelled its own `/api/…` path. ADR-0074 D10 named the missing function (`apiPath(host, rest)`) and recorded the limit
this story closes. The acceptance is five bullets — a screen that lists machines and their state, add and remove, a
visible version skew, sessions grouped by machine, and no new-session affordance on a hub.

The shape worth stating first: **this story adds no route, no migration, no config key and no server-side reader.**
Everything the screen draws is an answer the build already serves — `GET /api/hosts`, and for each row one forwarded
`GET /api/hosts/<name>/health` and one forwarded `GET /api/hosts/<name>/sessions` — so the new work is a page that
asks several machines the same two questions, and the rules that make those questions answerable.

## D1 — role, uptime and counts are read live, never stored

Arm (b) — a `hosts.role` column fed by the boot's probe — was refused. A row's `reach`, `version` and `preview` are
already as old as the boot (ADR-0074 D5) and that is tolerable because nobody acts on them mid-day; `role` is not,
because it is the field this screen exists to see, and a column would keep saying `hub` about a machine that was
redeployed as a `both` hours ago. **The health hop is the live answer**, costs one request per machine inside
ADR-0074's 5 s bound, and the payload already carries `role`, `version` and `uptime_s`. So no migration, no
`SCHEMA_VERSION` move, and nothing to refresh.

The same argument settles the counts, one route over: `GET /api/hosts/<name>/sessions` is the peer's own list,
forwarded byte for byte, and the page folds it. An aggregate route on the hub — one answer with every machine in
it — was refused twice over: it would be a *second* reader of every machine's sessions on the server side, and a
route whose own latency is N hops, which item 3's "a remote machine going down mid-stream produces a clear
client-visible error, not a hang" would then have to answer for ×N. The page's own N parallel requests each get
their own `502`/`504` and each card degrades alone.

**What a count means.** `live` is the three states a container can be in — `starting`, `running`, `idle` — and
`stopped` is **every other state a row can hold**: `stopped`, `error` and the transient `deleting`. That is a
deliberate "the two numbers add up to the machine's row count" rule rather than a shortcut. A card that said
`2 live · 0 stopped` beside three rows would be arithmetic the operator has to reconcile, and `error` is a parked
session that can be resumed, so it belongs with the stopped ones and not with the work in flight. A machine whose
read failed shows `—`, never `0`: a zero is a claim about a machine nothing measured.

**Whose window.** The count of `stopped` rows is not filtered by any window on the page: the peer's own list already
is what the peer holds, and the day it removes one is the peer's `stopped_ttl_days`, which rides on that same answer.
So a row's countdown and its machine's count are about one machine, not about the hub's config.

## D2 — `apiPath(host, rest)` is the only way a path is spelled, and the host travels by closure

`apiPath(host, rest)` is `local` → `/api/<rest>` and anything else → `/api/hosts/<host>/<rest>`, which is ADR-0074's
prefix read backwards: the hub's forwarding route is `/api/hosts/<name>/<rest>` → the peer's `/api/<rest>`, so one
function spells both halves and `local` is the word that means *here* (and `/api/hosts/local/<rest>` is the `308` to
`/api/<rest>` that proves it).

Every request the page makes goes through it: the session's four actions, the stream, a history page, the four
Changes reads, the delete, the merge and the publish — and, the one that is easy to miss, **the paths the server
computed**. `tool.end`'s `output_ref` and `images[].output_ref` are written by the machine that ran the tool
(`/api/sessions/<id>/tool-output/<tool>`), and a forwarded frame is that machine's bytes; the hub's own route for
that path answers a `404` about a session it does not have, which reads to an operator as "the output is gone"
rather than as "you are looking at it from the wrong machine". `hostRef(host, ref)` re-roots it and leaves `local`'s
byte for byte.

`main.ts` keeps passing the host the way it already passes the id: each section's closure captures it
(`composerSection(host, id, …)`, `changesSection(host, id, …)`, `deleteSection(host, id, …)`,
`openSessionStream(host, id, …)`, `applyChange(…, host)`), so there is no module-level "current machine" to get out
of step with what is on screen. A row carries its host in `data-host`, because the row is also a stream's owner and
`main.ts` keeps no state beyond the DOM (ADR-0017).

**The session's page names its machine in the address**: `/sessions/<id>` is this machine's and
`/hosts/<host>/sessions/<id>` is another's — the API's own prefix, one level out. A query string would have been the
cheaper change and the second spelling of "whose session is this"; the segment means a reload lands on the same
machine's transcript, and HS-074's host-aware notification has a path to link to instead of inventing one.

**The author of a session's address is the page that fetched it, and nothing on the server changed.** No
`sessions.host` column: the owning machine is in the URL the caller used (ADR-0074's own sentence), and a row
forwarded from a peer cannot be edited on the way through — so the host a group is drawn under is the page's fact.

## D3 — grouping is a section per machine, and an unreachable machine is a sentence

The Sessions screen draws one section per machine, the machine's registry name as its heading, every row carrying
its host. This is one list *per machine fetched separately* rather than one fan-in list with a `host` key added by
the page, and the difference is where a failure lands: a fan-in needs the hub to answer for a machine it could not
reach, which means inventing a row kind the peers never send. A section per machine fails as a section — the
refusal's **sentence** in the place the rows would have been — which is the same shape as a card.

An unreachable machine is never an empty list. `{"host": …, "kind": "unreachable"}` was considered and refused: a
list is what a machine holds, not what a page knows, and an empty list would be a claim that a machine nothing could
reach has no sessions. The sentence says which machine was asked, at which origin, and what went wrong — the hop's
`message`, not through `hostFailure`, because a code in front of it would be a word to look up about a machine that
is simply away.

**This machine is always first, and it is always there**, from `/api/health` and `/api/sessions`, whether or not the
registry names it. A machine's own row is a row like any other, but the page is being served by it and does not need
a row to describe it. A registry row whose `url` is the origin this page was served from *is* this machine and is not
drawn twice — a text comparison after one trailing slash, which is all a page can do: the server's `hosts::is_local`
also folds a scheme's default port, and a page that invented a second copy of that folding would be a second
authority on what "here" means. A row registered with the port spelled out is a second machine **for the hop**, and
forwarded to over the network, exactly as ADR-0074 says the server treats it — but a `DELETE` of that row is refused
as this machine's own (`409 host_busy`), because `hosts::is_local` folds a scheme's default port and the page does
not. The asymmetry is the server's and it is recorded here rather than papered over: the page's comparison answers
"is this card this machine", and the registry's answers "may this row be forgotten while this machine has live
sessions".

## D4 — the preview link is this machine's listener with the peer's name in the first segment

The session screen's preview link becomes `previewUrl(origin, host, id)` — this machine's `preview_origin`
(`/api/health`'s fifth key) and `/<host>/<id>/` as the path. The host segment is the **row's** name, and the origin
is **always this machine's**, because the hub's preview listener is the front door: its `plan` is what rewrites a
peer's preview to `/local/<id>/…` and dials the row's measured `preview` origin (ADR-0074). A browser pointed
straight at that measured origin would arrive at the peer's listener as itself, and ADR-0075 would refuse it — so the
row's `preview` stays the *server's* forwarding target and never becomes the client's link.

A machine that forwards nothing offers no remote preview: `previewShown(state, host, role)` is the offered state
**and** (`local` or a forwarding role). A `server` has no preview hop (ADR-0074 registers one only where a machine
forwards), so a link out of it into a peer's preview would be a page nobody serves.

## D5 — version skew is a sentence, and it is about the API

The row's `version` is what the peer reported at boot; the card shows the version the hop just reported and, when it
differs from this build's, one sentence: *version 0.0.1 · this machine runs 0.1.0, so a call one of them does not
know answers 404*. Never a refusal — ADR-0074 forwards to a peer whatever version it reports, because gating on a
version turns an upgrade into a flag day — and never a warning about the machine being *wrong*, because it is not:
the acceptance's reason is that a skew "affects which API a call can use", and a `404` is what a call the older build
does not know answers.

When the hop fails, the registry's boot-time `version` is the fallback the card shows, and the sentence is the
failure's rather than a skew nothing measured.

## D6 — add and remove show the API's own sentence

The Add form validates nothing. The rules are `POST /api/hosts`' (ADR-0073) — the name's alphabet, the reserved
`local`, the URL's scheme, host and port, and whether the machine answers as a Holosmith — and `hostFailure(code,
message)` prints the envelope's `message` **verbatim** for exactly those three codes (`bad_host_name`,
`bad_host_url`, `not_holosmith`), keeping the code in front of every other, because a code is a word to act on only
where it names something the operator cannot see in the form. That is `createFailure`'s rule for a project's
refusals, one screen over, and it is HS-072's rule too: the CLI and the form showing two sentences for one refusal is
the fork this decision exists to prevent.

Removal is a request to **this** machine's registry and never a forwarded one — a registry is each machine's own,
and the hub's route refuses to relay `hosts/…` (ADR-0074). `409 host_busy` shows its message *and* offers one forced
retry (`?force=true`), which is `deleteSection`'s `unsaved_work` arm one registry out: the API's sentence names the
sessions that are still live, and the button is the operator saying they meant it.

The screen redraws from the server after an add or a remove rather than patching a card from a guess: the probe's
verdict is an answer the server already has, and the page holds no state (ADR-0017).

## D7 — a hub-only machine offers no new-session form, from `role`

`/api/health`'s `role` is the whole of it: `createOffered(role)` is `role !== "hub"`. A hub runs no sessions and
refuses `POST /api/sessions` with `403 sessions_disabled` (ADR-0074), so a form there is a form whose only answer is
that refusal — no new field, no probe, and no client-side guess about Docker. The create still posts to this machine
only; creating a session **on a peer** is HS-074's, and inventing it here would put a second machine's name in a form
whose validation story belongs to that story.

## D8 — what the gate can measure, and what only the demo can

`case_computers` is two boots on one box (`case_forward`'s shape, and its reason: a row has to spell the port before
either machine binds) and it measures, in order: the hub's own `role`, its empty list and its refused create; the
registry's two rows; the peer's health **through the hop** (role, version, uptime); the peer's sessions through the
hop, before and after a session is created on it (so the fold has a row to count); a machine that is not there,
answering `502 upstream_unreachable` with a sentence naming the machine and its origin, for both of the reads a card
makes; the two registration refusals with their codes and their sentences; an add and a remove from the registry;
that `/computers` is this tree's `web/dist/index.html` byte for byte; and that the **served bundle** carries the
screen's seven ids, the three classes its grouping and its cards are built from, and the seven sentences a
decision fixed. One arm measures the fact behind D3's second paragraph: the **peer** — a `server`, seeded with a row
naming the hub — is asked for that row's sessions through its own listener and answers `404 not_found`, which is the
sentence the page would otherwise have drawn as a group.

What it cannot measure is the DOM. A needle in a bundle is not a rendered element — the weak half is why each of
those sentences is a `transcript.ts` function with its own test in `web/test/transcript.test.ts`, and why the demo
watches the page draw. The three refusals are measured where they live: `not_holosmith` is `case_hosts`' against a
live TLS listener and `tests/hosts.rs`' otherwise, and `byte for byte` of a forwarded row is `case_forward`'s.

`case_pwa` loses one needle in the same commit: `id="nav-link"` became `nav-link`, because three screens mean two
links in the header and an id two elements share is not an id (`navLinks(pathname)` is the rule, and `SCREENS` is the
list a fourth screen joins instead of a branch in `main.ts`).

## D9 — what carries forward

`GLOSSARY.md` gains **Computers screen** and amends **Sessions screen** (grouped by machine) and **Session screen**
(its path can name another machine); **Local** already carried the reserved name these paths use. HS-072 must reuse
these registration sentences rather than spell its own; HS-074 owns the host-aware notification deep link this
address shape makes possible; and "a machine that forwards nothing offers no remote preview" is recorded here rather
than fixed, because a `server`-role machine is not a front door and making one into one is a topology decision no
story owns.
