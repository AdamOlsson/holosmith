# The unified diff is one bounded read of the same repository, and a path is a member of a list

HS-053 is spec item 12's second bullet — `GET /api/sessions/:id/changes/diff?path=` returns the unified diff,
capped at 256 KB, computed with `--no-ext-diff --no-textconv` — under the item's governing sentence: "all git work
for review and merge runs on the host in the project repository; containers never merge". It is a neighbour of
HS-052 rather than new ground: the repository, the base commit, the branch's tip and the ordered refusals are the
summary's, and `changes.rs` already held the file list this story validates a path against. What this ADR records
is the eleven decisions a later reader could take differently — including the two that have a wrong answer which
looks right (the marker's own length, and a path that is resolved instead of compared).

## D1 — A second function in `changes.rs`, and a second handler in `http/changes.rs`

`changes::diff(&Worktree, base_commit, path: Option<&str>) -> Result<Diff, DiffError>`, beside `Changes::read`.
The four ordered refusals and the branch's tip were **extracted** from `Changes::read` into a private
`basis(&Worktree) -> Result<String, ChangesError>` and both call it. Not copied, because "what is this session's
diff base" must have one answer: a `path=` that validated against one list and a patch measured from another would
be a bug nobody could see in either function alone.

The alternative — a third module (`src/patch.rs`) — was rejected for the reason the summary lives here: the
refusal order *is* the changes view's, and a second copy of it would drift the first time an arm is added. The
HTTP half is a second handler in the same file for the same reason: the row lock, the `base_commit` refusal and
the `Worktree::of` call are the summary's scope, and a sibling file would repeat them.

`DiffError` has two arms: [`ChangesError`]'s two families (the four 4xx facts and git's failure) through a
`From`, and the one new fact — `NotChanged`. Folding the path refusal into `ChangesError` was rejected: that enum
documents *why there is nothing to review*, and "you asked for a file this diff does not contain" is a different
sentence about a different thing.

## D2 — The answer is a `text/plain` body, and `charset=utf-8` is a claim about the bytes

A patch is text, a phone's browser renders it, and the cap and the marker are byte arithmetic on one body. JSON
wrapping a diff would base64 it or escape every newline, which is worse on both a screen and a wire.

The `charset` is claimed **only when the bytes are valid UTF-8**, which is not the same rule `http/tool_output.rs`
can use: that store cuts on a character boundary, so its file is UTF-8 by construction, whereas here the bytes are
a *file's* — git prints them as they are, so a Latin-1 file's diff carries `0xe9` and is not UTF-8. A
`charset=utf-8` there would be a lie, and a lossy conversion would be a worse one: the operator asked to read the
file. `text/plain` with no charset lets the client do what it does with an unlabelled text file.

## D3 — The cap is 256 KB on the **whole body**, so the marker's length is inside it

