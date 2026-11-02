# Browser-facing guards are one layer, and every refusal is a 403 envelope

HS-005 is the story that pays ADR-0002's promise. Item 4 of the spec names four checks — the `Host` allow-list, the control origin on non-`GET`, the JSON content type plus `X-Holosmith-Client` on non-`GET`, and the `Tailscale-User-Login` owner when one is configured — and one prohibition: no CORS headers, ever. ADR-0007 said the guards would attach inside the one `Router::layer` that `App::new` calls, so that a route registered later cannot skip them. This records what that layer does, in what order, and where a literal reading of item 4 would have produced something the operator could not use.

## Measured before deciding

Measured against the tree at `fe5ea6c` on the debug binary, with a temp config and a temp data root, over raw sockets; and by reading the vendored crates.

- **A request with no `Host` header reaches the router.** `GET / HTTP/1.1` with no header block answered `200`, and so did `GET / HTTP/1.0`. hyper 1.12's h1 parser has no `Host` requirement, so `bad_host` on a missing `Host` is a check we perform, not one the stack already performs for us.
- **Duplicate headers reach the router too.** `GET /api/nope` with `Host: a` and `Host: b` answered `404`; `POST /api/nope` with two `Origin` headers answered `404`. Nothing refuses them before the layer sees them, so "at most one of each checked header" is enforceable.
- **A guarded but unrouted `POST` already answers the envelope.** `POST /api/nope` with `Content-Length: 0` answered `404 {"code":"not_found","message":"no such endpoint"}`. The demo's third case — every header right, no route yet — needs no new route.
- **`OPTIONS /api/health` answers `405` with an `Allow` header today,** from axum's `MethodRouter`. Behind the guards it answers `403`: `OPTIONS` is not `GET`, so it is not exempt, and it carries no `Content-Type`. Neither answer carries a CORS header.
- **`http` 1.5.0's `HeaderMap` has no `retain`** — only `keys`, `insert`, `get_all` and `remove` — so "strip every `access-control-*` header" is collect-then-remove: gather the names from `keys()`, then `remove` each. `tower-http` is not a dependency and is not added for this.

## The guard reads a resolved value, not the config

`Guards` is built once by `Guards::from_config(&Config)` and passed to `App::new(routes, guards)`. It holds the allow-list already folded to lowercase with `localhost` and `127.0.0.1` added, the control origin already lowercase, and the owner. `AppState` keeps the whole `Config`, because handlers want it; the duplication is deliberate: the request path does no parsing, case folding happens once instead of per request, and a test can build guards for a machine it never configures. There is still one construction point — the single `App::new` call in `commands/serve.rs`.

The layer is one `middleware::from_fn`, and it keeps `edge`'s job. In order: count the request, run the checks, call the rest of the stack or answer the refusal, strip `access-control-*` from whichever response came back, and write the one `debug` line. A second layer is the tempting shape and is wrong twice over: applied outside `edge` it would answer before the counter ran, so a rejected request would not be counted, and applied inside it, ADR-0007's "`Router::layer` runs in exactly one place" would stop being true. A rejected request is counted like any other, which is what an operator reading the log expects.

## The order of the checks, and why the owner applies to a `GET`

`bad_host` (every non-exempt request) → `bad_origin` (non-`GET` only) → `bad_client` (non-`GET` only) → `bad_owner` (every non-exempt request, only when `owner` is configured). First failure wins and exactly one envelope is sent, because ADR-0008 fixes the body and a client should not have to read two.

The owner check comes last but is **not** `GET`-exempt. Item 4's sentence — "When behind `tailscale serve`, require the `Tailscale-User-Login` header to equal the configured owner" — is about who is calling, not about what the method does, and a proxied `GET` from the wrong tailnet user must be refused exactly like a `POST`. Ordering it last makes the failure the caller sees the one about their method's preconditions first, which is the more actionable of two true statements.

`is_guard_exempt` — shipped with the router in HS-003 and unchanged — is the only exemption list, and an exempt request skips **all four** checks rather than some. So `GET /api/health` answers even with a foreign `Host` and even from a tailnet peer who is not the owner. That is accepted: the payload is name, version, role and uptime, the operator's `systemd` and `curl` probes need it, and item 4's host-registration probe from a hub (HS-070) is exactly such a request. A test pins it so it stays a decision rather than becoming a discovery.

## The `Host` allow-list, spelled the way a browser sends it

