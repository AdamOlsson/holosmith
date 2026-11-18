# One provider row dispatches on the endpoint pi appends

A machine whose only provider key is `opencode-go` answered `404 status code (no body)` for every
`openai-completions` model. The session was `opencode-go/deepseek-v4.1-flash`, its `models.json` carried
`http://key-proxy:8080/opencode-go`, pi appended `/chat/completions`, and the key proxy forwarded
`https://opencode.ai/inference/go/anthropic/chat/completions` — the anthropic base path with an OpenAI endpoint
under it. opencode.ai serves the OpenAI shape at `/inference/go/openai/v1` and reads the key from
`Authorization: Bearer`, so two of the three values in the forwarded request were wrong.

ADR-0050 saw this coming: "pi appends `/v1/messages` or `/chat/completions` to the `baseUrl` it is given, so a
single row can serve exactly one of them", and it left the OpenAI shape to a second row named
`opencode-go-openai`. That row is not what a machine needs, and this record says what replaced it.

## The shape is a property of the model, and pi is the only component that knows it

pi's bundled catalog carries the shape per model, and it carries the URL with it: an `openai-completions`
`opencode-go` model has `baseUrl: https://opencode.ai/inference/go/openai/v1` and an `anthropic-messages` one has
`https://opencode.ai/inference/go/anthropic`. Each provider client appends its own endpoint to the base URL it is
handed — the OpenAI client posts `/chat/completions`, the Anthropic client `/v1/messages` — and this build hands
every session's pi the **proxy's** base URL instead of the provider's.

So the endpoint in the request path is pi telling the proxy which API shape the model it selected speaks. Reading
it is not a second opinion about the model; outside pi there is no opinion at all, because the model list, the
context windows, the costs and the reasoning levels all live in pi's catalog and none of them exist in this
build's tree.

## D1 — one row, and a small shape table on it

`key::Provider` gains `shapes: &'static [Shape]`, empty for every row but `opencode-go`, where a `Shape` is one
endpoint pi appends, the base path that provider publishes it under, and the header its client sends the key in.
`Provider::shape_for(endpoint)` selects one by exact endpoint match (or the endpoint and then further segments,
longest endpoint first). `opencode-go` names two:

| endpoint pi appends | opencode.ai's base path for it | header |
| --- | --- | --- |
| `/v1/messages` | `/inference/go/anthropic` | `x-api-key` |
| `/chat/completions` | `/inference/go/openai/v1` | `Authorization: Bearer` |

`/v1/chat/completions` is not in that table because pi cannot produce it: this build writes the proxy's base URL,
which has nothing after the provider id, and the OpenAI client appends only `/chat/completions`. A shape names
endpoints that are reachable, and the table stays exact.

The row's own (host, `base_path`, `auth`) triple stays the anthropic one, so an endpoint that matches no shape is
forwarded exactly as it was before shapes existed, and every row without shapes routes exactly as it always has.
`keyproxy::Route` carries the chosen `auth` beside the forwarded `path`, and `upstream` writes the real key in that
header instead of the row's.

## What was refused

- **A second provider id** (`opencode-go-openai`). pi resolves a model from *its* catalog keyed by provider, so a
  provider id pi does not carry has no models: the catalog metadata — context window, cost, `compat`,
  `thinkingLevelMap` — would have to be copied into every session's `models.json` to make the id resolve anything.
  Worse, the operator would have to know which of two ids a given model speaks, which is the one thing the catalog
  already knows better than they do. `key set` would gain a provider that is not a provider.
- **A request-controlled host or path.** ADR-0050 refused that and nothing here reopens it: the host is the row's,
  both base paths and both endpoints are compile-time constants in `key.rs`, `..` is still refused after the prefix
  is chosen, and the set of URLs reachable with one key is still the two API roots opencode.ai publishes for it —
  the exact set two static rows would have allowed. A client cannot name a third path: an endpoint that matches no
  shape gets the row's own base path, and the host never moves.

## A create says so before the session exists

The session in the measurement was created and then failed every turn from inside a container. So `POST
/api/sessions` now refuses a `model` whose first `/`-separated segment names a provider this machine cannot serve:
a segment this build does not forward to, or one the key store holds no key for. One code,
`422 provider_without_key`, and a sentence per fact — the provider table and `key set` for an unknown segment,
`holosmith key set <provider>` for a missing key — because the client's move is one and the operator's next step
is the sentence's.

A **bare** model id names no provider and is allowed exactly as before: pi's catalog is the only list of model ids
this build has, so a machine cannot call a bare id wrong, and `claude-sonnet-4.5` is what the quick start, the
form's example and every check in `scripts/verify.sh` sends.

The refusal rides `create`'s chain of the machine's own refusals, which `create_locked` raises **after** the
idempotency key's lookup and **before** the cap's count. Both halves are load-bearing: a retry whose first attempt
made a row still answers that row even if the key has gone since, and a refused create cannot evict an idle session
on its way to saying no.

## What this does not cover

pi's catalog lists two further shapes for this provider: `google-generative-ai` at
`https://opencode.ai/inference/go/google/v1beta` and `openai-responses` at `/inference/go/openai/v1` with
`/responses` appended. A model of either still gets the row's anthropic default and is not forwarded correctly.
They are shapes on this row like the other two — no new mechanism — and they are not added here because the
report this fixes is the `openai-completions` one and each deserves its own measurement against a real key
rather than a guess.

## What pins it

`key::tests::opencode_go_names_one_shape_per_endpoint_and_every_other_row_names_none` pins the table: two shapes
with those exact paths and headers, the anthropic triple as the row's own, an endpoint matched exactly and not as a
prefix of a longer segment, and no other row carrying a shape.
`keyproxy::tests::an_open_code_go_route_takes_its_shape_from_the_endpoint_pi_appended` pins the forwarded request
line, host and header for both endpoints, `a_query_string_survives_the_shape_s_rewrite` the preserved query,
`a_row_without_shapes_keeps_its_own_path_and_header_whatever_the_endpoint` the rows that did not change, and
`a_dummy_key_never_survives_into_the_upstream_request` that the selected shape's header is the only one that carries
a key. `tests/sessions_api.rs` pins the create's refusal, its two sentences, the bare-id allowance, a retry
answering its row after the key went away, and the idle session the refusal must not evict. The by-hand proof is
ADR-0050's measurement redone: a machine whose only key is `opencode-go` prompts an `openai-completions` model and
reads an answer, and the proxy's log line names the request that reached it.
