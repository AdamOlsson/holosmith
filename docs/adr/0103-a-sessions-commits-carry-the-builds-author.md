# A session's commits carry the build's author, from a file git is told about

A session's container has a working repository and no git author. Measured in the agent image:

```text
$ docker run --rm --user 995:990 agent-image:0.1.0 sh -c 'git var GIT_AUTHOR_IDENT'
Author identity unknown
...
$ docker run --rm --user 995:990 agent-image:0.1.0 sh -c 'git init -q /tmp/r && git -C /tmp/r commit --allow-empty -m x'
Author identity unknown

*** Please tell me who you are.
```

Everything else about committing is already right — the `.git` gitdir and its common directory are mounted writable,
so `add` and `commit` write objects, refs and the index (ADR-0035) — but git refuses before it writes anything,
because there is no `/etc/gitconfig` in the image and `$HOME` is not a place a session can put one: it is the image's
`node` home, and a session may run as the **data root's owner** instead (ADR-0087), for whom that directory is not
even writable. The workaround until now was for the model to pass `-c user.name=… -c user.email=…` on every command,
which is not a product behaviour: it is an instruction an agent has to remember, in a prompt, forever.

The decision is that a session's commits carry the build's own constant, delivered as a file:

```gitconfig
[user]
	name = holosmith
	email = holosmith@localhost
```

at `<data>/sessions/<id>/pi-home/gitconfig`, and that `GIT_CONFIG_GLOBAL` names that path in the container's
environment.

## D1 — one constant, delivered as a file, named by git's own variable

`snapshot::IDENTITY` is the pair `holosmith`/`holosmith@localhost`, and it was already the author of two commits this
build writes: the WIP snapshot's (`-c user.name=… -c user.email=…` on its own `commit-tree`) and a merge's (the same
pair on `git commit`). A session's commits join them, so the repository has **one** author for everything the system
writes and no fourth copy of the strings.

The delivery is a file rather than a command-line flag because the container's commits are the *agent's*: this build
does not run them, does not see them, and cannot add `-c` to a `git commit` a model types. A file is the only shape
that reaches every git process in the container, and `GIT_CONFIG_GLOBAL` is git's own name for "the user's global
config is this file" — not a wrapper, not a `git` shim on `PATH`, not an inherited `GIT_AUTHOR_*` pair.

It carries a path, like `PI_CODING_AGENT_DIR`, and so it is not one of `agent::ENVIRONMENT`'s four switches: those are
values that keep pi off the network, and this one is where git reads its config. `container::SessionContainer::run_args`
composes it in the launch vector immediately after the agent directory, which is the same kind of fact in the same
place, and `container::{GIT_CONFIG_ENV, GIT_CONFIG_FILE, git_config}` are the one spelling of the variable, the file and
the text.

## D2 — written on every start, not seeded with the agent directory

`DockerBackend::start_container` writes the file beside `models.json` and `mcp.json`, before `Mounts::check`, for the
reason those two are written there: the agent's directory is a mount source, and `docker run -v` creates a missing
source as an empty root-owned directory rather than an error.

It is written on **every** start rather than seeded once, and the rule it must not be put under is
`agent::ensure_home`'s: that function seeds only what is not there, which is right for pi's settings and wrong for
this. A resume finds the directory the previous container left, and the file is the *build's* — a session whose
`pi-home` an earlier start, an earlier release, or the agent itself left something else in must carry what this build
writes, not what it found. `write_git_config` is one private function in `backend/docker.rs`, the twin of
`write_mcp_config`, with a `BackendError::failed("the container step: …")` sentence naming the path that failed.

## D3 — identity is not a credential, and that is the whole argument

Two rules here refuse things a container must not have: the mount rule (no home directory, no `.ssh`, no
`.gitconfig`, no `.git-credentials`, no key store — spec item 1 and ADR-0035) and the environment rule (no injected
secret — spec item 2). Both exist for a **secret**, and ADR-0066 D8 states the same thing from the other side: the
host's own credentials stay the host's, and a child sets none of its own.

`user.name` and `user.email` are not secrets. They are two strings the resulting `git log` prints to everyone who can
read the repository, and the file holds nothing else — no `credential.helper`, no `url.*.insteadOf`, no
`core.sshCommand`, no `include.path`, no `http.*.extraheader`. A file that holds only those two lines is not a
credential store, so the rule is restated rather than relaxed: `tests/publish.rs`'s assertion becomes a **named
allow-list** — `GIT_CONFIG_GLOBAL` permitted, `GIT_ASKPASS`, `GIT_SSH*`, `GIT_CONFIG_SYSTEM`, `GIT_CONFIG_COUNT`,
`SSH_*` and anything named for a credential still refused, explicitly and by the blanket rule — and its third read
gains the file: the agent directory holds `settings.json` and `gitconfig`, and the `gitconfig` holds exactly the three
lines above. The mount half is unchanged and still passes, because the new file is *inside* the pi-home mount and is
not a mount of its own; an operator's own `.gitconfig` is still the leak that rule exists to refuse.

