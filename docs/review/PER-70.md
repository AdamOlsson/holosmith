# PER-70 / HS-038 — review of `4434c10..ae216d6`

Two axes, reported separately and not reranked against each other. Standards = does the change follow this repo's documented standards; Spec = does it do what HS-038 asked.

**Method.** Diff fixed at `4434c10..HEAD` (`ae216d6`), 11 files: `GLOSSARY.md`, `crates/holosmith/src/selftest.rs`, `crates/holosmith/tests/selftest.rs`, `docs/adr/0046-the-checkbox-is-topology.md`, `docs/build.md`, `docs/stories/README.md`, `scripts/verify.sh`, `web/src/{main.ts,style.css,transcript.ts}`, `web/test/transcript.test.ts`. Compared against `docs/stories/phase-1b-docker-backend.md` §HS-038, `HolosmithSpec.md` item 2 + V6, ADR-0044/0045/0046, `AGENTS.md`, `GLOSSARY.md`, `docs/build.md`. Read-only: no shell, no edits, nothing re-run. The 57-case / 579-assertion green run and the demo transcript in the brief are taken as supplied, not re-verified.

---

## Standards

| # | Sev | Where | Finding |
|---|---|---|---|
| S1 | **P1** | `scripts/verify.sh:1816-1819` | The `on` half's proxy assertion SKIPs on precisely the bug it exists to catch, and the SKIP's stated reason is never asserted to exist. |
| S2 | **P2** | `scripts/verify.sh:1870-1874`, `:95-96` | The trap backstop is disarmed *before* the deletes it is meant to back up, and cannot remove an `on` session's network anyway. |
| S3 | **P2** | `docs/build.md:236`, `docs/adr/0046-…:57` | Both claim the case "boots the artifact with `--backend docker`". It does not — it writes `backend = "docker"` into a config file. |
| S4 | **P2** | `web/src/transcript.ts:553-563` | `networkText`'s doc comment justifies itself by a duplication that does not exist (one call site). |
| S5 | **P2** | `crates/holosmith/src/selftest.rs:14`, `:78`, `:149` | Three doc comments in the edited file still describe a two-target probe. |
| S6 | **P3** | `web/test/transcript.test.ts:884` | The test's title asserts "neither says 'internet access'"; no assertion checks that phrase for the two sentences. |
| S7 | **P3** | `scripts/verify.sh:1758-1762` | `const REFUSED` and `const NOT_LISTED` in the embedded node probe are dead. |
| S8 | **P3** | `scripts/verify.sh:1694`, `:1838` | `wait_idle` is defined inside the case (global, but absent if the case SKIPs); `want` is never `local`ized. |
| S9 | **P3** | `scripts/verify.sh:1680` | `$root` is removed at the start and never at the end, so `target/verify-toggle-$$` survives every green run. |
| S10 | **P3** | `docs/build.md:236-246` | The insert splices a new long parenthetical into the egress-proxy sentence and leaves the guard-code sentence dangling after it; the "third question" is a `docker inspect`, not a container question. |
| S11 | **P3** | `scripts/verify.sh:1829-1834` | "modulo the session id" is asserted by comparing each container against a hard-coded `http://<id>@egress-proxy:3128` — a fifth spelling of the proxy URL, in shell. |

**S1 — the P1.** `verify.sh:1816-1819`:

```
*"proxy EAI_AGAIN"* | *"proxy ENOTFOUND"*)
  printf 'SKIP %s/on: the egress proxy is not on the session network (the boot said why: %s)\n' \
    "$name" "$(grep -i 'egress' "$serve_err" | tr '\n' ' ')"
```

Two failures in one arm. (a) `EAI_AGAIN` from a session network is produced by *both* causes: the proxy could not start on this machine (`crates/holosmith/src/commands/serve.rs:338-351` logs a `warn`) **and** a build whose `apply_proxy` never joins (`crates/holosmith/src/backend/docker.rs:359-368`). ADR-0045's "a proxy failure is a `warn`, not a refusal" justifies tolerating the first; it does not justify tolerating the second, which is HS-038's own subject. Delete the join at `backend/docker.rs:361-365` and this case still exits 0. (b) The `$(grep …)` is interpolated, never checked, so even the *evidence* for the SKIP can be empty — which is what a broken join would print, and the run would show `the boot said why: )` and go green. This is the pattern the brief names (HS-020, HS-030, HS-061 all shipped one).

