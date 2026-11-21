---
description: Orchestrate one Linear ticket end to end — worker implements in a worktree, parent runs the full gate, reviewer writes findings to a file, worker fixes, one squashed commit merges to master, worktree cleaned up
argument-hint: "<TICKET> [notes]"
---
Start the `$1` implementation. `$1` is the Linear ticket id the invoker names (an `HOL-` identifier, or a
`PER-` one from before the move); `${@:2}` is anything extra the invoker wants honoured on top of this script.

**`$1` is supplied by the invoker. If it is empty, stop and ask for one.** Never pick a ticket yourself, never
invent an id, and never start from an issue you happened to read.

Linear work here is scoped to the project **Holosmith** (`P-HOL-2`, team `HOL`). Every `list_issues` and
`save_issue` call carries `project: "Holosmith"`; an issue outside that project is a blocker to report to the
invoker, not a thing to work on. Invoking this prompt is the delegation authorization: use `subagent` directly,
one writer per worktree at a time.

Then, in this order:

1. **Read the ticket.** `mcp__linear__get_issue` on `$1`. State its title and what "done" means in one line
   before touching the tree. Move it to **In Progress** and comment the branch and worktree you are about to
   create.

2. **Create the worktree**, from a current `master`, never inside the repository tree: put it where this
   repository's other worktrees live (`git worktree list` shows them; today that is
   `~/.local/share/holosmith/worktrees/<TICKET>-<slug>`, defaulting to `../worktrees` beside the repo). The
   directory and branch are `<TICKET>-<slug>`, the slug three to five lowercase words from the title.
   Read `git log --oneline --left-right master...origin/master` before you touch it: local `master` is often
   **ahead** of origin with unpushed merges, so "current" means the local branch, and nothing in this flow
   resets it — a `git reset --hard origin/master` throws those commits away. Then
   `git fetch origin && git worktree add <path> -b <TICKET>-<slug> master`.

   **The directory name has a length budget, and the rule is "shorter than the main repository's own path".** A
   gate case binds a unix socket under the worktree at `<repo_root>/target/verify-doctor-tool-key-<pid>/`, and
   `AF_UNIX` caps the path at 107 bytes; at the main repository's own root that socket already reaches 107–108, so
   the repository is the deepest tree the gate is known to pass at, and a longer worktree risks a failure whose
   message names a missing socket and not the depth. The path must therefore be **shorter** than
   `git rev-parse --show-toplevel`'s, not merely short. When the slug would push it past that, use a short
   `<TICKET>` directory and keep the full name on the branch; say which you did on the ticket. Everything below
   happens in that worktree unless a step says `master` or the main repository.

3. **Launch the worker subagent** for the implementation. Give it the ticket's intent and acceptance criteria
   verbatim, the seams you found, and these rules:
   - `. ~/.pi/toolchains/env.sh` first, then **only** `scripts/gate-quick.sh` to verify itself: `--list` for the
     binaries, name every one whose module it touched, and `--lib-only` (saying so in its report) if the change
     names none. It must not run `cargo test` or `scripts/verify.sh` — those are the parent's.
   - Commit nothing, merge nothing, touch no Linear issue. Git and Linear belong to the parent.
   - Escalate an unapproved decision instead of deciding it.
   - **`cargo fmt` first, then one `gate-quick`**: a quick gate that stops on formatting is a cycle spent on
     nothing, and the formatter is the cheapest thing to run before it rather than after.
   - If the ticket changes an image recipe, a throwaway build to de-risk it is worth the wait — but the parent's
     gate rebuilds the product image anyway, so ask the worker to separate what it measured by hand from what
     only `scripts/verify.sh` can measure.
   Run it async (the run wakes this session when it finishes) and return control; do not hold the turn open
   waiting or poll it.

