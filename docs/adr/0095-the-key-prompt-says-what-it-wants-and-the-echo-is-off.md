# The key prompt says what it wants and the echo is off

`holosmith key set opencode-go` on a terminal printed nothing, read stdin to EOF and wrote
`<data_root>/secrets/keys.toml` when it got there. The command was not stuck — fourteen seconds of an operator's
`time` with `user 0m0.001s` is a process waiting on a file descriptor — but nothing on the screen distinguished it
from one, because there was nothing on the screen. A terminal's `read_to_string` returns on **Ctrl-D**, which is not
a key an operator presses after pasting a secret, so the two outcomes were a Ctrl-C that lost the key and a
Ctrl-D that an operator discovers by accident. `README.md` said `# paste the key, then Ctrl-D`, which is the
symptom written down as an instruction.

The echo was the second half. `read_to_string` on a terminal echoes what is typed, so the one value the whole
store exists to keep out of argv, shell history and crash reports was drawn on the screen — and, pasted under tmux
or a screen share, into a scrollback that outlives the terminal.

**A typo'd provider was the third.** `Store::set_provider` refused an id outside `PROVIDERS` *after* the read, so
`key set not-a-provider` waited for a key it then threw away, and the sentence naming the eleven ids arrived only
once the operator had given up and pressed Ctrl-D.

## D1 — stdin has two shapes, and stdin decides which

`std::io::stdin().is_terminal()` — not stdout, not a flag. The shape follows the input: a pipe (every test, and
`printf '…' | holosmith key set <p>`) is read to EOF and gets **no prompt**, because a prompt there lands in
whatever captured stderr; a terminal gets a sentence, then one line.

The two shapes are two readers rather than one reader with a terminator option, because the facts differ: a pipe's
whitespace is the caller's business and the store trims it, while a terminal's line ends at the `\n` its Enter
sends. Both drop the trailing `\n` and one `\r` before the store sees the value — the same two bytes the old
`strip_suffix` chain dropped, so a pasted key and a scripted one store identically.

## D2 — one Enter, not a Ctrl-D

The terminal read is `read_line`, so Enter finishes it. Ctrl-D still works (it ends the line for `read_line`), and
nothing has to. A key is one line; a paste that carries a second line is not a key, and the first line is the one
stored.

## D3 — echo off through one `stty` child, and a prompt that says which it is

There is no `termios` here to call: the binary is safe Rust, `unsafe_code` is denied, and the crate has no `libc`.
`stty -echo` and `stty echo` are two children of the shape `server deploy` already uses for `install` and `chmod`,
with stdin inherited so they touch the terminal the read is on. A `Drop` guard restores echo on the read's own error
path as well as the happy one; a Ctrl-C during the read kills the process with no code run, and the operator's shell
puts its own modes back when it prompts again, which is the same bargain every `read -s` makes.

**The prompt never claims a field it cannot hide.** `stty` missing from `PATH` (a host without coreutils, a
container with a tty) keeps the prompt and loses only the hiding, and the sentence says so: `the key will be visible
as you type: stty is not on PATH`. A hidden read with no warning would have been the original bug in a new costume —
nothing appears as you type, so a silent prompt looks exactly like the hang it replaced. The prompt is on **stderr**
(ADR-0092): it is the conversation, and `stored <provider>` on stdout stays the verdict.

## D4 — the provider is checked before stdin is read

`commands::key::set` asks `key::provider` first and prints `key::unknown_provider` — one spelling of that refusal,
shared with `Store::set_provider`, which still asks it for a caller that skipped the step. A refusal that arrives
before the read does not need the operator to end a read they were never going to satisfy.

## Evidence

- `crates/holosmith/src/commands/key.rs`'s `the_prompt_names_the_provider_the_key_and_whether_the_input_is_hidden`
  is D3's clause: the hiding prompt says `nothing is shown`, the visible one says `will be visible` and must not say
  the other.
- `crates/holosmith/tests/key_cli.rs`'s `an_unknown_provider_is_refused_before_stdin_is_read` is D4, and it can
  fail: the child's stdin is a pipe that is never written and never closed, so a command that reads first hangs
  and the test's ten-second deadline reports it instead of hanging the suite.
- The piped path is unchanged, and `set_reads_stdin_writes_mode_0600_and_list_never_prints_the_value` is the
  assertion that it stays a pipe-first read with no prompt on either stream.
- Measured by hand under `script -qec` (a pty): Enter stores the key, `stty -a` afterwards shows `echo` and not
  `-echo`, and `PATH=/nonexistent` prints the visible-input sentence and still stores.
