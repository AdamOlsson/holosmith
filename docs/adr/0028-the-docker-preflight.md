# The Docker preflight is a startup refusal, and its sentences are data

HS-024 is spec item 10 §1's first line: "Preflight, as `doctor --quick`: Linux with systemd, Docker
reachable, git 2.38 or newer, Tailscale logged in, free disk above `min_free_gb`, ports free." Three
decisions follow from it — where the check runs, what it prints, and how the same sentence reaches three
different readers.

## The check is a startup refusal, not a per-request one

The story's own words are "I fail at startup instead of mid-turn", and item 10 §5 puts the same check in
the unit file as `ExecStartPre=holosmith doctor --quick`. So `serve` runs `preflight::check` once, when the
run's backend is Docker *and* its role is not `hub`, after the data root exists (one check is about that
filesystem) and before the database is opened and before the listener binds; a failure prints every
complaint and exits 1. No database, no migration, no banner, no port — and the gate case asserts both the
exit code and that no `app.db` appeared.

The one case this does *not* cover is the demo's other half — "creating a session fails with one clear
message". A server that refuses to start has no create to answer, and a per-create check would ask the same
question on every tap. The resolution is that both readers get the same sentence: `serve` prints it and
refuses, `doctor` (HS-045) prints it on demand, and a session that reaches the Docker backend anyway lands
in `error` with the backend's own sentence — the refusal shape HS-011 gave that backend, which is what a
machine with a daemon but no Docker stories yet already sees.

`stub` runs are not checked at all: the stub needs no daemon, no free space and no container, and the gate
boots with it in an environment with none of those. **A `hub` is not checked either**, whatever its backend:
it runs no sessions, and item 10 §6's `hub deploy` says in as many words that the startup runs "without the
Docker and git checks". A hub with no daemon is the normal hub, so a rule that refused it would refuse the
role the spec invented for it.

## Every failure is one sentence naming the fix, and they are data

A complaint is a `(Check, String)` pair, printed as `docker: …`, `git: …` or `disk: …`. The sentence says
what is wrong *and* what to do: "the docker daemon at unix:///var/run/docker.sock did not answer (…): start
it with `sudo systemctl start docker`, or set docker_host to the daemon you run". Every complaint is
reported at once, which is ADR-0006's rule for config applied to a machine: two problems should be two
edits and one restart, not two restarts.

They are a type rather than three `eprintln!`s because three readers need the same words — `serve`, HS-045's
`doctor`, and any later check that wants to say "this is the same failure" — and because a test can assert
the *fix* is present without pinning the prose.

**A check that cannot run is never a pass.** `docker_host` on an `ssh://` host is a complaint, and so is a
`df` or `git` that is missing or refused — with `df`'s own words in the sentence, because "install coreutils"
is not advice about a program that just answered. No story owns an ssh check yet; when one does, it replaces
that complaint rather than sitting beside it. This is the rule that keeps a
preflight from being decoration: the failure mode of a check that silently passes is a machine that looks
healthy and cannot run a session.

## No new dependency, so `df` instead of `statvfs`

The crate denies `unsafe`, so `statvfs` is unreachable without a `libc` dependency, and a dependency for one
number is not worth its lock-file edge. The two external programs the check runs are `git --version` and
`df -Pk <data root>`; the daemon is asked with a hand-written `GET /_ping` over a `std::os::unix::net`
socket (or TCP), with a three-second read timeout. Docker's own API documents the answer — `200` and the
body `OK` — so the request is four lines of text and the answer is its status line.

`Tools` is the seam that lets a test point the two programs at scripts it wrote. It exists because the
alternative — setting `PATH` for the test process — is `unsafe` in edition 2024 and shared state a
concurrent test could see.

**The commands have no timeout**, and that is a recorded limitation rather than an oversight: `std`'s
`Command` cannot wait with one, and the alternative (a thread and a channel per call) buys a bound this
startup does not need yet. A `df` on a hung network filesystem blocks `serve`'s startup for as long as the
kernel's mount timeout would block any other `df`; when a data root on NFS is a thing item 10 describes, this
is the line to revisit. The two socket calls are bounded: read and write timeouts on both, connect timeouts on
`tcp://` (`UnixStream::connect` has none in `std`, and a unix connect does not wait for a peer).

## Docker Desktop, where the operator will hit it

The caveat is in `docs/build.md`'s Docker section rather than in a doc nobody reads: a container's published
port is opened by the Desktop VM's forwarder, so a port that looks "published" is reachable from the host
and not from the phone or the hub. That is why the control listener is the host's own process and why
`docker_host` on a dev machine is a `unix://` socket, and it is the reason the preview listener (HS-041)
belongs to the host too.

## What this story left alone

**Not `doctor`.** HS-045 owns the command, its `--quick`/`--full` modes, the Tailscale question, the port
question and the systemd question. This story owns the three checks this build can answer without them, and
the type HS-045 will print.

**Not the conformance suite's body.** `tests/backend_contract.rs` holds the two promises both backends must
keep today — `undo` is infallible for every step, and a failure names the thing it could not do — and each
later Docker story adds the case its behaviour needs. A suite that claimed to cover work no backend does yet
would be a test of nothing.

**No Docker operation implemented, no new config key, no schema change, no change to the default backend.**

## Consequences

- `crates/holosmith/tests/preflight.rs` proves, in six tests, against a machine it builds: a clean machine
  has no complaints and the daemon was really asked; an absent socket is one sentence naming both fixes; an
  old git and a full disk are reported together, each naming the key or the upgrade; a git that cannot run
  is a complaint; a daemon answering `500` is a complaint (a listener that merely accepts is not a pass); and
  every sentence leads with its check's word.
- `crates/holosmith/tests/backend_contract.rs` runs the same generic suite against the stub and Docker
  backends, so "the backend works" has one definition from here on.
- The gate adds `case_docker_preflight`: the shipped artifact with `docker_host` at a socket that cannot
  exist exits 1, prints the daemon sentence with its fix, and prints no banner. It is deterministic on a
  machine that *has* a daemon, which a case that waited for "no docker here" would not be.