`snapshot::IDENTITY`'s doc comment said "`-c` on the command, never a global config write". That was true of the
snapshot and is now half the story, so it says both: `-c` is still how the snapshot commits, and the same pair is also
written into a session's `gitconfig`.

## The honest consequence: one author, and no attribution

Every commit in a session carries this author, and so does every WIP snapshot and every merge. The history is
therefore **coherent** — one build wrote all of it, and nothing in it pretends to be a person — and it says nothing
about *who drove it*. A reader cannot tell an agent's commit from a snapshot from a merge by its author, and there is
deliberately no second value to tell them apart: a per-session name or address would be a value that has to be
invented, stored, rendered and explained, for a question the transcript already answers. The session id is in the
branch (`agent/<id>`), which is where this build's attribution lives.

## Rejected alternatives, and why

- **Mount the operator's `~/.gitconfig`.** It is the file that already has their author, and it is also where an
  operator's machine-specific configuration lives: a `credential.helper` that would be handed the session's pushes, a
  `url.*.insteadOf` that rewrites a url the session's egress rules did not allow, a `core.sshCommand` that would run
  the host's ssh, an `include.path` that pulls in anything, a `core.fsmonitor` that runs a program on every status. It
  is the credential leak the mount rule names, and it makes a session's behaviour depend on one person's dotfiles.
- **Point `HOME` at the pi-home.** A single `--env HOME=…` would make git find `$HOME/.gitconfig` with no new file, and
  it moves every child's home into a per-session directory that is mounted, writable and backed up: pi's own caches,
  `ssh`'s `known_hosts`, npm's and cargo's caches, all per session, all in the tree an operator restores from. The need
  is git's author, and this pays for it with every program in the image.
- **`worktrees/<id>/config.worktree`.** A worktree-scoped config is the tidiest-looking home for it and needs
  `extensions.worktreeConfig` on, which is a **repository-wide** switch: it changes how the project's own repository
  reads its config, in a checkout this build does not own and whose committed files it must not rewrite (ADR-0030).
- **Bake `/etc/gitconfig` into the agent image.** Measured working, and then the fact becomes an **image** fact rather
  than the binary's: a machine with a stale local `pi-agent:local` keeps the old behaviour with nothing red anywhere,
  the version that matters is a Dockerfile's, and `cargo test` can no longer see the mechanism at all. This build's
  behaviour belongs to the binary that also writes `models.json` beside it.
- **`GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`.** Values in the environment need no file and would work
  in any child, and they leave the agent nothing to read: `git config --get user.name` would answer from a variable
  nobody can point at, and the pair would sit in `docker inspect` as if it were a secret worth hiding. A file is
  discoverable, and a user who needs to override it can.

Ordering is not a decision this ADR has to make: `GIT_CONFIG_GLOBAL` **replaces** the user-scope path rather than
adding to it, so a project `.git/config` with its own `[user]` still wins, and an environment-set `GIT_AUTHOR_NAME`
or `GIT_COMMITTER_EMAIL` still beats the file — git's precedence is untouched, which is what keeps a user who wants to
differ able to.

## Out of scope: signing

A project whose `.git/config` sets `commit.gpgsign = true` still fails in a session: there is no key in the container
and no pinentry, and this ADR gives it neither. The author is metadata; a signature is a capability, and distributing
one is a decision about a private key this build has no business making.

## Rollback

The file is a `pi-home` write on the existing per-start path, so switching back to the previous release is
uneventful: its next start of each session does not write `gitconfig` and does not name `GIT_CONFIG_GLOBAL`, and an
orphan file in the pi-home is a file nothing reads. No config key is added, no migration, no store row, no image
change. A session resumed rather than started keeps the newer release's file until its next start, exactly as it keeps
any other rendering from an earlier start — and with the variable gone it changes nothing about that container's git.

## What pins it

- `tests/container.rs` commits inside a real session container — through the shipped backend, with the image and the
  daemon a machine has — with **no** `-c` flags and no `GIT_AUTHOR_*`/`GIT_COMMITTER_*`, and asserts that
  `git log -1 --format='%an <%ae>'` answers `holosmith <holosmith@localhost>`. It then stops the container, starts it
  again and commits a second time, which is the resume half: the file is per start, so the second container must
  answer the same. Without the change, the first commit fails with `Author identity unknown`; that is the fact this
  case was written against.
- `container.rs`'s unit test pins the launch environment item by item, `GIT_CONFIG_GLOBAL=<pi-home>/gitconfig` in its
  place after `PI_CODING_AGENT_DIR`; `backend`'s unit test pins that the file's text is `container::git_config()` and
  that a second start rewrites it.
- `tests/publish.rs`'s credential assertion is the negative one: the allow-listed variable, the five refused names,
  the directory's two files, and the file's three lines.
- `scripts/verify.sh` starts real sessions and names the third `pi-home` file in the comment beside the key-leak grep,
  so the sentence that says which files could carry a credential stays true.

## What it does not measure

That a *model*, given a prompt, commits without help. The container test runs `git commit` itself, as the container's
user, in the container's worktree — which is the mechanism — and no test drives an agent into a commit. The by-hand
version is one session, one prompt asking for a commit, and `git log` on the host answering
`holosmith <holosmith@localhost>`.
