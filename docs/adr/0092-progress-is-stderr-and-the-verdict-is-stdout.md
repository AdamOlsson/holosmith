# Progress is stderr and the verdict is stdout

An install printed nothing until it was over. `install` ran the deploy, wrote the unit, set linger and waited for
`/api/health`, and the first byte a reader saw was the report — minutes later, because step 3 builds two Docker images
and the first of those downloads pi inside the build. Step 3's own log goes to a temporary file
(`docker::build_bounded` keeps it for the failure sentence), so even `docker` said nothing. The second silence was the
end: `doctor --full`, a one-second wait, then `systemctl --user`, `loginctl` and a wait for `/api/health` that may take
the configured minute. A first install on a bare machine is the one command in this tree whose whole promise is
"from nothing to serving", and its silence is indistinguishable from a hang — which is what a reader concludes, and
then interrupts.

The report cannot fix that: it is written when the work is done, and one line per minute of work is not progress. So a
run says what it is doing **while** it does it, and what it says goes somewhere else.

## D1 — a sink, not prints from inside the steps

`crate::progress::Progress` is two methods, both with a body that does nothing:

```rust
fn begin(&mut self, label: &str, phrase: &str)  // a step is about to be evaluated
fn note(&mut self, label: &str, phrase: &str)   // one fact inside a step
```

Two implementations: `Silent`, which is the default and says nothing, and `Announce`, which prints. The seam is
`&mut dyn Progress`, passed down and never stored, and the run calls it **before** the work it describes — a line
after the fact it announces would be a line that arrives when the reader no longer needed it.

**Why a sink rather than an `eprintln!` where the work happens.** The two long steps are in three different modules
(`deploy`, `images`, `commands::install`), and a print in each is three spellings of one format, three places that
decide the stream, and no way for a test to assert a sequence without capturing a process's stderr. The trait is also
what makes the asymmetry in D4 possible at all: the same call site tells one caller nothing and another caller
everything.

**Neither method can fail.** A sink is an observer; an observer that could refuse would be a second place the run's
order is decided. `Announce` ignores a write error for the same reason — a closed `stderr` is a reader who stopped
reading, not a step that failed.

## D2 — stderr, with the verb and the step's own label

Every progress line is `install: [<label>] <phrase>`, on **stderr**:

```text
install: [3/6 images] building the two images — a first install downloads pi, so this takes minutes
install: [3/6 images] building holosmith-proxy:0.1.0
install: [3/6 images] built holosmith-proxy:0.1.0
install: [3/6 images] building agent-image:0.1.0
install: [3/6 images] built agent-image:0.1.0
install: [6/6 health] asking `/api/health` and `doctor --full` about the machine
install: [health] waiting for `/api/health` on 127.0.0.1:8443 for up to 60 s
```

The report then follows on stdout, and says the same steps in the same order — which is what the labels are for.

- **stderr, because stdout is the report.** `holosmith install > out 2> err` has to leave `out` exactly the verdict —
  the bytes a gate greps and a person reads — whatever the builds printed while it was being produced. A progress line
  on stdout would also be a line whose presence depends on how long a build took, which is the one thing about a
  report that may not vary.
- **The label is the report's own number and word** (`DeployStep::label`, `3/6 images`), so a line that arrived while
  waiting and the line that arrives with the verdict are matched by what they say. install's own three lines carry
  their own words (`unit`, `linger`, `health`) and no number: they are not the spec's six steps, exactly as in both
  report renderings (ADR-0091, ADR-0085).
- **The phrase is a duration expectation as much as a description.** "a first install downloads pi, so this takes
  minutes" is what tells a reader the silence is work rather than a hang, and it lives in `DeployStep::beginning`
  beside the word rather than in a caller's formatting.

## D3 — one line per evaluated step, one line per fact

- **A begin line goes after the skip answer and before the step's first read.** A step this half has no work for —
  a hub's images and proxies — emits nothing: `skipped` has already answered that there is nothing to wait for, and a
  begin line over that silence would be a lie about what it is.
- **The two image builds get a note each, before and after.** The tag is in the line, because two minutes of
  `docker build` and two more are the same silence otherwise, and the second note carries the build's own answer —
  `built <tag>`, or `did not build <tag>` — because the step's report line, which is where the reason is, does not
  arrive until the step is over: a note that said `built` over a failure would be the one line a reader is watching
  asserting the opposite of what happened.
