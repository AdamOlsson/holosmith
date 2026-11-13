# The session container runs as the service user, and that uid is read rather than guessed

ADR-0035 decided that a session's container carries **no `--user`** — "the image's own default user is what runs the
agent, and the deploy step is what guarantees that user is the one that owns the host's files" — and that "naming a
uid is a guess about a mapping the deploy step owns, and a wrong guess is worse than none". The reasoning was sound
for the install it was written against (a machine whose operator, service user and image user were all uid 1000) and
wrong for the install ADR-0086 now ships, where the service user is a **system** account and the image's user is
whatever its base's `node` happens to be.

This ADR keeps ADR-0035's property — **the host can commit what the container wrote** — and moves it from an
assumption to a measurement: the uid is read from the tree, and named on the command line.

## Measured before deciding

- The agent image's user is the base's `node`: `USER node`, and `agent.Dockerfile`'s own comment says "uid 1000 is only
  what this repository's Linux machines and this base happen to spell it". So the container runs as **1000**.
- `useradd --system` takes its uid from below `UID_MIN` (`/etc/login.defs` here says `1000`; the system accounts on
  this machine sit in 119–123), and ADR-0086's layout creates the service user with `--system`. So the host user who
  owns the worktree is **not** 1000, and the two identities differ.
- `git worktree add` creates `<projects>/.worktrees/<id>` with the invoking user's umask — `0755`, owned by the
  service user. A container at uid 1000 is neither the owner nor in the owning group, so it cannot create a file in
  that directory at all. Every session fails at the first write, mid-turn, which is the failure ADR-0035 says the
  decision exists to prevent ("which shows up as permission denied in the middle of a turn").
- The build already names a uid in one place, and it reads it: `KeyProxy::run_args` passes
  `--user <keys.toml's owner>` because `--cap-drop ALL` takes `DAC_OVERRIDE` away and a container at uid 0 cannot read
  a 0600 store (`src/container.rs`). The session container was the exception, not the rule.
- The crate denies `unsafe` and carries no `libc`, so a process cannot ask for its own uid with `getuid()`. It can read
  it: `std::os::unix::fs::MetadataExt::uid()` on a path it owns. That is the same call the key proxy already makes, and
  it is why this ADR needs no dependency.

## D1 — the identity is a read, not a constant

The uid and gid are read once per process from **the data root's** metadata — the directory `serve` must own to work
at all, and which `doctor`'s `perms` finding already measures — and every session container is started with
`--user <uid>:<gid>`. Nothing is hard-coded, nothing is derived from the image, and one machine's number is not
another's.

`deploy`'s step 2 reads the same fact from `id -u holosmith`, so the install's report and the runtime's arguments are
one number read two ways rather than two numbers hoping to agree. A machine whose data root is owned by somebody else
is not a machine this build will run a session on: `perms` already fails there, and the container would name an
identity the tree does not have.

The key proxy keeps its own read (the store's owner) because that is the file it must open; the two agree by
construction, since both are the service user.

## D2 — the non-root rule stays; the invariant is restated

ADR-0035's spec-derived rule — the container is not root — is unchanged and still asserted: the gate fails on
`id -u` = `0` inside a session container. What changes is the *second* assertion. It used to be "the image's default
uid equals the invoking user's", which is a statement about a base image. It becomes:

> the container's uid is the owner of the worktree it is mounted on, and that owner is the user `serve` runs as.

That is the property ADR-0035 was reaching for ("a worktree the host can commit to"), stated as the thing measured
instead of as a coincidence of two numbers.

`scripts/verify.sh`'s image case keeps its comparison for the **image's own** contents (a session container's identity
is now the tree's, so comparing the image's default user with the gate's uid would measure the wrong thing), and the
session case compares the container's `id -u` with the data root's owner.

## Refused

- **`chmod` the worktree open, or ACLs.** Permissions that let two identities write one tree are how "the host can
  commit what the container wrote" stops being true in the other direction.
- **Make the image's user a build argument.** The base is pinned by digest and its user is the base's; a second pin
  that overrides it is a second place to get the uid wrong, and it only moves the guess.
- **`--userns=keep-id`.** That is Podman's flag; Docker has no equivalent, and the daemon this build talks to is
  `docker`.
- **Rootless Docker.** Out of scope by decision: its subuid mapping makes this exact problem worse (container 1000
  becomes a subordinate uid), so adopting it later is its own ADR, not a fallback for this one.
- **Keep the image's uid and make the service user match it.** That would mean `useradd --uid 1000`, which fails on any
  machine whose operator already holds 1000 — the common case — and fails silently in the direction that matters
  (a colliding uid is refused, and the operator is told to pick a number, which is the guess again).
