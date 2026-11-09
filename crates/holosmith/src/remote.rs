//! The `--ssh` half of a deploy: send this build to another machine, verify the copy, and run the deploy there
//! (HS-073, ADR-0078).
//!
//! The story changes only **how** a deploy reaches a machine, and that is the whole shape of this module: it
//! builds four `ssh` children, checks what they answer, and hands the target the same command line the operator
//! would have typed there. It never decides which steps run — the target's own binary calls
//! [`crate::deploy::run_as`] with the same [`crate::deploy::Kind`], the same `--root` and the same `--dry-run`, so
//! a remote hub deploy installs the hub's five steps and no images. **A second step plan here is the failure this
//! module exists to avoid.**
//!
//! # The four children
//!
//! | # | what it is | why it is there |
//! |---|---|---|
//! | 1 | `ssh … uname -m` | the target's architecture, so a mismatch is refused **before** anything is copied |
//! | 2 | `ssh … 'mkdir -p … && cat > <path> && chmod 0755 <path>'` | the copy: this binary on the target's stdin |
//! | 3 | `ssh … sha256sum <path>` | the copy's digest, compared with the digest this side computed from the same bytes |
//! | 4 | `ssh -t … <path> {server\|hub} deploy …` | the deploy, with its stdio **inherited** so its lines are this command's output |
//!
//! No `scp`: it is a second program to require of the target — and a second one for a test to stand in for — and it
//! cannot carry the file's mode. Nothing here reads the internet: the bytes that go over the wire are the bytes of
//! a file this process already had, which is the spec's "the CLI itself downloads nothing" in its only measurable
//! form.
//!
//! # Where the copy lands
//!
//! `$HOME/.cache/holosmith/deploy-<12 hex of the digest>`, named by the digest so that a re-run overwrites the same
//! file and two builds never collide. **Not `/tmp`**: a temporary directory is commonly mounted `noexec` on exactly
//! the machines a deploy is aimed at, and a copy that cannot be executed would fail one step later with a message
//! about the wrong thing. Nothing removes it — a fifth child whose only job is to fail is worse than a file the
//! report names.
//!
//! # What "streaming the output" is
//!
//! The fourth child's three channels are the command's own ([`std::process::Stdio::inherit`]): the target's lines
//! appear as they are written, un-parsed and not reprinted. Every line **this** command writes carries
//! [`LINE_PREFIX`] in front, so a reader always knows which machine spoke.
//!
//! # `-t`, and the one thing it changes
//!
//! The run child is given a pseudo-terminal exactly when both of this command's ends are terminals
//! ([`Tools::interactive`]): a pty is what gives the target's `sudo` a tty to ask its password on (ADR-0056's
//! landmine one hop out), and *not* allocating one when the output is piped keeps the deploy's lines
//! LF-terminated and greppable.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};

use crate::deploy::Kind;
use crate::images;

/// The prefix every line this module's caller prints about the transport carries, so the target's own `deploy: …`
/// and `1. preflight: …` lines can never be confused with this machine's.
pub const LINE_PREFIX: &str = "deploy: ssh: ";

/// How long `ssh` may spend connecting before the child fails. The deploy that follows may legitimately take
/// minutes (step 3 builds two images), so the bound is on the **connect**, which is the part that would otherwise
/// hang on a machine that is asleep.
pub const CONNECT_TIMEOUT: u32 = 20;

/// The remote directory the copy lands in, relative to the target's `$HOME` — written as a shell token, because
/// that is what it is: the remote side expands it, and this side never learns (or needs) the absolute path.
pub const COPY_DIR: &str = "$HOME/.cache/holosmith";

/// The ssh flags every captured child carries.
///
/// `BatchMode=yes` is what turns "an impossible prompt" into a clean refusal: these three children have no
/// terminal to ask on (one of them has the binary itself on stdin), so an unverified host key or a password-only
/// login would otherwise consume the file meant for the copy and report a mangled authentication failure. The run
/// child is deliberately **not** batched: it keeps the operator's stdin, so ssh may ask whatever it needs to there.
fn batch() -> Vec<String> {
    vec![
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={CONNECT_TIMEOUT}"),
    ]
}

