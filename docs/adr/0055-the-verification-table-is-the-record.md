# A verification cell named its automation, and a skip was never a pass

The phase-1 verification table (V1–V11) was the record of how each check was run. Each result cell carried the verdict and named the automation that produced it: a case in `scripts/verify.sh`, a cargo test, a script, or a by-hand procedure in `docs/build.md`. The table and the specification that held it have since been retired. Two rules it established remain:

- **A skip is not a pass.** A case that prints `SKIP` withholds its `ok` line; a green run must not count a check that measured nothing. The same rule applies to a deploy step: a step that could not read its subject reports `skipped`, never `satisfied`.
- **A check has one entry point.** Every automation lives in the gate, a cargo test, a script, or a by-hand procedure. A fifth entry point would be a second way to run the same check and a second thing to keep green.

`doctor` stayed a machine-fitness command: a check that needs a session or a provider key belonged to a gate case, not to `doctor`. That is why the credential path (V3–V5) is a by-hand procedure in `docs/build.md` while the daemon-free checks are `doctor`'s.

## Consequences

- A skipped step is reported as skipped, never as satisfied.
- A refusal is one sentence with one code, and a second syntax for the same operation is a second entry point.
- `doctor` answers what this machine can do without starting a session; the gate answers what the shipped artifact and a real daemon actually did.
