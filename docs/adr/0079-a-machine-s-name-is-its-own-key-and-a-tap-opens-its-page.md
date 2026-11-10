# A machine's name is its own config key, and a tap opens that machine's page

HS-074 is spec item 11's last bullet and two sentences of the **Roles** section ("a hub-only machine has no Docker and
no sessions… the hub is the only thing the operator opens"), and its five acceptance bullets. Four values were
missing, and every one of them is a *name*: what a machine calls itself in a payload, which machine a create names,
which machine a subscription belongs to, and what a page says when its machine is away. Nothing about the transport
changed — `/api/hosts/<name>/{*rest}` already carries every route this needs, five registrations deep (HS-069).

Six decisions carry it, and two of them are decisions about *not* adding something: no `machine` key on a create's
body, and no `sessions.host` column.

## D1 — `payload.host` is the machine's own `machine_name`, a config key that defaults to `local`

`push::Payload` has carried `{host, session_id, kind}` since HS-066 with `host` a **constant** (`preview::HOST`, the
literal `local`). It becomes `config.machine_name`, validated by `hosts::machine_name_problem` — the registry's own
alphabet (`[a-z0-9-]`, 63 octets, no leading or trailing `-`) with one exception: `local` is allowed, because it is
the reserved word that means *the machine whose page this is* (GLOSSARY's **Local**) and it is what a single-machine
deployment needs.

Why a key and not something derived:

- **the machine's `control_origin` host is self-known but is not the name the tap needs.** The path a notification
  opens is `/hosts/<host>/sessions/<id>` on the **hub's** origin, and `<host>` is a **registry row name** — which the
  hub's `hosts.toml` chose. A MagicDNS name would be a second guess at the same string.
- **learning the hub's name at registration costs a new channel** (a hub→peer write, and a name the peer keeps but
  cannot verify) for a string the operator types once in two files they already own.
- **keeping `local` everywhere and resolving it in the hub cannot work at all.** The notification is drawn by a
  service worker on the hub's origin, which has no page and no machine.

