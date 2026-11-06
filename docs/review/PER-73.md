# PER-73 (HS-041, preview listener) — two-axis review

Two-axis review (`~/.agents/skills/code-review/SKILL.md`) against `c121f8b`, as returned by the reviewer child.
The `## Resolution` table below the review is the orchestrator's.

## Standards

**Correct:** the `tunnel.rs` extraction matches the `egress`/`keyproxy` shape rather than copying, the pure `Route`/`parse_head`/`rewrite` split follows `egress.rs`, and ADR-0051 + the ADR index + the new landmines section are in the same commit. `Route.host`'s unused-in-production field is a documented, spec-required value (ADR-0051) — suppressed, not flagged.

**1. P2 · `docs/build.md` is not in the diff.** notes.md:659: "**`docs/build.md` is a story deliverable, not orchestrator-only.** If a story changes prerequisites or gates, the doc is in scope, and its prose must be reflowed". The gate now boots a second listener, reads a third banner value (verify.sh:4219-4227) and writes a new key into the shared config (verify.sh:4274); build.md's serve section still says only "`control_listen` and `control_origin` are both read from the startup banner", and its `serve_cases` enumeration has no `case_preview`. Same omission as docs/review/PER-54.md:37.

**2. P2 · the eight-line stderr contract.** notes.md:563-566: "`tests/serve_cli.rs` asserts stderr has **exactly eight lines** at default filter. A new `info!` at startup therefore fails a gate test". The new `unavailable()` warn (serve.rs:378) prints at that filter (logging.rs:13 `DEFAULT_FILTER: &str = "info"`), so a boot whose preview port is taken writes nine; serve_cli.rs:190-199 filters ` WARN ` out to keep counting eight. ADR-0051:112's "the eight-line startup contract is still eight lines (a `warn` is a tracing line, not a banner line)" is true of the banner, not of stderr.

**3. P2 · ADR-0017 not marked superseded.** notes.md:668: "An ADR that a later story changes gets a one-line 'Superseded by ADR-00xx' note, not a rewrite". ADR-0017:150 still reads "The health payload keeps its four keys"; ADR-0017:25-26 pins it at "four keys". health.rs:27 makes it five.

**4. P3 · Duplicated Code.** preview.rs:131-136 `is_session_id` is byte-identical to commands/container.rs:14-19 `valid_id`.

**5. P3 · Repeated Switches.** `Refusal` is matched three times over seven variants — status (preview.rs:171), code (184), sentence (197) — where the control listener keeps one constructor (`ApiError::new(status, code, message)`, http/error.rs:140-160). A new variant is three edits.

**6. P3 · inert CSS.** style.css:92-95 `.preview { opacity: 1 }` sits inside `.session-meta { opacity: 0.8 }` (style.css:78-84); group opacity means the child cannot be restored — the declaration does nothing.

**7. P3 · claims the diff falsified, plus one missed config.** notes.md:657 "(thirty-one today)"; verify.sh:975 "The health payload: exactly four keys"; tests/db_migrations.rs:270 still boots `serve` with `preview_listen = "127.0.0.1:8081"` while every other boot moved to `:0` — the collision now appears only as the new warn, which hides it.

**Standards: 7 findings; worst is the un-updated `docs/build.md` (P2), the repo's bolded, reviewer-enforced story deliverable.**

## Spec

**Correct:** every acceptance bullet has an artefact — preview.rs + tests/preview.rs, `web/test/transcript.test.ts`, `case_preview`, and V7's row plus ADR-0051. Declared non-goals held: no hub forwarding, no new crate, no `axum::serve` in a handler, and the health change is authorised by RESUME §1.6 q4.

**1. P2 · the upstream port is never told to the container.** config/mod.rs:188-195 pins one "port a session's own dev server listens on **inside its container**" and preview.rs:401-402 connects to it, but container.rs:472-497 passes only `HS_SESSION_ID`, `PI_CODING_AGENT_DIR`, `agent::ENVIRONMENT` and `PROXY_ENV`. ADR-0051:56 says outright "the agent chooses its own dev-server port at run time". The slice's demo is "a stub HTTP server in a container is reachable at `http://<preview-origin>/local/<id>/` from another machine" — reachable only because the gate forces the port into its own stub (`HS_PREVIEW_TEST_PORT=$upstream_port`, verify.sh:2417). Smallest fix: pass the port in beside `HS_SESSION_ID`, or record the gap as a follow-up; the pinned key and "the agent chooses its own port" cannot both stand.

