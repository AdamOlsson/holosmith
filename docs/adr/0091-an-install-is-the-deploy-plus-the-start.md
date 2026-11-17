# An install is the deploy plus the start, and the deploy's health line is not its failure

ADR-0088 made a machine the operator's own account: one uid, a user unit at `~/.config/systemd/user/holosmith.service`,
`loginctl enable-linger` for the boot, and no command left for root except the daemon's group. What it left open is the
shape of the first run. `server deploy` writes the tree, the config and the unit and then **prints** the two
`systemctl --user` commands, because a deploy is the steps and a step that enabled a unit would be a step that managed
a service. ADR-0088 D2 named the gap and left it: "Setting it is still outstanding". So first setup was five commands
and a reading exercise, and the reading was "which of these printed lines are yours".

`holosmith install` closes it. It runs the deploy for the machine's own half — `hub` installs the hub's, `server` and
`both` the server's, which is `Kind` and nothing else — and then does the three things the deploy prints or leaves:
`systemctl --user daemon-reload`, `systemctl --user enable --now holosmith.service`, and `loginctl enable-linger`. It
ends by waiting for `/api/health`, printing the control URL and the one next step the half it installed earns. The
deploy keeps its own behaviour, and stays the lower-level command: an install is not a seventh step, it is a caller.

## D1 — the daemon's group is a refusal, before anything is written

A machine whose account cannot open the daemon's socket cannot run a session. The deploy prints the one `usermod` for
it (ADR-0088 D5) and carries on, because a deploy's tree is worth writing either way. An install **refuses**: it prints
the command, says why (the socket is `root:docker` with mode 0660), says a group reaches a login only at the next one,
and exits non-zero with **nothing written** — no root, no config, no unit, no manager child.

- **The question is `Kind::group`'s**, so a hub and a daemon reached over `tcp:` are never refused: neither is entered
  through a group, and a `usermod` there would change nothing.
- **A dry run is refused too.** The plan exists to be acted on, and a plan for a machine that will be refused at its
  first line is a lie about what the next run does.
- **An unreadable account is not a refusal.** `id` that cannot say who is running leaves `Account` unread, and the
  deploy's step 2 is the sentence about it — ADR-0090's rule about an unread uid, one fact over: refusing on a
  measurement that did not happen would refuse a supported run.
- **The command spells `$USER`**, where `deploy::Account::command` spells the account's name. Both are correct and they
  are different jobs: the deploy's is a sentence about a machine, and this one is a line to paste into the shell the
  operator is sitting in. The two never appear in one run, because this refusal happens before the deploy that prints
  the other.

## D2 — the deploy's health line is true, and it is not the install's verdict

The deploy's last step asks whether the machine serves. On a first install the answer is **no**, and it is no for the
right reason: step 5 wrote the unit and nothing has started it, which is exactly what an install does next. So the
report install prints carries a `failed` line — the honest state of the machine at that moment — and the run continues.
Install's own `deploy::wait_for_health`, after `enable --now`, is the verdict; a false one exits non-zero with the
deploy's own sentence, which names `journalctl --user -u holosmith.service -n 50`.

Two consequences, both deliberate:

- **Any other failed step stops the run.** The tree, the images and the proxies are what the two manager commands and
  the wait stand on, so a report that failed earlier has not earned them, and install exits non-zero having printed the
  deploy's resume-shaped line for this verb.
- **The deploy pass is given a one-second health wait**, not the configured deadline. That step cannot change its
  answer however long it waits — the thing that changes it is `enable --now`, which has not run — so the configured
  minute would be dead time in the one command whose whole promise is "from nothing to serving". A second is what the
  one machine it *can* answer for needs (a re-run whose unit is already serving answers on the first ask without
  sleeping) and keeps the step's own sentence ("for up to 1 s") one a reader can act on. Install's own wait keeps the
  configured deadline.

The cost is stated rather than hidden: a `doctor --full` finding that fails that step on an **already-serving** machine
is printed but does not fail an install, because the wait after it answers. That is the price of one health line
meaning "the moment before the start" and the other meaning "after it", and it is why the module says which is which.

