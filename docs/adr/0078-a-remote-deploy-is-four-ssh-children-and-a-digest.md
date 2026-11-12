# A remote deploy is four ssh children and a digest, and the same command runs there

This is the spec's one sentence about `--ssh` ("the CLI copies the same binary to the target, verifies its SHA-256
after the copy, and runs the same deploy there with sudo, streaming the output; it refuses if the target's CPU
architecture differs, unless `--binary` supplies a matching build; the CLI itself downloads nothing from the
internet"), and its five acceptance bullets. The story changes **how** a deploy reaches a machine and nothing about
what one installs: the target's own binary calls `deploy::run_as` with the same `Kind`, the same `--root` and the
same `--dry-run`.

Six decisions carry it. Two of them are about *not* writing something — a second step plan, and a second tail.

## D1 — four `ssh` children, and no `scp`

| # | what it is | why |
|---|---|---|
| 1 | `ssh -o BatchMode=yes -o ConnectTimeout=20 <host> uname -m` | the target's architecture, so a mismatch is refused **before** anything is copied |
| 2 | `ssh -o … <host> 'mkdir -p $HOME/.cache/holosmith && cat > <path> && chmod 0755 <path>'`, stdin = the binary | the copy |
| 3 | `ssh -o … <host> sha256sum <path>` | the copy's digest, compared with the digest this side computed from the same bytes |
| 4 | `ssh [-t] <host> <path> {server\|hub} deploy [--config P] [--root D] [--dry-run]` | the deploy, with all three stdio channels **inherited** |

`scp` was refused on two counts: it is a second program to require of the target *and* a second one for a test and
the gate to stand in for, and it cannot set the file's mode — a copy that arrives 0644 fails one step later with a
message about the wrong thing. The mode is `chmod`'s, in the same shell string as the copy, because the copy child's
string is already a shell string and a fourth child for one word would be a fourth failure point.

The copy lands at **`$HOME/.cache/holosmith/deploy-<first 12 hex of the digest>`**, and both halves of that are
decisions:

- **Not `/tmp`.** A temporary directory is routinely mounted `noexec` on exactly the machines a deploy is aimed at
  (it is the standard hardening a server gets), and the failure it produces — `Permission denied` from a child that
  was supposed to be the deploy — names the wrong thing. The operator's own cache directory is writable by the
  login user, is not usually `noexec`, and is where a per-user copy belongs.
- **Named by the digest.** A re-run overwrites the same file instead of accumulating copies, two different builds
  never collide, and the name carries enough of the digest to be recognised in a log without being a second digest
  claim: the authority is the check child, which compares all sixty-four characters.
- **Nothing removes it.** A fifth child whose only job is to fail is worse than a file the report names, and the
  resume path copies again anyway.

