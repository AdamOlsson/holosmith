# A root invocation is refused, before any verb

`sudo ./holosmith status` used to work. It read root's config, opened root's database and then wrote root's files, and
the damage has no sentence of its own: the `0600` secrets and the SQLite files end up owned by root, `doctor`'s
permission check fails from then on against files the operator can no longer read, `deploy`'s directory actions
(`install -d -m` with no owner) answer "changed" on every run because the directory is no longer the operator's to
change, and git refuses the repository with `dubious ownership`. A session's container takes its `--user` from the
data root's owner (ADR-0087), so one root-owned directory is also enough to start uid-0 containers.

ADR-0088 made the account the operator's own, which is what turns all of that into a fact about **one uid**: there is
no second account whose files these could be, so a root-owned file in an install is a file that nothing but root will
ever read.

## Measured before deciding

- **A uid-0 process is readable as one, and the refusal is one line.** `bwrap --unshare-all --uid 0 --gid 0`
  gives a process whose `/proc/self` reads `0`; this build there prints `this runs as you, drop the sudo: holosmith
  …` on stderr, nothing on stdout, and exits 1 — for `status`, `server deploy --root /x`, `key set opencode-go`,
  a bare invocation and `--help` alike.
- **The proxy image's own user was root, and the egress proxy names no `--user`.** `docker run --entrypoint id <proxy>
  -u` read `0`; with the `USER` line below it reads `65534`. A `0600` file owned by the operator is `Permission
  denied` to that user under `--cap-drop ALL`, and readable with `--user $(id -u):$(id -g)`.
- **Bubblewrap maps the caller's uid to itself**: inside `--unshare-all --tmpfs / --proc /proc`, `id` and a `stat` of
  `/proc/self` both read `1000` on this machine, so the empty-root run `scripts/check-static.sh` makes is not refused
  — and does not need to be.

## The decision

**A process whose effective uid is 0 is refused before anything is dispatched.** `main` asks
`identity::root_refusal(identity::euid())` before `Cli::parse`, prints one sentence to stderr and exits 1:

```text
this runs as you, drop the sudo: holosmith status
```

- **Every verb, and no exemption.** The check is before the command line is parsed, so a verb cannot opt out and a
  flag cannot ask to — even `--help` is refused. The decision has nothing to exempt a verb *with*: it takes a uid and
  the words that were typed, and those words are only ever printed back.
- **The sentence names the command to type**, not the mistake. What an operator who typed `sudo` needs is the line to
  type without it, so everything after the program name — verb, subcommand and flags — is carried through unchanged,
  and a bare `holosmith` ends the sentence at the program name.
- **A uid that cannot be read is not refused.** `std` has no `getuid`, this crate denies `unsafe` and carries no
  `libc`, so the uid is `/proc/self`'s owner — the read `identity` already had. A `/proc` that cannot be read answers
  `None`, and `None` is never root: refusing on an unread identity would refuse a supported run.
- **The decision takes a uid, so both answers are testable.** `identity::root_refusal(uid, command)` sits beside
  `euid()`, and `tests/root_refusal.rs` enumerates clap's own verbs: the sentence is the right one for every name clap
  knows, and a verb added later is measured without an edit. That no verb can be exempt is the structural argument
  above — the check precedes `Cli::parse`, which is where the suite points rather than what it asserts.

## What it cost

- **The proxy image declares its own user.** `holosmith proxy egress` runs from this binary inside a container, so the
  refusal stops *it*: `proxy.Dockerfile` was root and the egress proxy passes no `--user`. The image now carries
  `USER 65534:65534`. It costs the egress proxy nothing — it mounts no host path and writes no file — and it removes
  the last container in this build whose uid was 0 by default.
- **The key proxy's `--user` is now load-bearing.** `KeyProxy::run_args` already named the key store's owner, because
  the store is a `0600` file and `--cap-drop ALL` takes `DAC_OVERRIDE`; that flag now stands between a working key
  proxy and one that cannot start, and it overrides the image's user. `scripts/verify.sh`'s `case_server_images` boots
  that container by hand against its own `0600` file, so it passes `--user "$(stat -c %u:%g <file>)"` — the owner,
  the way the flag itself derives it — and it asserts that the proxy image's own `id -u` is not 0, the same question
  it already asks of the agent image.
- **One hint changed its advice.** `config`'s permission-denied sentence used to say a `sudo` reads root's home. A
  `sudo` never gets that far now, so the sentence names the file and what to do to it — make it this account's — rather
  than a second account to run as.

## The honest limit, and what is not measured

- **The read is procfs', so it answers for the namespace the process is in.** Bubblewrap maps the caller's uid to
  itself, so the empty-root run reads `1000` and is not refused — correct, because that run is not root in any
  namespace that owns a file. A namespace that maps no uid the process can name shows procfs's overflow uid instead
  of 0, so that shape is not refused either; this machine cannot create one (`unshare -r` fails here with `write
  failed /proc/self/uid_map: Operation not permitted`), so that half is procfs's documented behaviour rather than a
  measurement. What the guard refuses is a process that can write root-owned files here.
- **A gate case is the refusal, and it does not make the gate root.** `case_root_refusal` imports a root filesystem
  holding the shipped artifact — a static binary needs no base image — and runs it as uid 0 in a container, which is
  the one uid the decision is about; `tests/root_refusal.rs` still covers the decision at its own layer, for every
  verb clap knows. What neither measures is a host run as root, and the namespace whose procfs shows the overflow uid
  (above) is still not a refusal.
- **The command modules stopped telling an operator to type `sudo holosmith …`, and the docs followed.** The
  promotion's restart moved to the operator's own manager, so `crates/holosmith/src/commands/upgrade.rs` and
  `rollback.rs` now describe a promotion that checks no account and escalates nothing, and the
  `deploy::privilege_problem` this decision replaced is gone from the tree. `crates/holosmith/src/identity.rs` and
  `main.rs` still spell `sudo holosmith …`, in explanation rather than instruction — what the shell would have run,
  and why uid 0 is refused before it reaches a default path — which is the right place for it, and the refusal's own
  sentence is built from the typed words. `README.md`'s day-two verbs, its tailnet steps and the install prose that
  ADR-0088 replaced have since been swept: the only `sudo` left in the operator-facing path is the `usermod` that puts
  an account in the daemon's group, which is root's work by definition.

## Consequences

- A `sudo` on any verb is one sentence and exit 1, and a machine that already has root-owned files keeps them: this
  refuses the run that makes the mess, and repairing one is `chown`'s.
- The proxies start as non-root for two different reasons — the image's `USER` for the egress one, the key store's
  owner for the two that read it (`hs_key` and `hs_mcp`) — and each is pinned where it can fail: the image by a gate
  assertion, the flag by a unit test that compares it with the file's owner.
