# A plan's actions are lines under its step

**Amended:** this block shape is what `--verbose` prints, and the default is one marked line per step — the section
"The terse report is the default, and the marks are its words" below. What a run prints *while* it works, beside the
report, is ADR-0092's. **Amended by ADR-0094:** a `satisfied` or `skipped` line's remark is printed under its mark, so
the clause that says which binary the unit ends up running reaches the default report rather than only `--verbose`'s.

ADR-0056 made a deploy step print **one line**, `<n>. <step>: <word>: <sentence>`, and six of the seven steps put a
list in that sentence. The shape was right for the words that carry a verdict — `satisfied`, `skipped`, `failed`, and
step 3's `changed`, which is one sentence about two images — and wrong for every step that prints what it would do: a
first deploy's step 2 is ten actions, joined with `; `, and on an 80-column terminal that is one paragraph of eight
wrapped lines with no way to see where one action ends. The `useradd` that has to succeed before any `install` can is
the first thing the wrap loses, and it is the first thing an operator needs to read. The dry run is the report an
operator reads *before* granting sudo, so the list is the whole point of it.

## The step's line is the contract; the list goes under it

The step line is unchanged — `2. files: needs: 10 actions:` — and everything the line carried that is not the list
stays on it: the count, and the remark a few steps carry about what the run does *not* do. Under it, **one action
per line**:

```text
deploy: dry run
deploy: into the system paths, with sudo where each write needs it

1. preflight: satisfied: 11 findings: 10 pass, 0 fail, 1 unknown, 0 info (release)

2. files: needs: 10 actions:
    - useradd --system --groups docker --home-dir /var/lib/holosmith --shell /usr/sbin/nologin holosmith
    - install -d -m 0755 /etc/holosmith
    - install -m 0644 <staged> /etc/holosmith/config.toml
    …
    - chown -R holosmith:holosmith /var/lib/holosmith

3. images: satisfied: …
```

**The example's *contents* predate ADR-0086** and are kept as the shape's record: there is no `useradd`, no `chown`
and no `/etc/holosmith` any more, `--root` is no longer a privilege mode, and the lines a deploy prints are the
operator's to type rather than its own children. The shape — one line per action under the step's line, the count on
the step's line, a remark beside the count — is this ADR's and is unchanged.

Three facts about that shape are decisions:

- **The count stays on the step's line, and the step's line stays one line.** It is what `scripts/verify.sh` greps
  (`^N. <step>: <word>: `), what `tests/deploy.rs` asserts, and what two runs are diffed by. Moving the count into the
  block would have left the step's own line saying only `needs:`, and a reader diffing two runs would have to read the
  list to learn whether it grew.
- **The bullet is `    - ` and not `1. `.** A number would collide with the step number: the gate reads step order
  with `sed -n 's/^\([1-7]\)\. .*/\1/p'`, and a plan whose actions began `1. `, `2. ` would be counted as steps. The
  indent is four spaces, deep enough to read as a list under the step and shallow enough to leave a long `useradd` on
  one line on a normal terminal.
- **A remark takes the count's colon.** `2. files: needs: 10 actions; --root: no service user is created, because the
  tree belongs to the invoking user` — the remark (`ROOT_REMARK`, `ROOT_UNIT_REMARK`, the `agent_image` note) is a
  statement about the whole step, so it belongs beside the count, not after the last action where a reader who
  skimmed the list never sees it. It already starts with `; `, which is why the colon gives way rather than doubling
  into `:;`.

**Every block is separated by one blank line, the header's included, and the tally has a block of its own.** Seven
multi-line steps stacked without a gap is the wall the list was meant to break up, and the tally is a summary rather
than an eighth step.

## What this changes for a caller

`Line::sentence` may now hold newlines, which is the one type-level consequence: `Line`'s `Display` is
`<n>. <step>: <word>: <sentence>` and always was, and a sentence with a list in it simply prints as more lines. The
three words that do not list actions (`satisfied`, `skipped`, `failed`) are unaffected, and so is step 3's `changed`,
whose sentence is `images::ensure`'s own.

The gate's *content* checks move with it: `scripts/verify.sh`'s `deploy_block` extracts a step's line and the
indented lines under it up to the blank line, so "step 5 names the unit's path" and "step 6 names both `--https`
targets" are assertions about the block rather than about a single grep'd line. That is the one place the new shape
could have quietly weakened a gate — a content check that had kept grepping the step's line would have passed over a
plan that said nothing.

## The terse report is the default, and the marks are its words

The block above is written for the operator who must *act* on a step: every sentence says what the read found or the
apply did, and the list says it one line at a time. That is the right shape for a machine that needs something and a
wall of text when all six steps passed — a green run is six paragraphs to say "yes", and the two lines an operator
actually needs (the URL, the next command) are at the bottom of it. So the report has two renderings and the **terse
one is the default**:

```text
1. preflight ✓
2. files ✓ (3 actions)
3. images ✓ (built 2 images: holosmith-proxy:0.1.0, agent-image:0.1.0)
4. proxies ✓ (2 actions)
5. service ✓ (1 action)
    still yours to run: systemctl --user daemon-reload; systemctl --user enable --now holosmith.service