- **install's own three steps get a begin line each, and the health one names its bound and its address** — `waiting
  for `/api/health` on 127.0.0.1:8443 for up to 60 s` — because that is the second silence, and "up to a minute" is
  the fact a reader needs to not interrupt it.
- **A plan prints nothing.** `--dry-run` does the reads and nothing slow, so `install::run` hands it `Silent`.
- No heartbeat, no spinner, no timer thread, no `\r` rewriting: one line per fact, flushed, and then silence until the
  next fact. A spinner is a process that has to be kept alive and a terminal that has to be a terminal; a line is a
  byte that survives a pipe, a log file and a `grep`.

## D4 — only `install` announces

`deploy::run_as`, `deploy::run`, the promotion and a plan all pass `Silent`. `install` is the verb whose silence this
is: it is the recommended first command, it runs unattended for minutes on a machine where nothing else is happening,
and it is the one an operator watches. `server deploy` and `hub deploy` are run on a machine that is already there, by
an operator who is reading the report rather than the void, and a promotion prints its own pipeline's line per step as
it goes (`upgrade: images: …`) — a build's two lines there would be a second spelling of one progress stream.

`run_as` keeps its exact signature and delegates to `run_as_with(…, progress)` with `Silent`, which is what keeps the
sixty existing call sites and both test suites asserting a `Report` rather than a stream; `images::ensure` gained the
sink and the label as two parameters, and `commands::install::execute` gained the sink, because install's own three
lines are slow too.

## What measures it

- `tests/deploy.rs` drives `deploy::run_as_with` with a recording sink: exactly one `begin` per evaluated step, in the
  spec's order, the two `note` calls around each build with the tag in them — `built <tag>` when the build answered
  yes and `did not build <tag>` when it did not — and **no** begin line for a hub's two skips.
- `tests/install.rs` drives `install::execute` the same way: the six steps, then `unit`, `linger` and `health`, with
  the health phrase naming the address and the bound — and a plan handing its sink the same seven lines, because the
  silent sink is `install::run`'s choice and not `execute`'s.
- `scripts/verify.sh`'s `case_install` and `case_install_terse` both run the shipped artifact under `bwrap` with the
  report on stdout and the progress on stderr, and both assert one `install: [<label>] ` line per step the run
  evaluated, no line for the two steps a hub skips, stdout holding not one progress line and stderr not one step line
  — and a `--dry-run` section that puts nothing on stderr at all.
- **The terse shape is measured once, by bytes.** `case_install_terse` diffs a real run's stdout against a file of the
  expected lines (`diff -u`), which is the only thing that pins the rendering an operator actually reads; the greps in
  `case_install` pin the same stream on a fixture of its own — two runs over the manager's tree and a server-half
  config — and neither case is redundant with the other.

## Refused

- **A spinner or a progress bar.** It needs a terminal (so a pipe, a log file, `2>` and every gate case get nothing),
  a timer thread or an async task to turn it, and a line it rewrites — which is bytes whose meaning depends on the
  reader. One static line per fact costs nothing and reads the same in all four places.
- **A heartbeat every N seconds.** "Still working" is not a fact, and it is exactly the line a reader learns to
  ignore; the two lines a build owes are the ones that say *which* image and *whether it ended*.
- **Progress on stdout.** It is the natural place for a CLI's output and the wrong one here: stdout is the report,
  which is the artifact a gate greps and a person diffs between two runs, and interleaving it with lines whose presence
  depends on timing is how a report stops being comparable.
- **One stream, with the report in it, ordered by arrival.** Considered and rejected for the same reason: `install >
  out` is a documented way to keep the verdict, and a merged stream makes it a log.
- **Printing from inside `images::build`.** The build's own log goes to a temp file so the failure sentence can quote
  it; a print there would be a second consumer of that file and a second place the step's order is decided.
- **Making `deploy` announce too.** Tempting for symmetry and wrong for it: `deploy` writes no image of its own
  outside step 3, is the verb an operator runs on a machine they are already watching, and its report is the whole of
  what it prints — the silent default is what lets the one caller that needs progress say so.
- **A `--quiet`/`--progress=auto` flag.** `install` has nothing else to be quiet about, and a tri-state that is one
  state in practice is a flag whose second value nobody measures.