**2. P3 · `/local/<id>` without a trailing slash.** preview.rs:82-85: "a path with no trailing slash is the upstream's root, which is why `/local/<id>` and `/local/<id>/` are the same request." They are for the upstream, not for the browser: the document's base URL is `/local/<id>`, so the dev server's relative `./assets/x.js` resolves to `/local/assets/x.js` → host `assets` → 404. Item 3 asks for `/<host>/<id>/*`; a 308 to the slashed path is the smallest fix.

**3. P3 · the gate reads the port from the wrong config.** verify.sh:2393-2394 reads `preview_upstream_port` from `$WORK/config.toml` while the boot being measured is `config-preview.toml` (verify.sh:2290). If the files diverge, the stub binds a port the running listener does not forward to, and the failure is attributed to the listener.

**Spec: 3 findings; worst is the dev-server port pinned in config but invisible to the container (P2).**

Must run: `scripts/verify.sh` (the new `case_preview` and the bundle greps at verify.sh:2367 are unverified here), plus `cargo test --test serve_cli --test preview --test db_migrations` and `cd web && npm test && npx tsc --noEmit`.

## Resolution

| # | Finding | Verdict | Where |
| --- | --- | --- | --- |
| S1 | `docs/build.md` not updated | **Fixed.** The serve section names the preview banner value and why the config asks for port `0`; the `serve_cases` prose gains `case_preview`'s two halves, its one `SKIP`, and why it boots a stub backend of its own | `docs/build.md` |
| S2 | stderr is nine lines when the preview bind fails | **Half fixed, half dismissed with citation.** The banner *is* still eight lines and a healthy boot's stderr is still exactly eight (`the_startup_lines_are_exact_and_name_the_bound_port` proves both). The ninth line is a conditional `warn` on a failure path — the shape HS-036's egress/second-network warns and ADR-0044's `Unknown` warn already have, and notes' rule is about an unconditional startup line ("a new `info!` at startup"). ADR-0051's sentence was imprecise, and is now exact, naming the test that counts banner lines by excluding the warn it asserts | ADR-0051 |
| S3 | ADR-0017 not marked amended | **Fixed.** Both places that pinned "four keys" now carry an "Amended by ADR-0051" note, with the reason the fifth belongs there rather than on a new endpoint | ADR-0017 |
| S4 | `is_session_id` duplicates `commands/container.rs::valid_id` | **Fixed.** One home: `sessions::is_id`, used by the preview's path parser and by `container snapshot`; the `commands` copy is now a delegation | `src/sessions/mod.rs`, `src/preview.rs`, `src/commands/container.rs` |
| S5 | `Refusal` matched three times | **Fixed.** One `parts()` returning `(status, code, sentence)`, with the three accessors reading it — one edit per new variant, and a variant cannot have a status without a sentence | `src/preview.rs` |
| S6 | inert CSS `opacity: 1` | **Fixed.** The declaration is gone; the comment says why a child cannot undo a group's opacity | `web/src/style.css` |
| S7 | three stale claims and one stale boot | **Fixed.** notes' "thirty-one" is thirty-two; `case_health`'s comment says five keys; `tests/db_migrations.rs` asks for `preview_listen = "127.0.0.1:0"` like every other boot | `docs/notes.md`, `scripts/verify.sh`, `tests/db_migrations.rs` |
| P1 | the port is pinned but never told to the container | **Fixed.** `HS_PREVIEW_PORT` beside `HS_SESSION_ID` (`container::PREVIEW_PORT_ENV`), from `ContainerSettings::preview_port`, asserted in the launch-environment test and covered by the gate's `docker inspect` case. ADR-0051's "the agent chooses its own port" is rewritten to say the machine decides and the container is told | `src/container.rs`, ADR-0051, `docs/notes.md` |
| P2 | `/local/<id>` without a slash breaks a page's relative URLs | **Fixed.** A page asked for at the unslashed *root* is answered `308` to the slashed path, query kept; an upgrade is not redirected (a redirected handshake is not a handshake), and no deeper path is (`Route::slashless_root`). Unit test + wire test | `src/preview.rs`, `tests/preview.rs` |
| P3 | the gate reads the port from the wrong config | **Fixed.** It reads `config-preview.toml`, the file the boot under test actually read | `scripts/verify.sh` |

Every finding is fixed or dismissed with a citation. No P1 was raised. The gates were re-run after these edits
(`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cd web && npm test && npx tsc
--noEmit`, `scripts/verify.sh`), and the demo was re-run against the rebuilt artifact.
