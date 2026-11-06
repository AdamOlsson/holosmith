# The list object, one stream per visible session, and where the form's default comes from

HS-003 shipped an app shell that renders the health payload and nothing else. HS-013 is the story
that makes the product usable at all: the operator has to see the sessions the machine holds and
start one from a phone. The spec settles what the screen shows (item 2's Internet-access checkbox,
item 8's states, item 15's retention) but not the list's body, not where the checkbox's default
comes from, not how many streams a page opens, and not what a page does with a retry. Each of those
is cheap now and hard to change once HS-014 reuses it, so this records the choices.

## The list is one object, and the machine setting rides on it

`GET /api/sessions` answers `{"sessions":[…],"network_default":false}`.

An object rather than a bare array, for two reasons. A bare array cannot grow a field without
breaking every client, and HS-061 adds paging metadata to this answer. And `{"sessions":…}` cannot
be mistaken for ADR-0008's `{"code","message"}`: a client that reads `response.json()` can tell
which of the two it got without looking at the status first.

Each element is the `Session` row — its fifteen columns, in column order, serialised by the row's
own `Serialize` impl — and not a projection built for the screen. A projection is a second field set
that can drift from the columns, and the create's answer is already the same row plus `events`.
`tests/session_list.rs` pins both halves of that: fifteen keys, and no `events` on a list element. A
client therefore decodes one thing whether a session arrived from the list or from a 202.

`network_default` rides here rather than on `/api/health` because the health payload is pinned at
four keys (ADR-0009's banner rule and HS-003's payload test; **amended by ADR-0051**, which makes it five by adding
`preview_origin`, the one setting a screen needs on a phone and the only vehicle that costs no new request) and because the screen needs both
answers to draw itself. One round trip, no new endpoint. The alternative — a `GET /api/config` that
the browser could read a single boolean out of — hands the browser the machine's whole configuration
and owns a disclosure question far larger than the checkbox needs; if a later story wants more than
one setting on the screen, that argument belongs in that story's ADR, not here.

The order is the repository's `list(None)` order, `last_active_at DESC, id ASC`. Neither the
handler nor the page sorts: two sorts are one that can disagree with a future paging cursor. The
repository's state filter has no caller yet and nothing here invents one; HS-061 owns paging, and
with it any filter or truncation this answer later carries.

The handler takes the database lock in one synchronous block, calls `list(None)`, and maps a failure
through `internal("list", …)`, so a 500's message stays a fixed sentence rather than a rusqlite
message (ADR-0008). It is registered with `Routes::get`, which is what records `ReadOnly` in the
manifest and puts the route behind the guards with nothing added (ADR-0010, ADR-0007).

## One `EventSource` per visible live session

One stream exists — `GET /api/sessions/{id}/events` (ADR-0014) — so a page with *n* live sessions
opens *n* sources and none for a session that is not live. The cap bounds *n*: `max_live_sessions`
is 3 by default, and `stopped`, `error` and `deleting` are the states the cap does not count
(glossary, *Live session*). Two alternatives were considered and not taken:

- **One multiplexed stream over every session.** It is a new endpoint with its own replay rule
  (where does a new subscriber start?), its own resume rule, and its own lag report, and no story
  asks for it. ADR-0014's per-session stream already answers "what happened to this session".
- **Polling the list.** A second way to learn a state, disagreeing with the stream whenever both
  are in flight, and it turns a phone's screen into a load source. The stream is the one path.

The page's frame rule follows from the stream's own rule. A stream replays history when the client
has no last-seen seq (ADR-0014) — the whole log here, and since ADR-0033 the newest `history_window`
messages plus a `history.truncated` frame — so the first frames a page sees are history and the last
frame is the truth: frames are applied **in arrival order** and a later one overwrites an earlier
one. A page cannot send that header itself — `EventSource` resends the last SSE `id` it saw, which is
the seq — so the reconnect rule is the browser's, not the page's. The page never has to know where
the replay ended, and it holds no seq.

Two frame shapes, one handler:

- `from === to` is a provisioning **step report** (ADR-0016): the row's step line shows `worktree…`,
  `network…`, `container…` or `pi…`, and nothing else changes. That is how item 8's "provisioning
  steps are shown while `starting`" is met without a second event kind.
- `from !== to` is a transition: the state text becomes `to` plus its reason when there is one, the
  step line is cleared, and the row's source is closed when `to` is `stopped`, `error` or
  `deleting`.

