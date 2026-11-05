# The checkbox is topology, and the off case is measured by a name that does not resolve

Spec item 2 gives the operator one checkbox — "Internet access" — and one promise behind it: **"off" means off**.
HS-013 drew the checkbox, HS-037 built the proxy and the join, and the wiring between them turned out to be
already true: `sessions.network` is a column, `POST /api/sessions` parses it, `DockerBackend::apply_proxy` joins
the machine's `hs_egress` to the session's network when the row says so and leaves it when it does not, on the
create *and* on `resume`, and `SessionContainer::run_args` emits `HTTPS_PROXY=http://<id>@egress-proxy:3128`
unconditionally. So what this story had left to do was not the wiring. It was **the three things a wiring with no
measurement cannot claim**: that "off" is measured rather than assumed, that the phrase an operator reads about
the choice is a rule, and that V6 is a gate rather than a checklist somebody ran once.

## Off is a name that does not resolve, not a timeout

HS-036's self-test already ran its probe container on a temporary internal network with **no proxy joined to
it** — topologically exactly a session network that asked for no internet — but it only asked whether the
internet and the host answer. A proxy on the wrong network, which is what "the checkbox does not work" looks
like, was invisible to it: the probe would report nothing reachable and the boot would pass.

The fix is one target, `selftest::PROXY_TARGET = "egress-proxy:3128"` — the **alias** a session's container
resolves the proxy by, never the container's name (`hs_egress` resolves on the `egress` network only, so
probing for it would pass on every machine, joined or not). Measured on this machine:

| network | `1.1.1.1:443` | `172.17.0.1:2375` | `egress-proxy:3128` |
| --- | --- | --- | --- |
| `--internal`, proxy not joined | `ENETUNREACH` | `ENETUNREACH` | `EAI_AGAIN` |
| `--internal`, proxy joined as `egress-proxy` | `ENETUNREACH` | `ENETUNREACH` | `connected`; `CONNECT example.com:443` → `403` |

A proxy that answers is `connected` — **reached**, by the rule ADR-0044 already rests on — so the verdict needed
no new machinery, only a second sentence: a proxy that answers where nothing should leave is a wiring mistake,
and the isolation sentence (which names `--iptables=false` and the default bridge) sends the operator to the
wrong machine setting. A direct leak still wins when both are reached, because it is the worse finding. `decide`
gained one branch and `leaked` the predicate behind it.

**And the probe runs twice, on the same network.** A silence on a network nothing is attached to is only evidence
if the alias answers the moment something is: otherwise a build whose `join_network` never adds the alias — the
exact bug the off case exists to catch — passes it, because a name that never resolves is indistinguishable from a
proxy that is not there. So after the off probe the self-test joins the machine's `hs_egress` to the same network
under the alias and asks again, about the alias alone, and leaves. That half is **not a verdict**: a proxy that
will not join or is not running is HS-037's `warn` (it fails a download, not a session — refusing the boot over it
would take the control UI down for a machine whose sessions otherwise work), and it is logged as one `debug` line
when it works and one `warn` when it does not. What it buys is the discrimination, at boot, that no other check
makes.

**What it still does not do**, stated rather than implied: the probe runs on the self-test's own temporary network,
not on a session's, and it reads no row. Item 2's sentence is "a probe **on a session network** without the egress
proxy must reach nothing except the key proxy", and the session-shaped half of that is
`case_internet_toggle`'s — two real sessions, their own networks, the row's choice behind each. The boot owns "a
network nothing is joined to reaches nothing, and joining the proxy is what changes that"; the gate owns "a
session's row produces that topology".

**Why a name, and not a port that is closed.** A closed port answers `refused`, which ADR-0044 counts as
*reached* — so an unanswered port is not evidence of an absent proxy, it is evidence of nothing at all. A name
that does not resolve cannot be answered by anything, whatever is listening behind it, and it is the only fact
that separates "this session asked for no internet" from "the proxy is missing by accident".

