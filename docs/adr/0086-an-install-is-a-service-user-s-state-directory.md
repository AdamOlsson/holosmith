# An install is a service user's state directory, and the deploy never escalates

An install today is the machine's own paths — `/etc/holosmith`, `/var/lib/holosmith`, `/run/holosmith`,
`/opt/holosmith`, `/etc/systemd/system/holosmith.service` — written by `server deploy` as root, one
`sudo`-prefixed child per file (ADR-0056), plus `useradd`, `usermod -aG docker` and three `chown`s to hand the tree
to the `holosmith` user it just created (ADR-0058 D2). That is four conventions and one escalator for a program whose
whole job is to hold state for one user and talk to two sockets. This ADR replaces the paths, the escalator and the
ownership step with the shape every packaged daemon on a systemd machine already has: **one service user, one state
directory systemd keeps correct, a system unit, and a deploy that prints privileged commands instead of running them.**

It also settles what `--root` meant. `Target` (`src/deploy.rs`) prefixed every installed path under a root and
reported `privileged()` from "no root was given", which made the same program two installs with two privileges and
one caveat — "a unit in a tree is not one systemd can load" (ADR-0082). With one layout there is one root, named by
`HOLOSMITH_ROOT` on a machine, and nothing is prefixed.

## Measured before deciding

On the build machine: `id -u` is `1000`; `/etc/login.defs` has `UID_MIN 1000`, so `useradd --system` takes its uid
from below that (the existing system accounts on this machine run 119–123). The systemd is 255, which has every
directive this ADR uses: `StateDirectory=` and `RuntimeDirectory=` (v211+), `ProtectHome=` (v214+) and
`ProtectHome=tmpfs` (v233+).

From `systemd.exec` (v262), three facts that decide the spelling rather than decorate it:

- **`ProtectHome=yes` cannot be combined with a re-exposed path.** "It is not possible to use those options for
  mount points nested underneath paths specified in `InaccessiblePaths=`, or under `/home/` and other protected
  directories if `ProtectHome=yes` is specified" — and the doc names the alternative in the same paragraph: `tmpfs`
  "is useful to hide home directories not relevant to the processes invoked by the unit, while still allowing
  necessary directories to be made visible when listed in `BindPaths=`".
- **`StateDirectory=` is created for `User=`.** "when the unit is started, one or more directories by the specified
  names will be created … below `/var/lib/`" — considered and **not used**: it creates a directory whose name is
  fixed, while the root is `HOLOSMITH_ROOT`, and the deploy already writes the root as its owner.
