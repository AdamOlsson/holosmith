# The preview listener is a byte tunnel to the container's own address

HS-041 is spec item 3, and item 3's V7 is the measurement that decides its shape: *"that the host can reach a
container's IP on an internal network (needed for the preview). Fallback: run the preview proxy as a container
on the session network."*

## V7 passed, so the fallback is not built

Measured on this machine's real daemon before the design was written:

```text
docker network create --internal hs_v7probe                     -> c50c5e070790…
docker run -d --rm --name v7stub --network hs_v7probe pi-agent:local node -e '…listen(8099)…'
docker inspect v7stub --format '{{range $k,$v := .NetworkSettings.Networks}}…'
                                                                -> hs_v7probe ip=172.18.0.2
host:  curl http://172.18.0.2:8099/      -> HTTP/1.1 200 OK, body "hello from 78f93835ee98/"
host:  python3 http.client(172.18.0.2, 8099) -> 200 b'hello from 78f93835ee98/\n'
container -> its own gateway 172.18.0.1:8099  -> ECONNREFUSED
```

The last line is ADR-0044's separate fact and the reason this measurement had to be made rather than assumed: an
internal network's own gateway *answers from inside it*, so "the container cannot reach out" says nothing about
"the host can reach in". Both are now measured. The host reaches in, so **the preview proxy is in-process** and
the spec's documented fallback (a proxy container on the session network, which would then need a published port
or a second network to be reachable at all) is deliberately absent.

One container demonstrably being reachable is not the same as "the shipped listener does it", so `case_preview`
re-measures the same fact through the artifact: it starts a container on `--internal` `agents_<id>`, asks the
**host** directly first, and then asks the preview listener on the same path.

## A third byte listener, not a second router

The preview listener is `src/preview.rs`: raw TCP, read a head → decide → connect → relay the rewritten head →
`pump_both` until both sides close. That makes it the third tunnel in this binary, after `egress` (HS-037) and
`keyproxy` (HS-039), and each of the three reasons is a fact about this repository rather than a preference:

* **Transparency is the whole feature.** A dev server emits chunked bodies, SSE and `101 Switching Protocols`. A
  tunnel forwards those bytes; a proxy built on an HTTP stack has to *decode* chunked framing to re-frame it, and
  the 101 it produces is its own rather than the upstream's. `case_preview`'s WebSocket round trip is a
  hand-written handshake and one frame each way for exactly this reason: nothing in the path understands them.
* **The alternative costs two crates.** `axum` 0.8 is built here without `ws`, and forwarding an exchange through
  axum would need hyper's `client` feature, which pulls `want` into `Cargo.lock`. `Cargo.lock` is committed and
  registry-only; ADR-0028, ADR-0045 and ADR-0048 all chose a subprocess or hand-written bytes over a dependency.
* **"No API routes" becomes a property of the construction.** The listener has no route table, so the acceptance
  cannot be broken by adding one; the only paths that exist are the ones `Route::strip` accepts, and everything
  else is a plain-text 404 (never ADR-0008's envelope — that is the control listener's contract).

The byte-moving half is `src/tunnel.rs`, extracted from `egress.rs` rather than copied: `read_head`, `pump`,
`pump_both`, each with its bounds as parameters so each listener keeps its own policy. The one thing that could
not be shared is the tracing target — `tracing`'s `target:` must be a literal — so `pump_both` takes a `Tunnel`
enum and attributes a broken connection to the right listener.

## The port is a config key, because nothing else knows it

`docker image inspect pi-agent:local --format '{{json .Config.ExposedPorts}}'` answers `null`: the agent image
declares no exposed port, so "read the port from the container" has nothing to read — and an agent left to pick its
own dev-server port would build something the listener never dials. The three candidates were a key, the image's
exposed ports, and a per-session column. The key won: the ports option is empty (measured), and a column is a
schema change for a value no row knows better than the operator.

The key is only half of it, so the same commit closes the other half: a session's container is told the port as
`HS_PREVIEW_PORT` (`container::PREVIEW_PORT_ENV`), next to `HS_SESSION_ID`. A number the machine pins and the
container is never told is a preview nobody can open, and which port an agent should serve on is the machine's
decision — it is the one the listener forwards to.

