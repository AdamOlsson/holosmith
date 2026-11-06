# What a delete keeps, and how it says so

HS-043 is the last story of spec item 9's sentence about deletion, and item 9 says three things this
repository had no code for:

> Kept in the repository: branch `agent/<id>` if it has commits beyond the base and is not merged into a
> target branch, and `refs/holosmith/wip/<id>` if it differs from the branch. Recover with
> `git branch rescue refs/holosmith/wip/<id>`.

Before this story `Worktree::remove` ran `git branch -D` unconditionally, so every delete destroyed the
branch whether or not it held anything. Five decisions follow.

## The rule is `worktree.rs`'s, and `remove` is told what not to delete

`Worktree::remove` is the function that deletes the branch, and the module holds every host git command a
session makes (ADR-0029). So the rule is a value the module answers — `Worktree::retained(base_commit,
targets) -> Result<Retained, GitError>` — and `remove(&self, retained: &Retained)` is told what to leave.
The alternative was a `Deleter` that ran git itself, in a module that has none: it would need the
repository path (the backend's), the `[merge] targets` (the config's) and a `spawn_blocking` of its own,
and `tests/worktree.rs` could not prove the rule on a real repository without a daemon, a backend and a
delete request.

`Retained` is one of two answers:

```rust
pub enum Retained {
    /// The repository answered: these names survive a teardown, and either may be `None`.
    Known { branch: Option<String>, wip: Option<String> },
    /// The repository could not be read, so the teardown keeps the branch and the answer claims nothing.
    Unknown,
}
```

`Known` rather than two booleans because the answer *is* the report: the 202 says which names survive and
the recovery command names the ref, and deriving the names twice would be two spellings of one decision.
`Unknown` exists because "cannot tell" must never mean "delete". It has two causes and one rule: a
repository git cannot read, and a row with no `base_commit` to measure ahead from (a worktree step whose row
write failed — `Provisioner::record` logs and carries on — leaves a session that can reach `idle` and later be
deleted with nothing to measure against). Both keep the branch, and the delete's answer is `kept: null` rather
than a claim about names the rule could not confirm.

## "Commits beyond the base" is `ahead`'s, with the row's base commit

The first half of the bullet is `Worktree::ahead`, which already exists and already takes the row's
`base_commit`. Two edge cases are decided rather than discovered:

- **No `base_commit`.** The provisioner writes one on every successful worktree step (ADR-0029), so a row
  without one never got that far — and a session with no worktree step has no branch either. The rule
  keeps when it cannot measure, so a branch that somehow exists on such a row survives; a session that
  never provisioned keeps nothing because there is nothing to keep.
- **A base that is not a commit in this repository** (a garbage-collected head, a row written by another
  binary). `ahead` fails, the failure is `Retained::Unknown`'s, and the branch is kept.

Zero commits beyond the base is the ordinary case — a session that was provisioned and never worked — and
it keeps nothing: there is no work on the branch that the base does not have.

## "Not merged into a target branch" is one `--is-ancestor` per target, and a missing target is not a merge

`git merge-base --is-ancestor <branch> <target>` exits 0 when the branch is an ancestor of the target and
1 when it is not. For each name in `[merge] targets`:

- **the target exists** → ask; any 0 means merged, and a merged branch is not kept (the work is already in
  the target, which is where it was supposed to go);
- **the target does not exist** → not a merge. This matters: the default list is `["main", "master"]`
  because a machine may have either, and a repository with only `master` must not treat the missing `main`
  as an unanswered question — that would keep every branch for ever on half the machines;
- **no target exists at all** → nothing can answer. A repository whose integration branch is `trunk` would
  otherwise lose its work silently, so the branch is kept.

`--is-ancestor` and not "is the target's tip a descendant of the branch's": they are the same question, and
the first is the one git spells as a question. A branch that is *equal* to its target (nothing on it) is an
ancestor of it, so it is not kept — which is right, and which `ahead` has already decided by then.

The rule is therefore: **keep the branch iff it has commits beyond the base and the merge question did not
answer "yes".** Reading a merge is all this story does; writing one is HS-054's.

## The wip ref "differs from the branch" is a tree comparison, and nothing deletes it

The second half of the bullet asks whether `refs/holosmith/wip/<id>` "differs from the branch". No code
deletes that ref on a delete — a worktree teardown removes a registration and a branch, never an
arbitrary ref — so "kept" is already true of it, and what was missing is the *report*:

