# The isolation is measured at startup, and only a proven leak refuses the boot

Spec item 2 makes the enforcement a network rather than an environment variable: a session's network is
`--internal`, so there is no gateway and no route out. built that and put pi on it. What neither
could say is whether the *machine* honours it — `--internal` is a flag on a daemon, and a daemon started with
`--iptables=false`, or one whose bridge still has a route to the host, would run sessions that look isolated
and are not. Item 2's answer is a startup self-test, and this is the shape it took.

## One probe, two networks, one distinction

The probe is `templates/selftest-probe.js`, embedded with `include_str!` and passed to **the image sessions
run** as `node -e <script> <targets…>`: one image rather than a second one a machine could be missing, and no
mount, because a script passed as an argument needs no file the daemon has to see (ADR-0035's measurement). Its
verdict is its **exit code**, because that is the one thing `docker run` reports without parsing anything: `0`
means nothing was reachable, `9` means something was, and anything else is a probe that did not finish.

Two targets:

- **The internet** — `1.1.1.1:443`, a public resolver's HTTPS port, so a pass cannot be a coincidence.
- **The host** — the gateway `docker network inspect bridge` reports, on port `2375` (the conventional
  unauthenticated daemon API). Read from the daemon rather than guessed, and absent on a rootless daemon that
  has no such gateway, in which case the internet target alone is measured and a *failure* says which targets it
  tried; the passing line therefore claims only "nothing it probed was reachable", not "the host is
  unreachable".

**And the distinction the whole test rests on:** `connected` and **`refused`** are *reached*;
`ENETUNREACH`, `EHOSTUNREACH` and a timeout are not. A connection the far side refuses proves the packet
arrived and the host answered with a reset — on this repository's machine the default bridge answers
`172.17.0.1:2375 refused`, while an internal network gives `ENETUNREACH` for both targets. A rule that counted
only `connected` would pass a machine whose containers can reach the host, which is precisely the leak item 2
is about. Measured, both ways, in `tests/selftest.rs`: the internal network passes and the bridge finds the
host, so "passed" means something.

**A flag would not have told us.** An `--internal` network still reports an IPAM gateway in `docker network
inspect` (measured: `172.21.0.1` on this machine's session networks). So "the network says internal" is not a
measurement of reachability, and neither is any other field: the only honest check is to try.
`tests/container.rs` therefore asserts the flag on a session's network and leaves reachability to the probe.

**And one measured limit, recorded rather than glossed over.** That gateway address *answers* from inside the
internal network: the probe against a temporary internal network's own gateway gets `refused`, not
`ENETUNREACH` (measured 2026-11-05, Docker 29.9.2). So "there is no gateway to the host" in item 2 is not
literally true — what is true, and what the self-test measures, is that there is **no route out**: the internet
is `ENETUNREACH` and the *default* bridge's gateway is `ENETUNREACH`. A consequence worth knowing:
anything on the host bound to `0.0.0.0` is reachable from a session container at its own network's bridge
address, the control listener included, and its defence is the guards of item 4 rather than the network. Nothing
here tries to add that address as a target — a probe that must fail on every Docker installation is not a test,
it is a lockout — and closing the gap (a firewall on the bridge, or a network driver that does not answer) is a
decision or the phase-1 verification run owns.

## Three outcomes, and only one refuses

| Outcome | When | What the boot does |
| --- | --- | --- |
| `Passed` | nothing reached | one `debug` line, and it serves |
| `Failed` | something reached | one sentence naming the target, the targets tried and one thing to check; the boot **refuses** |
| `Unknown` | the probe could not run (no image, a daemon that cannot create a network, a probe that crashed) | one `warn` line, and it serves |

`Failed` is a complaint in `preflight`'s own list (`Check::Network`), printed with the same words and refusing
the same way, so the `doctor` can print it beside the other three.

**`Unknown` must not refuse, and that is the one decision worth arguing.** "We could not measure" is not "we
measured a leak": a machine whose image has not been built yet, or a daemon that cannot create networks, would
otherwise be unable to serve *at all* — including the control UI an operator needs to see why. Those machines'
sessions fail at their own network step with a message about that step, which is a better sentence than a boot
refusal, and the `doctor --full` is where "unverified" becomes actionable. This is also what keeps every
gate case that boots Docker against a `_ping`-only socket working: those boots get `Unknown`, say so once, and
carry on.

Both are one line in the log and neither is `info`, because `tests/serve_cli.rs` pins a stub boot's banner at
exactly eight lines — the rule and followed for their own startup facts.

## The temporary network, and the machine's own

The probe runs on `holosmith-selftest`, **created and removed by the same call**, reused first because a
previous run that died left it. A name of its own rather than a session's, so an operator who finds it after a
crash can tell what it was and the orphan sweep can leave it alone.

The **`egress` network** is the machine's, created once at startup on a Docker run, *not* internal — the
proxies that arrive and attach to it and to each session's network, and that is the only route
out of a session. **Its creation failing is a `warn`, not a refusal**, for the reason `Unknown` is: a session's
own network step already fails with a message about itself, and taking the control listener down over a missing
proxy network would take away the screen that says so.

## What this story does not do

- **The proxies.** `` (egress) and `` (keys) are the containers that make `egress` useful; this
  story only guarantees the network they attach to exists.
- **Orphaned `agents_*` networks.** The acceptance names `` for that, so it is owed there rather than
  half-implemented here — the same way ADR-0038 recorded `Exits::record` having no caller until.
- **A `doctor` command.**. The sentence exists as data (`Complaint`) so it can print the same words.
- **A per-boot cache.** The self-test runs on every boot; it costs one container and about a second, and a
  cached answer would be a claim about a machine that has since changed.
