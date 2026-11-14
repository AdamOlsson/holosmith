# An install is the operator's own account, and the unit is a user unit

ADR-0086 made an install a **service user's state directory**: one `holosmith` system account created with `useradd`,
one root under `/var/lib` handed to it with `chown`, a system unit carrying `User=holosmith`, and an operator who types
`usermod -aG docker`, `sudo -u holosmith holosmith deploy`, `sudo -u holosmith holosmith key set` and one
`/etc/sudoers.d` line so that a promotion can restart the unit it has just switched. The property it was arguing for —
one state directory, one path to back up and one to delete — was right. The account it hung that state on was not.

Holosmith's own first sentence is "runs coding agents in containers on machines you own, and lets you drive them from a
browser or phone": one operator, their own computer, their own repositories, their own git credentials, and the only
person who can read the transcript. A service account is the shape for a daemon whose *administrator* and whose
*beneficiary* are different people — a database on a shared host. On a machine you own there is no second person, so
what the account buys is a boundary between the operator and the operator, drawn with the one tool that makes an
install go wrong quietly.

ADR-0086 counted that cost and mis-assigned it. Its Refused section rejects a user unit on three grounds — "it drags
`PrivateUsers=` and unprivileged user namespaces into every sandbox directive, hides `/run/user` under
`ProtectHome=`, and buys nothing once the operator is typing `sudo` for the user anyway". The first is real and is D3's
subject; the second is real too, and it is one of the reasons no `ProtectHome=` spelling survives there; the third is
the one that is wrong, and it is the one the decision rested on. `sudo` is not an independent fact about installs that
a service user might as well be paid for — the service user is precisely what *causes* it: every privileged command in
that ADR's list exists to create an account, join it to a group, hand it a tree, or put a unit where only root may
write.

So this ADR reverses D1–D4 of ADR-0086. The state is `~/.local/share/holosmith`, the config is
`~/.config/holosmith.toml`, the daemon is a **user** unit kept alive by `loginctl enable-linger`, restarted with
`systemctl --user restart` and read with `journalctl --user`; the `holosmith` user, `useradd`, `usermod`, `chown`,
every `sudo -u holosmith` and the sudoers line are deleted. ADR-0086 D2's *principle* — a deploy escalates nothing —
is kept, and now there is nothing left for it to escalate.

What the reversal costs is measured, and it is not small: a user unit carrying any mount or namespace directive gets
`PrivateUsers=` whether it asked for it or not, and the mapping that implies makes the sandbox **grant** privilege
rather than remove it. The unit therefore carries fewer directives than ADR-0086's did, and D3 states that as a loss
rather than as a judgement about who needs protecting from whom.

## Measured before deciding

On this machine, systemd **255.4-1ubuntu8.18**, with the operator at uid `1000` and the account in the `docker` group.
One user unit, its `ExecStart` running a probe in place of the daemon, one directive set at a time — the probe prints
its own `uid_map`, its group list, and the result of reading `/etc/shadow`:

| Directives in a **user** unit | `uid_map` | groups inside | `/etc/shadow` |
| -- | -- | -- | -- |
| none, `NoNewPrivileges`, `UMask`, `LockPersonality`, `RestrictSUIDSGID`, `RestrictNamespaces`, `SystemCallFilter`, `RestrictRealtime` | identity | full (incl. `docker`) | denied |
| `PrivateTmp`, `ProtectSystem=strict`, `ProtectHome=tmpfs`, `ProtectKernelTunables`, `ProtectControlGroups`, `ProtectHostname` | `1000 1000 1` | `65534`×8, `1000` | **readable** |

**Any one of the second row's directives is enough**, not the whole list: each of them is implemented with a mount or a
network namespace, and in a *per-user* service manager that implicitly enables `PrivateUsers=` — the sentence
`systemd.exec` carries, and the one ADR-0086 already quoted when it chose a system unit. What ADR-0086 did not follow
that sentence to is what the mapping then does:

- **Only the process's own uid and gid stay mapped.** Host files it does not own read as `nobody:nogroup`, and every
  supplementary group it had is gone — the `docker` group included, which is how a sandbox directive would break the
  daemon's only socket.
- **The process ends up holding gid `65534`**, and `/etc/shadow` (0640 `root:shadow`) is readable from inside the unit.
  The directory that a directive was added to protect became a file the sandbox opened: **the sandbox grants
  privilege.**
- **`PrivateUsers=no` beside the directives does not opt out** — tested, and it is the obvious escape hatch, so the
  choice is the whole row or none of it.