/// The external programs a remote deploy runs, and whether this command has a terminal at both ends.
///
/// [`Tools::ssh`] is this machine's program; `uname` and `sha256sum` run **on the target** through it, and they are
/// fields for [`crate::preflight::Tools`]'s reason: this crate denies `unsafe` and so cannot set `PATH`, which
/// leaves naming the program as the only way a test can point a check at a script it wrote. On a machine all three
/// are their bare names.
///
/// [`Tools::interactive`] is the one field that is not a program (the [`crate::deploy::Tools::health_wait`]
/// precedent): it is read from this process's own stdio by [`Tools::default`], and a test sets it explicitly to
/// measure both arms of the `-t` rule rather than whichever one the harness happens to be.
#[derive(Debug, Clone)]
pub struct Tools {
    pub ssh: PathBuf,
    pub uname: PathBuf,
    pub sha256sum: PathBuf,
    /// Whether to ask ssh for a pseudo-terminal: true exactly when stdin and stdout are both terminals.
    pub interactive: bool,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            ssh: PathBuf::from("ssh"),
            uname: PathBuf::from("uname"),
            sha256sum: PathBuf::from("sha256sum"),
            interactive: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        }
    }
}

/// The machine an `--ssh` deploy copies to.
///
/// The host is validated once, at construction, because the string becomes an **argv element of ssh's**: a value
/// that starts with `-` would be read by ssh as one of its own options, and one carrying whitespace or a newline
/// would join the remote command line rather than name a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    host: String,
}

impl Remote {
    /// The target an operator's `--ssh user@host` names.
    pub fn parse(text: &str) -> Result<Self, String> {
        match host_problem(text) {
            Some(sentence) => Err(sentence),
            None => Ok(Self {
                host: text.to_string(),
            }),
        }
    }

    /// The `user@host` as the operator typed it, which is also what every sentence about the target uses.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Copy [`Binary`] to the target and verify it there, executing nothing.
    ///
    /// The order is the acceptance's: the target's architecture first (so a mismatch copies nothing), then the
    /// bytes, then the digest of what arrived. An `Err` is one sentence, and it means the deploy has **not** run on
    /// the target — every failure here is before the fourth child.
    pub fn send(&self, tools: &Tools, binary: &Binary) -> Result<Sent, String> {
        let machine = self.machine(tools)?;
        if machine != binary.machine {
            return Err(format!(
                "{} is {}, and the binary to send ({}) is {}: this deploy refuses before copying anything, \
                 because a {} build cannot run there — build holosmith for {} and run this command again, or pass \
                 `--binary` with a build for it",
                self.host,
                machine.word(),
                binary.path.display(),
                binary.machine.word(),
                binary.machine.word(),
                machine.word()
            ));
        }
        let (path, shown) = binary.remote_path();
        // One child, one shell string: what ssh does with its arguments is hand them to the target's login shell,
        // so the directory, the copy and the mode are one round trip rather than three.
        let copy = format!("mkdir -p {COPY_DIR} && cat > {path} && chmod 0755 {path}");
        let mut arguments = batch();
        arguments.push(self.host.clone());
        arguments.push(copy);
        ask(
            &tools.ssh,
            &arguments,
            Some(Stdio::from(binary.handle()?)),
            &format!("the copy to {} did not arrive", self.host),
        )?;
        // The check is a digest computed **on the target** from the bytes that arrived, compared with the digest
        // computed here from the bytes that left: the only measurement that can tell a truncated pipe from a good
        // one, and the reason the file's name is not trusted for anything.
        let mut arguments = batch();
        arguments.push(self.host.clone());
        arguments.push(format!("{} {path}", tools.sha256sum.display()));
        let answer = ask(
            &tools.ssh,
            &arguments,
            None,
            &format!(
                "the copy on {} could not be measured, so nothing there has run",
                self.host
            ),
        )?;
        let arrived = digest_of(&String::from_utf8_lossy(&answer));
        match arrived {
            Some(arrived) if arrived == binary.digest => {}
            Some(arrived) => {
                return Err(format!(
                    "{} answered {} for {shown}, and this side computed {} for {}: the copy is not the bytes that \
                     were sent, so nothing on the target has run (a re-run overwrites the file)",
                    self.host,
                    arrived,
                    binary.digest,
                    binary.path.display()
                ));
            }
            None => {
                return Err(format!(
                    "`{} {path}` on {} answered something that is not a SHA-256 ({:?}): the copy cannot be \
                     verified, so nothing on the target has run",
                    tools.sha256sum.display(),
                    self.host,
                    String::from_utf8_lossy(&answer).trim()
                ));
            }
        }
        Ok(Sent {
            host: self.host.clone(),
            token: path,
            shown,
            machine,
            size: binary.size,
            digest: binary.digest.clone(),
        })
    }

