# The service, the keys and the origins are three more reads and applies

HS-049 is spec item 10 §"What `server deploy` does" steps **2**, **5** and **6**, plus step **7**'s body, and its
acceptance is five sentences about a real host: the service user, its group and the ownership that lets `serve`
write the tree; the VAPID keys written at first start and **never** regenerated; the unit installed, enabled and
started; both Tailscale origins configured; and "a wrong mode fails `doctor`". The demo is the same claim from the
other side: a fresh host ends with a working HTTPS URL on two ports, and still does after a reboot.

## What the story closed, and what it did not

Two items of step 2 were HS-047's leftovers — the service user and the VAPID pair — and steps 5, 6 and 7 were the
three that still said `skipped` after HS-048 filled step 3. **Nothing in `server deploy` is `skipped` any more, so
the machinery that said so is deleted rather than left to rot**: the owner table (`SERVICE`, `ORIGINS`, `HEALTH`),
`skipped()`, `Report::owners()` and the `deploy: incomplete: N of 7 steps belong to later stories` line, and step
2's `REMAINDER` remark, which named stories for two items the step now does itself. A `skipped` sentence means
"another story owns this body" (ADR-0056), so with no other story owning a step, every one of those sentences would
name an owner that does not exist.

Step 7 has no other owner in the backlog, and that is worth saying plainly because it looks like a step somebody
else might pick up: HS-050 is `status`, `logs` and `uninstall`, HS-072 is `hub deploy`, HS-078/HS-079 are
`upgrade`/`rollback`. The `/api/health` wait and the `doctor --full` that ends it are this deploy's own two
measurements, so leaving them skipped would have left the last step of a seven-step list with no story to fill it.

## D1 — the VAPID pair: one writer, one file, one child

The spec's own step-2 sentence says "Generate the VAPID keys", so step 2 is the writer. `serve` only reads, and the
consumer that signs with the pair is HS-066. "At first start" therefore means the first `server deploy` of this
installation: the second run reads the file the first one wrote.

The file is `<data>/secrets/vapid.toml`, mode 0600, inside the 0700 `secrets` directory the provider keys already
live in — and it is **not** `keys.toml`, because that file is the provider-key store `key set` and `key list` own
(ADR-0048), a VAPID pair is not a provider key, and `key set` never touches this path. What it holds is two
unpadded base64url strings: the 65-byte uncompressed P-256 point with its leading `0x04`, which is 87 characters,
and the 32-byte scalar, which is 43, under `public_key` and `private_key`, behind one header line that says why the
file is never rewritten. `vapid::parse` is strict in both directions — a line that is neither blank, nor a comment,
nor one of the two assignments is a refusal, and a value that is not 65 bytes starting `0x04` and 32 bytes is not a
pair — because a file somebody edited into something else must not be read as a key.

Generation is **one child whose stdout is the answer**: `openssl ecparam -name prime256v1 -genkey -noout -outform
DER`, followed by the same `install -m 0600` every other file action ends in (the plan's line names both children, so
the count it leads with is the number of processes the run will start). The DER is parsed in
this crate by `vapid::from_der` rather than by a dependency: the crate has no encoder dependency and no `unsafe` to
reach one with, the SEC1 walk is about forty lines (`from_der` and `Reader::element`), and it is unit-tested against
a DER built by hand from the shape the plan measured — `30 77`, version 1, the scalar as an OCTET STRING,
`prime256v1`'s OID as context `[0]`, and the point as context `[1]`'s BIT STRING behind a zero unused-bits byte.
`openssl` is a prerequisite this repository already had rather than a new one: `hs_key` spawns `openssl s_client`
for the TLS connection to the provider (ADR-0048), and the proxy image carries it (ADR-0057).

"Never regenerated" is three facts rather than an intention, and each has a test. **Absent is the only state that
generates**: the read answers `Action::Vapid` for a path that is not there and never for one that is. **A present
file that is not a pair is a failed step**, with a sentence that says it will not be regenerated because every push
subscription made against the old pair would stop working — the one place in step 2 where a wrong file stops the
run instead of being put right. And **the second run leaves the bytes identical**, which the gate proves with a
`cmp` and `tests/deploy.rs` with byte equality, the same pair of detectors `keys.toml` has.