Smallest fix — make the SKIP conditional on the boot having said something:

```sh
*"proxy EAI_AGAIN"* | *"proxy ENOTFOUND"*)
  [ -n "$(grep -i 'egress' "$serve_err")" ] ||
    fail "${name}_on_proxy" "the proxy is not on the on session's network and the boot did not say why: $verdicts"
  printf 'SKIP %s/on: …\n' "$name" "$(grep -i 'egress' "$serve_err" | tr '\n' ' ')"
  ;;
```

**S2.** `verify.sh:1872` sets `docker_session_id=""` *before* `remove_session …off_delete` (`:1873`) and `…on_delete` (`:1874`); every sibling case disarms *after* its deletes (`:636`, `:762`, `:926`). The comment at `:1870-1871` says the backstop is disarmed "because the API did the work" — but it is disarmed before the work is attempted, so a delete that answers anything but `202`/gone leaves the other session live with no cleanup. Compounding it, `cleanup()` at `:95-96` does `docker rm --force s_<id>` then `docker network rm agents_<id> || true`, and `crates/holosmith/src/docker.rs:295-299` documents that Docker refuses to remove a network with an attached endpoint — so for the `on` session the backstop cannot remove the network at all while `hs_egress` is on it. A `fail` anywhere after the two creates therefore leaves `agents_<on_id>` behind with the proxy attached: the orphan HS-044 owns, manufactured by this case. Fix: keep the id set until both `remove_session` calls return (and the one id can only describe one session; either extend the trap to two or accept and say so), and disconnect `crate::container::PROXY_NAME` in `cleanup()` before the `network rm`.

**S3.** `docs/build.md:236` and `docs/adr/0046-…:57` both say "boots the artifact with `--backend docker`". `verify.sh:1687-1689` writes `backend = "docker"` to `config-toggle.toml` and calls `start_serve`, which shells `serve --config … --port 0 --data-dir …` (`:3477`). `docs/build.md` itself says one paragraph earlier that "the backend comes from the *file* rather than from a `--backend` flag on purpose". Fix: "boots the artifact on a `backend = "docker"` config".

**S4.** `transcript.ts:556-561` says the function exists "because it is written twice (the create form's label and the session screen's meta)". `networkText`'s only call site is `main.ts:1050`; the create form's label is still the literal at `main.ts:355`. The ADR repeats the claim ("both used by `main.ts`", ADR-0046:52-56). Either use `networkText` for the label or drop the parenthetical.

**S5.** `selftest.rs:14` ("reach the internet and the host"), `:78` ("three seconds per target, two targets"), `:149` ("the internet, and the host when it has one") all predate this diff and are false after it — `targets()` at `:165-176` now emits internet, proxy, host. Add "and the egress proxy".

**S6.** `transcript.test.ts:884` is titled "…and neither says 'internet access'". The two *phrases* are pinned exactly (`:888-889`) so they cannot, but the two *sentences* are only checked for `/allow-listed/`, `/proxy/`, `/nothing outside/` and the env-var regex (`:896-907`). `networkDetail(true)` could gain "internet access" and stay green. One `assert.doesNotMatch(sentence, /internet access/i)` in the loop at `:905-907` closes it.

**S7.** In the embedded probe (`verify.sh:1749-1785`) `REFUSED` and `NOT_LISTED` are declared and never referenced; the shell's `not_listed` (`:1668`) is what the assertions read. Dead constants that look like the source of truth.

**S8.** `wait_idle` is defined at `:1694`, inside the case, and bash makes that global — fine today, but the name does not exist on a machine that SKIPs at `:1669`/`:1674`. `want` (assigned in the resume loop) is the only name in the case not in a `local` (`:1665`).