4. **Run the full verification yourself**, in the worktree, detached with the output going to `target/verify.log`
   and poll that file for the sentinel (`verify: <n> assertions, ok` on the last line; a `FAIL case/assertion`
   line names the failure, and the `work dir kept:` line under it names the tree holding that case's evidence).
   Check the log's mtime against the run you started before trusting the sentinel. **Spend ten seconds on the
   preconditions first** — the ones AGENTS.md's prerequisite list names, and each cheaper than the run it saves:
   a daemon that answers, free disk (a run is about ten gigabytes of `target/`, and the three product images'
   build cache is what keeps the rebuild near six minutes), and a worktree path the gate's sockets fit. An
   existing `agent-image:<version>` saves nothing: `case_server_images` untags both product images and asserts
   they are gone before it rebuilds them. If the diff touches `web/`, also run its own gate:
   `cd web && npm ci && npm test && npm run build`. A red gate goes back to the worker with the failing case or
   assertion named — not to the reviewer.

5. **Launch the reviewer subagent** with the diff and the tree it can read. Reviewers here have no shell but
   they do have `read`, so give them both: `git add -A` first (a plain `git diff` hides a new file — a new ADR can
   be a large share of the change), write `git diff --cached` and `git diff --cached --stat` to
   `<main repo>/target/<TICKET>-diff.txt`, paste the stat and `git status --short` into the task, and point the
   task at that file and at the worktree path — so the diff's claims can be checked against the tree, not only
   read. Add the ticket's acceptance criteria. Tell it to **write its findings to a file**:
   `<main repo>/target/review-<TICKET>.md` (gitignored, outside `docs/`, so it survives the worktree's removal and
   is never committed). Use the `code-reviewer` agent — read-only except for that one file; the builtin `reviewer`
   cannot write.
   Ask for P0/P1/P2 with evidence and a merge verdict, and for exactly `No issues found.` when there is nothing.

6. **Launch the implementer again** to fix the review comments from that file: P0 and P1 all, P2 only where it is
   trivial or the ticket asked for it, `scripts/gate-quick.sh` over the binaries it touched, and the same
   no-commit/no-Linear/no-merge rules. Then re-run the full `scripts/verify.sh` (step 4) — a gate that ran before
   a fix says nothing about the fix. Comment the review verdict and the fix summary on the ticket.

7. **Squash to one commit** and merge with master. In the worktree: `git add -A`, then `git reset --soft master`
   (this folds whatever the workers committed into one — and is a no-op when they committed nothing, which is the
   usual case, so a staged tree that is already one commit's worth is the expected shape), then commit one subject
   line in the log's voice (`A refused tool key does not refuse a deploy, and a probe cannot hang one`) and a body
   saying why. **No ticket id and no issue citation in the message**, as in the rest of the log. Then in the main
   repository: `git merge --no-ff <branch> -m "Merge: <the same subject>"` — the shape every merge in this history
   has. Check `git log --graph -3` shows one branch commit under its `Merge:` commit and `git status` is clean.

8. **Clean up**: `git worktree remove <worktree>`, `git branch -d <branch>`, `git worktree prune`, and keep
   `target/review-<TICKET>.md` as the record. Then move the ticket to **Done** with a comment: what changed, the
   commit subject and its merge, the gate result (`verify: <n> assertions, ok`, plus the web gate when it ran),
   and the review verdict.

Throughout:

- **Keep the ticket current.** In Progress when the work starts; a comment when the gate goes red and again when
  it goes green; the review verdict and the fix round; Done only once the merge commit exists and the worktree is
  gone. Never Done on a red gate, an unmerged branch, or a `BLOCK` verdict.
- **Stop and report instead of improvising**: no ticket, a ticket already `Done` whose work is in `master` (check
  before cutting a worktree: an id is not an instruction to redo merged work), a ticket outside Holosmith, a red
  gate after two fix rounds, a conflict on master, a failed subagent launch, a `BLOCK` verdict after the fix pass.
  Leave the worktree and branch where they are, say exactly what state they are in, and say the same on the ticket.
- One `scripts/verify.sh` at a time, and never `pgrep -f` a pattern that appears in your own command line.
- Nothing in this flow adds an issue identifier to code, comments, help text, docs or commit messages.

Finish with a short report: ticket, subject line, gate result, review verdict, merge commit, cleanup state.