    /// The target's architecture, as `uname -m` answers and [`Machine::parse`] maps.
    fn machine(&self, tools: &Tools) -> Result<Machine, String> {
        let mut arguments = batch();
        arguments.push(self.host.clone());
        arguments.push(format!("{} -m", tools.uname.display()));
        let answer = ask(
            &tools.ssh,
            &arguments,
            None,
            "the target's architecture is the first thing this deploy asks, because a build for another one \
             cannot run there",
        )?;
        let word = String::from_utf8_lossy(&answer);
        let word = word.trim();
        Machine::parse(word).ok_or_else(|| {
            format!(
                "{} answered `{word}` to `uname -m`, which this build cannot name: it is built for x86_64 and \
                 aarch64 only, so there is no way to say whether the binary being sent would run there",
                self.host
            )
        })
    }

    /// Run `plan` on the target with [`Sent`]'s copy, streaming its output onto this command's own stdio.
    ///
    /// An `Err` is ssh itself not starting; a child that starts and exits non-zero is the target's answer and comes
    /// back as a status, because "the deploy failed there" is not "this command could not ask".
    pub fn run(&self, tools: &Tools, sent: &Sent, plan: &Plan) -> Result<ExitStatus, String> {
        let mut arguments = Vec::new();
        if tools.interactive {
            arguments.push("-t".to_string());
        }
        arguments.push(self.host.clone());
        arguments.push(sent.token.clone());
        arguments.extend(plan.arguments());
        let rendered = format!("{} {}", tools.ssh.display(), arguments.join(" "));
        Command::new(&tools.ssh)
            .args(&arguments)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| {
                format!(
                    "`{rendered}` could not be run ({error}): the deploy itself was not started, so nothing on {} \
                     ran",
                    self.host
                )
            })
    }
}

/// What `ssh` will do with an operator's `--ssh`, as one rule: `None` when it names a machine, a sentence when it
/// does not.
///
/// Whitespace is part of the rule because the host is joined into the remote command line: `me@box server deploy`
/// would be a machine called `me@box` and a command nobody wrote. A leading `-` is refused because ssh would read
/// it as an option of its own — an operator would never mean that, and the failure it earns (`unknown option`) is a
/// sentence about ssh rather than about the value they typed.
pub fn host_problem(text: &str) -> Option<String> {
    if text.is_empty() {
        return Some(
            "`--ssh` needs a target (`user@host`): this deploy copies the binary over ssh and runs it there"
                .to_string(),
        );
    }
    if text.starts_with('-') {
        return Some(format!(
            "`--ssh {text}` starts with `-`, which ssh would read as an option of its own: this deploy passes the \
             target as an argument, so it must be a machine name (`user@host`)"
        ));
    }
    if text.chars().any(char::is_whitespace) {
        return Some(format!(
            "`--ssh {text:?}` contains whitespace, and the target is one argument: ssh joins everything after it \
             into the command to run there, so a target with a space in it is not one this command can name"
        ));
    }
    None
}

/// The CPU a Linux binary is built for, which is the only thing the architecture check compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Machine {
    X86_64,
    Aarch64,
}

impl Machine {
    /// The word `uname -m` may answer, mapped to the one this build uses. `None` for a machine this build cannot
    /// name: two architectures are what `scripts/build.sh` makes artifacts for.
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "x86_64" | "amd64" | "x86-64" => Some(Self::X86_64),
            "aarch64" | "arm64" => Some(Self::Aarch64),
            _ => None,
        }
    }

    /// The word this build's own sentences use, which is the ELF target triple's first field.
    pub fn word(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }

    /// The ELF header's `e_machine`, which is what a file *is* rather than what a name says about it.
    fn of_elf(machine: u16) -> Option<Self> {
        match machine {
            62 => Some(Self::X86_64),
            183 => Some(Self::Aarch64),
            _ => None,
        }
    }
}

/// The file to send: this process's own executable, or the one `--binary` names.
///
/// Its three facts are read once and carried, because they are what the report and both checks are made of: the
/// **machine** from the ELF header (a file that has not run yet cannot be asked `version --json`, and the file is
/// what has to run there), the **digest** from [`images::sha256`] (the crate's one SHA-256), and the **size**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binary {
    pub path: PathBuf,
    pub machine: Machine,
    pub size: u64,
    pub digest: String,
}

