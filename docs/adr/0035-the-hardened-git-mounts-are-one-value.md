# The hardened `.git` mounts are one value, and V1 passes as specified

This is spec item 1's mount list and the **V1** verification row: with the four mounts in place, three
writes must fail (`.git/hooks`, `.git/config`, renaming `.git/hooks`), three git commands must work
(`commit`, `branch`, `worktree list`), and the process must be unable to remount or unmount. V1's own
fallback column says what to do if that is not true: mount only `.git/objects`, `refs`, `logs` and
`worktrees/<id>` writable and the rest read-only, re-verify, and record the outcome.

## V1 passes on this machine, and the fallback is therefore not applied

Measured on a real daemon (29.9.2) with a real worktree and the spec's four mounts, `--cap-drop ALL`,
`--security-opt no-new-privileges` and the image's own non-root user:

```text
1 hooks write:   cannot create …/.git/hooks/pre-commit: Read-only file system
2 config append: cannot create …/.git/config: Read-only file system
3 hooks rename:  mv: cannot move '…/.git/hooks' to '…/.git/hooks2': Device or resource busy
4 commit:        <the branch's head advanced>
5 branch:        * agent/abc123   agent/test-branch   main
6 worktree list: …/project 7604bc1 [main]
                 …/project/.worktrees/abc123 <sha> [agent/abc123]
7 remount:       mount: /: permission denied … failed mount system call
8 umount /work:  umount: /work: must be superuser to unmount.
```text

Stacking the two read-only mounts on top of the writable `.git` works exactly as the spec predicted, and
`tests/container.rs` plus `case_container_mounts` assert it from now on. The measurement is the reason
this ADR exists: the fallback is a *different design*, and the decision not to take it should be re-openable
by a later reader, not silently assumed.

## The mounts are one value, and host path equals container path

`container::Mounts` is spec item 1's list as data: `worktree`, `git_dir`, `hooks`, `config`, each with a
host path, a container path, a writable flag and the kind of source it must be. It is built from a
`Worktree`, so the naming rule for `<projects>/.worktrees/<id>` still lives in exactly one function.

**The host path and the container path are the same string for the repository's three mounts**, and that
identity is load-bearing rather than cosmetic: a worktree's `.git` is a *file* containing
`gitdir: <absolute path>`, and that absolute path is `<project>/.git/worktrees/<id>` on the host. Mount the
repository at a different path inside the container and git resolves a `gitdir` nothing has mounted — the
session looks like it has no repository. `/work` is the one mount whose container path differs, because the
directory is new to the container and the `gitdir` it points at is mounted at its host path.

## `--user` is not passed, and the non-root requirement is asserted instead

The spec says the container is not root. There are two ways to get there: name a uid in the run arguments,
or let the image's default user do it. Naming a uid is a guess about a mapping the deploy step
owns, and a wrong guess is worse than none — it would give the agent's files to a uid the host does not
own, which shows up as "permission denied" in the middle of a turn. So the image's own user runs the agent,
and the requirement is *checked* instead: the gate reads `id -u` in the container and fails if it is `0`.
An image whose default user is root is a broken image, and (the real pi launch) is where the image's
contents are the subject.

## A mount whose source is missing is refused before `docker run`

`docker run -v /nope:/work` does not fail: it creates `/nope` as an empty root-owned directory and mounts
that. A projects root that is wrong — a typo, an unmounted disk, the `$WORK` directory of a test — would
therefore give the agent an empty worktree and a `.git` that is not the repository, with no error anywhere.
`Mounts::check` asks the question first and answers **every** source that is wrong, with its kind, because a
broken projects root is usually broken in more than one place. The kind matters too: Docker mounts a
directory where a file belongs without complaining, and git then fails to find its config.

This was not a hypothetical: a first draft of the V1 test wrote its repository into `/tmp`, which in a
sandbox is a tmpfs the daemon cannot see, and spent a cycle debugging "the container has no repository"
before noticing that Docker had created empty directories instead of mounting files.

## The container step is not filled by this story

A container with no session network is the (`--network agents_<id>`, the limits, the lifecycle flags)
and the network itself is that story's. So `Step::Container` keeps refusing and naming, and
`case_worktree`'s assertion that a Docker-backed create ends `error` at the network step with a reason
naming stays true until those stories change it. What this story adds is the value those steps use,
plus the measurement that says the mounts hold.

## Consequence: `agent_image` is a config key

An argument builder with no image cannot run a container, so the image became a key: `agent_image`, a
string, defaulting to `agent-image:<this binary's version>` — the name spec item 10 §3 builds. It is a
machine setting rather than a constant for the same reason `backend` is: a development machine holds an
image under whatever name it built it (this one has `pi-agent:local`), and a key is how it says so without
a rebuild. This is where the image's *contents* start to matter (its pi version must equal `pins/pi.txt`).

One consequence worth knowing: the default carries the binary's version, so `tests/golden/config-default.toml`
pins it and **a version bump makes that golden stale** — the same rule as a new config key, for the same
reason (the golden is the output, and the output changed).