**What is deliberately not in the list yet: the key proxy.** Both acceptance bullets that mention it — bullet 1's
"only the key proxy is reachable" and bullet 5's "nothing but the key proxy answers" — need a container that does
not exist until HS-039. Today the off case measures *nothing* answering, which is the same measurement with the
one legitimate answer removed; a target nothing answers would make the sentence about it a claim rather than a
measurement. HS-039's job is therefore not to add a name to `targets()` (a target must not answer) but a **second
question**: the alias that is *expected* to answer on a session network. `decide` gets that shape when there is
something to point it at.

## The words are the operator's, so they are a rule

The checkbox is not a setting with a direct route behind it — the container carries the same `HTTPS_PROXY` in
both cases, and only the topology differs (ADR-0045). So the phrase the session screen shows is a function in
`transcript.ts` (`networkText`, `internet on` / `internet off`) with a test rather than a ternary in `main.ts`, on
the rule that a sentence an operator reads is a rule here (`createFailure`, `budgetNotice`, `stoppedNotice`).

**The rendered output is byte-identical to what HS-013 shipped**, and that is deliberate: phase 1b says nothing in
it "may require a UI or API change", and this story's acceptance bullet 4 ("visible in the UI") was already
satisfied by HS-013's checkbox and HS-013's meta span. The story therefore adds **no element, no listener and no
style** — only the tested spelling of an existing string. An explanation *under* the checkbox was drafted and cut
for exactly this reason: it is a new visible element, and it belongs to the story that owns the form, not to a
phase that forbids UI work. (Nothing was lost by cutting it: the operator already has the screen's own phrase, and
the sentence about *why* a download fails belongs with the error they will see.)

## V6 is a container on each network, and the flag is the endpoint count

`case_internet_toggle` boots the artifact on a `backend = "docker"` config, creates **two real sessions that differ only
in the body's `network`**, and asks each session's own network three questions from a probe container: the
internet directly (unreachable in **both** cases — a case that checked only the on half would pass a build whose
networks had stopped being internal), the proxy's alias (must not resolve in the off case, must answer `403` for
a non-listed host in the on one), and the container's `HTTPS_PROXY` (the same string modulo the session id,
which is the acceptance's "the difference is topology, not configuration", read off the containers the daemon is
really running).

Then the half no container can show: **a resume keeps the choice**. The proxy is detached from each session's
network, the container is killed (its exit lands the row in `error`, a resumable state), the session is resumed,
and the endpoint count is **1** for the off session and **2** for the on one. The detach is what makes the count
evidence rather than an inheritance: the resume re-creates the network idempotently, so a network that survived the
stop keeps whatever the create attached to it and an `on` session reports 2 whether or not the re-join happened.
Detached and then 2 is a rejoin; detached and then 1 is a missing one — **measured** by mutating the network step
to skip the join when its network already exists, which is precisely that bug: the case goes red with
`expected '2', got '1'`.

(The join lives in the **network step**, which a resume runs again through the provisioner; `SessionBackend::resume`
is the ladder's path and joins too, but it is not what `POST /resume` takes.)

The image has no `curl` (measured), so V6's `curl --noproxy '*'` is asked by node — the same `net.connect` the
self-test's probe is built on, for the reason ADR-0044 gives: one image, and the one sessions already run.

## Consequences

- A machine whose daemon cannot create networks, or that has no `agent_image`, gets `Unknown`: one `warn`, the
  boot continues, and nothing about the off case is claimed (ADR-0044's rule, unchanged).
- A **proxy on a no-internet network** is now a *refusal*, not a `warn`. It is only reachable by joining
  `hs_egress` to `holosmith-selftest` by hand — the self-test's own network is created and removed within one
  run, and `apply_proxy` only ever joins `agents_<id>` — so it is a misconfiguration an operator made, and the
  alternative (a `warn` on a line nobody reads) is what lets "off" quietly become "on".
- The gate's Docker cases now end with `?force=true` deletes: the WIP snapshot a delete asks for is HS-042's, so
  a Docker session with a worktree answers `409 unsaved_work` until then.
- Orphaned `agents_*` networks from a crashed run are still HS-044's; this story removes the two it makes.