The header value has an optional `:port` and a potentially bracketed IPv6 literal, and it is compared ASCII-case-insensitively after the port is dropped; `Config`'s own validator already spells an entry as a bare name or a bracketed literal, so the two spellings line up. Exactly two names are implicit — `localhost` and `127.0.0.1` — because `Config::default()` has an empty `allowed_hosts` and the default build must serve its own app shell out of the box (ADR-0006). `[::1]` is deliberately **not** implicit: it is a different address family reaching a different socket, and a machine that wants it can list it, which the validator accepts. A missing `Host` is `bad_host`: an unchecked request defeats the check it was meant to pass.

## No CORS header, ever, is enforced rather than reviewed

Every response leaves the layer with any `access-control-*` header removed — the guard's own 403s, a handler's 200, an asset, the app shell. The prohibition is what makes the client header meaningful (a cross-origin page cannot set `X-Holosmith-Client` on a `POST` without a preflight it cannot get), and a rule that lives only in review comments is a rule a later handler breaks by adding a perfectly reasonable-looking `Access-Control-Allow-Origin`. Stripping makes the prohibition structural: a handler that adds one has it removed, and a test proves both the removal and that nothing else leaks one. A future story that wants CORS must therefore delete the strip and say so, which is the point.

## A `GET` route cannot be mutating

Item 4's "GET endpoints are read-only and never change state" is not a property a test can observe from outside — an idempotent-looking `GET` can still write. So the route table classifies at registration: `get` and `head` record `ReadOnly`, `post`, `put`, `patch` and `delete` record `Mutating`, and the manifest is reachable from a test through a doc-hidden accessor, the way `App::counter` already is. `Routes::route` is deleted rather than kept with a kind, so there is one vocabulary and no shape that would have to fail its own test to be used. The manifest is evidence, not enforcement — the types are the enforcement: each helper declares its own kind rather than deriving it from the method, so a `GET` recorded as `Mutating` would be a visible edit in one helper, not something the method implies, and the tests assert both that no `GET`/`HEAD` entry is `Mutating` and that a `Mutating` entry is recorded at all, so the pair cannot pass vacuously. What the types enforce is the label, not the behaviour: a handler registered with `.get()` that writes still passes `no_get_route_is_mutating`, so a mutating `GET` is prevented by the registration vocabulary plus review of the handler body, and that test is a tripwire for a future helper that mislabels a method, not a proof that no handler writes.

`RouteKind` and `ManifestEntry` are public but deliberately not in `GLOSSARY.md`: the glossary defines the system's domain language, and the route manifest is test evidence rather than a domain concept — the same treatment `App`, `Routes` and `RequestCounter` already get.

## Consequences

- `App::new` takes a second argument. `commands/serve.rs` is the only call site in the crate; `tests/http_contract.rs` keeps its `Served::start(routes, grace)` helper, which builds default guards, so its assertions do not change.
- `Routes` gains six method helpers and loses `route`. Four registrations move to the new spelling — the health route, `app.rs`'s own unit test, and the `/slow` and `/hang` fixtures in `tests/http_contract.rs` — and no assertion moves with them.
- `OPTIONS /api/health` changes from `405` plus `Allow` to `403 bad_client`, which still carries the route's `Allow` header, attached by axum outside this layer. Removing that `Allow` is not part of this change; it would need the second layer ADR-0007 forbids. Neither answer carries a CORS header, so a preflight reaching the control listener is answered by the guard, never by a route.
- An absent `Tailscale-User-Login` is accepted even when `owner` is configured: the server cannot tell whether a request arrived through `tailscale serve` or directly, so a request that never traversed the proxy skips the owner check. The `Host` allow-list is what bounds that direct access.
- `GET /api/health` is exempt from all four checks, including the owner check, so a tailnet peer who is not the owner can read the name, version, role and uptime. Accepted for the reasons above.
- A handler that adds a CORS header loses it silently. That is deliberate and is the cost of making "ever" unconditional.
- The 403 codes are `bad_host`, `bad_origin`, `bad_client` and `bad_owner`, from the one module of constants ADR-0008 created. The messages are one sentence each and no test pins their text.
- Guard assertions live in `crates/holosmith/tests/guards.rs` and are **not** added to `scripts/check-static.sh`; HS-006's `scripts/verify.sh` is where they are lifted into the one-command check.
- The preview listener's own Origin check (item 3), the `allowed_hubs` plus whois check for machine-to-machine calls (item 4's last line), rate limiting and authentication are not part of this layer and are not implied by it.