The consequence is recorded rather than left for the consumer to find: **no `sub` claim is stored here.** A JWT's
subject is a claim, not key material, so HS-066 decides it and must read this file as it is; a shape change would
be a new file, not a rewrite of this one.

## D2 — the service user, the docker group and the chown

The unit runs `serve` as `User=holosmith`, and `serve` drives the machine's daemon through its socket, so the user
has to exist and has to be in the `docker` group. Step 2 reads `id -u holosmith` (exit 0 means the user exists)
and, when it does, `id -nG holosmith` for the group. The apply is `useradd --system --groups docker --home-dir
<data root> --shell /usr/sbin/nologin holosmith` when the user is absent, and `usermod -aG docker holosmith` when it
exists without the group. A spawn failure is a failed step rather than "there is no user": an `id` this machine does
not have cannot answer the question, and a deploy that guessed would run `useradd` over a user that is already
there.

Ownership is the second half and it is not decoration. A privileged install's writes are `sudo install`'s, so the
tree would be root's — while the unit's `User=holosmith` means `serve` writes the data root, the config file's
directory and the admin directory as the service user, which cannot write a root-owned directory. So the read is
`stat -c %u` against the uid `id -u holosmith` answered, and the apply is `chown -R holosmith:holosmith <data
root>` plus a non-recursive `chown` for `config.toml` and the admin directory. The admin directory is why all three
roots are compared and not only the data root: `/run/holosmith` lives on a tmpfs, so every reboot recreates it and
a privileged `install` recreates it root-owned; a read that looked only at the data root would leave a 0700
directory the service user cannot put its socket in. A run that is creating the user has no uid to compare, so its
tree is the chowns' too.

`--root` skips the user and the chowns, and **the line says so**: `; --root: no service user is created, because
the tree belongs to the invoking user`. That is the shape ADR-0056 gave step 2's old `REMAINDER` remark — a line
that reports a pass says what it did not read — kept and turned around, now that the step does all five of its
items.

## D3 — the unit's bytes are the deliverable

`<root>/etc/systemd/system/holosmith.service`, 0644, is rendered by one public function of exactly two facts: this
process's own path, and whether the install is privileged. A privileged install's bytes are:

```ini
[Unit]
Description=Holosmith server
After=network-online.target docker.service
Wants=network-online.target
Requires=docker.service

[Service]
Type=simple
User=holosmith
Group=holosmith
ExecStartPre=<exe> doctor --quick
ExecStart=<exe> serve
Restart=on-failure
RestartSec=5s
KillMode=mixed
TimeoutStopSec=30s

[Install]
WantedBy=multi-user.target
```

`User=` and `Group=` are the two lines `--root` **omits** rather than blanks: a unit in an install tree is not one
systemd can load, so naming a service user there would describe a machine this install does not have. The unit's
directory is an item of the same list — `install -m` does not make one, and under `--root` this step is the only
thing that ever asks for `/etc/systemd/system`.

**The `ExecStart`/`ExecStartPre` path is `std::env::current_exe()` rather than the literal `holosmith`, and that is
a recorded decision, not a missed word of the acceptance.** This binary does not install itself anywhere, so it
cannot promise what a bare `holosmith` resolved through systemd's `PATH` would be; a unit naming a command instead
of a path is a coin flip between the artifact the operator just built and whatever else answers to the name. The
acceptance's literal line is the argv *shape* — `doctor --quick` as `ExecStartPre` — which the gate greps as
`^ExecStartPre=.* doctor --quick$`, and the literal becomes true by `docs/build.md`'s by-hand procedure, which
copies the artifact to `/usr/local/bin/holosmith` and runs the deploy from there. Neither line carries `--config`,
because both commands' default is `/etc/holosmith/config.toml` and that is the file step 2 wrote at the path the
default names: a flag would be a second spelling of one default.

