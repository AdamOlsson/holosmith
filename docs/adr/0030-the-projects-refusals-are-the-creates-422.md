# The project's refusals are the create's 422, read on the request's path

HS-026 is spec item 1's third bullet: three repositories that must not be mounted into a session's
container, each refused with its own code. Four decisions follow from making a *background* refusal an
*answer*: where it is decided, who asks, what is read, and what a retry means.

## The refusal is the create's, not the provisioner's

The provisioner is a spawned task: it has no request to answer, so a refusal discovered there can only
reach the operator as a row's `status_reason` and an `error` state. Spec item 1 says "refused with 422 at
session creation", and item 9 §4 already makes the create the synchronous validation point (422 for bad
input, 409 for the cap). So the three checks run in `POST /api/sessions`, **before** the 202 and before the
row exists, and they answer ADR-0008's envelope with their own code. A session the machine refused does not
exist at all, which is what the story's fourth bullet ("a refused session leaves nothing behind") means once
the 202 is gone: no row, no directory, no branch, nothing to resume or delete.

The consequence is a change to HS-025's behaviour, recorded rather than papered over: a create naming a
project the machine does not hold used to be accepted and end `error` with git's sentence ("the worktree
step: …"), resumable once the repository was cloned. It is now refused with `422 git_not_directory`, because
a project with no `.git` is one of the shapes the spec refuses. A *direct* backend provision of such a
project still fails that way — the rule lives at the create, not inside the worktree step — so a project
deleted between a create and its provisioning is reported exactly as before.

## The backend asks, and the stub answers "no repository"

`Backends::refuse(project)` is the question, `DockerBackend::refuse_project` is the Docker half, and the stub
answers `Ok(())`. Three reasons for that placement rather than the handler's:

- the stub provisions nothing outside the process, so there is no repository to mount — a machine running
  `--backend stub` must not need a projects root full of checkouts, and that is what every stub test and
  the gate's own boot look like (ADR-0015);
- the projects root belongs to the backend, and the handler would otherwise have to reach through the
  backend's private fields to name the repository;
- the question is the same shape as the other host reads the backend already owns, so a second backend
  (HS-068's hub, HS-073's `--ssh`) answers it too.

The rules themselves are `worktree::refuse`, beside the module that already runs git on the host — the
repository is what the worktree is made from, and one module owning "everything this machine asks a project
repository" is one place to read.

## What is read, and why those readings

- **`.git` is a directory.** A `.git` *file* is a linked worktree's or a submodule checkout's shape, and the
  repository it points at is not the path HS-027 mounts.
- **No submodules: no `.gitmodules` and no `.git/modules`.** The spec names the hooks that live under
  `.git/modules`; `.gitmodules` is the declaration an agent inside the mount could use to initialise them
  itself. Either file refuses — a filesystem read, not `git submodule status`, because the question is
  "could this repository carry those hooks", and the answer does not depend on whether they exist yet.
- **`extensions.worktreeConfig` is off**, read through `git config --type=bool --get`: git's own spellings
  (`true`, `yes`, `on`, `1`) all answer one word, and an unset key exits 1, which is the common case and not
  a failure. A value git itself cannot parse (exit 128) is not "safe": it is a repository nothing can be
  promised about, so `Refusal::Unreadable` carries git's words to the log and the client gets
  `500 internal` — ADR-0008's "a reason no client can fix".

No cache. A project can be made unsafe between two creates, and a cache of "this repository was fine" is a
second source of truth about a directory the operator owns. The checks are two `stat`s and one `git config`,
which is the same order as the `git worktree add` the create is about to schedule.

Subprocesses do not run on a runtime worker (ADR-0029), so the read is wrapped in `spawn_blocking` on the
request's path. A `JoinError` is not a refusal — it is reported as `internal`, because a task that panicked
has told us nothing about the project.

## Where it sits in the create's fixed order

`request_parts` (body, key, project/model/thinking) → **the project's refusals** → the idempotency lookup
→ the cap → the insert. Two placements are deliberate:

- **after the idempotency lookup**, so an `Existing` row answers first. ADR-0012 promises that a retry
  creates nothing and is answered from the row it already has; a project that became unsafe after that row
  was written must not turn a retry into a refusal. The read still happens on every create — the key's
  lookup is inside the one lock scope, so the refusal travels *into* it as a value and is raised only on the
  path that would insert.
- **before the cap**, so a refused create never evicts. Otherwise a full machine would stop an idle session
  to make room for a create that is about to be refused, which is the one outcome spec item 9's "nothing is
  deleted" cannot survive.

The code is therefore the project's, not `cap_reached`, when a full machine is asked for an unsafe project —
and a test pins that, because the ordering is a choice and not an accident of the code's shape.

## One sentence, no code, on the page

The story asks the UI to explain the refusal "in one sentence rather than showing a code". Every other
answer in the create form is drawn as `code: message`, which is right for a client bug (`bad_project`) and
wrong here: `git_not_directory` is not something the operator acts on, the sentence is. `createFailure` in
`web/src/transcript.ts` holds the rule (the three codes print the sentence alone), both the list of codes and
the sentence are tests with node's own runner, and the form reads it for every refusal — because a *third*
place that decided between the two shapes would be the place they disagree.
