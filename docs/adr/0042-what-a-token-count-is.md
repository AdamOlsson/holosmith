# What a token count is, and what "no usage" means

HS-023 built the budget against the stub, and its own review recorded the hole: the ceiling was exercised
against a script's numbers, so nothing had yet said what a *provider's* numbers mean. HS-034 is that story,
and it turns out to be two decisions and one piece of evidence.

Spec item 13 is one line long: "Sessions gain `usage_in` and `usage_out`, updated from the `usage` on
`message.end`. If pi does not report usage, the token budget is disabled; confirm in the vertical slice." V3
repeats it as "a real turn … reports usage".

## A prompt's cached part is part of what it cost

pi reports six counters per assistant message: `input`, `output`, `cacheRead`, `cacheWrite`, `reasoning` and
`totalTokens`. HS-032's mapping kept the first two and deferred the rest here, on purpose.

Keeping only `input` would make the budget a fraction of the bill: a conversation with a system prompt and a
growing transcript is mostly **cache reads** — the second message of the committed tool fixture reports 93
prompt tokens and 1664 cached ones — so a session could run for a long time on a ceiling that thought it had
spent almost nothing.

So the contract's two counts are:

```text
in  = input + cacheRead + cacheWrite
out = output
```

`reasoning` is deliberately **not** added: it is already inside `output`. That is not a guess — the fixtures
show it. The thinking turn reports `input: 1492, output: 10, reasoning: 4, totalTokens: 1502`, and only
1492 + 10 reaches the total; the tool turn reports `input: 1408, output: 76, cacheRead: 256, totalTokens: 1740`,
and only 1408 + 76 + 256 does.

That equality is the *test*, not the comment: `tests/rpc.rs`'s
`every_usage_the_fixtures_report_adds_up_to_pis_own_total` renders every usage record of every committed
fixture through the production mapping and asserts `in + out == totalTokens`. A provider that starts reporting
`reasoning` outside `output`, or a counter that moves inside `input`, fails there and says which fixture and
which record — which is a better answer than a constant that would be wrong in the same way the code was.

The payload does not change shape: `message.end` still carries `{"usage": {"in", "out"}}` (ADR-0019, amended by
ADR-0040). What changes is what the two numbers *count*, and this ADR is that change's record.

## A report of nothing is not a report

A provider that answers `usage: {input: 0, output: 0, …: 0}` used to produce `usage: {in: 0, out: 0}` on the
frame. Two things then went wrong quietly: the session screen could not say "no usage reported", because a
usage frame *had* arrived, and the budget counted a zero for ever.

Spec item 13 has one condition — "if pi does not report usage, the budget is disabled" — so this build has one
answer: a total of zero emits **no** `usage` key at all, exactly as a message with no `usage` object does. The
screen's sentence (`budgetNotice`, HS-023) and the disabled ceiling then agree by construction, because both
read the frame rather than a separate flag.

Corollary, and the reason the rule is on the *sum* rather than on `input`: a message whose entire prompt was
served from cache reports `input: 0` with `cacheRead > 0`, and that is a real cost — 512 cached tokens is not
"no usage". Only a sum of zero is silence.

## Where the warning is

The acceptance asks that missing usage "surfaces a visible warning once per session". The surface is the
session screen's one sentence, and it is already there: `budgetNotice` renders "no usage reported — the token
budget has nothing to count" when a turn has settled and no message declared its cost. Two properties follow
from where it lives, and they are why it lives there:

- **It is keyed on the log**, so it says the same thing after a reload, after a reconnect, and after a history
  page has been folded in. `history.truncated`'s `turns_before` is what keeps "a turn settled" true for a
  windowed replay (HS-061).
- **It is one sentence, not one per turn.** A `warn` in the server log would be either per-turn noise or
  per-session memory for a log line; the row already says `0`, and the operator reading the screen is the one
  who needs to know.

So: no new column, no new key, no server-side warning. ADR-0027's budget stays exactly where it was.

## "Against the real backend", honestly

A container cannot complete a turn in this build: its network is `--internal` and it holds no provider key
until the key proxy exists (HS-039), which is what `case_docker_turn` asserts by reading pi's own refusal on
the row. So the phase's "against the real backend, not only the stub" is answered with the closest thing that
exists:

- **The numbers are a provider's.** `tests/token_budget.rs`'s
  `a_real_turns_usage_becomes_the_sessions_tally_and_trips_the_ceiling` reads the committed tool fixture — a
  real turn, captured with the operator's own credentials — maps it through the production `rpc::Mapping`,
  appends the frames to a real writer, asserts the **row's** two columns are that turn's counts, and then runs
  the real `Turn::run` over that row with a ceiling one token below it and asserts `budget_exceeded`.
- **The container is HS-046's.** The phase-1 verification run replays this on a live host with the proxy in
  place, and V3 is where the whole path (deltas, a tool round trip, usage) is confirmed end to end.

Recorded rather than glossed: until then, the ceiling's *arithmetic* is proven against real records and its
*wiring* by the stub, and nobody has yet seen a container's usage land in a row.

## What this story does not decide

- **`reasoning` in `out`** is pi's current behaviour; the equality test is what would notice a change, and
  folding it in explicitly would double-count today.
- **A per-message breakdown** (which message cost what) is not stored: `message.end` carries it in the log and
  the row carries the total; a screen that itemised spend would need a projection, not a column.
- **The cost in money.** pi reports `cost` per message and this build ignores it: the spec's ceiling is tokens,
  and a currency total has a pricing table's lifetime to answer to.
