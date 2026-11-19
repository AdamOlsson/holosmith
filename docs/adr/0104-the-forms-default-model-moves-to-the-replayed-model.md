# The form's default model moves to the model the smoke recording replays

ADR-0093 D4 made the new-session form start at `<default provider>/<model>`, and the model it named was
`claude-haiku-5-5` — the tail of the pair the credential path was measured with before the `opencode-go` row could
serve an OpenAI shape (ADR-0050). That tail now predates the row's second shape: ADR-0097 added an
`openai-completions` endpoint to `opencode-go`, and this build's own recording and create tests moved to
`deepseek-v4.1-flash` instead. The smoke command's fixture is a real turn recorded from that model, replayed
through a fake pi with no provider call, and `tests/sessions_api.rs`'s `KEYED_MODEL` — the model the keyed create tests
store — is `opencode-go/deepseek-v4.1-flash`.

What none of that is, is a measurement: ADR-0097 says the real-key turn for this model is **not measured on this
branch**, so its request-shape assertions and that recording are all the evidence there is, and the by-hand
V3-style run with `deepseek-v4.1-flash` in place of `claude-haiku-5-5` is what a promotion still owes. This ADR
follows the recording; it does not claim the provider answered.

So the field a fresh machine starts on named the one model on that provider this build no longer exercises, and the
model it does exercise was nowhere in the form. The decision is that the default arm now names
`deepseek-v4.1-flash`.

## The provider rule is untouched; only the tail splits

`modelPrefill` already had to decide which provider the machine holds a key for. The model tail is a second
question, and the two now answer separately:

```ts
export const DEFAULT_MODEL = "deepseek-v4.1-flash";
export const FALLBACK_MODEL = "claude-haiku-5-5";

export function modelPrefill(keys: KeyFacts): string {
  const provider = keys.providers.includes(keys.default_provider)
    ? keys.default_provider
    : (keys.providers[0] ?? keys.default_provider);
  const model = provider === keys.default_provider ? DEFAULT_MODEL : FALLBACK_MODEL;
  return `${provider}/${model}`;
}
```

- **The default arm means the machine's default provider.** That is `opencode-go` on every machine of this release —
  the provider table's first row, which no config key moves — so the arm's model is `deepseek-v4.1-flash`, and the
  tail is the one the smoke recording captures and the keyed create tests store. `DEFAULT_MODEL` is coupled to that first
  row: it is a tail of `opencode-go`'s catalog, so a release that made another provider the first row would have to
  choose that row's tail with it.
- **The fallback arm means another provider, and the form does not know its catalog.** `FALLBACK_MODEL` therefore
  stays the tail the credential path measured: a machine whose only key is, say, `anthropic` gets
  `anthropic/claude-haiku-5-5`, which remains an **example the operator edits**. Handing that machine
  `anthropic/deepseek-v4.1-flash` would name a model the provider may not serve, which is worse than an example that
  at least ran once.
- **The no-key arm keeps the default provider and therefore the default model,** so the field and the `key set`
  sentence beside it still name the same pair.
- **A peer that has not answered gets the same default arm.** `targetKeyFacts` hands the form this build's own
  `default_provider` while a peer's answer is still out (the test pins it with a first row of `groq`), so the prefill
  can name a provider that peer holds no key for, and Start before the answer arrives is
  `422 model_provider_unserved`. ADR-0093 D4's target rule already argues that arm and ADR-0097's create refusal is
  its consequence; neither changes here.

The value is still qualified (`<provider>/<model>`), for ADR-0093's reason: a bare id is resolved across every
provider that has a key, and pi answers several candidates with `status: "ambiguous"`.

## Why this is a web change and nothing else

There is no server-side default model: `POST /api/sessions` requires `model`, and the form always sends one. The
form is the only place a starting value exists, so the change is two constants and one ternary in
`web/src/transcript.ts`. No config key, no migration, no API shape and no container change: the model was already in
the catalog the proxy dispatches, and ADR-0097's shape table is what makes it reachable at all.

## What it is not

This does **not** make the default model configurable per machine. A machine still starts on this build's choice; a
`default_model` config key, or a per-machine setting on the create form, is a larger decision than the one here and
is left open.

## What pins it

- `web/test/transcript.test.ts` pins the four provider arms and both tails **literally** — the default arm's
  `opencode-go/deepseek-v4.1-flash` and the fallback arm's `anthropic/claude-haiku-5-5` — and the constants
  themselves, because the earlier assertions interpolated the one constant and would pass even if it named a model
  nothing in this build replays.
- `crates/holosmith/fixtures/smoke-turn.jsonl` is the recording `holosmith smoke` replays, and its `model` is
  `deepseek-v4.1-flash` on the `openai-completions` API shape. The replay makes no provider call, so it pins the
  string, not a served model.
- `tests/sessions_api.rs`'s `KEYED_MODEL` is the same pair, stored by the create tests that need a keyed provider. That create's refusal is
  about the key store rather than the model, so these tests exercise the string the form now starts on, not a
  provider that serves it.

The recording and the create tests are the reason the string moved, not the thing being changed: the form is
brought to the model this build already replays, never the other way round.

## Rollback

Changing the string back is the rollback, and it is uneventful: the model is a field's starting value and is written
into a session only when an operator presses Start. Sessions already created keep the model they were created with
(the row stores it), so a rollback does not touch a running or resumable session and rewrites no transcript.