`satisfied` is the bytes **and** the machine. The file must equal the rendering, and on a privileged install
`systemctl is-enabled holosmith.service` must exit 0 — one word and one exit code, the question "would a boot start
this?", which is why it is the comparison rather than a parse of some other command's prose. The apply is `install
-m 0644` for the file, then `systemctl daemon-reload` and `systemctl enable --now holosmith.service`; `daemon-reload`
comes first because `enable --now` on a unit systemd has not re-read would start the previous rendering, which is
exactly the version an upgrade is replacing. Under `--root` the machine half is not attempted — a unit under an
install tree is not one systemd can read — and **all three arms** of the line (`satisfied`, `needs` and `changed`)
carry `ROOT_UNIT_REMARK` to say so, because a plan is exactly where an operator learns what a run will not do.

## D4 — `tailscale serve`: two children and one JSON read

The control origin is `tailscale serve --bg --yes --https=443 http://127.0.0.1:<control port>`, and the preview
origin is the same command with `--https=8444` and the preview port. `--bg` because a deploy may not block: the
plain form of `serve` runs in the foreground, and a step that waited on it would never reach step 7. `--yes`
because a first `serve` on a tailnet without HTTPS certificates enabled may ask a question, and a step cannot answer
one. Neither is `sudo`-prefixed, because `tailscale serve` writes to the daemon and not to a root path: privilege
follows the tree (ADR-0056), and this is a machine fact of the same kind as the docker socket, so `--root` does not
change it.

The read is `tailscale serve status --json`'s `Web` object, which is `ipn.ServeConfig` — one entry per
`"<host>:<port>"` whose `Handlers["/"]` carries the `Proxy` this machine already serves there. Only the missing
origin is applied; both present is `satisfied`. That is ADR-0035's rule a layer out — ask for the field rather than
parse the human text — and it is also what makes the second run honestly `satisfied` instead of re-applying
forever.

A Tailscale that cannot be asked is a **failed** step carrying the child's own line, because a deploy cannot
configure what it cannot read. Step 1's `tailscale` finding stays `unknown` on the same machine, and the two are
not in conflict: `unknown` is `doctor`'s word for "no verdict about whether `serve` may run here" (ADR-0054), and a
machine that serves by hand is a supported machine. This step asks a different question — "what does this daemon
serve?" — and an unanswerable answer to that is a step that failed, not a machine that is unfit.

## D5 — `perms` is a new check word, and the owner reference is the tree's

"A wrong mode fails `doctor`" is one fact, so it is one word in the one table (ADR-0028, ADR-0054):
`preflight::Check::Perms`, spelled `perms`, added to the check list next to `database` and asked of every role,
because a hub has a data root too. What it reads is the six installed paths with the modes the deploy writes — the
data root 0755, `secrets` 0700, `keys.toml` 0600, `vapid.toml` 0600, the admin directory 0700 and
`/etc/holosmith/config.toml` 0644 — as **one** finding, because a tree whose modes are wrong is usually wrong in
one way an operator fixes with one `chmod`.

Three outcomes. **`unknown`** when the data root itself is absent: a machine that has not deployed has no install
tree, and not having one is not a wrong mode — which is also what keeps `doctor` runnable on a host that serves by
hand. **`fail`** on a present path with a wrong mode, or with an owner that differs from the **data root's owner**,
with both values and the fix in the sentence (`` … is 0755 where `server deploy` writes 0700: chmod 0700 … ``).
**`pass`** otherwise, naming the paths that are not there yet, so a pass never reads as "everything was checked".

The owner reference is the tree's, and that is the decision: not `getuid` (unreachable without `libc`, and ADR-0056's
point that "may I write this tree" is a property of the tree) and not the string `holosmith` — the deploy writes the
tree as whoever ran it and then chowns it on the system path, so "every installed path belongs to the same user as
the data root" is the invariant that holds on every machine, `--root` included, without this module knowing that a
service user exists. **The limit is recorded rather than hidden**: the check asserts consistency and the modes the
deploy writes, not "the right user owns it". The latter is what the deploy's `chown` writes and what the unit's
`User=` names, and a check that claimed it would be reporting a fact it does not have.