`BatchMode=yes` on the three captured children is the other half of that reasoning: none of them has a terminal (one
has the *binary* on stdin), so an unverified host key or a password-only login would consume the file meant for the
copy and surface as a mangled authentication error. Batched, ssh refuses with its own sentence instead. The run
child is deliberately **not** batched — it keeps the operator's stdin — and it gets `-t` exactly when both of this
command's ends are terminals, because a pty is what gives the target's `sudo` a tty to ask its password on
(ADR-0056's landmine, one hop out), while *not* allocating one on a piped run is what keeps the deploy's lines
LF-terminated and greppable. The cost of that rule is recorded rather than discovered: `… deploy --ssh me@box | tee
log` typed at a terminal has a terminal stdin and a piped stdout, so it allocates **no** pty and a system-path
deploy's `sudo` there has no tty to ask on. The two pulls are real — a pty for `sudo` against LF output for a pipe —
and the answer is "both ends or neither", which the operator can see before it matters.

The check is the **target's** `sha256sum`: coreutils is on every systemd Linux this spec targets (and in busybox),
and the alternatives are worse — `openssl dgst` is a second program a minimal target may not have, and trusting the
copy's name needs no program and proves nothing. The digest this side compares with is `images::sha256`, the crate's
one SHA-256, so "the same bytes" means the same arithmetic on both sides.

`ConnectTimeout=20` bounds the part that would otherwise hang on a sleeping machine. The deploy that follows may
legitimately take minutes (step 3 builds two images), so nothing bounds it.

## D2 — `--binary` is the file to send, and the architecture is read from *that file's* ELF header

The spec's "unless `--binary` supplies a matching build" reads as "this file instead of the one the command is
running", not as "a second binary to run on the target while this one stays the CLI" — the second reading needs an
answer to "which one runs `deploy`" that the sentence does not give.

The check therefore cannot be `version --json`'s `target`. A file that has not run yet cannot be asked, and the file
is what has to execute there, so [`remote::elf_machine`] reads the ELF header directly: `e_machine` 62 is
`x86_64`, 183 is `aarch64`, and anything else is a refusal naming the number. A non-ELF file, a truncated header and
a big-endian header are one sentence each, each naming the path — `--binary` is an operator's typo as often as it is
a build, and six bytes at a fixed offset is cheaper than a `readelf` child that might not be installed.

The comparison is `uname -m` on the target, mapped through the four words a Linux answers (`x86_64`/`amd64`,
`aarch64`/`arm64`); anything else is a refusal naming what it answered, never a guess. It happens **first**, so a
mismatch copies nothing.

## D3 — the remote command line is the operator's own, and `--root` means the target's tree

Over ssh, `--root DIR` is the same flag with the same meaning on the other machine. Two consequences, both wanted:

- `deploy::Target::privileged()` keeps its one meaning — "no `--root`" — so a remote system-path deploy is the
  deploy's own `sudo`-prefixed children, exactly as a local one is, and a remote `--root` deploy needs no privilege
  at all. No `sudo` is prefixed to the **run** child: the binary that runs is the invoking user's, and every write
  it makes escalates per child the way it already does.
- The configuration a remote run reads is **the target's**, so `Config::load` is not called at all on this path and a
  laptop with no `config.toml` can deploy. `--config` travels verbatim and names the target's file (that story's
  two-configs rule, one hop out); making it absolute here would name this machine's.

**"Verbatim" costs one quoting helper, and it is not optional.** ssh hands the target's login shell *one string*, so
an unquoted `--root /srv/a b` arrives as two words, `/srv/*` arrives as whatever it matched, and a `;` arrives as a
second command — while the identical local command needs no such care, because a local `Child` is argv all the way
down. [`remote::quote`] therefore wraps a value in single quotes whenever it contains anything a shell acts on (`~`
included: unquoted, the *target* would expand it, and the word the operator wrote is what must arrive), and returns a
plain path unchanged so the report still reads like the command the operator typed. The same function renders the two
values in the resume line, because that line is pasted into a local shell and then travels through a second one.

`--binary` without `--ssh` is a `clap` usage error (`requires = "ssh"`): without `--ssh` the binary a deploy installs
is the one it is running, so the flag would be a lie about which file was used.

## D4 — streaming is inheritance, and the two authors are distinguishable

The run child's stdin, stdout and stderr are this command's. The target's lines appear as they are written,
un-parsed, un-buffered and not reprinted — which is the spec's "streaming the output" in its only literal form.
Every line **this** side writes carries `deploy: ssh: ` in front ([`remote::LINE_PREFIX`]), so a reader can always
tell which machine spoke, and a gate can grep for one author or the other.

The three lines this side prints before the run are the three facts it measured: what was sent where and what it is
(machine, size, digest), that the target's `sha256sum` answered the same digest, and **which command the target will
run** — the last because the two flags that travel name the target's paths, and the operator has no other place to see
them. It says `will run`, not `runs`: the child has not been spawned when the line is written, and this report's rule
is that a word means what it measured.

## D5 — a remote `--dry-run` runs the deploy's dry run there

The spec says "`--dry-run` lists the steps without running them", and the story's demo is "`--dry-run` … lists steps
remotely". So a remote dry run is the target's own dry run: the seven lines in the spec's order, `needs` on the
applies and a read's verdict on step 1, and nothing written on the target except the copy.