- **`loginctl enable-linger` succeeds unprivileged** (polkit's `set-self-linger` is `allow_any yes`), so nothing about
  a user unit needs root to become a boot service.
- **`systemctl --user restart` works, and `systemctl --user show -p ActiveEnterTimestamp --value` answers**, so the
  user manager is scriptable in the two ways a promotion needs (ADR-0083 D7's restart-then-poll).

## D1 — the two paths are XDG's, and the uid that owns them is the operator's

```text
~/.config/holosmith.toml                    0644  the config: the file a person edits
~/.config/systemd/user/holosmith.service    0644  the unit
~/.local/share/holosmith/                   0700  the root: the state
  data/                                           `data_root`: app.db, secrets/, sessions/, merge/, backups/
  data/run/                                 0700  the admin socket's directory, made by `serve`
  releases/<tag>/, current                        the release tree
  projects/<name>/.worktrees/<id>                 the repositories sessions are made from
```

The root keeps ADR-0086 D1's shape — one directory, `data/` a child of it rather than the whole of it, `projects`
outside the data — and changes who owns it: the account that runs the daemon, which is now the operator's own, so
nothing is `chown`ed and no `install -d -o` is needed for the parent. The modes carry over unchanged, and only their
subject moves: 0700 now means what it means everywhere else on the machine — another local account cannot read the
secrets directory.

**The config moves out of the root, to `~/.config/holosmith.toml`.** ADR-0086 kept it inside because it was "the
deploy's own rendering" and a service user had to be able to rewrite it; with one uid, that argument expires and XDG's
split is the better description of the two files: `data/` is state — written by the daemon, backed up, deleted,
regenerated — and the config is the one file in an install a person wrote. It is also the file that sits beside the
unit under `~/.config`.

`HOLOSMITH_ROOT` still names the root and `--root DIR` is still the same value's flag form rather than a privilege
mode, as ADR-0086 D1 decided. The unit's `ExecStart` still names the config explicitly, so the daemon never resolves
`~` and never decides its paths from who ran it; D4 is the rule for the commands that do have to.

ADR-0087's decision is untouched — the uid is still read from the data root — while its sentences still name `id -u
holosmith`, an account this ADR removes. What it reads is a different account in any case: a session's container still
runs as the owner of the data root, which is now the operator's own uid — `1000` on the machine that made its
measurement, and whatever the passwd entry says anywhere else, because it is read rather than assumed. The property
ADR-0087 needed ("the host can commit what the container wrote") is if anything easier to see, since the host user and
the image's `node` user no longer have to be distinguished by a paragraph.

## D2 — the unit is a user unit, linger is the boot, and the sudoers line goes

The unit is written to `~/.config/systemd/user/holosmith.service`, enabled with `systemctl --user enable --now
holosmith`, and ordered `WantedBy=default.target`. It loses `User=` and `Group=` because a user unit runs as its
owner, and it keeps `ExecStartPre=<exe> doctor --quick`, the pre-line ADR-0084 D4's release finding is written for;
it works the same through the user manager.

**`loginctl enable-linger` is what turns it into a boot service**, and a machine without it starts the daemon only
while a session is open — which is not an install. It is measured unprivileged, so the deploy sets it rather than
printing a command for the operator to copy, and the set of commands ADR-0086 D2's rule leaves to print is now empty.
`journalctl --user -u holosmith` is the log, one flag away from the system spelling.

**The sudoers line is deleted, and with it ADR-0086 D4's whole argument.** That decision existed for one reason: a
promotion writes the release tree as its owner and then restarts the unit, which was root's, so the machine carried
`holosmith ALL=(root) NOPASSWD: /usr/bin/systemctl restart holosmith.service`. `systemctl --user restart` is the
operator's, so `upgrade` and `rollback` keep ADR-0083 D7's restart-and-poll with **no privilege at all** — no `-n`, no
`Tools::sudo`, and no sentence in the failure path telling an operator to edit `/etc/sudoers.d`.

One guarantee does not survive the move and is recorded rather than papered over: **a user unit cannot order itself
after a system unit.** The rendered unit's `Requires=docker.service`/`After=docker.service` mean nothing to the user
manager, which does not own that name, so "the daemon starts after Docker" becomes "the daemon starts, finds no daemon,
is refused by the preflight, and `Restart=on-failure` tries again". That is a retry loop where the system unit had an
ordering, and this ADR has not measured a boot where the two managers race — it is the one thing about the move that a
later release should look at on a machine that boots with both.

## D3 — the unit keeps only the directives that need no namespace, and the loss is a loss

The directive set is the measurement table's first row: `NoNewPrivileges=yes`, `UMask=0077`, `LockPersonality=yes`,
`RestrictSUIDSGID=yes`, `RestrictNamespaces=yes`, `SystemCallFilter`, `RestrictRealtime=yes`. Five of those the system
unit did not carry, so in that one respect the new unit is stronger; `RestrictNamespaces=yes` is in the set for the
plain reason that it is the one directive that *forbids* the thing systemd has already done for the unit.

Three of the table's second row are gone, and what they gave is not recoverable: `PrivateTmp`,
`ProtectSystem=strict` with its `ReadWritePaths=`, and `ProtectHome=`. Those are the whole of that row ADR-0086's unit
actually carried; the other three — `ProtectKernelTunables`, `ProtectControlGroups`, `ProtectHostname` — were
candidates this ADR rules out, never directives the install had. Not because the root is now under `/home` and
`ProtectHome=tmpfs` would hide the state — although that is true, and `unit_text` already drops `ProtectHome=yes` when
a root starts with `/home` — but because any one of them brings the mapping, and the mapping opens `/etc/shadow`, drops
the `docker` group and reads every host file the process does not own as a stranger's.

**So the loss, stated as a loss:** what is gone is the defence against a compromised `serve` writing to `/etc`.
`ProtectSystem=strict` was the guard and `ReadWritePaths=` its one exception, and no version of this unit carries them —
where ADR-0086 could say a server's writes were confined to its root, this one can only say the daemon is a non-root
uid that cannot escalate (`NoNewPrivileges`, `RestrictSUIDSGID`) and cannot make a namespace of its own
(`RestrictNamespaces`). Nor is the boundary that `ProtectHome=yes` was *defence in depth* for still there: **the uid was
always the control, and it is now the same uid**, so "the daemon cannot read the operator's home" is not a property
this build has at any directive setting. The honest accounting is that the install traded a boundary nobody on a
single-user machine was crossing for the deletion of a second account, the five privileged commands ADR-0086 D2
printed and the sudoers line ADR-0086 D4 added — and that the trade was made on the uid, in D1, not on the sandbox.

## D4 — the home is the password entry's, never `$HOME`

A default path under `~` needs a home directory, and there are three ways to get one. The rule is: **the home of the
uid the process is running as, from the password database** — not the `HOME` environment variable, and not a
compile-time guess.

`HOME` is not a fact about the account: `sudo` makes it whatever its policy says — the invoking user's by default, the
target's under `-H` or `set_home` — and an `ssh` can forward one while a shell can export anything. The old spelling
would therefore let one typed `sudo holosmith …` read a different config depending on a setting nobody in that sentence
controls — the invoker's `~/.config/holosmith.toml` by default, the target's under `-H` — and would let a forwarded
`HOME` point a command at a tree its uid does not own. The password database has no such freedom: `sudo holosmith …`
reads root's entry because it *is* root, so "which home" and "which uid" are one fact, and the uid is the fact that
decides what the process may read anyway.

The same argument covers `XDG_DATA_HOME` and `XDG_CONFIG_HOME`: they are environment variables, and a variable exported
in a shell is not the environment a user manager was started in, so an operator who keeps their data elsewhere says so
with `HOLOSMITH_ROOT`, `--root` or `--config` — the three spellings that already exist — rather than with a variable
that half the processes on the machine will not have.

Under systemd the two answers agree, because the user manager sets `HOME` from the account; the rule exists for the run
that is not under systemd, which is every command an operator types.

## D5 — what the deploy and the promotion lose

Step 2 loses the account outright: `deploy::SERVICE_USER`, `deploy::SERVICE_GROUP`, the `ServiceUser` read
(`id -u holosmith`, `id -nG holosmith`), `ServiceUser::commands` and its printed `useradd`/`usermod`, the `remark`
about the group, and the `sudo install -d -o … -g …` fallback for a root whose parent is root's. Step 5 loses the
staged `sudo install` and `unit_commands`' printed form — the unit is written where systemd's user manager reads it and
enabled through that manager. What is left of step 2 is what it was always doing when the account was already right:
the tree, the config, and the two files that are generated once.

**The daemon's group does not disappear with the account — its subject changes.** A machine's daemon is reachable
through a socket the `docker` group may open, and the question "may the account that runs `serve` talk to Docker" is
now a question about the operator's own membership. It is the same read about a different account (`$(id -un)` instead
of `holosmith`), which a machine that can run `docker ps` already answers, and `preflight`'s docker finding is the
sentence when it does not.

`promote`'s restart becomes `systemctl --user restart holosmith`: `Tools::sudo`, the `-n`, the sudoers refusal sentence
and its ADR-0086 D4 citation go. The two hints that told an operator to escalate also go — `config`'s "run `sudo -u
holosmith holosmith key set`", which is now the operator writing their own 0600 file, and `remote`'s note about the
machine's sudoers rule.

The move is half in the tree. Step 5 has it: the unit it renders carries no `User=` or `Group=`, the sandbox is the
reduced set, it is enabled with `WantedBy=default.target` and the two `systemctl --user` commands, and it is written
into the user manager's directory beside the config. Still outstanding: step 2's account and the `useradd`/`usermod` it
prints, `promote`'s restart and the sudoers line its refusal names, `logs`'s journal spelling, `uninstall`'s words, and
`loginctl enable-linger`. The decision is recorded here, and what D5 lists is the shape the code moves to rather than a
description of the tree as it stands.

## Refused

- **A system unit with `User=<you>`.** It is the one shape that keeps the whole sandbox: a system unit gets no implicit
  `PrivateUsers=`, so the table's second row does what it says when a unit carries it. It is refused because the price is
  the thing being removed: `install -m 0644 <unit> /etc/systemd/system/` needs root, `systemctl enable --now` needs
  root, and `upgrade`'s restart needs root — so the sudoers line comes back, ADR-0083 D7's promotion is root's again,
  and the log is root's unless the operator is put in the `systemd-journal` group. That is ADR-0086's install with a
  different `User=` value, and it is refused for ADR-0086's reason read the other way round: the privilege is what an
  install on your own machine should not need.
- **Keeping both shapes — a service user where one exists, a user unit where one does not.** ADR-0086 refused two
  layouts and was right: two of everything, two unit spellings, two sets of paths, and one of them untestable in the
  gate. The reversal is not a third layout; it is the only one.
- **`$HOME` for path resolution.** See D4: it is an inherited variable, so which config a command read would depend on
  who typed it and how, rather than on the account that may read it.
- **`PrivateUsers=no` beside the mount directives, to keep `ProtectSystem=strict` and the mapping's costs apart.**
  Tested, and it does not opt out: in a per-user manager the mapping is implied by the directive, not requested by the
  unit. A unit that says both is a unit whose sandbox is decided by which systemd reads it, which is worse than either
  answer.
- **A user unit that keeps `ProtectHome=tmpfs` and re-exposes the state with `BindPaths=`.** ADR-0086 D1's own
  quotation of `systemd.exec` says the combination is legal, and it is still refused: it is a line of configuration
  whose only purpose would be to make the mapping acceptable, and the mapping is what D3 refuses. It would also re-hide
  `/run/user`, which is where the user manager's own socket lives.

## What this does not measure

- **One systemd version.** `255.4-1ubuntu8.18` on one machine. Which directives imply `PrivateUsers=` is a fact about
  a systemd release, and the unit's directive set is the only place this build would notice a change.
- **No gate case.** `scripts/verify.sh` has no user manager and no root, so the unit's bytes are checked and its
  sandbox is not — the same limit ADR-0086 D3 recorded about its own directive list.
- **The `/etc/shadow` reading is a measurement about a sandbox**, not a claim that the daemon does anything with it.
  What it decides is that a directive whose effect cannot be predicted from its name is not one this unit can carry: a
  reader has to understand a mapping they did not ask for, and that is a worse property than a shorter directive list.
- **The Docker ordering** in D2 is reasoning, not a boot: the two managers' race has not been watched on a machine that
  starts both at once.

## Consequences

- The `holosmith` account is not part of the install this ADR describes, and only ADR-0086 carries the pointer. The
  documents that still describe the install it replaced — `docs/build.md`, `docs/quickstart.md`, `docs/config.md`'s
  `<root>/config.toml` and its `/var/lib/holosmith` default, ADR-0056's privileged step list, ADR-0058's ownership
  section, ADR-0082's root check and ADR-0087's measurement of a system account — are later stories' to rewrite, and
  until they are they describe an install this one replaced.
- **ADR-0006's rejection of XDG is reversed, on its own premise.** It ruled the XDG paths out because the file was read
  by "a systemd unit running as the `holosmith` service user, which has no `$HOME` worth depending on", and with one uid
  the file is read by the account whose home it is, so "XDG is for per-user applications" now describes this install
  rather than excluding it. Everything else 0006 decided — one typed config value, loaded once, strict on unknown
  keys — stands.
- Every `systemctl` and `journalctl` this repository's instructions give for the daemon grows `--user`, and the system
  spelling stops working: `systemctl status holosmith` on such a machine answers "Unit holosmith.service could not be
  found" rather than the daemon's state.