`DIFF_CAP = 256 * 1024`. The acceptance's bullet is "response is capped at 256 KB with an explicit truncation
marker", so `body.len() <= DIFF_CAP` is the invariant, marker included: the budget for the diff's own bytes is
`DIFF_CAP - marker.len() - 2` (the marker's trailing newline and the separator before it). The `tool.rs`
precedent caps the *output* and appends the marker after it, which is right for a stored file whose reader can
look at the size; a response has no such second channel.

The cut lands on a **line boundary** — a diff is line-oriented, and half a hunk line is a lie about content the
reader is looking at — and then on a UTF-8 boundary **only when the cut fell mid-sequence** (`error_len()` is
`None`). A patch that is genuinely not UTF-8 keeps its bytes and says so in the header (D2).

The marker names **the total**, never "showed N of M": N is the cut, the cut depends on the marker's length, and
that circle either truncates the marker or breaks the cap. What was shown is whatever the body holds above the
marker line, which a client can measure.

```text
[diff truncated: the diff is 2097152 bytes; only the first part is shown]
[diff truncated: the diff is at least 8388608 bytes; only the first part is shown]
```

## D4 — A binary file is summarised by git's own line, and `path=` naming one is a `200`

Measured on git 2.43: `git diff <base> <head> -- bin.dat` prints

```text
diff --git a/bin.dat b/bin.dat
index 0000000..d997920
Binary files /dev/null and b/bin.dat differ
```

for an added file, and `Binary files a/x and b/x differ` for a modified one, with **no NUL byte** on stdout. So the
summary is free, and inventing a sentence ("binary file, 1.2 MB, 0 lines") would be a second vocabulary for a fact
git already names and a client already knows how to render. `--no-textconv` is what makes it trustworthy: a
`.gitattributes` filter cannot make a binary file look textual, and `--no-ext-diff` means the repository cannot
substitute its own program for either.

`path=` naming a binary file answers `200` with that line rather than a `4xx`: a summary *is* the answer to "show
me the diff", and refusing would tell a client the file is not in the changes — which is false.

## D5 — `path` is validated by **exact membership** in `changes::files`, and a miss is `404 not_found`

The list is the summary's answer, from the same two-dot basis with the same `--no-renames`, so the index a client
picks from and the list the patch is validated against are one list. One `404` with its own sentence covers the
three ways to miss — a file the branch did not touch, an absolute path, a `..` traversal — because the client's
move is one: pick a path out of `files[]`. `422 bad_query` would say the parameter is malformed, and it is not.

**Traversal is impossible by construction, not by cleaning up.** The parameter is never joined to a filesystem path
and never given to git until the membership test has passed; it is only ever compared with strings git itself
printed with `-z`. `sub/../a.txt` is the case that proves the difference: it *normalises* to a changed file, so an
implementation that resolved it would return a patch, and this one returns the same `404` as `keep.txt`. `--`
precedes the path in the argument vector anyway, so a name beginning with `-` can never be read as an option.

The handler percent-decodes the value before comparing (`%20` is a name with a space; a multi-byte character
arrives as several decoded bytes, read the way `files[]` reads a path — lossily), and an escape that is not two hex
digits stays literal, which makes it a name no file has rather than a second kind of error.

The vector also leads with **`--literal-pathspecs`**, because a git pathspec is a *glob* by default while `files[]`
is a list of exact names: a changed file literally called `a*.md` would otherwise hand git a pattern and get back
the union of every match, and a leading `:` would be read as pathspec magic. The membership test is what refuses;
this is what makes the patch that follows it *that file's* patch.
(`a_name_that_looks_like_a_pattern_is_the_name_and_not_a_pattern` measures it, and goes red without the flag.)

## D6 — No `path` is the whole branch, and an empty branch is a `200` with an empty body

The demo asks for both, so the parameter is optional. The whole-branch answer is capped at the same 256 KB — "how
big is a patch" does not depend on how many files it covers — and it truncates the same way, with the same marker.
Nothing changed yet is a real answer: `200`, `text/plain`, zero bytes, which
`tests/changes.rs`'s `an_empty_branch_is_an_empty_patch_rather_than_an_error` measures on a session with no commit
on its branch.

## D7 — `--no-renames` on the patch, at the cost of a rename reading as a delete plus an add

`files[]` is built with `--no-renames` (ADR-0061's D9), so a rename is two paths. A patch that detected renames
would print `{old => new}` in one header, and a `path=` validated against `files[]` would name a file the patch's
own output did not contain. One spelling per path is worth more than the shorter patch: the reader still sees
every line that changed, and `files[]` is the index that names both halves.

## D8 — Commits only: `<base_commit>..<head>`, never the working tree

Uncommitted work is `dirty`'s fact on the summary, and the WIP snapshot (`refs/holosmith/wip/<id>`, ADR-0049) is
how a reader sees the working tree. A patch that included it would answer "what is in `/work`?" under two commit
ids, and would move between two identical requests while the agent works. `tests/changes.rs`'s
`the_result_does_not_depend_on_the_working_tree` writes an uncommitted change and asserts the body does not move.

## D9 — The child is bounded, not buffered: 256 KB kept, 8 MiB counted, then killed

`Command::spawn` with a piped stdout, read in 64 KiB chunks; the first `DIFF_CAP` bytes are kept and everything
else is counted up to `COUNT_MAX = 8 * DIFF_CAP`, past which the child is killed and the marker says "at least N"
instead of printing a number it stopped measuring. **stdout's** memory is the cap plus a chunk; time is bounded by
8 MiB of git's output rather than by the size of a file an agent chose. stderr is drained whole on its own thread —
bounded by what git prints there, which for `git diff` is a sentence, and reading it to the end is what keeps a
large answer from deadlocking. Every diff under 8 MiB — the demo's 2 MB included —
gets an **exact** total, which is what makes the marker worth reading.

stderr is drained on its own thread, so a git that says a great deal cannot fill its pipe and deadlock against our
reading of stdout. A non-zero exit **before** the ceiling is `ChangesError::Git`, the `500`, with git's own words
in the log — never an empty `200` (the `Merge::Unasked` instinct). An exit status observed *after* a kill is not a
verdict, because the ceiling is ours and not git's.

## D10 — Host git, `spawn_blocking`, no lock

The same as ADR-0061's D9/D10, and less: `git diff` writes nothing at all — no working tree, no index, no ref, no
object. It is the one session route that needs nothing from the backend and nothing from the database after the
row's `base_commit`.

## D11 — The envelope is the summary's, plus one flag in the patch's vector

| Condition | Status | Code |
| --- | --- | --- |
| no such session | 404 | `not_found` |
| no `base_commit`, worktree gone, unregistered, branch gone, repository gone | 409 | `no_worktree` — the summary's four sentences, byte for byte |
| git cannot be run, or exited non-zero | 500 | `internal`, git's words in the log |
| `path` is not in `files[]` | 404 | `not_found`, its own sentence |

The diff inherits the summary's order and sentences because they are the same facts about the same session: a
client that learned one route learned both, and a session with no worktree must not answer one code for the
summary and another for the patch. A path refusal is a different fact — about the request, not the session — and
so does not reuse `no_worktree`.

`--no-color` is appended in the patch's own vector rather than added to `DIFF_FLAGS`: a repository's
`color.ui = always` would otherwise put escape codes in a body meant to be read on a phone, and the two counting
reads are never coloured, so the three flags that change an *answer* stay one list.

## What this ADR does not decide

`POST …/merge` and its rules (HS-054 – HS-057), `publish` (HS-058), the Changes sheet, the summary's fields, any
write of any kind, and `worktree.rs`'s create refusals. `publish`'s warning before a remote is `touches_ci`, which
is already public and unaffected by anything here.