- the ref **exists** and its **tree** differs from the branch's tree (or there is no branch) → `Known {
  wip: Some("refs/holosmith/wip/<id>") }`, and the 202 carries the spec's recovery command;
- the ref does not exist, or its tree equals the branch's → `wip: None`, and no command is printed. A snapshot
  whose tree equals `HEAD`'s removes the ref (ADR-0049), so a session that settled with nothing uncommitted
  has nothing to recover and must not be told a command that would fail.

One residue is worth naming: because nothing deletes the ref, a delete can leave one the answer did not
name. It takes a forced delete past a *failed* snapshot to reach — the pre-existing ref is then never
re-taken, so it can hold exactly what the branch already committed — and the ref survives as one more name
for the branch's own tree. Deleting it would be a teardown deciding, on a comparison, to destroy a ref; the
story names two refs to *keep*, and a residue nobody is pointed at is the smaller cost.

The tree, not the commit: a branch is a name for the work in a commit, and an agent that commits exactly
what the snapshot captured has not left anything behind. Comparing commits would print a recovery command
about work the branch already has.

## One additive key on the 202, and the fifteen keys still at the top level

The row is the API's body everywhere (ADR-0038 kept `expected_stop` off the wire for that reason), and the
story asks the *delete's own answer* to carry the command. So the 202 is the row flattened plus one key:

```json
{"id": "…", "status": "deleting", …, "kept": {"branch": "agent/<id>", "wip": "refs/holosmith/wip/<id>",
 "recovery": "git branch rescue refs/holosmith/wip/<id>"}}
```

`#[serde(flatten)]` on a wrapper in `http/sessions.rs`, so `Session`'s own serialisation — pinned to fifteen
columns in column order by a test — does not move, and no other route grows a key. `kept`'s three fields are
explicit, including `null`, for the reason `session.status`'s payload states its nulls: a client should not
have to tell "absent" from "nothing survived". The stub answers all three as `null` (it mounts no
repository, ADR-0015), which is also the shape a machine without a project sees.

The command is built once, by `Retained::recovery`, from `snapshot::reference` — the same function the
snapshot writes the ref with. The page does not spell it a second time; it shows what the answer carried.

## Where in the flow it is computed, and why the task is handed the answer

The handler computes `retained` **after** the snapshot and **before** the transition:

1. the snapshot may *write* or *remove* the ref, so an answer computed before it would be about the wrong
   repository;
2. the worktree must still be there — `ahead` and the registration check are `git -C <repository>` reads,
   but the branch's tree comparison needs the refs, which only exist while the repository does.

The transition follows (a refusal there leaves the row where it was), the 202 carries the answer, and the
spawned `Deleter` is handed the same value — so what the operator was told is the decision the teardown
applies. Computing it twice would let a commit that landed in between make the answer and the removal
disagree. A failure at the compute is a `warn` and `Retained::Unknown`: the delete proceeds, keeps the
branch, and claims nothing.

`?force=true` forgives a failed snapshot only (ADR-0023). The keep-rule is applied identically to a forced
delete: force is the operator saying "the uncommitted work may go", not "the repository may be emptied".

## The UI half is this story's, and it is a confirm and a notice

ADR-0023 deferred the delete's affordance to the hub's sheet. This story's acceptance names the UI —
"the delete response **and the UI** show the recovery command" — and a recovery command nobody can reach
does not satisfy it, so the session screen gets one:

- **a Delete button plus a confirm step.** A destructive affordance with no second tap is the mis-tap
  ADR-0023 was right about; the confirm is the cheapest honest answer and adds no endpoint.
- **`409 unsaved_work` offers the forced delete.** The server's sentence already names `?force=true`; the
  screen turns it into a second button rather than an instruction to type a query string on a phone.
- **the 202's `kept` is the notice.** `transcript.ts`'s `deletedNotice` is a rule with tests, like every
  other sentence the screen shows: it says "nothing was kept" when both names are null, names what
  survived, and appends the command only when there is a ref to run it against.

`stoppedNotice`'s two sentences are deliberately *not* reused: "nothing was deleted" is a promise the stop
family can make and a delete cannot.

## Consequences

- `tests/worktree.rs` proves the rule on a real repository without a daemon, a backend or a request: a
  branch ahead of `main` survives `delete`; the same branch merged into `main` goes; a branch with nothing
  beyond its base goes; a repository with only `master` keeps an unmerged branch; a repository with no
  target keeps it too. Each of them goes red when the guard it names is deleted.
- `tests/delete.rs` asserts the 202's `kept` (nothing, for the stub) and the exact command for a session
  whose snapshot wrote a ref, and that the row is still removed last.
- `scripts/verify.sh` grows `case_delete_keeps`: the stub boot's `kept` triple, the shipped page's
  affordance and command, and — with a daemon and `agent_image` — the real half: commit, modify, delete,
  `agent/<id>` and the ref still there, the worktree and the session directory gone, and
  `git branch rescue refs/holosmith/wip/<id>` succeeding.
- A kept branch is now a thing a machine accumulates. Nothing expires it: HS-064 owns stopped-session
  expiry and these names are not rows, and HS-044's reconcile starts from the branch rather than pruning it.
