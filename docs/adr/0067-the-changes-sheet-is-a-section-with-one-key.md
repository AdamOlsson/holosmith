# The Changes sheet is a section of the session screen, and its pickers read one key

HS-058 is item 12's last bullet — the screen the other five routes exist for. Every route it draws was already
built and measured (`GET …/changes` HS-052, `…/changes/diff` HS-053, `POST …/merge` HS-054–056, `POST …/publish`
HS-057), and three earlier ADRs deliberately deferred text to this story: ADR-0065 D4's commit prompt, ADR-0064
D10's Resolve affordance, ADR-0066 D11's CI warning. So this ADR decides only what a *screen* adds: where it
lives, where its rules live, how a disabled button knows why, and where two lists a phone cannot otherwise learn
from come from.

One key was added to an existing answer, and D4 says why one and not two, and why not `/api/health`.

## D1 — a section of the session screen, folded away behind one button

Item 12 says "the Session screen **gets** a Changes sheet", and the app has exactly two screens (`/` and
`/sessions/<id>`, ADR-0007's one fallback route, `web/src/main.ts`'s `sessionIdFromPath`). A third screen would
mean a third path the server's fallback had to answer by accident, a second `historyPath`-shaped rule, and a
navigation affordance nothing has asked for. So the sheet is a `<section id="changes">` in the session screen,
between the meta row and the transcript: a *Changes* button, and a body that is hidden with one `[hidden]`
attribute until it is tapped. Not an overlay or a `<dialog>`: the transcript stays where it was, a merge's notice
cannot be dismissed by Escape, and the fold costs no new layout idiom.