impl Binary {
    /// Read `named`, or this process's own binary when `--binary` was not given.
    pub fn read(named: Option<&Path>) -> Result<Self, String> {
        let path = match named {
            Some(path) => path.to_path_buf(),
            None => std::env::current_exe().map_err(|error| {
                format!(
                    "this process's own path could not be read ({error}): `--ssh` sends the binary it is running, \
                     and `--binary PATH` names another one to send instead"
                )
            })?,
        };
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "the binary to send ({}) could not be read ({error}): `--ssh` sends a file this machine already \
                 has, which is `--binary PATH` or the binary this command is running",
                path.display()
            )
        })?;
        let machine = elf_machine(&path, &bytes)?;
        Ok(Self {
            path,
            machine,
            size: bytes.len() as u64,
            digest: images::hex(&images::sha256(&bytes)),
        })
    }

    /// The copy's path on the target, as the shell token that names it and as the operator reads it.
    ///
    /// The short form is the digest's first twelve characters — a name, not a measurement: the measurement is the
    /// child that compares all sixty-four against this side's.
    fn remote_path(&self) -> (String, String) {
        let short = &self.digest[..12.min(self.digest.len())];
        (
            format!("{COPY_DIR}/deploy-{short}"),
            format!("~/.cache/holosmith/deploy-{short}"),
        )
    }

    /// The file, opened for the copy child's stdin.
    fn handle(&self) -> Result<std::fs::File, String> {
        std::fs::File::open(&self.path).map_err(|error| {
            format!(
                "the binary at {} could not be opened to send it ({error})",
                self.path.display()
            )
        })
    }
}

/// A copy that has arrived and matched: everything the local lines say, and everything the run child needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub host: String,
    /// The path as the **remote shell** names it (`$HOME/.cache/holosmith/deploy-…`): the form the copy, the check
    /// and the run all use, because the target is the side that resolves it.
    pub token: String,
    /// The same path as the operator reads it (`~/.cache/holosmith/deploy-…`).
    pub shown: String,
    pub machine: Machine,
    pub size: u64,
    pub digest: String,
}

impl Sent {
    /// The line that says what was sent where, and what it is — the report's first fact, measured before this line
    /// is printed.
    pub fn sent_line(&self) -> String {
        format!(
            "{LINE_PREFIX}{} is {}; copied {} bytes ({}) to {}",
            self.host,
            self.machine.word(),
            self.size,
            self.digest,
            self.shown
        )
    }

    /// The line that says the copy was verified, and by what.
    pub fn verified_line(&self) -> String {
        format!(
            "{LINE_PREFIX}the target's `sha256sum` answered the same digest, so {} on {} holds the bytes that were \
             sent, mode 0755",
            self.shown, self.host
        )
    }

    /// The line that says which command the target runs, in the target's own vocabulary.
    ///
    /// It is printed because the two flags that travel — `--config` and `--root` — name **the target's** paths, so
    /// the line is where an operator sees which tree and which file will be read over there.
    pub fn command_line(&self, plan: &Plan) -> String {
        format!(
            "{LINE_PREFIX}the target runs: {} {}",
            self.shown,
            plan.arguments().join(" ")
        )
    }
}

/// What a `--ssh` deploy asks the target's binary to do: the same command line, with the same four flags
/// ([`crate::commands::deploy::DeployArgs`]'s), and nothing else.
///
/// `--config` and `--root` are carried **verbatim**, and that is a deliberate difference from the local resume
/// line: over ssh both name paths on the target (which may not exist here at all), so making them absolute against
/// *this* machine's working directory would name a tree nobody asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub kind: Kind,
    pub dry_run: bool,
    pub config: Option<PathBuf>,
    pub root: Option<PathBuf>,
}

impl Plan {
    /// The remote command line after the binary's path: `server deploy --config … --root … --dry-run`.
    pub fn arguments(&self) -> Vec<String> {
        let mut arguments = vec![self.kind.word().to_string(), "deploy".to_string()];
        if let Some(config) = &self.config {
            arguments.push("--config".to_string());
            arguments.push(config.display().to_string());
        }
        if let Some(root) = &self.root {
            arguments.push("--root".to_string());
            arguments.push(root.display().to_string());
        }
        if self.dry_run {
            arguments.push("--dry-run".to_string());
        }
        arguments
    }
}