`preview_upstream_port` (default `3000`, refuses `0`) is **one key for every session**, because each container has
its own network namespace — two sessions may both listen on 3000 inside their own container without touching each
other. `KNOWN_KEYS` goes from 31 to 32, the golden file, `config show`'s re-validation and the banner's config
line all move in the same commit, as every config key does.

## What is forwarded, and what is not

Forwarded: the request line's target becomes `/<rest>` — the prefix stripped and the query kept — and the body is
relayed as it arrived, including bytes that came in the same read as the head (`split_head`). `Host` becomes the
container's address, because a dev server's own host allow-list judges that header and `localhost:8081` is not a
name the container has. A request that is not an upgrade is sent `Connection: close`: one client request is one
upstream connection, which is what makes "relay until the other side closes" a complete answer.

**Not rewritten, and therefore not promised:** the response's headers, HTML, absolute URLs, `Location`, cookies.
An app that serves its assets from `/` works. An app that hard-codes a base path or a redirect to the origin root
needs its own configuration — that is a dev server's `base` setting, not this listener's. Item 3 asks for
`/<host>/<id>/*` to be *served*, not for a dev build to be rewritten, and a rewriting proxy is a second product.

## What is refused, and what refuses nothing

| Request | Answer |
| --- | --- |
| `/api/health`, `/api/sessions`, `/` on the preview port | `404`, plain text, naming `/<host>/<id>/` |
| `/<other-host>/<id>/…` — another machine's | `404`; the host segment is a **value** `Route::strip` returns, so HS-069 forwards it rather than adding a parser |
| a second segment that is not a session id, or no row holds | `404` |
| the row exists but its container is not running, or nothing answers at its address | `502`, naming the address |
| an **upgrade** whose `Origin` is missing or is not `preview_origin` | `403`, naming both origins |
| a plain `GET` with a foreign `Origin` | **served** — item 3 checks upgrades only, and the spec accepts in writing that a preview is readable by anyone who can reach the origin |
| any method (`POST`, `PUT`, `DELETE`, `OPTIONS`) | **proxied** — a dev server's form post is what a preview is for |

`preview_origin` is where the origin check comes from, and it is also the origin the session screen's link must
use. The page needs it, so it became `GET /api/health`'s **fifth key**: the page already fetches health before
either screen draws, so the link costs no request and no new route. The alternative — injecting it into the app
shell — would break `case_app_shell`'s byte-for-byte comparison against `web/dist/index.html` and would put a
dynamic value in a static asset.

The residual risk is the spec's own and is unchanged: previews of different sessions share one origin, and
`access-control-*` headers from a dev server are passed through because a tunnel cannot rewrite what it forwards
without lying about it. The control listener's unconditional CORS strip (ADR-0010) is its own rule and still holds.

## Shutdown, a bind that fails, and the two `:0`s

The second listener takes the same `StopSignal` and the same bounded drain. `serve.rs` installs **one** signal
handler and forwards it through a `watch` to both listeners, so the process still has exactly one `StopSignal` and
both drains start together; a listener that ends on its own stops the other, so nothing a session start left
behind outlives the process.

A preview bind that fails is **one `warn`, and the boot continues**: the control UI is the screen an operator
needs when something else on the machine already holds that port (HS-036's rule), and `config` already refuses
`preview_listen == control_listen`, which is the collision that is a mistake rather than an accident. The banner
line says it in as many words — `preview_listen unavailable (<addr>: <reason>)` — so the line can never claim a
listener this run does not have. **The banner is still eight lines**; stderr is eight lines plus that one `warn`,
because a `warn` at the default filter is a tracing line and not a banner line — which is why
`a_preview_port_that_is_taken_is_a_warning_and_the_control_listener_still_serves` counts the banner's lines by
excluding the `warn` it is there to assert. A healthy boot's stderr is still exactly eight, which is the pin
`the_startup_lines_are_exact_and_name_the_bound_port` holds.

`preview_listen_problem` gained one clause to make `127.0.0.1:0` usable for both listeners: a port of `0` is the
kernel's to choose, so two of them are not a collision. The rule is about sharing a port, not about sharing a
spelling of "any". Without it, every test and every case would have had to pick a fixed preview port and race
whatever else on the machine wanted it.
