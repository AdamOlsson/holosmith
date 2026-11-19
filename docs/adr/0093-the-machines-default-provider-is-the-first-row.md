# The machine's default provider is the table's first row

The credential path became `opencode-go`'s before anything an operator reads did: `key::PROVIDERS` gained the row
and V3/V4/V5 were measured against `opencode-go/claude-haiku-5-5` (ADR-0050). The first run still named
`anthropic` — the quick start's second command, the dependency table, `key set`'s help, and the first id in both
sentences that enumerate the table — so a machine whose only key is `opencode-go` was told to fetch one it does not
have, and the example that did work was nowhere in the docs. Three of those sentences are load-bearing, which is
what makes this a decision rather than an edit.

The sharper half is the seeded pi-home. It carried the row's model as a bare id, and a bare id is resolved across
every provider that has a key; pi's own resolver answers several candidates with `status: "ambiguous"` rather than
a model. So the operator typed an Anthropic-anchored name, pi guessed or refused, and nothing in the flow said
"you have an `opencode-go` key, that model is `opencode-go/…`". On a machine with **no** key at all `key::models`
writes `providers: {}`, pi has no model, and the first thing the operator meets is a session that cannot answer —
while the proxy's `503 no <provider> key is stored on this machine; the operator runs holosmith key set <provider>`
is unreachable, because a request is what produces it and there is no provider for pi to route one to.

## D1 — the table's first row is the default, and `key::default_provider()` is the only place that is a choice

`PROVIDERS` is a literal with rows in it, and its order is now load-bearing: the first row is the machine's default
provider. One function returns that id, so the four sentences that name it read one spelling rather than assuming
`PROVIDERS[0]` themselves. The rows are otherwise matched by id (`key::provider`), so nothing but those sentences
can tell the order changed.

`opencode-go` is the first row because it is the provider this build's credential path was measured with
(ADR-0050, V3/V4/V5): the ids a refusal lists are the ids a machine can actually use, and the one an operator is
told to store first should be the one repository evidence exists for. `anthropic` stays a row and stays in the
list, and a machine holding an Anthropic key keeps its key path, its seed and its sessions exactly as they were.
What changes for that machine is the form: its example is now a qualified id rather than a bare one, and the tail
of it is a model this build measured against `opencode-go` and not against `anthropic` — so the pair is one the
operator edits (D4).

Every operator-facing example therefore names it: the README's quick start (`key set opencode-go`), the
dependency table in `docs/requirements.md`, `key set`'s and `holosmith key remove`'s provider help, ADR-0048's
`keys.toml` sample, and the form's model example. Sentences that state the **rule** keep `<provider>`: a doc that
says `holosmith key set <provider>` is describing the command, not demonstrating one.

## D2 — a `provider/model` request is seeded as two settings

`agent::seed` splits the row's model at the **first** `/`: `opencode-go/claude-haiku-5-5` writes
`defaultProvider: "opencode-go"` beside `defaultModel: "claude-haiku-5-5"`. `defaultProvider` is pi's own settings
key (`docs/settings.md`, and `settings-schema.d.ts` in the pinned install), so the seeded file says what the launch
line's `--model` already said — the flag keeps the whole `provider/model` string, which is the shape the verified
turn used.

Three properties are deliberate:

- **The tail keeps any further `/`.** The split is at the first one because the tail is the provider's own model id.
- **A bare id writes the template's two keys and nothing else.** No `defaultProvider`, no refusal: the field is
  the operator's, and a model string with no `/` is the shape every earlier release seeded.
- **An empty half is not a provider.** `/x` and `x/` pass the create's model rule, and neither half of a split at
  an empty string is an id: they take the bare arm, so no session is seeded with `defaultProvider: ""`.

The template stays two keys. It is embedded once for every session, and a `defaultProvider` in it would be a
provider the row did not name — the third key is added per row, and only when the row's model names one.

## D3 — the machine's provider ids ride the session list, and only ids

The app needs two facts it cannot derive: which providers this machine holds a key for, and which id is the
default. They ride `GET /api/sessions`' answer, beside `network_default` and `stopped_ttl_days`, for that route's
own reason (ADR-0017): the home screen already makes this one read per machine, `/api/health`'s payload is pinned at
four keys (ADR-0009), and a hub forwards the list per host already. A second endpoint for two strings would be a
second round trip on the first screen.

**The ids only — never a value, a length, or a prefix of one.** The body is drawn by a browser and forwarded across
a hub from another machine, and a key is the one thing that may travel with neither. `providers` is in the table's
order, so "the first provider this machine has" is a stable answer rather than a name sort, and `default_provider`
is `key::default_provider()` so the page never spells that id.

The store is read on each answer rather than cached: `key set` runs between two list reads and must show, and a
cache would be a second source of truth for the file whose whole job is being the only one. A store the machine
**cannot read** is the request's own `500 internal` and not an empty `providers`, because an empty list means "run
`key set`" and that operator already has a key.

