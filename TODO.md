# TODO

## Workflow Improvements
### Less repeated work

- [x] **Put the gate invocation in AGENTS.md at the top of "Build and gates"** (the agent already added this at your request): `. ~/.pi/toolchains/env.sh && scripts/verify.sh`, ~15 min, success sentinel `verify: N assertions, ok`. Also state: *run it detached with a log, poll the log, don't use `pgrep -f` on a string in your own command line.*
- **Add a `scripts/gate-quick.sh`** (fmt, clippy, `cargo test --lib`, plus the named test binaries for changed areas) and tell workers to use it; reserve full `cargo test` and `verify.sh` for the parent. Workers died re-running the 2-5 minute full suite 2-3 times.
- **Set `timeoutMs` explicitly** for implementor/fixer runs (e.g. 3600000), and tell workers in the task: *run the full gate at most once, at the end, redirecting to a log file and grepping it.* Use `checkpointBeforeDeadlineMs` so a dying worker reports state rather than vanishing.
- [ ] **Create a Linear id map** Make the task prompt say "fetch by title query `HS-083` within project Holosmith" so the first `get_issue HS-083` failure doesn't recur.
- **Make a reusable "ticket pipeline" skill/prompt template** (implement → review → fix → gate → commit → Linear). This session's chain was the same shape as the prompt; encode it once, including: branch from `gitBranchName`, Linear comment checkpoints, which model/thinking flags, exclude-file list, commit message style (imperative subject, prose body).

### Less searching for tools

- [ ] **Define the agents you actually use** in `~/.pi/agent/agents/` (or project `.pi/agents/`): `hs-implementor` (worker, writer, with a quick-gate instruction baked in), `hs-reviewer` (reviewer + `write` limited to a review file path), `hs-fixer`. Pin `model: opencode-go/deepseek-v4.1-flash` and `thinking` in frontmatter so the `:medium` suffix isn't needed on every call. Document the model string format in AGENTS.md once.

### Fewer misunderstandings

- [ ] **State acceptance gates unambiguously in the ticket template**: "gate = `. env.sh && scripts/verify.sh`" rather than "scripts/verify.sh if the environment allows". The worker's "can't run here" was a reasonable reading of that hedge.
- [ ] **Add a rule: "an environment limitation claim must cite where you looked."** The wrong "no musl toolchain" claim came from not reading `docs/build.md`'s first code block. A line in the system addendum like "before reporting a tool as missing, grep docs/ and AGENTS.md for its setup command" would have caught it.

### Harness and settings

- [ ] **Raise or tune the "needs attention" threshold** for known-long commands (`cargo test`, `verify.sh`), or have those run via a wrapper that streams to a log; the 240s notice cost a diagnosis detour. A `toolTimeoutMs` is fine here since cargo test legitimately runs minutes.
- [ ] **Child hygiene:** leaked processes (`holosmith serve` from 4h earlier, many `push-receiver.mjs`) accumulate from failed gate runs. Add a `scripts/gate-cleanup.sh` (kill by pidfile or `target/verify-*` pattern) and call it at the start of `verify.sh` or in the parent's pipeline.
- [ ] **Sequence verify.sh before the review.** The review ran on code never gate-tested at full depth. Order: implement → quick gate → full `verify.sh` → review → fix → re-run only affected cases. Then the 0664 fixture issue would have surfaced before review rather than after merge-prep. Consider `verify.sh --case NAME` support so reruns after fixes cost seconds not 15 minutes.