So the contract is the operator's: `machine_name` on the machine, the same string as the row in the hub's
`hosts.toml`. A mismatch is visible, not silent — the tap opens `/hosts/<name>/sessions/<id>` and the hub's forwarding
route answers its own `404 unknown_host` (ADR-0074's decision 3), which the session page draws as a sentence.

The value is also the **seventh `/api/health` key**, for HS-067's sixth key's reason, and it has the reader that
promise needs: the page's `Health` declares it and `transcript.ts`'s `ownLabel` labels **this machine's own row** (on
the settings screen and as the create form's first option) with the name its notifications carry — so an operator can
check the one string the tap depends on, and a mismatch with the hub's row is visible rather than only audible as a
`404`. A hub reads a peer's claim through the hop with the same key. `null` is not one of its arms: a machine always
has a name, and `local` is the one with no name of its own (which is why `ownLabel` says `local · this machine`).

## D2 — a create names its machine **in the URL**, and the hub's own create stays refused

`POST /api/hosts/<name>/sessions` is HS-069's forwarding route reaching the peer's own `POST /api/sessions`, so:

- the answer is the peer's own row (its id, its `status`, its worktree — facts a hub cannot invent);
- the hub's `POST /api/sessions` keeps answering `403 sessions_disabled` (ADR-0074 D3: the *path* names the owner);
- there is no `machine` key on a create's body and no second route, so nothing on the hub pretends to be a proxy.

The picker offers this machine first (when `createOffered(role)` — a `hub` runs no sessions of its own) and then every
registry row whose last probe said `reachable`. An unreachable machine is **not** offered: the create would be the
hop's own refusal, and `createOffered`'s rule (a control whose only answer is a refusal is not an affordance) is the
same rule one screen over. A hub with nothing to offer says why instead of drawing a form.

## D3 — the **page** subscribes, one call per machine, with that machine's key

Item 11's last bullet is explicit: "each machine has its own keys, so the web app subscribes on every machine it uses
(through the hub)". `PushManager.subscribe({applicationServerKey})` is per-origin **and per-key**, so N machines are N
calls and N `push_subscriptions` rows — each sealed to the pair of the machine that will sign for it:

```
navigator.serviceWorker.ready → registration.pushManager.getSubscription({applicationServerKey: <that machine's>})
                              → registration.pushManager.subscribe({applicationServerKey: <that machine's>})
                              → POST /api/hosts/<name>/push/subscribe   (or /api/push/subscribe for `local`)
```

The key is read through the hop — `GET /api/hosts/<name>/health`'s `application_server_key` (HS-067) — so the settings
screen's row per machine is drawn from facts that machine answered. The worker keeps its three events and carries
the host in the notification's `data` — `push.ts`'s `notificationData`, so the two fields are written and read by one
function — because `notificationclick` runs on the hub's origin and has no page to ask which machine an id belongs to.
That last sentence is the story's one P0: the first draft carried the host in the *title* and left `data` with the id
alone, so every tap opened the hub's own `/sessions/<id>`.

Three consequences worth naming:

- **`pushsubscriptionchange` in the worker can only speak for the page's own origin.** A rotated subscription for a
  *peer's* key is not something the worker can attribute (it has no machine list and no key that names one), so it
  keeps its one attempt for the origin and the settings screen is where a rotated peer subscription shows up — as
  `not subscribed` on that row, which is the truth and the one control that fixes it.
- **`getSubscription()` with a key is the browser's own answer**, and a browser that ignores the option answers the
  *default* subscription; the page takes the safe half (`null` → `not subscribed`) rather than claiming a subscription
  it cannot attribute (HS-071's `false` rule, in a second screen).
- **an unreachable machine is a third state, not a `false`.** Its key and its stored row are both things only it can
  answer, so the row is its sentence and no control — and a row that has not been read yet says its name and
  `reading…` rather than `not subscribed` (`machineSubscription` returns `null` for "nothing read").

## D4 — the delete loops **every** machine, and `404` is the answer the operator wanted

One `DELETE` per machine, against that machine's own route with **that machine's** endpoint (each subscribe mints a
different one, from `push.ts`'s `subscriptionRoute`). `404` is success — the row is already gone, which is the rule the
local unsubscribe has kept since HS-067 — and anything else is that row's own sentence through `awaySentence`: the
**message alone** for the two upstream codes (`502`/`504`), because `upstream_unreachable: workshop could not be
reached` reads as a word to look up about a machine that is simply away, and `hostFailure`'s rule otherwise
(ADR-0076 D6 — which drops the code only for the three registration codes, so the two screens that show an away
machine use the second rule rather than citing the first, as this ADR's first draft wrongly did). A machine that is
unreachable is therefore reported per row while the machines that answered are still cleared.

## D5 — an away machine degrades **per session**, and nothing is invented locally

Two shapes, one rule (ADR-0017: every value on screen came from a response or a frame):

- **the page's first read refused** with `502 upstream_unreachable`/`504 upstream_timeout` (`transcript.ts`'s
  `isAwayCode`): the session's screen keeps the way back and the machine's name, says the machine's sentence through
  `awaySentence` (no code), and offers *Try again*. The four composer actions, the delete affordance and the Changes sheet are **not drawn**: every
  one of them is a request to that same machine.
- **the machine leaving under an open page**: the stream's own connection failure is not a frame (`EventSource`
  dispatches a plain `Event` named `error`, which the transcript's loop already ignores), so one line above the
  transcript says the machine stopped answering and that what is below is the last thing it sent. The next frame it
  receives takes the line away.

No cached transcript, no `—` counts, no false `idle`. The list's own half of this acceptance was already built
(HS-071's one group per machine, `COUNTS_UNKNOWN`'s `—`, `hostFailure`'s sentence); this is the half a *session's*
page was missing.

## D6 — the gate measures it with two boots and a receiver

`case_cross_machine`: a peer (`role = "server"`, `machine_name = "workshop"`, a budget of one token so its own stub
turn trips `budget_exceeded`) and a hub (`role = "hub"`, a `hosts.toml` naming that peer `workshop`), on one box, plus
`case_push`'s `node:https` receiver standing in for the phone with **two** endpoints — one per machine, because each
pair mints its own — the case sends one browser key to both, because what it measures is the store and the wire, not
the browser's key schedule. What it asserts is the acceptance item by item: the peer's own name and pair read through the hop;
one `push_subscriptions` row in the **peer's** `app.db` and the hub's own in the hub's (and not each other's); a
forwarded create whose row the peer's own route answers; a payload that arrives as
`workshop|<the hub's session id>|budget_exceeded`; the peer stopped ⇒ `502 upstream_unreachable` naming it; the hub's
own create still `403 sessions_disabled`; and a delete that empties the peer's table.

The path rule itself is `web/test/push.test.ts`'s — `notificationPath(host, id)` compared against `transcript.ts`'s
`sessionHref(host, id)` over a table of hosts, because `push.ts` cannot import `transcript.ts` (node's ESM resolver
wants an extension on every hop, HS-071's landmine). Two spellings, one test that makes the second one safe; the
subscribe route is the same shape (`subscriptionRoute` against `apiPath(host, "push/subscribe")`), and the app half of
bullet 1 — that the page passes *that machine's* key to *that machine's* route — is `case_pwa`'s two bundle needles
(`applicationServerKey`, `push/subscribe`) plus that pairing rule. What no gate can see is the browser deciding: the
settings rows and the picker are `main.ts`, and no gate may need a browser.

## What this does not measure

No real phone, no push service, no iOS, and no second machine: the receiver is the browser, both machines are boots on
one box, and `SSL_CERT_FILE` stands in for a public CA. A machine whose `machine_name` disagrees with the hub's row
name is a misconfiguration with a visible symptom and no boot-time check — recorded as a follow-up in the story rather
than guessed at startup.