Exactly one source per live row, and none for a dead one: the same function that opens a row's
stream closes it, and it runs both when a row is drawn and after every frame, so a session that
returns to a live state (HS-018's resume) gets a source again. `beforeunload` closes all of them,
because a source the page forgets keeps a registration alive on the server until it notices — and
HS-066 reads that count to decide whether to push.

The stream URL is derived from the id. The create's answer carries `events` and a list element does
not, but both spell `/api/sessions/{id}/events`, and deriving it is what lets one rule open a stream
whatever a row arrived from.

## The page holds nothing but the DOM

Every value the screen shows is a text node written from a response or a frame: a list element is
rendered from the `Session` it came with and patched from frames, and there is no array of sessions,
no map of states and no reconcile step after a reconnect. That is the whole rule, and its
consequence is what makes it worth having: a page that kept its own copy would have to decide, after
every reconnect, whose value is newer — its copy's or the replayed stream's. There is nothing to
decide when there is no second copy.

The one value the page keeps that is not on screen is the pending **Idempotency-Key**, in the form's
closure. It is recorded here so the rule is not read as "no state at all": a key is not a view
value, nothing renders it, and it has to outlive a refusal, which a view does not.

Every mutation goes through `document.createElement` and `textContent`; no `innerHTML` is handed a
payload. The row carries operator-chosen text (project, model) and later stories add agent text, so
the renderer is where the injection class is excluded rather than where it is reviewed.

The relative time ("3 min ago") is computed once, from `last_active_at`, when the row is drawn. No
frame carries a time, so a tick would only make the label look fresher than the answer behind it.
HS-014 rebuilds this row for the session screen and owns the question then.

## The retry keeps its key; the next create does not

`crypto.randomUUID()` makes the key on submit, and HS-008's rule decides what happens to it:

- **A 202 drops the key.** The next press is a new intent, and answering it with the old session
  would be exactly the silent collision an idempotency key is supposed to prevent. A key is a
  request's identity, not a session's.
- **A refusal keeps the key.** `!response.ok` renders the envelope's `code` and `message` in the
  form's error line — `cap_reached` and `bad_project` are the two an operator meets — and the next
  press sends the same key, so a create that was accepted but whose answer was lost is answered
  with the session it made rather than refused by the cap. A `fetch` that never answered keeps it
  for the same reason.

The 202's row is drawn as `starting` **before** its stream says anything. The answer is the row, and
a page that waited for the first frame would show nothing while a container step runs.

## A stopped session's days remaining is a placeholder

The story says "wired to HS-064; placeholder until then". The row renders an em dash with a `title`
naming HS-064, never a number: a fabricated "7 days" that HS-064 later changes is a figure the
operator will have planned around, and `stopped_at` is on the row, so the temptation to divide it is
real.

## Not decided here

- Paging and the history window (HS-061), which add fields to this list object and truncate the
  stream's replay.
- A global or multiplexed stream. HS-066's open-stream count is the nearest thing to one, and it is
  read on the server, not by a page.
- Eviction, idle stop and the retention number (HS-020, HS-021, HS-064), and the refresh tick that
  will make a long-open page's relative times and states current.
- What the page does when the list request itself fails: it renders the envelope and stops, exactly
  as the health path already did. A retry button is a decision for the story that adds error
  recovery, not for the first screen.
- `crypto.randomUUID` needs a secure context. The control listener is plain HTTP over the tailnet
  unless it is put behind TLS, and the story names this call. If a phone proves to be without it,
  the fallback is a story, not a silent second key generator here.

## Consequences

- `scripts/verify.sh` grows `case_sessions_list`: on the shipped artifact, `200`,
  `content-type: application/json`, the id `case_create_session` made, and a `network_default`
  field. 21 cases, 66 assertions.
- `crates/holosmith/tests/session_list.rs` is the socket-level proof: an empty machine, a created
  row with fifteen keys and `starting`, the config's `network_default` on the wire, `403 bad_host`
  from a foreign `Host` before the list is read, and `GET /api/sessions` recorded `ReadOnly` while
  `POST` on the same path stays mutating.
- The health payload keeps its four keys. A later story that needs a second machine setting on the
  screen extends the list object or argues for the config endpoint here. **Amended by ADR-0051:** it
  is five, and the fifth is `preview_origin` — the session screen's preview link needs the origin, and
  the page already fetches health before it draws, so the alternative (a `GET /api/config`) would own
  the disclosure question this paragraph declines to open.
- The screen stays one column, `max-width: 32rem`, with 44 px controls, and adds no dependency: the
  shell is still Vite and TypeScript, `EventSource`, `crypto.randomUUID` and `fetch` are the
  platform, and no framework, router or state library arrives with the first list.
- A list element is the whole `Session` row, so the list answers `client_key` as well. It is already
  in the create's 202 and behind the same guards, and HS-014 and HS-061 inherit it: an idempotency
  key is extracted material, not a credential, and hiding one copy would leave the other.
- The form's initial `project`, `model` and `thinking` are literals (`holosmith`,
  `claude-sonnet-4.5`, `high`), because no endpoint enumerates projects or models yet. HS-025 and
  HS-031 are where those lists exist, and the literals become their defaults rather than a second
  vocabulary.
- `crypto.randomUUID` exists only in a secure context, and the control listener is plain HTTP unless
  it sits behind `tailscale serve`. The page therefore generates the key with `randomUUID` when it
  can and with one `getRandomValues` fallback formatted as a v4 UUID when it cannot: the same
  CSPRNG, no second generator, and no dead form on the dev instance. A browser with neither shows
  the reason in the form's error line instead of doing nothing.
- HS-014 reuses the row's source for the transcript; HS-015 to HS-017 add the composer; HS-061 adds
  the paging fields this object was shaped to accept.
