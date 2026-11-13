# A plan's actions are lines under its step

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

1. preflight: satisfied: 11 findings: 10 pass, 0 fail, 1 unknown (release)

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