The collateral of that choice is deliberate and worth naming: the home screen reads this route to draw its list, so
a `keys.toml` that cannot be parsed takes the **whole Sessions screen** down to the envelope's one generic
sentence — no rows, no create form, no controls — where before this change the damage reached only the key proxy,
which `serve` treats as survivable (`proxies.rs`). The alternative is the lie above. What repairs it is the file
itself, and only by hand: it is the operator's, and `holosmith key set` cannot heal it, because `Store::set_provider`
loads before it writes (`key.rs`) and so refuses the same parse. Nothing in the product rewrites a store it could
not read, and no new error code names the case: the `500` is the route's answer and the file is where the operator
looks.

## D4 — the form starts at a provider, and says the command when there is none

The model field's starting value is `<provider>/<model>`, never a bare id: the machine's default provider when it
has a key for it, otherwise the first provider it does have, otherwise the default again. Two model-id constants,
not a table — nothing in this repository can keep a catalog current — and the default arm's tail is
`deepseek-v4.1-flash` (ADR-0104), while the fallback arm keeps `claude-haiku-5-5`, the tail of the pair this
build's credential path was measured with on the default provider. The default pair is the one this build's smoke
fixture replays and the keyed create tests store, though ADR-0097 still owes the real-key turn for it; the field's other
arm is that fallback tail qualified with whichever provider a non-default machine holds, which is an example **the
operator edits**, because nothing in this repository can know that provider's catalog. It is still the right
starting value: the alternative the issue names is a bare id, which pi's resolver answers with `ambiguous` when
several providers have a key.

"The machine" here is the machine the picker names, which is not always the one drawing the page. A create posts to
`POST /api/hosts/<host>/sessions`, so on a `hub` — where `createTargets` offers peers only — every fact this form
starts from is a peer's, and a prefill composed from the hub's own empty store would name a provider that peer may
hold no key for at all, which is the session that cannot answer this decision exists to remove. The facts are
therefore the selected target's, read from that machine's own list answer, and the model is re-derived when the
picker moves and again when the named peer's answer arrives — but only while the field still holds what the page
wrote, because a model the operator typed is theirs. The no-key sentence is drawn only when the target is this
machine: it names a store and a command on this machine, and a hub is precisely the machine the product never asks
to hold a key (`server deploy` skips `key set` there). A peer that has not answered yet contributes nothing but
`default_provider`, the table's first row, which every machine of one release answers identically.

A machine with no key gets the sentence beside the form, built from the default id the list carried: `no provider
key is stored on this machine; run holosmith key set opencode-go`. It is in the form's own area rather than the
error paragraph, because a refusal replaces that paragraph and this fact is true whether or not anyone presses the
button. The key proxy's `503` keeps its shape and becomes reachable for the first time: the default provider now
has a route for it to name.

**Amended by ADR-0104:** the default arm's tail is now `deepseek-v4.1-flash`, the model this build's smoke fixture
replays and the keyed create tests store, on the `openai-completions` shape ADR-0097 added; the non-default arm keeps
`claude-haiku-5-5` as the example the operator edits. Everything else in this section stands — the provider rule,
the target rule, the no-key sentence and the refusal shape are unchanged.

## What measures it

- `key::tests::the_first_row_is_the_default_provider_and_the_refusal_opens_with_it` pins the id, that it is the
  row the proxy forwards to, and that `key set bogus`'s sentence opens with it and still names the whole table;
  `tests/key_cli.rs` drives that refusal as the operator runs it.
- `agent`'s unit tests pin the three seed shapes: a bare id (the template's two keys, unchanged), a qualified id
  (`defaultProvider` plus the tail, one key more than the template), a tail holding a `/`, and a split with an
  empty half. `tests/publish.rs`'s two-key assertion is the bare case, which is why it still holds.
- `tests/session_list.rs` pins the answer's five keys on an empty machine, the ids of a machine with two keys in
  table order with neither value in the body, and a damaged key file answering `500 internal` rather than an empty
  list. The same file's configs name a data root of their own: a test left on the default one would read the
  machine running it.
- `web/test/transcript.test.ts` pins the prefill's four arms — the default with a key, the default over an earlier
  key, the first key when the default has none, and no key — and both tails literally: the default arm's
  `opencode-go/deepseek-v4.1-flash` and the fallback arm's `<provider>/claude-haiku-5-5` (ADR-0104). It also pins the
  no-key sentence built from the id it was given rather than a literal, and the target rule: this machine's own answer
  for `LOCAL_HOST`, a peer's own answer for a peer, the default provider's qualified example while a peer's answer is
  still out, and no sentence about a peer's store.

## Refused

- **A `default_provider` config key.** The table's order is a build-time fact, and a machine setting would make
  "which example does the doc name" depend on a file the reader does not have.
- **A `GET /api/providers` endpoint.** One more round trip on the first screen, on the hub as well as the machine,
  for two strings the list's answer already carries.
- **Prefilling a key's value, a length, or a prefix.** Not a display decision: a key that reaches a browser has
  left the machine, and the whole credential path exists to keep it from doing that.
- **Refusing a model string with no `/`.** The create's rule allows one byte set and no separator rule, and every
  session made before this change has a bare model: a refusal here would fail a resume.
- **Deriving the model from the provider's catalog.** pi has a bundled catalog (`get_available_models`, V4), but it
  answers inside a container, per provider, under `PI_OFFLINE=1`; the form needs one editable string before any
  container exists.