The acceptance bullet's wording — "`--dry-run` over SSH performs the preflight only" — is **read as "applies
nothing"**, and the reading is recorded rather than glossed: the alternative (copy, run `doctor --quick`, print one
line) makes the demo's sentence false, and it is a second remote execution mode beside `run_as`, which is the thing
this story must not create. What a remote dry run *does* perform is the deploy's reads, step 1's preflight among
them; what it never reaches is an apply.

## D6 — the tail over ssh is the target's, and a failure's last line is the local command

`deploy: role:`, `deploy: url:` and `deploy: next:` are printed by the **target's** binary, because the target
measured them. A local `deploy: url:` would be a URL this process never asked about — the report's first lie, in the
shape ADR-0056's five words exist to prevent. So the local half prints no tail at all.

A failed remote run does print one line of its own: `deploy: resume: holosmith server deploy --ssh me@box
[--binary PATH] [--dry-run]`. Two resume lines therefore appear when the target's run fails — the target's, naming
the command to run *there*, and this one, naming the command to run *here* — and both are wanted, because they are
different commands on different machines. The local one carries `--config`/`--root` **verbatim** for D3's reason.

A child that starts and exits non-zero is the target's answer, not a failure to ask: its exit code becomes this
command's, and one local line says so. A failure *before* the run child (an illegal host, an unreadable file, an
architecture mismatch, a copy that does not verify) is a sentence on stderr plus the resume line, and no deploy has
run on the target.

## D7 — the host is validated before it becomes argv

`--ssh`'s value is an argument of `ssh`'s own, so `remote::host_problem` refuses an empty target, one containing
whitespace (ssh joins the words after the host into the remote command line, so a space in the host names a command
nobody wrote) and one starting with `-` (which ssh would read as an option of its own — `--ssh=-oProxyCommand=…` is
an injection, not a target). One rule, one sentence per arm, and `clap` already refuses the space-separated form
because a value may not start with `-`.

## D8 — what the gate measures, and what it cannot

`case_deploy_ssh` puts an `sh` `ssh` on `PATH` that records every argv and runs the words after the host through
`sh -c` — which is ssh's contract rather than an approximation of it — with `$HOME` pointed at a directory the case
owns. "The target" is therefore this box, and everything after the transport is real: the shipped artifact's bytes,
the machine's own `sha256sum`, and the artifact's own seven steps in a `--root` tree.

It measures: the server arm's **seven lines streamed from the target** with the copy and the check in the shim's log
and the copy on disk with the artifact's bytes, mode 0755 and a digest-derived name; a hub install over the same
transport (steps 2, 5, 6 and 7 applied, 3 and 4 `skipped` — the `Kind` travelled) and a second run that is idempotent,
with the same digest-named copy overwritten rather than duplicated; the architecture refusal with **one** child in the
log and no copy on the target, and the same target accepting `--binary` when it matches; a `cat` that corrupts one
byte refused after the check with **no run child**; `--binary` without `--ssh` and an option smuggled through `--ssh`
both refused before any child; a run the *target* refuses, which is what measures that its exit code becomes this
command's, that the target's sentence arrives on stderr and the local resume is the last line on stdout; and a
`curl`/`wget` recorder that must never fire ("the CLI downloads nothing").

What it cannot measure is recorded in the case's own comment: a real ssh, a key, a second machine, and a real `sudo`
(there is none on this machine). `tests/remote.rs` covers the four argvs, every refusal, the `-t` rule and the
stdio inheritance's build-up, and `tests/deploy.rs` the `sudo`-prefixed children.

## D9 — what carries forward

`GLOSSARY.md` gains **Remote deploy**. A later change owns cross-machine sessions and the notification deep link. those stories
(`upgrade`/`rollback`) take the same `--ssh`/`--binary` and must reuse this module's four children rather than write
a second transport — an upgrade is "send the new binary and run `deploy`", which is exactly [`remote::Remote::send`]
plus a different command line.

The follow-up this story leaves: the copy is never removed from the target, so a machine deployed to many times
accumulates nothing (the name is the digest) but keeps at least one binary in the login user's cache. A later story
that owns a target-side layout is where that becomes an install
rather than a copy.