/// The ELF header's answer to "which machine is this", or one sentence saying why the file cannot be asked.
///
/// The bytes are read directly rather than by spawning `readelf`: the deploy may run on a machine where the only
/// thing that can be exec'd is the file being sent, and `e_machine` is six bytes at a fixed offset.
fn elf_machine(path: &Path, bytes: &[u8]) -> Result<Machine, String> {
    let header = match bytes.get(..20) {
        Some(header) if header.starts_with(b"\x7fELF") => header,
        _ => {
            return Err(format!(
                "{} is not an ELF binary, so this deploy cannot tell which machine it was built for: `--ssh` \
                 sends a static Linux build of holosmith (or the file `--binary` names)",
                path.display()
            ));
        }
    };
    // EI_DATA: 1 is little-endian, which is what both architectures this build targets use.
    if header[5] != 1 {
        return Err(format!(
            "{} is an ELF binary with byte order {}, which this deploy cannot read: it is built for little-endian \
             x86_64 and aarch64 only",
            path.display(),
            header[5]
        ));
    }
    let raw = u16::from_le_bytes([header[18], header[19]]);
    Machine::of_elf(raw).ok_or_else(|| {
        format!(
            "{} is built for ELF machine {raw}, which this build cannot name: it copies x86_64 and aarch64 builds \
             only, and a build for another architecture cannot run on the target",
            path.display()
        )
    })
}

/// The digest `sha256sum` printed, which is the first field of its first line.
///
/// `sha256sum` writes `<hex>  <path>`; a `busybox` or a wrapper that adds a prefix would print something else, and
/// that is a sentence rather than a match. The comparison is case-insensitive because a hex digest has one value.
fn digest_of(answer: &str) -> Option<String> {
    let first = answer.lines().next()?.split_whitespace().next()?;
    let lowered = first.to_ascii_lowercase();
    let hex = lowered.len() == 64
        && lowered
            .chars()
            .all(|character| character.is_ascii_hexdigit());
    hex.then_some(lowered)
}