The section's shape is `deleteSection`'s (HS-043's affordance): a function that builds nodes, draws itself from an
answer, exposes `update(state)`, and owns its own requests. What it adds is `refresh()`, called from the screen's
one `showState` — see D6.

## D2 — the sheet's rules are `transcript.ts`'s, and that is what makes the acceptance reviewable

Every sentence and every branch the sheet shows is a pure function in `web/src/transcript.ts` with a test in
`web/test/transcript.test.ts`: `fileLine`/`countsText`/`statusWord`, `countsLine`, `ciWarning`, `mergeBlocked`,
`targetOptions`/`preselectedTarget`, `publishVisible`/`publishQuestion`, `conflictLine`, `mergedNotice`/
`publishedNotice`, `diffPath`, `sheetFailure`, and the four constants (`CI_WARNING`, `COMMIT_PROMPT`,
`RESOLVE_PROMPT`, `COMMIT_LABEL`/`RESOLVE_LABEL`). `main.ts` keeps only the DOM: which node, which event, which
request.

That split is ADR-0018's rule applied to the first story since phase 1 that adds a screen's worth of copy, and it
has a second consequence the gate depends on: **no `case_*` may need a browser** (RESUME §3), so `npm test` is the
only gate that can see a sentence. A rule that only the DOM could show would not be measured anywhere, which is
why the file rows, the two counts, the warning, all three disabled reasons, the pickers, the confirm step, the
conflict line and the two prompts are functions — and why `case_changes_screen` (D10) measures only that the
artifact *carries* them.

## D3 — the disabled reason is the read plus the row, in the route's order

`mergeBlocked(state, changes)`:

| Order | Fact | Sentence |
| --- | --- | --- |
| 1 | the row is `running` | "the agent is working in this branch, so a merge waits for the turn to end" |
| 2 | `target === null` | the answer's own `mergeable_reason` (it already names the configured list) |
| 3 | `dirty` | "the session has uncommitted work, so only commits can be merged" |

The order is `http::merge`'s — the row before the worktree — because it is also the order of the operator's next
step: an agent that is working will commit, so that reason is the actionable one even when both hold.

Only `running` disables by state, which is ADR-0065 D2's fact and not a second opinion: `idle`, `starting`,
`stopped`, `error` and `deleting` all still merge, and a `!== idle` guard here would disable the button for four of
the five (the `error` row a failed provision leaves is the one an operator most wants to merge).
`every row state but running leaves the button alone` is the test that would go red.

The client's facts are the **last read** and the last `session.status` frame, and the refusals stay the backstop: a
turn can start between the read and the tap, and a file can be written between them, so `409 session_running` and
`409 dirty_worktree` are rendered as the envelope's sentence in the same notice area. Two spellings of one next
step is what this arrangement avoids and cannot remove.

The dirty reason's shortcut is ADR-0065 D4's prompt, verbatim (`COMMIT_PROMPT`), sent to `POST …/prompt`. The
button appears only when the reason is that one — an "ask the agent to commit" beside a *running* turn would ask an
agent that is already working to do what it is doing.

## D4 — the pickers' options are one additive key on the changes answer: `choices`

```json
"choices": {"targets": ["main"], "remotes": ["origin"]}
```

- **`targets`** is the configured `[merge] targets` **that exist as local branches**, in the config's order, so
  `target` is its first element whenever it is non-empty. Filtered because an option the merge refuses is a button
  that cannot work: `MergeError::NoTarget` is `422 bad_target`, and the default list is `["main", "master"]` on a
  repository that has one of them. The walk is `changes::existing_targets`, which is `first_target` grown a
  return value — one `rev-parse --verify --quiet` per configured name, two by default, and the read no longer stops
  at the first hit.
- **`remotes`** is `[publish] remotes` **verbatim**, and the Publish button exists iff it is non-empty. Not
  filtered: whether the project's repository *has* each remote is the publish route's question (`git remote
  get-url` exits 2 → `422 bad_remote`), with its own sentence, and a configured-but-absent remote is a
  config/repository mismatch the operator should see said out loud rather than silently dropped from a picker.

**One key, not two**, because the sheet asks one question — *what may I do with this branch from here?* — and
answers it from one fetch, and because RESUME §1.8's non-goal allows a single additive key. **Not
`GET /api/health`**, although ADR-0051's `preview_origin` is the precedent for a config value the page needs: health
is the machine's payload (five keys, now pinned by an ADR, its own unit test and four stand-ins in tests and the
gate's shims), and these are one session's, one repository's facts — the kind `target` already is.

The key is last in the struct, because it is the one key a *view* renders rather than measures. The eleven keys and
their `null` shapes are untouched: `target: null` still means "none of the configured targets exist", `behind:
null` still means "there is no branch to be behind", and no client may turn either into a zero.

## D5 — the no-target reason is the server's sentence, not the client's

A `target: null` read already carries `mergeable_reason` ("this project has none of the configured merge targets
(main, master), so there is nothing to merge into"). The sheet renders that string as the merge's disabled reason
rather than writing the second spelling the client would need to keep in step with a config key. The client's own
fallback (`NO_TARGET_REASON`) exists only for a shape the server does not send — a `null` target with no reason —
so a missing sentence cannot disable a button in silence.

## D6 — refresh on a state change, never on a delta

A `changes` read is five git subprocesses (`ahead`, `files`×2, `dirty`, `target`, and the merge trial). The sheet
therefore re-reads at three moments and no others: when it is opened, when the screen's `showState` fires (a
`session.status` transition — a turn ending is exactly when a branch moves, and a step report is not a transition),
and after each of its own requests. Deltas never refresh anything, and the stream's `showState` is the same
callback the badge, the preview link, the composer and the delete affordance already read — a fifth reader of one
seam, not a second seam.

A refresh keeps the operator's selected file and re-fetches its diff, so the pane is never a patch of a branch that
has moved; a path that has left `files[]` clears the pane.

## D7 — one conflict area, two sources, and the retry is the Merge button

Rule 3's state arrives two ways: the read answers `mergeable: false` with `mergeable_reason` naming the paths
("the merge conflicts in: a.txt"), or the tap answers `409 conflict` with the paths in the envelope's additive
`conflicts` key. `conflictLine` renders the paths of a refusal (`conflicting: a.txt, b.txt`, and "git named no
path it could not merge" for an empty list), the area shows whichever sentence is newer, and one **Resolve with
agent** button sits under both — there is one conflict, so there is one affordance.

The button posts `RESOLVE_PROMPT` (D8). The 202 makes the row `running`, so the merge is disabled by D3's first
reason until the turn ends, and **that** is item 12's "then retry": the same Merge button, one turn later. Nothing
invents a second "Retry" button that would have to re-read the branch to know whether it is worth pressing. A
successful read that says `mergeable: true` clears the refusal's words, because the agent's commit is what a fixed
conflict looks like.

## D8 — the two prompts, one reused and one written here

ADR-0065 D4 named the commit prompt when it built `409 dirty_worktree`, so this story reuses that sentence
verbatim; ADR-0064 D10 left the Resolve affordance to this story and named only what it does ("prompt the session
to merge the target into its branch and fix the conflicts, then retry"), so this ADR is where its prompt is written:

> Merge the target branch into your branch and resolve the conflicts, then commit the result and tell me the
> commit's hash.

Both carry "tell me the commit's hash" for the same reason: the retry is a merge of *commits*, and the operator has
nothing to merge until the agent has made one.

The CI warning (ADR-0066 D11's sentence) is one constant shown in two places — beside the counts, and inside the
confirm step before the second tap — because the confirm is the moment the operator agrees to it.

## D9 — the sheet's only state is what the server said last

Two closure variables (the last answer, the last row state) plus three transient facts the body's own actions set
(the selected path, a `busy` flag, a refusal's `conflict` pair). They are the composer's `canPrompt` class of
value — derived from the last update, re-derived on every render, with no lifetime of their own — and
`render()` decides every element's state from them on every pass, so a button cannot stay enabled after the
answer that disabled it.

`busy` exists because a re-render happens while a request is in flight: without it, the merge's own 202-triggered
`showState` would re-enable the button mid-merge and a second tap would be a second merge. Merge, publish and the
two prompts each disable their own buttons, and Merge and Publish use their own `fetch` — rule 6's 60 seconds is
the hub's (HS-069, ADR-0065 D8, ADR-0066 D12), and a blanket timeout on the shared `post` helper would cut the
delete's own `abort_grace` wait.

The sheet does not resume a session for its prompts: the composer owns that ladder (HS-018). A prompt sent to a
stopped session answers `409 session_stopped`, and that sentence names the step (resume it first).

## D10 — `case_changes_screen` measures the shipped artifact, and the rules are `npm test`'s

The gate case (its own repository and projects root, a `_ping` socket, a session whose worktree it re-makes) asserts
three things:

1. `GET /sessions/<id>` is `web/dist/index.html` byte for byte, the comparison `case_session_screen` already makes
   — a `dist/` built before the sheet fails here;
2. the **served** bundle (the `<script src>` in that page, fetched from the running server and compared with
   `web/dist/assets/…` byte for byte) contains the sheet's fifteen element ids and the three ADR sentences
   verbatim — a tree that dropped a rule, or an artifact built before it, is red;
3. the changes answer carries `"choices":{"targets":["main","master"],"remotes":["origin"]}` — both existing
   configured targets in the config's order, and the configured remote though no repository here has one — with
   `touches_ci: true` from a `Jenkinsfile`.

The third is the new key measured over the wire, which `cargo test` also does at the module level; the second is
the only measurement of "the artifact a phone loads contains this screen", which `cargo test` cannot make and a
browser is not allowed to. Neither claims the sheet *works* — that is `web/test/transcript.test.ts`'s, and the
mutation for it is deleting a rule and watching its test go red. A grep of a bundle is weak evidence about
behaviour and honest evidence about shipping, which is why it is a gate case at all and not the whole acceptance.