- **In a *user* unit, this whole family implicitly enables `PrivateUsers=`** ("only available for system services, or
  for services running in per-user instances of the service manager in which case `PrivateUsers=` is implicitly
  enabled (requires unprivileged user namespaces support)"). A **system** unit with `User=holosmith` needs none of
  that, which is one of the reasons the unit stays a system unit below.

## D1 — the layout: one root, and the service user's home is it

```text
/var/lib/holosmith                 holosmith:holosmith 0700   the root: everything a machine has
  config.toml                                                 the deploy's own rendering
  data/                             holosmith:holosmith 0755   `data_root`: everything the binary writes
    app.db, app.db-wal, app.db-shm                            the database
    secrets/                        holosmith:holosmith 0700
      keys.toml                                             0600  providers + per-session dummies
      vapid.toml                                            0600  the push pair, never regenerated
    sessions/<id>/, merge/<id>/, backups/
  releases/<tag>/, current                                    the release tree
  projects/<name>/.worktrees/<id>                             the repositories sessions are made from
  data/run/                         holosmith:holosmith 0700   the admin socket's directory, made by `serve`
/etc/systemd/system/holosmith.service   root:root 0644        the unit, installed by the operator
```

Four children, not three, and `data/` is one of them: `config` refuses a `projects_root` that is equal to or inside a
`data_root`, because "deleting data must never touch a repository" — so the projects cannot sit beside the database in
one directory, and the root is the parent of both. `data_root`, `projects_root` and the release tree are the three
paths the config and the code already name, each now with its default under the one root.

`HOLOSMITH_ROOT` names the root; the unit's `ExecStart` names the config explicitly
(`serve --config /var/lib/holosmith/config.toml`), so the daemon never resolves `~`, never reads the environment and
never disagrees with the CLI about where the machine lives. `--root DIR` stays as the *same value's* flag form, which
is what the gate hands a test's own tree; it is no longer a second privilege mode because there is none to be in.
The service user's home is `/var/lib/holosmith` itself (`useradd --system --create-home --home-dir /var/lib/holosmith
--shell /usr/sbin/nologin holosmith`). That is the spelling `adduser --system --home /var/lib/foo` has used for
twenty years, and it makes `~` and the root agree: `~holosmith` *is* the data root, so a `git` run by the service
user writes `.git-credentials` inside the one directory, and a backup of "the machine" is a backup of one path.

**`~/.holosmith` was the alternative and is refused** — not because `/var/lib` is prettier, but because with a second
user the tilde stops being a fact. `sudo -u holosmith` preserves the *caller's* `HOME` unless `-H` is passed, so a
root of `~/.holosmith` resolves to `/home/operator/.holosmith` for the human and to `/home/holosmith/.holosmith` for
the service, and the two would disagree exactly when the deploy is run as the service user — which is the only way a
deploy can write a tree the service user owns. A default that depends on who is asking is a bug with a friendly name.

## D2 — the deploy prints; it never escalates

`server deploy` becomes a program that assumes it is already the right user. Its privileged work is **printed as
exact argv** on the step's own line (`needs:` with a list, ADR-0085's shape) and **verified** on the next run: the
user exists, the user is in `docker`, the root exists with the right owner and mode, the unit file is the bytes this
binary renders, `systemctl is-enabled` says what a boot would do.

That deletes `Tools::sudo`, `Tools::{useradd, usermod, chown}`, the `UserAdd`/`UserMod`/`Chown` actions,
`running_as_root()` and `privilege_problem()`, and the reason ADR-0056 gave for the child inheriting stdin ("so
`sudo` can ask for a password on the operator's terminal"). The four commands an operator types, once:

```sh
sudo useradd --system --create-home --home-dir /var/lib/holosmith --shell /usr/sbin/nologin holosmith
sudo usermod -aG docker holosmith
sudo install -d -o holosmith -g holosmith -m 0700 /var/lib/holosmith
sudo install -m 0644 <the unit deploy rendered> /etc/systemd/system/holosmith.service
sudo systemctl daemon-reload && sudo systemctl enable --now holosmith
```

`sudo -u holosmith holosmith deploy` is the run itself, and `sudo -u holosmith holosmith key set <provider>` is the
secrets step — `sudo -u postgres psql`'s shape, and the only way a 0600 file in a 0700 directory is written by its
owner.

**A printed plan is not a README checklist.** The four ways this step goes wrong silently — no `--create-home`, a shell
left on `/bin/bash`, a uid that collides, a `usermod` the running session never picked up — are exactly what the
following run's reads catch, which is why the print and the verify are one change and not two.

## D3 — the socket moves inside the root, and nothing has to repair it

The admin socket is `<data root>/run/drain.sock`, and `serve` creates that directory 0700 when it binds. That is
ADR-0058 D2's `/run` problem solved by refusing to have one: that decision had to `chown` the admin directory because
"`/run/holosmith` lives on a tmpfs, so every reboot recreates it, and a privileged `install` recreates it root-owned".
A directory inside the root is on the root's filesystem, is created by the user who owns it, and survives a reboot as
it is — so the deploy stops managing it altogether, and one `Item` with its mode and its `chown` goes away.

`StateDirectory=` was the alternative (systemd creates `/var/lib/holosmith` and repairs its ownership at every start)
and is refused for one reason: the name is fixed while the root is not, so a machine with `HOLOSMITH_ROOT` set
elsewhere would get a second, empty directory created beside its real one.

`ConfigurationDirectory=holosmith` (`/etc/holosmith`, root-owned, service-readable) is **refused**: the config is the
deploy's own rendering and is rewritten when it differs, which needs a directory the service user can write. It lives
in the root, one file beside the state it describes, and the FHS's `/etc` gives up nothing here because nothing else
on the machine reads this file.

The unit also carries the cheap hardening a system unit can have, with no user namespaces involved: `NoNewPrivileges=`
yes, `PrivateTmp=yes`, `ProtectSystem=strict`, `ProtectHome=yes`, `ReadWritePaths=/var/lib/holosmith`, and
`UMask=0077`. `ProtectHome=yes` is free only because the root is not under `/home` (D1), and it is defence in depth
rather than the control: the uid is what keeps the operator's home out of reach. It is also the one part of this ADR
`scripts/verify.sh` cannot measure — a gate with no root cannot boot a systemd unit — so the set is deliberately the
directives whose failure would be loud at the next `systemctl status`, and the gate checks the file's bytes only.

## D4 — one sudoers line, so a promotion can still put itself back

`upgrade` and `rollback` write the release tree (the service user's, fine) and then restart the unit (root's). Without
that restart they cannot poll for a process younger than it and cannot revert a release that does not answer, which is
ADR-0083's whole guarantee. So the machine carries one line, installed by the operator once:

```text
# /etc/sudoers.d/holosmith
holosmith ALL=(root) NOPASSWD: /usr/bin/systemctl restart holosmith.service
```

One exact program, one exact argument, one unit. A `polkit` rule for `org.freedesktop.systemd1.manage-units` would
avoid `sudoers` and is refused as more machinery for a narrower, less auditable result: a rules file whose failure
mode is a silent denial, where this one's is a sentence from `sudo -l`. A machine without the line is supported rather
than broken — `upgrade` switches the release, says the restart is the operator's, and does not claim the health check
it did not make.

## D5 — Tailscale leaves the deploy

Step 6 configured two `tailscale serve` origins. Tailscale is a layer above this machine, not part of it: the deploy's
list becomes six steps, and `DeployStep::Origins`, `CONTROL_HTTPS`, `PREVIEW_HTTPS`, `served()` and its
`ipn.ServeConfig` parse are deleted with their gate cases. The documentation's origin wiring moves to
`docs/quickstart.md` as the operator's own step, where it already reads like one.

## Refused

- **Keeping both layouts (system and `--root`).** Two roots is two of everything: two privilege modes, two unit
  spellings, two sets of paths to test, and the caveat that one of them cannot be loaded by systemd. The gate's
  install cases already build their own trees under `--root`, so one layout is where they already point.
- **`~/.holosmith`** — D1's tilde argument: `sudo -u` keeps the caller's `HOME`, so the root would mean two
  directories.
- **A user unit plus `loginctl enable-linger`.** Genuinely rootless on stock polkit (`set-self-linger` is `allow_any
  yes`), and refused because it drags `PrivateUsers=` and unprivileged user namespaces into every sandbox directive,
  hides `/run/user` under `ProtectHome=`, and buys nothing once the operator is typing `sudo` for the user anyway.
- **`/etc/holosmith` for the config.** See D3.
- **The deploy running as the operator and `chown`-ing afterwards.** That is the shape being deleted: it needs
  `sudo`, it needs a uid to chown to, and it writes root-owned files into a tree a service user must then own.
- **`polkit` instead of the sudoers line.** See D4.
