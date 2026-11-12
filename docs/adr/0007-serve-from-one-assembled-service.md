# Serve from one assembled service, with the web app embedded and a deadline on shutdown

This is the first story that runs a server, and the next seventy add routes to it. Three things therefore had to be settled together: what the HTTP stack is, where cross-cutting middleware attaches so that a later route cannot skip it (ADR-0002), and how the web app becomes part of a binary that must build without fetching anything undeclared (ADR-0005).

## The stack, and the one place a route can be built

axum 0.8 on tokio, with cross-cutting behaviour as tower middleware. axum is the only mainstream candidate that is hyper- and tower-native, so the guards, the event stream and the drain are `tower::Layer`s over one service instead of four integrations in four frameworks. Everything needed was built and measured on the build machine for both musl targets (axum 0.8.9, tokio 1.54.1, hyper 1.12, tower 0.5, tracing 0.1, include_dir 0.7.4): the aarch64 artifact was `statically linked`, had no dynamic section, and served `/api/health` under `qemu-aarch64-static`.

Two measured facts decide the shape. `axum::serve` does **not** accept a tower-wrapped service — `ServiceBuilder::new().layer(..).service(router)` fails the `for<'a> Service<IncomingStream>` bound — so a layer cannot simply be applied at the call site; and `Router::layer` applies to the routes that exist *when it is called*, which turns "the guards are applied somewhere" into a rule somebody eventually breaks by adding a route afterwards. So the crate exposes two types, and nothing else that can be served:

```rust
pub struct Routes(Router<()>); // what feature code builds routes on: no constructor from a Router, no accessor returning one
pub struct App {               // the only servable type
    router: Router<()>,
    requests: RequestCounter,  // observation: `edge` counts what it saw
}
impl App {
    pub fn new(routes: Routes) -> Self { /* `routes.0.layer(edge)`, once, here */ }
    pub(crate) fn into_router(self) -> Router<()> { self.router }
}
```

`Router::layer` inside `App::new` was verified to wrap every route **and** the fallback, and a request that only the fallback matches passes through the same layer. A route can be born outside `Routes` — nothing stops a caller building a bare `Router` in its own test — but no router *this crate serves* can skip the layer: `into_router` is `pub(crate)`, `serve::run` is its only caller, and `axum::serve` appears exactly once in the crate. That is the compile-time rule, and it is why adds its guards in exactly one function. The `edge` layer today holds request accounting and a `debug` log line: it is the extension point, not a guard. The exemption predicate `is_guard_exempt` (true only for `GET` or `HEAD` on `/api/health`) ships with the router, so reads a list instead of inventing one.

## The control origin is one a browser can send

 compares a browser's `Origin` header with `control_origin`, so the value has to be something a browser would actually send. Two ways of writing it are not. `--port 0` leaves the port unknown at override time, so the value would read `:0`; and `:80` on `http` or `:443` on `https` is a port no browser puts in `Origin`. So `Origin::with_port` leaves a scheme's default port out, and when `--port 0` asked the kernel for the port, `serve` re-derives the control origin from the bound address after `bind` and before the app is assembled: the origin will read is the one the startup banner printed. A port written in the file is never rewritten, because it may deliberately name a proxied address rather than the local listener (ADR-0006: the file is the truth unless the CLI overrides it). The origin is the seventh startup line, right after `control_listen`, so the operator can read the value the guard will compare against even with diagnostics off.

## The embedded web app, and the placeholder

The Vite app's build output is embedded with `include_dir!`, and `cargo build` still never runs npm: `scripts/build-web.sh` runs `npm ci` and `npm run build`, and `scripts/build.sh` calls it before cargo. A tree without `web/dist` would otherwise fail as a proc-macro panic (`"… is not a directory"`, measured), so `build.rs` owns the decision: with the directory present it embeds it; without it, it writes a one-file placeholder into `OUT_DIR` whose HTML carries a fixed marker, prints a `cargo:warning` naming the missing app, and — when `HOLOSMITH_REQUIRE_WEB=1`, which `scripts/build.sh` always sets — fails the build with the same instructions. `scripts/check-static.sh` then greps the artifact for that marker and fails if it finds it, so a debug build stays useful (the served shell says the web app was not built) while no shipped artifact can contain anything but the real app.

This is a declaration, not an exception, to ADR-0005's "no repo script downloads anything". That sentence is about the qemu dev tool being machine-local and optional; `npm ci` fetches **declared** dependencies from `web/package-lock.json`, the same category as `cargo fetch` and the one the fourth criterion allows. Nothing else may be fetched: no script may `npm install <pkg>`, and nothing may rely on a package's install script (npm 11 refuses them by default — measured, and the Vite build works anyway).

## The shutdown deadline

Both SIGTERM and SIGINT stop the listener, drain, and exit 0. Measured on the built binary: an idle keep-alive socket does not delay shutdown (6 ms), and an in-flight 3 s request completes with its body before the process exits. What has no natural end is a streaming response — with one open, a graceful shutdown without a deadline had not returned after 29 s, which is what would inherit. So the drain has a deadline: `ServeOptions::shutdown_grace`, 10 s in production, a parameter so a test can use 200 ms, after which open connections are cut and the process still exits 0 (measured: hanging stream, 300 ms grace, exit 0 at 306 ms). Live streams are not special-cased; may emit a final event or a `retry` before the deadline, and revisits the number when `drain` becomes real. Implementation note for whoever writes it: `WithGracefulShutdown` implements `IntoFuture`, not `Future`, so the deadline is a `select!` over `.into_future()`, and the grace must start when the signal arrives, not when the process starts.

Where each half of that is proved: `tests/serve_cli.rs` drives the real binary with both signals and asserts exit 0; `scripts/check-static.sh` runs the shipped artifact in an empty root and proves the process stops, not its status, because `bwrap` exits with the signal's own 143 instead of forwarding the child's exit code. That split is deliberate and is the reason the artifact check keeps its `|| true`.

Shutdown here is process-level and is **not** the Drain of item 10 (refusing new work and waiting for running turns). The glossary's `Drain` entry keeps `_Avoid_: shutdown` because the two are not synonyms.

## Diagnostics: stderr, and the filter is not a config key

The binary logs through `tracing` to stderr, one line per event, with the filter taken from `HOLOSMITH_LOG` (default `info`) and echoed as the first startup line. The filter is deliberately not a config key: it belongs to the *invocation* — a systemd unit, a debug session — rather than to the machine's settings that `server deploy` writes, which is the line ADR-0006 draws. Writing no log files is equally deliberate: the spec's `holosmith logs` reads journald, and a session's durable **Events** are data, never diagnostics.

## Consequences

- The artifact grows by roughly a megabyte of code (a 2.05 MB static probe against a 1.28 MB baseline): the price of tokio and hyper in every release.
- `cargo test` and `cargo clippy` work on a clone with no node and no `web/dist`; the page served then says the web app was not built, and so does a hand-run `cargo build --release`. Only `scripts/build.sh` guarantees the real app, and `scripts/check-static.sh` is what makes that guarantee checkable.
- `web/dist` and `web/node_modules` stay git-ignored; the app's source is the only committed thing.
- Routes are added to `Routes` and served as `App`. There is no `Router` in any public signature, and `into_router` is crate-private, so the seam is a compile-time rule rather than a review checklist.
- The control origin is re-derived from the bound port when the port was ephemeral, and a default port is never written into it: can compare a browser's `Origin` with it as text.