/// One captured `ssh` child: its stdout, or the sentence its failure earns.
///
/// `why` completes the sentence, because the same helper answers three different questions (the architecture, the
/// copy, the digest) and one wording for all three would be a sentence about none of them.
fn ask(
    program: &Path,
    arguments: &[String],
    stdin: Option<Stdio>,
    why: &str,
) -> Result<Vec<u8>, String> {
    let rendered = format!("{} {}", program.display(), arguments.join(" "));
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(stdin.unwrap_or_else(Stdio::null))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output: Output = command
        .output()
        .map_err(|error| format!("`{rendered}` could not be run ({error}): {why}"))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(format!(
        "`{rendered}` answered {} ({}): {why}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20-byte ELF header for `machine`, which is all any reader needs to answer the question.
    fn elf(machine: u16) -> Vec<u8> {
        let mut bytes = b"\x7fELF\x02\x01\x01\x00".to_vec();
        // `e_ident`'s remaining bytes, then `e_type`, then `e_machine` at offset 18.
        bytes.extend([0u8; 8]);
        bytes.extend([0u8; 2]);
        bytes.extend(machine.to_le_bytes());
        bytes.extend([0u8; 2]);
        bytes
    }

    #[test]
    fn the_elf_header_names_the_two_architectures_this_build_makes() {
        let x86 = elf(62);
        let arm = elf(183);
        assert_eq!(elf_machine(Path::new("x"), &x86), Ok(Machine::X86_64));
        assert_eq!(elf_machine(Path::new("x"), &arm), Ok(Machine::Aarch64));
    }

    #[test]
    fn a_file_that_is_not_an_elf_binary_is_a_sentence_naming_it() {
        let error = elf_machine(Path::new("/tmp/script"), b"#!/bin/sh\nexit 0\n")
            .expect_err("a shell script has no ELF header");
        assert!(error.contains("/tmp/script"), "{error}");
        assert!(error.contains("ELF"), "{error}");
    }

    #[test]
    fn a_machine_this_build_does_not_make_is_a_sentence_naming_the_number() {
        // 243 is RISC-V, which nothing in this repository builds for.
        let error = elf_machine(Path::new("x"), &elf(243))
            .expect_err("243 is not a machine this build makes");
        assert!(error.contains("243"), "{error}");
    }

    #[test]
    fn a_truncated_header_is_not_read_as_a_machine() {
        assert!(elf_machine(Path::new("x"), b"\x7fELF\x02\x01").is_err());
    }

    #[test]
    fn a_big_endian_header_is_refused_rather_than_read_backwards() {
        let mut bytes = elf(62);
        bytes[5] = 2;
        let error = elf_machine(Path::new("x"), &bytes).expect_err("big-endian is not read");
        assert!(error.contains("byte order 2"), "{error}");
    }

    #[test]
    fn the_uname_words_this_build_may_hear_are_four_and_nothing_else() {
        for word in ["x86_64", "amd64", "x86-64", "X86_64\n"] {
            assert_eq!(Machine::parse(word), Some(Machine::X86_64), "{word:?}");
        }
        for word in ["aarch64", "arm64", " arm64 "] {
            assert_eq!(Machine::parse(word), Some(Machine::Aarch64), "{word:?}");
        }
        for word in ["", "riscv64", "i686", "armv7l"] {
            assert_eq!(Machine::parse(word), None, "{word:?}");
        }
    }

    #[test]
    fn the_host_rule_refuses_what_ssh_would_read_as_its_own() {
        assert!(host_problem("me@box").is_none());
        assert!(host_problem("box.tail1234.ts.net").is_none());
        let option =
            host_problem("-oProxyCommand=curl example.com").expect("a leading - is refused");
        assert!(option.contains("option"), "{option}");
        assert!(host_problem("me@box server deploy").is_some());
        assert!(host_problem("").is_some());
        assert!(Remote::parse("-oProxyCommand=x").is_err());
        assert_eq!(
            Remote::parse("me@box")
                .expect("a plain target is accepted")
                .host(),
            "me@box"
        );
    }

    #[test]
    fn the_remote_path_is_named_by_the_digest_and_never_in_tmp() {
        let binary = Binary {
            path: PathBuf::from("/x"),
            machine: Machine::X86_64,
            size: 1,
            digest: "0123456789abcdef".repeat(4),
        };
        let (token, shown) = binary.remote_path();
        assert_eq!(token, "$HOME/.cache/holosmith/deploy-0123456789ab");
        assert_eq!(shown, "~/.cache/holosmith/deploy-0123456789ab");
        // `$HOME`, not `/tmp`: a temporary directory is routinely `noexec` on the machines this deploys to.
        assert!(!token.contains("/tmp"), "{token}");
    }

    #[test]
    fn the_plan_is_the_command_an_operator_would_have_typed_there() {
        let plan = Plan {
            kind: Kind::Hub,
            dry_run: true,
            config: Some(PathBuf::from("/etc/holosmith/config.toml")),
            root: Some(PathBuf::from("tree")),
        };
        assert_eq!(
            plan.arguments(),
            vec![
                "hub",
                "deploy",
                "--config",
                "/etc/holosmith/config.toml",
                "--root",
                "tree",
                "--dry-run"
            ]
        );
        // A relative `--root` stays relative: it names a path on the target, not one of this machine's.
        let bare = Plan {
            kind: Kind::Server,
            dry_run: false,
            config: None,
            root: None,
        };
        assert_eq!(bare.arguments(), vec!["server", "deploy"]);
    }

    #[test]
    fn a_digest_line_is_its_first_field_and_lowercase() {
        let hex = "a".repeat(64);
        assert_eq!(
            digest_of(&format!("{hex}  /home/me/.cache/holosmith/deploy-x\n")),
            Some(hex.clone())
        );
        assert_eq!(
            digest_of(&format!("{}  f\n", hex.to_uppercase())),
            Some(hex)
        );
        assert_eq!(digest_of("sha256sum: no such file"), None);
        assert_eq!(digest_of(""), None);
        assert_eq!(digest_of(&"a".repeat(63)), None);
    }

    #[test]
    fn the_captured_children_are_batched_and_the_run_child_is_not() {
        let flags = batch();
        assert!(flags.iter().any(|flag| flag == "BatchMode=yes"));
        assert!(
            flags
                .iter()
                .any(|flag| flag == &format!("ConnectTimeout={CONNECT_TIMEOUT}"))
        );
        // The run child's argv is built in `run`; what matters here is that nothing in it is a `-o`.
        let tools = Tools {
            ssh: PathBuf::from("ssh"),
            uname: PathBuf::from("uname"),
            sha256sum: PathBuf::from("sha256sum"),
            interactive: true,
        };
        assert!(tools.interactive);
    }
}