The consequence is what makes the word worth adding: `doctor --quick` is the unit's `ExecStartPre`, so a wrong mode
now **stops the unit from starting** without teaching `serve` a refusal it did not have. And because `serve`
creates the data root itself, `commands/serve.rs` now sets that directory to 0755 — but only when it created it.
`create_dir_all` gives the process's umask (0775 under umask 0002), and a root this process made would fail the
*next* boot's `ExecStartPre` over a mode nothing had changed; an operator's own mode on a directory that was already
there is theirs. Three programs create that directory — `serve`, `key::Store` (`holosmith key set`, the documented
next step after a deploy) and the deploy's own step 2 — so all three apply the same rule, each only to a directory
it created.

Two consequences came out of the review of this story rather than out of its plan, and both are decisions now.

**The tree the finding reads is the *caller's*.** The last two paths are not derived from the data root — the config
file and the admin directory are machine-wide — so they are `doctor::Tools` fields (`config_path`, `admin_dir`)
whose defaults are `/etc/holosmith/config.toml` and `/run/holosmith`, and `deploy::run` overwrites them from the
`Target`. Spelling them inside the check made a `--root` deploy's step 7 compare the *host's* real install with the
*target's* data root: a false verdict about a tree the run never wrote, and an instruction to `chown` a file it did
not install, on every host that has deployed for real. A check that reads a tree has to be told which tree.

**Step 1 downgrades the finding, because the deploy is what repairs it.** `perms` in `--quick` is what makes a wrong
mode stop `ExecStartPre`; but the repair for it is step 2's `chmod` and `chown`, so a deploy that *failed* at step 1
would make its own next two steps unreachable and leave the operator to run one `chmod` it knows how to run. So
`perms` joins `ports` as the second finding step 1 turns into a remark, with its own sentence, and `doctor`'s own exit
code is unchanged. That is a widening of ADR-0056's "exactly one divergence", recorded here rather than done quietly:
the rule that survives is "a finding the deploy is *about to fix* is a remark; a finding it cannot fix stops the run",
and `tests/deploy.rs` has an arm for each half of it (the wrong-mode fixture is repaired, and a failing `docker` still
stops at step 1).

## D6 — step 7: the health wait, then `doctor --full`

The read is two questions with one answer: the control port answers `GET /api/health`, **and** `doctor::run(config,
Depth::Full, …)` has no failing finding. It runs in-process, so the sentences are `doctor`'s own — step 1's rule one
step later, because a deploy, `serve`'s refusal and `doctor`'s line must not say different things about one machine
(ADR-0028, ADR-0054). The apply is a bounded wait — `HEALTH_WAIT`, 60 s, asking every `HEALTH_POLL` 250 ms — and
then `doctor --full`, whose first failure becomes the step's `failed` sentence verbatim.

The poll is the public `wait_for_health(listen, bound)`, and the bound is a parameter rather than the constant so
both arms are tested in **milliseconds** instead of a minute; a wait that took sixty seconds to prove it gives up is
a test nobody runs. `doctor`'s port check asks the same `health_answers` one attempt at a time, so "the endpoint
answers" means one thing wherever the deploy or a report says it. `Tools::health_wait` is the **one non-path field**
in that seam, and it exists for the same reason — the deploy is the caller a test must be able to shorten — which
its doc comment says, rather than leaving a reader to wonder why a `Tools` of programs holds a duration.

The price is recorded rather than hidden: **step 7's read runs `doctor --full`, so a dry run runs the network
self-test too.** A plan that read nothing is not a plan (ADR-0056) — step 6's origins and step 7's health are
questions only the machine can answer — and `--full` is what "no finding fails" means at that moment. The self-test
removes the network it makes (ADR-0044), so a dry run leaves no state behind; what it costs is seconds, and the
alternative is a plan describing a machine nobody measured.

## D7 — the five words stay, the owner table goes

