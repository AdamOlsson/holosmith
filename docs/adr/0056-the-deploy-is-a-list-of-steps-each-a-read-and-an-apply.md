# The deploy is a list of steps, each a read and an apply

HS-047 is spec §"What `server deploy` does" and the CLI's `holosmith server deploy [--ssh user@host]
[--binary PATH] [--dry-run]` row, and the slice's acceptance says four things about behaviour: it "runs
steps 1–7 from the spec in order, each checking current state first"; `--dry-run` "lists the steps and their
planned actions without doing anything"; a failure "stops at the failing step, leaves the machine in a
describable state, and prints the resume command"; and it "runs as the invoking user with sudo where needed,
never assumes it is root". The demo is the same claim from the other side: "run it twice on a fresh host; the
second run changes nothing and reports 'already satisfied' per step".

## The step is a pair, and it has five words

"Each checking current state first" and "the second run changes nothing" are one decision, so a step is a
**state read** and an **apply**, never one function that decides and acts. A dry run prints the read; a real
run prints the read's answer and then runs the apply. That is `doctor`'s one-function-two-arms shape
(ADR-0054) and `worktree::ensure`'s `Ensured { created }`.

Every step prints one line, `<n>. <step>: <word>: <sentence>` — `Finding`'s three colon-separated fields, so
the number is the spec's order and the word is the check's outcome. The words are exactly five:

| word | when |
| --- | --- |
| `satisfied` | the read found nothing to do |
| `needs` | a dry run: the actions the step would take |
| `changed` | the apply ran and did them |
| `skipped` | another story owns this step's body, **and the line names it** |
| `failed` | the read or the apply failed; the run stops here |

**A `skipped` step is never `satisfied`.** Three of the seven steps have no body in this release — step 3's
image builds are HS-048's, and steps 5 and 6 are HS-049's — and the wrong answer is a step that prints
"already satisfied" over a check that read nothing, which is the P1 shape ADR-0055 names. So a skipped step
carries the owning story in its sentence, in the dry run and in the real run alike, and the report's summary
says how many steps are still other stories' (`deploy: incomplete: 4 of 7 steps belong to later stories`).

**Step 7 is HS-049's entirely** — both its `/api/health` wait and its `doctor --full`. The wait is
unsatisfiable here because nothing starts `serve` (step 5 is the unit file) and `doctor --full` asserts the
image step 3 builds, so on a machine where step 3 is skipped the last step would fail for a reason another
story owns; the first draft of this story's plan proposed waiting against a boot the *gate* had started, which
is a measurement of the test harness rather than of the machine. The acceptance's "ends by printing the URL
and the next steps" is therefore the **command's tail**, not a step: it is printed by any run that reaches
the end, whether or not later stories' steps were skipped.

**Step 2 is split, and the split lives here rather than in the story file.** This story creates the data
directory, `/etc/holosmith/config.toml`, the 0700 secrets directory with its 0600 `keys.toml` and the admin
socket directory — the four paths the tree already writes (`key::Store::ensure_file`, `config::render`), which
is what the handoff assigned it. The **service user** and the **VAPID keys** are HS-049's: its slice claims
both, `useradd`/`usermod` are absent from this machine (measured), and VAPID has no consumer until HS-066, so
writing them now would be a file whose shape HS-049's consumer would have to change. `docs/stories/**` is the
source the Linear issues were imported from and is not edited, so HS-049 will *extend* step 2 rather than
rewrite it, and this paragraph is the instruction that says so. The admin socket directory's path is
`/run/holosmith` (`<root>/run/holosmith` under `--root`) — the spec names the directory and no path, so HS-051
may keep or move it.

## `--root` is the install tree, and privilege follows it

The spec's paths are system paths, and this sandbox lets an unprivileged user write `/var/lib`, so a test that
used them would silently install into its own root and still pass. `--root DIR` therefore prefixes **every path
the deploy installs** — and *nothing it reads about the machine*: `data_root` and `projects_root` are
prefixed (and the `config.toml` it writes carries the prefixed values), while the docker socket,
`doctor::SYSTEMD_DIR`, the listen addresses and step 4's binary mount source are facts about the machine and
stay as they are. That asymmetry is the flag's whole meaning, so it is stated in the type's doc comment rather
than left to be inferred.

Privilege follows the same flag: with no `--root` the paths are the system's, so each file action is one child
prefixed by `sudo`; with `--root` the tree is the invoking user's, so the same child runs without it. The
deploy never reads a uid — a `getuid` is not reachable without `libc` anyway, and "may I write this tree" is a
property of the tree, not of the caller. A missing `sudo` is a step failure with a sentence, not a panic.

Every file action is one program: `install -d -m` for a directory, `install -m` for a file that is absent or
whose bytes differ, and `chmod` to repair a mode. **`keys.toml` is never installed over an existing file** —
`install` truncates, and that file holds every stored provider key — so it is created only when absent and its
mode is repaired with `chmod`; `config.toml` is installed only when its rendered bytes differ, which is what
makes the second run honestly `satisfied`.

## The resume is the same command

The acceptance says a failure "leaves the machine in a describable state, and prints the resume command". Since
every step reads state first, the resume is **the command that just ran** — with the same `--root` and
`--config`, resolved to absolute paths, and never `--dry-run` — because a resume with different syntax would be
a second entry point, which is the thing ADR-0055 refused. No `--from` flag exists: it would be a way to skip a
step whose state read is the only thing that makes re-running safe.

## Step 1 runs `doctor --quick` with one recorded divergence

Step 1 is `doctor::run(config, Depth::Quick, …)`, not a second list — `serve`'s refusal, `doctor`'s line and
this step's line carry the same sentences (ADR-0028, ADR-0054). A failing finding stops the deploy, verbatim.

One finding is downgraded, and only one: **`ports`**. `doctor --quick` fails a held port on purpose — its two
moments are *before a start* and *after one, via `--full`* — but `deploy` is the one caller that legitimately
runs on a machine that is already serving, because the spec promises that re-running is safe. So a `ports`
failure becomes a remark on the step's line (`the control port is held — a re-run on a serving machine is not a
failure here`) and every other failure still stops the run. `doctor`'s own exit code is unchanged and
`tests/doctor.rs`'s two-moments test is untouched. The consequence, recorded rather than discovered: on a
machine that is serving, `doctor --quick` alone would refuse, and only `deploy` re-runs.

## Step 4 reads readiness, and does not reconcile the allow-list

Step 4's read is the `egress` network (`Docker::network_names`, bounded) and both containers' `running` state,
which is a real measurement of the daemon, so the second run says `satisfied` without re-applying. The
`[egress] allow` list is **not** reconciled: a changed allow-list takes effect on the next `serve` boot, which
replaces both proxies unconditionally because the list is an argument rather than a mount (ADR-0037). Comparing
a running container's `Args` with the vector deploy would pass was rejected — it is a second reading of
`EgressProxy::run_args` and would drift from the first.

The recipe itself moved out of `commands/serve.rs` into `proxies.rs`, because the boot and the deploy must
start the same two containers the same way (`rm -f`, bounded `run_detached`, the `is_file` checks, the key
file prepared before its directory is mounted). `serve` keeps its warn-and-continue; `deploy` fails the step.
The seam is a `proxies::Containers` trait, so a test drives step 4's real `docker run` vector through a fake
that records the calls, with no daemon — the `doctor::Machine` pattern.

## What this release cannot measure, said plainly

Steps 5 and 6 were **not measured**: this machine has no systemd (`/proc/1/comm` is `bwrap`,
`/run/systemd/system` is absent) and no `tailscaled` answering, which is the same fact `doctor` renders as
`unknown` (ADR-0054) and the reason they are HS-049's steps. Step 3 builds nothing, so step 4's proxies run
whatever `agent_image` names — on this machine `pi-agent:local`, tagged by hand as `docs/build.md` says.

**"A fresh host ends with a working HTTPS URL" (phase 2's exit criterion) is therefore HS-048's and HS-049's
demo, not this story's.** What this story proves is the framework and the three steps whose machinery exists:
the order, the words, the dry run that creates nothing, the second run that changes nothing, and a failure
that stops at its step with the earlier steps' effects still on disk and the resume command last.