**S9.** Five of the six Docker cases end with `rm -rf "$root"` (`:466`, `:639`, `:764`, `:928`, and `:1680`'s start-of-case counterpart pattern); this one removes it only at `:1680`. `case_network_selftest` (HS-036) is the other exception, so it is a precedent, not a rule.

**S10.** The changed lines of `docs/build.md` read as a run-on: `… see ADR-0045), and the Internet-access checkbox (…) . A case that must assert one code keeps every higher guard check correct, or it asserts another guard's code under its own name.` That last sentence is about the guard-refusal cases and now trails the toggle text. Also "three questions … and the container's `HTTPS_PROXY`" — the env is read by `docker inspect` from the host (`:1829-1834`), not asked from the probe container.

---

## Spec

| # | Sev | Where | Finding |
|---|---|---|---|
| SP1 | **P2** | `selftest.rs:63,165-176`; story `:249`; spec `HolosmithSpec.md:68` | Bullet 5 is met for the key proxy's absence, but the probe is not "on a session network" and never reads a row: two of the bullet's clauses are deferred and only one is documented as deferred. |
| SP2 | **P2** | `docs/adr/0046-…:67-68` | "the only end-to-end proof that `apply_proxy` runs on the resume path" is not what the endpoint count proves. |
| SP3 | **P2** | `GLOSSARY.md:554-557` | "the boot measures it rather than trusting the row's value (the self-test's third target)" — the boot does neither, and the proxy is the *second* target. |
| SP4 | **P2** | `web/src/main.ts:356-363`, `main.ts:440`, `web/src/style.css:158-165` | A new visible element on the New session form in a phase whose rule is "nothing … may require a UI … change"; the acceptance already had its UI visibility. |
| SP5 | **P3** | plan `PER-70.md` §Non-goals; story `:245` | Bullet 1 also says "only the key proxy is reachable"; the deferral is argued only in bullet 5's words. |
| SP6 | **P3** | `verify.sh:1829-1838` | Bullet 3 says "proxy environment variables are identical"; the case asserts `HTTPS_PROXY` only. True in code (`container.rs:496-511` emits `PROXY_ENV` and `agent::ENVIRONMENT` unconditionally), asserted for one variable. |

**SP1 — bullet 5, honestly assessed.** The story's bullet (`phase-1b-docker-backend.md:249`) and spec item 2 (`HolosmithSpec.md:68`) say: *a probe **on a session network** without the egress proxy must reach nothing except the key proxy.* What was built is a third target, `selftest::PROXY_TARGET = "egress-proxy:3128"` (`selftest.rs:63`), on the self-test's own temporary network `holosmith-selftest` (`:43`, `:190-200`). Nothing in the build joins `hs_egress` to that network — `apply_proxy` joins `agents_<id>` only (`backend/docker.rs:359-368`) and the proxy's own `run_args` puts it on `egress` (`container.rs:570-575`) — so on a supported machine the new target's verdict is fixed at `EAI_AGAIN`, the new `Failed` arm (`selftest.rs:241-247`) cannot fire, and nothing about a *session's* `network` flag is observed at boot. The deferral of the key proxy is argued openly and is right (HS-039 is blocked by HS-038; a target nothing answers proves nothing). The narrower fact — that the probe is not on a session network — is folded into "topologically exactly a session network that asked for no internet" (ADR-0046:21-23) and stated only in Consequences as "It is only reachable by joining `hs_egress` to `holosmith-selftest` by hand" (ADR-0046:77-80). That is honest about *reachability*, not about *scope*: the sentence in the boot's own failure message tells the operator to check "that a session's network is joined only when its row says it may be" (`selftest.rs:243-244`), which this probe can never observe. Verdict: **bullet 5 is half met** — the mechanism and the vocabulary exist, the measurement does not cover the clause the spec spells out. Doing less (dropping the arm) would leave the boot with no off-case check at all, which item 2 asks for; doing more needs one of two small things: probe a temporary network *that the self-test itself joins `hs_egress` to by the alias* (which would make the pair a boot-time fact, not only a test's stand-in), or state in ADR/GLOSSARY that the boot measures "the proxy's absence from a network nothing is joined to" and let `case_internet_toggle` own the session-shaped claim.

**SP2.** ADR-0046:67-68: *"That is the assertion a wrong flag cannot survive, and it is the only end-to-end proof that `apply_proxy` runs on the resume path rather than only on the create's."* The first clause is true; the second is not. `resume` (`backend/docker.rs:843-854`) re-creates the network idempotently (`docker.rs:115-125`) — the network survived the `docker kill`, and so did the proxy's endpoint on it. A build with `self.apply_proxy(session)` deleted at `:852` still measures 1 and 2, because the `on` session's count of 2 is inherited from the create. Delete the call and this case stays green. Smallest fix, one line of shell before the kill of the `on` session: `docker network disconnect --force "agents_$on_id" hs_egress`, which makes the count *prove* the resume re-joined (and turns a missing `apply_proxy` in `resume` into a red case). Failing that, narrow the sentence to what is proved: "the topology is correct after a resume".

**SP3.** `GLOSSARY.md:556`: *"so 'off' means nothing outside the machine is reachable, and the boot measures it rather than trusting the row's value (the self-test's third target)"*. The boot reads no row and creates no session network (see SP1), and the proxy target is the *second* entry in `targets()` (`selftest.rs:167`), the host being third. Two small corrections in one sentence: drop the causal claim, fix the ordinal.

**SP4 — the phase rule.** API, event contract and state table are **untouched**: no file under `src/http/`, `src/events/`, `src/sessions/` or `migrations/` is in the diff (11 files, enumerated above), so `phase-1b-docker-backend.md:3,7` is respected where it matters most. The UI, however, does change: `main.ts:356-363` adds a `<p class="hint">` plus a `change` listener, `:440` inserts it in the form, and `style.css:158-165` styles it — a new element an operator sees, in a phase that says "nothing in this phase may require a UI or API change … the change belongs in a story of its own". The acceptance's "visible in the UI" was already satisfied by HS-013's checkbox and HS-037's badge (`main.ts:1050`, whose rendered text `networkText` leaves identical). I judge this a *defensible* one-line explanation rather than a contract break — it adds nothing to the phase-3 preview's surface and the session screen's rendered output is byte-identical — but it is the one place the story exceeds its slice, and ADR-0046:49-62 presents it ("the whole of this story's UI work") without noting the phase rule it has to be tested against. At minimum the ADR should say why the rule does not bite here.

**SP5.** Story `:245` bullet 1: "the egress proxy is not connected to the session network; **only the key proxy is reachable**." The plan's Non-goals quotes only bullet 5's "only the key proxy answers". Same deferral, one missing sentence.

**SP6.** `verify.sh:1829-1838` compares `HTTPS_PROXY` for both containers. The acceptance says "proxy environment variables are identical in both cases". The code makes it true for everything (`container.rs:496-511`), so the risk is nil, but the assertion covers one variable. A `docker inspect` env diff (id stripped) would assert the acceptance's actual words.

---

## What I verified by reading, and what I could not

**Verified by reading** — the new arm cannot hijack the existing isolation sentence (order at `selftest.rs:241-259`; `leaked` is true for any non-proxy target, `:281-291`); `leaked`'s parse is right for the probe's real output (`target verdict`, comma-joined — `templates/selftest-probe.js:47-51`; `rsplit_once(' ')` handles `timed out`); the unit test at `:490-519` is **not** vacuous (delete the new arm and `sentence.contains(PROXY_NAME)` fails, because the isolation sentence names only reached targets); `PROXY_TARGET` is pinned against `PROXY_ALIAS` + `egress::PORT` (`:569-572`) *and* against `Docker::targets()` (`:582-585`), so it cannot drift; the off half cannot be satisfied by a wrongly joined proxy (it would answer `HTTP/1.1 403 Forbidden`, which the off patterns reject, `:1803-1810`); the direct check fails closed on an unreadable verdict (`:1791-1801`); both SKIPs precede any machine mutation (`:1669-1678`); `local projects="$root/projects"` is split onto its own line with the `set -u` hazard named (`:1660-1663`) and no verdict word is read out of the heredoc; `start_container` removes the leftover container before launching (`backend/docker.rs:446`), which is what keeps the endpoint counts meaningful (1 / 2, not 2 / 3); `join_network` force-disconnects first (`docker.rs:282-291`), so a resume over a surviving network does not log a spurious `warn`; `network_default` really is the form's default and never the body's (`http/sessions.rs:114-122`), as the new GLOSSARY sentence says; `sessions::NewSession::network` exists (`sessions/mod.rs:115`) so the machine name is right; 46 ADR files match README's "Forty-six" (`docs/stories/README.md:117`) and the ADR-0046 link resolves (`:164`); `pins/pi.txt`'s pinned version appears nowhere in the diff; no new npm dependency, no router, both new sentences are pure functions with a test, and the DOM write is `textContent` (`main.ts:360`), not `innerHTML`.

**Could not verify without running** — that `verify.sh` is green (taken from the brief); that the `on` session's `CONNECT example.com:443` really returns 403 through `hs_egress` (the `[egress] allow = ["crates.io"]` config is at `:1686-1688`; the wire behaviour is `src/egress.rs`'s and the demo's); that `docker kill` lands the row in `error` (the case asserts it, so a wrong assumption fails loudly rather than silently); that `.Containers` reports 1 and 2 after the resume (dependent on the killed container's endpoint being cleared — `remove_container` at `:446` says it should); and the exact node error word on this image (`EAI_AGAIN` is machine-dependent; the case accepts `EAI_AGAIN` **or** `ENOTFOUND`, the integration test asserts only exit 0 at `tests/selftest.rs:186-193`, so the word itself is unproven by any test — a `assert!(!bare.reached.contains(PROXY_TARGET))` there would pin it).

**Test command a supervisor should run** (I cannot): `. ~/.pi/toolchains/env.sh && scripts/verify.sh` on this tree, plus `cd web && npm test` for the transcript test — and, to confirm S1, a throwaway mutation: comment out `join_network` in `backend/docker.rs:361-365`, re-run `scripts/verify.sh --no-build`, and check that `case_internet_toggle` goes red (by my reading it goes green with a SKIP line, which is the bug).

---

## Verdict

- **Standards:** 11 findings — worst **P1** (S1, the `on` half's unconditional SKIP, `verify.sh:1816-1819`). **BLOCK** until S1 is fixed; S2–S5 are one-line fixes worth taking in the same pass.
- **Spec:** 6 findings — worst **P2** (SP1, bullet 5 half met; SP2, ADR-0046's resume claim is not what the count proves). **OK with notes** — no acceptance bullet is unaddressed and none is mis-implemented; two doc sentences and one missing `disconnect` would make the claims match the evidence.
- **Merge verdict: BLOCK** (on S1 only).
## Resolution

Every finding below is either fixed in the commit that carries this table or dismissed with a citation. S4 and SP4
were one edit: cutting the form's hint line removed `networkDetail` and the two-call-sites claim together.

**One redaction in the review body, and it is the repository's own rule:** the reviewer's "verified by reading"
paragraph spelled out the version literal that `pins/pi.txt` is the single source of, which
`tests/pin_single_source.rs` refuses a second tracked file for. It now reads "`pins/pi.txt`'s pinned version". Nothing
else in the review was edited.

| # | Verdict | What changed |
| --- | --- | --- |
| S1 (P1) | **Fixed** | The `on` half no longer skips on the bug it tests. "The alias does not answer" now has two causes and they are told apart with one `docker inspect`: `hs_egress` not **running** is HS-037's supported machine, and the SKIP is only taken when the boot actually printed a line saying so (`grep -i 'egress proxy' "$serve_err"`, and an empty reason is a failure). A proxy that **is** running and is not on the session's network fails the case, which is exactly what a deleted `join_network` would produce. |
| S2 (P2) | **Fixed** | `docker_second_session_id` exists, both are set after the creates, and the trap disarms **after** the two deletes. `cleanup()` now `disconnect`s `hs_egress` before `network rm`, so the backstop can remove an `on` session's network at all. |
| S3 (P2) | **Fixed** | `docs/build.md` and ADR-0046 say "on a `backend = \"docker\"` config". |
| S4 (P2) | **Fixed** | `networkText`'s doc comment no longer claims two call sites; and `networkDetail` is gone (SP4), so there is one sentence, one writer, one test. |
| S5 (P2) | **Fixed** | The module doc, `PROBE_TIMEOUT` and the trait's `targets` all say three targets now. |
| S6 (P3) | **Fixed** | The phrase test asserts `doesNotMatch(/access/i)` and the environment-variable regex on both phrases. |
| S7 (P3) | **Fixed** | The embedded probe's dead `REFUSED`/`NOT_LISTED` constants are gone; the words live in the code paths that produce them. |
| S8 (P3) | **Fixed** | `wait_for_idle_docker` is top-level (beside the two stub waits), and `want`, `running`, `why`, `pair`, `got` are `local`. |
| S9 (P3) | **Fixed** | The case removes its scratch root at the end, like `case_docker_turn`. |
| S10 (P3) | **Fixed** | The `docs/build.md` paragraph was rewritten: the question count and where each answer comes from are right (`docker inspect` is not a container question), and the guard-code rule is a sentence of its own again. |
| S11 (P3) | **Partly fixed, partly dismissed** | The parity assertion no longer hard-codes the URL: it normalises each session's id and compares the **whole** environment (SP6). One exact-string assertion remains on purpose — `http://<id>@egress-proxy:3128`, derived from the id — and the comment says why: `case_container_mounts` spells the mount vector again for the same reason, and an assertion built out of the code it checks cannot fail when the code changes. |
| SP1 (P2) | **Fixed, by the stronger of the two offered** | The probe now runs **twice on the same network**: bare (nothing may answer, including the alias) and then with the machine's `hs_egress` joined under the alias (the alias alone must answer). Without the pair, a build whose `join_network` never adds the alias would pass the off case, because a name that never resolves is indistinguishable from a proxy that is not there. The second run is a `debug`/`warn` and never a verdict (ADR-0045's rule), `case_network_selftest` asserts the boot said one of the two, and both ADR-0046 and the glossary now say which half of item 2's sentence the boot measures and which is `case_internet_toggle`'s. |
| SP2 (P2) | **Fixed** | The proxy is detached from each session's network **before** the kill, so the endpoint count after the resume is a rejoin rather than an inheritance from the create. Deleting `apply_proxy` from `resume` now fails the case. |
| SP3 (P2) | **Fixed** | The glossary sentence no longer says the boot reads a row (it does not) and no longer calls the proxy the third target (it is the second). |
| SP4 (P2) | **Fixed, by doing less** | The form's hint line, its listener and its `.hint` style are removed, along with `networkDetail`. The rendered page is now byte-identical to HS-013's, which is what phase 1b's "nothing may require a UI change" asks; ADR-0046 says why the copy was cut and where it belongs instead. |
| SP5 (P3) | **Fixed** | ADR-0046 states the deferral for **both** bullets that name the key proxy (1 and 5) and says what HS-039's job actually is: a second question, not another target. |
| SP6 (P3) | **Fixed** | The parity check compares every environment variable with the session id normalised, and the exact `HTTPS_PROXY` is a second assertion. |

### The reviewer's "could not verify without running" list, run

- `scripts/verify.sh` on the committed tree: **57 cases, 579 assertions, ok**.
- S1's mutation: with `join_network` deleted from `apply_proxy`, the fixed case fails with
  `case_internet_toggle_on_proxy — the egress proxy is running and was not on the on session's network` (before the
  fix the same deletion took the SKIP arm and the run stayed green, which is the reviewer's prediction confirmed).
- SP2's mutation: the network step made to skip its join when the network already exists — the "the resume
  inherited whatever the create attached" bug — fails `case_internet_toggle_on_resumed_endpoints — expected '2', got
  '1'`. (The API's resume runs the network step through the provisioner; `SessionBackend::resume` is the ladder's
  path, which is why the first attempt at this mutation — deleting `apply_proxy` from `resume` — stayed green and
  the mutation was moved to the step the path really takes.)
- Both were watched going red and then reverted, and the tree was re-run green afterwards.
- The `EAI_AGAIN` word: pinned in `tests/selftest.rs` (`the probe's own verdict for the alias is a name that does
  not resolve`), which asserts the probe's stderr line for `egress-proxy:3128` ends in `EAI_AGAIN` or `ENOTFOUND`.
- `docker kill` → `error`: the case asserts it, and the run above took that path twice.
- The endpoint counts after the resume: 1 and 2, with the detach in place.