## D3 — a refused linger is the operator's command, and the run succeeds

`loginctl enable-linger` is unprivileged where polkit allows `set-self-linger` (measured, ADR-0088). Where it is
refused, the machine still serves — what is missing is that it serves after a reboot — so the line is `needs`, the
deploy's word for a step whose remaining work is the operator's, and it ends with `sudo loginctl enable-linger $USER`.
The run exits 0. This is the only step that may: every other one is on the way to a machine that answers.

## D4 — a tree that is not the manager's still commands nothing

`systemctl --user` resolves the **name** `holosmith.service` in the manager's own search path, so a `--root`/`--config`
tree has a unit no manager reads. ADR-0088 already decided that such a tree's unit is a file and nothing else, and
`uninstall` already follows it; an install does the same, for the same reason — enabling that name would command the
account's own unit, a service the run was never asked about. Its line says which two commands are still the operator's.

**Linger follows the unit, not the tree's intent to be a service.** `loginctl enable-linger` sets a flag on the
*account*, and the flag means "this account's user manager runs at boot" — so on a tree no manager reads it would turn
on a boot service for the unit the manager *does* load, which is the account's and not this run's. The line is `needs`
with `sudo loginctl enable-linger $USER` beside the two `systemctl --user` commands, and neither `loginctl` nor
`systemctl` is run: "commands nothing" is about the account's real manager, which is where the write would have landed.

## What this does not measure

- **The gate drives `run`, and still has no manager and no daemon.** `tests/install.rs` drives a **hub's** install
  with the machine handed in, so the four container checks are `unknown` rather than measured; what a server's deploy
  does to a real daemon is `tests/deploy.rs` and `scripts/verify.sh`'s cases. `scripts/verify.sh`'s `case_install`
  drives `install::run` for a hub under `bwrap`: a passwd entry names a home the case owns, so the unit `--config`
  composes is the one the account's "manager" resolves, and `systemctl`/`loginctl` are recording stand-ins on `PATH`.
  That is what measures the argv, the unit's bytes, the linger lines and a second run's silence — and what it still
  does not measure is a real user manager: no unit is loaded and no linger flag is set.
- **No polkit-denied machine.** The refusal is what the shim says it is (`Interactive authentication required.`), which
  is the sentence `loginctl` gives; the run's own behaviour is measured, the machine's is not.
- **`systemctl --user is-enabled`/`is-active` are read, not `show -p …`** — two children rather than one parse of a
  multi-property `show`, because each exit status is the fact and neither has to be parsed out of a value list.

## Consequences

- First setup is one command as the operator's own account; `server deploy` and `hub deploy` stay as the lower-level
  verbs, and keep printing what they always printed.
- Two lines carry `sudo`, both of them printed and neither of them run: `usermod -aG docker $USER` as a refusal's last
  line, and `loginctl enable-linger $USER` as one step's remaining work.
- A second run applies nothing: the unit's read is `is-enabled` **and** `is-active`, the linger read is
  `loginctl show-user --property=Linger`, and the words are `satisfied` on both.
- A re-run that rewrote the unit's bytes while the old process still serves runs `daemon-reload` and `enable --now`,
  and `enable --now` starts nothing that is already running — so that line says the manager loaded these bytes and the
  serving process predates them, and names `systemctl --user restart` as the command that starts them. It is the one
  arrangement in which "started it" would be false, and this ADR takes the words over a restart: an install is not a
  promotion (ADR-0082), and interrupting a serving daemon is not a first setup's business.
- **The guide is the operator's account throughout.** The sweep this ADR deferred has landed: `docs/quickstart.md`
  §5–§8 now name `~/.local/share/holosmith`, run `key set` and `git clone` as the operator, read `journalctl --user`
  and restart with `systemctl --user`, and the day-two verb is `holosmith install`. `README.md`'s by-hand recipe is
  deliberately not part of that sweep.