6. health ✓
role: both
url: http://localhost:8444
next: holosmith key set <provider>
next: holosmith hub host add <name> <url>
```

`--verbose` prints the block this ADR described, byte for byte as every release before it did — the header, the root,
the blank line between blocks and the tally — and `install`, `server deploy`, `hub deploy` and the remote deploy all
print the same two renderings. Five facts are decisions:

- **The mark is the verdict, and it is defined once.** `Word::mark` answers `✓` for `satisfied` and `changed`, `–`
  for `skipped`, `!` for `needs` and `✗` for `failed`. One function rather than four `match`es in four renderers,
  because the marks and the renderers are one vocabulary. `satisfied` and `changed` share `✓` deliberately: a reader
  who wants to know what a run *did* reads the parentheses, and "applied everything" and "needed nothing" are both
  green.
- **A step with nothing to act on prints its mark, and the remark when it has one.** `satisfied` and `skipped`
  carry no detail: the sentences behind them are a count of findings and a reason for the skip, and both are
  `--verbose`'s — except for the remark, the `; `-separated clause a few steps append about what the run does **not**
  do or about which binary the unit ends up running, which prints on the indented line a `changed` line's remark uses.
  That clause is the one part of those sentences an operator may have to act on, so a rendering that dropped it would
  be a clause written for nobody.
- **A step that writes keeps its first clause, and a step that needs keeps its first line.** `changed` adds a
  parenthetical in the sentence's own words — `(7 actions)`, `(built 2 images: …)` — and, when the step carries a
  remark, that remark on an indented line under it, because a remark is a command the operator may still have to run
  (`systemctl --user enable --now holosmith.service` is one). `needs` keeps the sentence's whole first line — the
  count *and* the remark — and every indented action line under it: a step whose remaining work is the operator's
  (`usermod -aG docker $USER`) must not lose the command in the shortening.
- **A failure keeps its sentence and gains one `fix:` line.** `fix` is a field on `Line` (`Option<String>`) rather
  than prose a renderer cuts out, so it travels as a value and is printed as `    fix: chmod 0700 /…/data` — one
  line, pasteable, which is why it is a field and not a second sentence. A step that knows a better repair than a re-
  run
  supplies it (step 1 names `holosmith doctor --quick`); a step that does not is filled in by the command layer with
  the run's own resume command, which is the command that just failed. install's health line is the second kind: it
  names the journal in its own **sentence**, which is where a diagnostic read belongs, and keeps the resume command as
  its `fix:` — the field is the one command that puts a failed step right. So every failed step a reader sees carries
  exactly one command. **The verbose rendering has no `fix:` line**: there the resume line is still the run's last, as
  it always was, and a second copy of it would be one fact in two places.
- **The header and the tail are the command's, not the report's.** A real run prints no header in the terse shape; a
  dry run prints exactly one, and it says what a plan is — `install: dry run — nothing is written` — because a plan
  read as a result is the misreading the line prevents. The tally and `deploy: into <root>` do not survive into
  terse at all. The tail does, without the verb: `role: …` (deploy's), `url: …`, `next: …`, where the verbose shape
  keeps `deploy: url: …` and `install: next: …`. The remote deploy's own lines (`deploy: ssh: …`) keep their verb in
  both, because they are the transport's account of what it sent rather than a step of the report.

install's own three lines are unnumbered in both renderings — they are not the spec's six steps — and in the terse
one a `changed` line of theirs stops at the mark: those sentences narrate what the run just did, so their first clause
is the whole sentence and a parenthetical of it would be longer than the line it replaced. A `needs` or a `failed` one
of theirs keeps everything, for the reason above. That is why the one arm of install's unit line that leaves a command
— the manager was already running the unit, so the bytes this run wrote are served only after
`systemctl --user restart holosmith.service` — is `needs` and not `changed`: a `changed` line stops at the mark, and a
command must not stop at it.

**The gate pays for both renderings.** Every case that greps report words runs with `--verbose` and keeps its greps
unchanged. Two runs do not carry the flag, and what they measure is something else: one case,
`case_install_terse`, writes the expected terse lines to a file and `diff -u`s them against a real run's stdout, so
the shape an operator actually reads is measured once as bytes instead of as a family of patterns; and
`case_install`'s third section is a plan, where the words are not the point (its `--dry-run` report is the one the flag
always printed) but the silence on stderr is.

## Refused

- **Wrapping the line at the terminal width.** The report is printed to a pipe as often as to a terminal, `COLUMNS` is
  a request rather than a fact, and a soft wrap in the middle of `install -m 0644 /etc/holosmith/config.toml` splits a
  path. A wrapped line is also unparsable: `grep` sees the whole line, a reader sees a break that is not one.
- **A hanging indent aligned under the step's word.** Twelve columns of indent for every action, or an action whose
  first line starts at column 0 and whose second is indented — which is exactly the ambiguity a list is for.
- **One line per action with no count.** The count on the step's line is what a script and a diff read; without it a
  `needs:` line says nothing until the whole block is counted by eye.
- **Only the dry run.** A `changed` step lists the same actions after applying them, from the same helper: two shapes
  for one list would be a second spelling of the same thing, and the `--dry-run` output would stop being the run's
  output with one word changed.
- **Making the terse shape the only one.** The sentences are the record of what a run measured — an operator
  diagnosing a machine reads the findings count, the root, the tally and each step's own words — and a release that
  deleted them would make a deploy less explicable to make it shorter. `--verbose` is that output, unchanged.
- **A second word vocabulary for the marks.** `satisfied`/`changed`/`needs`/`skipped`/`failed` are the words the code,
  the tally, the tests and the ADRs use; the marks are those five words' short spelling, and one `Word::mark` keeps
  them from drifting into four renderers' opinions.
- **Cutting the detail a `needs` line carries.** The count alone was tried and dropped: step 2's `usermod` lives in
  that line's remark, and "a machine that needs something keeps everything it is the operator's to run" is the
  sentence this shape exists to satisfy.
- **A `fix:` line in the verbose rendering too.** It would duplicate the resume line that is already the run's last,
  and `--verbose` is byte-for-byte what earlier releases printed — a new line there would break every diff, grep and
  test that reads it.
