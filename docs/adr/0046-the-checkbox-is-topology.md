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

**Why a name, and not a port that is closed.** A closed port answers `refused`, which ADR-0044 counts as
*reached* — so an unanswered port is not evidence of an absent proxy, it is evidence of nothing at all. A name
that does not resolve cannot be answered by anything, whatever is listening behind it, and it is the only fact
that separates "this session asked for no internet" from "the proxy is missing by accident".

**What is deliberately not in the list yet: the key proxy.** The off case's promise in spec item 2 is that *only
the key proxy answers*. That container does not exist until HS-039, and a target nothing answers would make the
sentence about it a claim rather than a measurement. HS-039 adds it to `targets()`, and the off case then reads
"nothing but the key proxy" instead of "nothing at all".

## The words are the operator's, so they are a rule

The checkbox is not a setting with a direct route behind it — the container carries the same `HTTPS_PROXY` in
both cases, and only the topology differs (ADR-0045). So `transcript.ts` holds two functions, `networkText`
(the screen's `internet on` / `internet off`) and `networkDetail` (the line under the checkbox: allow-listed
hosts through this machine's proxy, or nothing outside reachable), both used by `main.ts` and both tested
against the alternative. That is the repository's existing rule that a sentence an operator reads gets a test
(`createFailure`, `budgetNotice`, `stoppedNotice`), and it is the whole of this story's UI work: the promise is
that the two sentences cannot drift into one, and that neither names an environment variable the operator could
go and change — because in both cases there is nothing in the environment to change.

## V6 is a container on each network, and the flag is the endpoint count

`case_internet_toggle` boots the artifact with `--backend docker`, creates **two real sessions that differ only
in the body's `network`**, and asks each session's own network three questions from a probe container: the
internet directly (unreachable in **both** cases — a case that checked only the on half would pass a build whose
networks had stopped being internal), the proxy's alias (must not resolve in the off case, must answer `403` for
a non-listed host in the on one), and the container's `HTTPS_PROXY` (the same string modulo the session id,
which is the acceptance's "the difference is topology, not configuration", read off the containers the daemon is
really running).

Then the half no container can show: **a resume keeps the choice**. The container is killed (its exit lands the
row in `error`, a resumable state), resumed, and the network's endpoint count is the flag itself — **1** for the
off session, **2** for the on one. That is the assertion a wrong flag cannot survive, and it is the only
end-to-end proof that `apply_proxy` runs on the resume path rather than only on the create's.

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