`satisfied`, `needs`, `changed`, `skipped` and `failed` are ADR-0056's contract and they are unchanged: a dry run's
plan is still `needs`, a failed step still stops the run, and the resume is still the command that just ran. What
goes is the machinery that used `skipped` as an owner. `Word::Skipped` stays in the public enum with **no producer
in this release** — deleting a word from a contract to make a release tidy is a bigger change than the release —
and the tally keeps printing `skipped` as its fourth count, always zero, because a gate greps the line's shape:
`deploy: 7 steps: N satisfied, N changed, N skipped, N failed`. A word that no longer occurs and a word that was
removed are different facts, and the enum keeps the first.

## D8 — what the gate proves

`case_server_service` is the new case, and it owns this story's subjects in two halves that skip independently. Two
shared helpers come with it, because both cases need them: `deploy_machine_shims` writes a `tailscale` stand-in that
records every argv line and answers `serve status --json` from the `<port> <target>` pairs it was asked to serve —
so a second run finds both origins, which is the whole point of the read — and `health_responder` starts one
`python3` process answering `200` to `/api/health` for the length of the case, because this machine has no systemd
and no unit boot, and something has to hold the port for step 7 to be a measurement rather than a minute's wait.

The **offline** half needs `python3` and nothing else. It builds the install tree by hand with `install -d` and
`install -m`, so every mode is this case's own choice rather than a umask's, and reads `doctor`'s `perms` three
times: a `pass`, a `fail` after `chmod 0755` on the secrets directory carrying both `0700` and `0755` and the fix in
one sentence with exit 1, and a `pass` once the mode is back. Then it dry-runs the deploy over a `_ping` socket and
asserts the plans for steps 5, 6 and 7 — the unit's path, both `--https` targets and the wait — that **nothing** is
`skipped`, and that the report has no `deploy: incomplete` line. The **online** half needs the daemon and an image:
a real `--root` install asserts `2. files: changed:`, `5. service: changed:`, `6. origins: changed:` and `7. health:
satisfied:`; the VAPID file at 0600 with an 87-character and a 43-character base64url field; the unit at 0644
carrying `Restart=on-failure`, both `ExecStartPre`/`ExecStart` lines, `WantedBy=multi-user.target` and **no**
`User=` (that is `--root`'s half); and the shim's log carrying both `--https` targets. The second run is all seven
steps `satisfied`, with the VAPID bytes and the unit bytes identical under `cmp` and **no** second `serve --bg` line
in the log — the read answered, so nothing was applied twice.

A case that skipped on "no systemd" would skip on precisely the thing it exists to catch: the unit's bytes and both
origins are a file and two argvs, and both are measurable without a systemd to load the unit or a `tailscaled` to
answer for it. So the gate measures what this machine has, and `tests/deploy.rs` drives the rest over recorders.
**The system-path user argv is the test's** — `useradd --system --groups docker --home-dir … --shell
/usr/sbin/nologin holosmith`, `usermod -aG docker holosmith` when the user exists without the group, the three
`chown`s, `systemctl daemon-reload` and `systemctl enable --now holosmith.service`, each `sudo`-prefixed and none of
them executed — because a gate may not create a system user, and a case that tried would be a case that only passes
as root.

## What this machine cannot measure

No systemd (`/run/systemd/system` is absent, and `systemctl` answers `System has not been booted with systemd`), so
**no unit is ever loaded and no boot happens**: step 5's file half is measured, and `is-enabled`, `daemon-reload`
and `enable --now` are `tests/deploy.rs`'s recorders. No `tailscaled`, so **the origins exist only in the
stand-in's answer** — the two argvs are real and the state they configure is not. No `useradd`, so **the service
user and the three `chown`s are recorders too**. And no reboot, which is the one measurement only a reboot can make:
that the unit comes back, and that `/run/holosmith` is recreated and chowned for the user that will put its socket
in it.

The "reachable over HTTPS" half of the story's demo is therefore `docs/build.md`'s by-hand procedure on a real host,
and both this ADR and the ship note say so. What is proven here is the plan, the bytes, the two argvs, the pair's
identity across two runs, the finding that turns a wrong mode into a startup refusal, and the handoff from a dry run
to a real one — which is everything about this story a machine without systemd, without a Tailscale daemon and
without `useradd` can hold.
