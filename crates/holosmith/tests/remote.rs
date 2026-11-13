//! `--ssh` deploys: the four children, every refusal, and the streaming run (ADR-0078).
//!
//! These tests drive [`holosmith::remote`] over a target they build, for the reason `tests/deploy.rs` gives about the
//! deploy itself: the subject is *another machine*, so the only way to test it without a second machine is to hand
//! every claim in. `ssh` is a script this test wrote — one that does what ssh does (record its argv, then join the
//! words after the host and run them in a shell) with `HOME` pointing into the fixture, which is what makes "the
//! target" a directory this process can inspect. `uname` is a script that answers a chosen word.
//!
//! Three things are driven **for real**, because a fake would prove nothing about them: the copy is `cat` reading the
//! binary's own bytes, the verification is the machine's own `sha256sum` over the file that arrived, and the digest
//! this side compares with is [`holosmith::images::digest`] — the same function the image labels use. That is the
//! whole of "verifies its SHA-256 after the copy", measured end to end rather than asserted.
//!
//! What these tests cannot measure is the gate's: a real ssh, a key, a second machine, and the target's *own* deploy
//! report streaming onto this process's stdout. `remote::Remote::run` inherits all three channels on purpose, which
//! is exactly why a test cannot see them — `case_deploy_ssh` measures that half from the artifact.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use holosmith::deploy::Kind;
use holosmith::images;
use holosmith::remote::{self, Binary, Machine, Plan, Remote, Tools};

mod support;
use support::TempDir;

/// Hold this for the whole body of a test here: one at a time.
///
/// `tests/doctor.rs` and `tests/deploy.rs` document the trap: `exec` of a file any process holds open for writing
/// fails with `ETXTBSY`, and a forked child inherits the write descriptor of a shim written a moment earlier. Every
/// test here writes shims and then runs them, so serialising each whole body is the whole fix.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An executable shim for `name` answering `body`, in `dir`'s own `bin`.
fn shim(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("the shim directory is creatable");
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("a shim is writable");
    let mut permissions = std::fs::metadata(&path)
        .expect("the shim exists")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("a shim is executable");
    path
}

/// A 20-byte ELF header for `machine`, which is every byte any reader here needs.
fn elf(machine: u16) -> Vec<u8> {
    let mut bytes = b"\x7fELF\x02\x01\x01\x00".to_vec();
    // `e_ident`'s remaining bytes, then `e_type`, then `e_machine` at offset 18.
    bytes.extend([0u8; 8]);
    bytes.extend([0u8; 2]);
    bytes.extend(machine.to_le_bytes());
    bytes.extend([0u8; 2]);
    bytes
}

/// A file to send, with `machine` in its ELF header.
fn binary_file(dir: &TempDir, machine: u16) -> PathBuf {
    let path = dir.file("holosmith");
    std::fs::write(&path, elf(machine)).expect("the fixture is writable");
    path
}

/// An `uname` that answers `word`, whatever it is asked.
fn uname_shim(dir: &TempDir, word: &str) -> PathBuf {
    shim(dir, "uname", &format!("echo {word}"))
}

/// The target's `HOME`: the directory the stand-in gives the command it runs, because on a real target ssh runs it
/// with the login user's home and a test may not write into this machine's.
fn target_home(dir: &TempDir) -> PathBuf {
    let home = dir.path().join("target-home");
    std::fs::create_dir_all(&home).expect("the target's home is creatable");
    home
}

/// Everything one test's stand-in needs to know.
struct Target<'a> {
    /// Where every argv is appended, one line per child.
    log: PathBuf,
    /// The directory `$HOME` names for the command the stand-in runs.
    home: PathBuf,
    /// A substring of the remote command that must fail, or `""` for none.
    fail_on: &'a str,
    /// The exit code the copied binary "exits" with.
    run_exit: u8,
}

/// The `ssh` stand-in: record the argv, then do what ssh does.
///
/// ssh's own contract is *join the words after the host and hand them to the target's login shell*, so `sh -c "$*"`
/// is not an approximation of it — it is the thing. Two additions are the stand-in's rather than ssh's, and both are
/// named: `HOME` is redirected into the fixture, and the **run** child (whose first word is the copied file, a
/// fixture rather than a program) answers with the report the target's own binary would print and `run_exit`.
fn ssh_shim(dir: &TempDir, target: &Target<'_>) -> PathBuf {
    let Target {
        log,
        home,
        fail_on,
        run_exit,
    } = target;
    let log = log.display();
    let home = home.display();
    shim(
        dir,
        "ssh",
        &format!(
            r#"log='{log}'
HOME='{home}'
export HOME
{{
  printf 'ssh'
  for argument in "$@"; do
    printf ' %s' "$argument"
  done
  printf '\n'
}} >>"$log"
# ssh's own options: `-o` takes a value and `-t` does not, so the skip has to consume both shapes.
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o)
      shift
      if [ "$#" -gt 0 ]; then shift; fi
      ;;
    -*)
      shift
      ;;
    *)
      break
      ;;
  esac
done
[ "$#" -gt 0 ] || exit 255
shift
[ "$#" -gt 0 ] || exit 255
case "$*" in
  *'{fail_on}'*)
    if [ -n '{fail_on}' ]; then
      echo "could not resolve hostname: the stand-in was asked to fail" >&2
      exit 4
    fi
    ;;
esac
# The remote run is `sudo -n -u holosmith <token> deploy …` (ADR-0086): the target's privilege is stripped the way
# a machine's sudo would grant it, and what is left is the command the login shell runs. A fixture without the prefix
# keeps the old shape, so this stand-in measures the run either way.
set -- $*
if [ "$1" = sudo ]; then shift 4; fi
# The text ssh would hand the login shell, before it expands anything: the copy's first word is the unexpanded
# `$HOME/.cache/holosmith/...` token, which is what makes this stand-in's "remote shell" the same one.
case "$*" in
  '"$HOME"'/.cache/holosmith/*)
    printf 'deploy: run\n'
    exit {run_exit}
    ;;
esac
exec sh -c "$*""#
        ),
    )
}

/// One test's machine: a temp directory, its stand-ins, and the tools that name them.
struct Fixture {
    dir: TempDir,
    tools: Tools,
}

impl Fixture {
    fn new(label: &str, arch: &str, fail_on: &str, run_exit: u8, interactive: bool) -> Self {
        let dir = TempDir::new(label);
        let home = target_home(&dir);
        let target = Target {
            log: dir.file("ssh.log"),
            home,
            fail_on,
            run_exit,
        };
        std::fs::write(&target.log, "").expect("the log is writable");
        let tools = Tools {
            ssh: ssh_shim(&dir, &target),
            uname: uname_shim(&dir, arch),
            // The machine's own program, one hop out: the check is a real digest of the bytes that arrived.
            sha256sum: PathBuf::from("sha256sum"),
            // The remote run is `sudo -n -u holosmith …`: the shim records the argv, so the identity travels as the
            // words an operator would read (ADR-0086).
            sudo: PathBuf::from("sudo"),
            interactive,
        };
        Self { dir, tools }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("target-home")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.file("ssh.log")).expect("the log is readable")
    }

    fn calls(&self) -> Vec<String> {
        self.log()
            .lines()
            .map(std::string::ToString::to_string)
            .collect()
    }
}

fn host() -> Remote {
    Remote::parse("me@box").expect("a plain target is accepted")
}

fn plan() -> Plan {
    Plan {
        kind: Kind::Server,
        dry_run: true,
        config: None,
        root: Some(PathBuf::from("/srv/tree")),
    }
}

#[test]
fn the_children_are_one_read_a_copy_a_check_and_the_run() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-happy", "x86_64", "", 0, false);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    assert_eq!(binary.machine, Machine::X86_64);
    assert_eq!(
        binary.digest,
        images::digest(&path).expect("the fixture is readable")
    );

    let remote = host();
    let sent = remote
        .send(&fixture.tools, &binary)
        .expect("a matching target takes the copy");
    // The path is named by the digest and lives in the target's cache, not in a temporary directory.
    assert_eq!(
        sent.shown,
        format!("~/.cache/holosmith/deploy-{}", &binary.digest[..12])
    );
    // The command the target runs names the same path through the shell's own variable, because the target is the
    // side that resolves it.
    assert_eq!(
        sent.token,
        format!("\"$HOME\"/.cache/holosmith/deploy-{}", &binary.digest[..12])
    );
    assert!(sent.sent_line().contains("me@box"), "{}", sent.sent_line());
    assert!(
        sent.sent_line().contains(&binary.digest),
        "{}",
        sent.sent_line()
    );
    assert!(
        sent.verified_line().contains("the same digest"),
        "{}",
        sent.verified_line()
    );
    assert!(
        sent.command_line(&plan())
            .contains("server deploy --root /srv/tree --dry-run"),
        "{}",
        sent.command_line(&plan())
    );

    // The copy really arrived, with the bytes that were sent and the mode the run child needs.
    let copied = fixture
        .home()
        .join(".cache/holosmith")
        .join(format!("deploy-{}", &binary.digest[..12]));
    assert_eq!(
        std::fs::read(&copied).expect("the copy exists"),
        std::fs::read(&path).expect("the fixture is readable")
    );
    let mode = std::fs::metadata(&copied)
        .expect("the copy exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755, "a copy without the executable bit cannot run");

    // The run child: the copied file, then the command an operator would have typed there.
    let status = remote
        .run(&fixture.tools, &sent, &plan())
        .expect("the stand-in starts");
    assert!(status.success());

    let calls = fixture.calls();
    assert_eq!(calls.len(), 4, "{calls:#?}");
    assert!(calls[0].contains("uname -m"), "{}", calls[0]);
    // The three captured children cannot prompt, so they are batched: an unverified host key or a password-only
    // login must not consume the copy's stdin.
    assert!(calls[1].contains("BatchMode=yes"), "{}", calls[1]);
    assert!(calls[1].contains("cat >"), "{}", calls[1]);
    assert!(calls[1].contains("chmod 0755"), "{}", calls[1]);
    assert!(calls[2].contains("sha256sum"), "{}", calls[2]);
    // The run child is not batched, and it carries no `-t` when this process has no terminal at both ends.
    assert!(!calls[3].contains("BatchMode"), "{}", calls[3]);
    assert!(calls[3].starts_with("ssh me@box "), "{}", calls[3]);
    assert!(
        calls[3].ends_with("server deploy --root /srv/tree --dry-run"),
        "{}",
        calls[3]
    );
}

#[test]
fn the_run_child_asks_for_a_terminal_only_when_there_is_one() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-tty", "x86_64", "", 0, true);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    let remote = host();
    let sent = remote
        .send(&fixture.tools, &binary)
        .expect("a matching target takes the copy");
    let _ = remote.run(&fixture.tools, &sent, &plan());
    let calls = fixture.calls();
    let run = calls.last().expect("four children ran");
    // `-t` is what gives the target's `sudo` a tty to ask its password on (ADR-0056's landmine, one hop out).
    assert!(run.starts_with("ssh -t me@box "), "{run}");
}

#[test]
fn a_mismatched_architecture_is_refused_before_anything_is_copied() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-mismatch", "aarch64", "", 0, false);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");

    let error = host()
        .send(&fixture.tools, &binary)
        .expect_err("an x86_64 build cannot run on an aarch64 target");
    assert!(error.contains("aarch64"), "{error}");
    assert!(error.contains("x86_64"), "{error}");
    assert!(error.contains("--binary"), "{error}");
    // The acceptance's word is *before*: the copy child never ran, and the target's cache does not exist.
    assert_eq!(fixture.calls().len(), 1, "{:#?}", fixture.calls());
    assert!(!fixture.home().join(".cache").exists());
}

#[test]
fn the_same_target_accepts_a_matching_build_handed_in() {
    let _guard = one_at_a_time();
    // The target is aarch64 and `--binary` names an aarch64 build: the acceptance's "unless `--binary` supplies a
    // matching build", measured rather than described.
    let fixture = Fixture::new("remote-binary", "aarch64", "", 0, false);
    let path = binary_file(&fixture.dir, 183);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    assert_eq!(binary.machine, Machine::Aarch64);
    let sent = host()
        .send(&fixture.tools, &binary)
        .expect("an aarch64 build runs on an aarch64 target");
    assert_eq!(sent.machine, Machine::Aarch64);
}

#[test]
fn an_answer_this_build_cannot_name_is_a_sentence_rather_than_a_match() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-unknown", "riscv64", "", 0, false);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    let error = host()
        .send(&fixture.tools, &binary)
        .expect_err("riscv64 is not a machine this build makes");
    assert!(error.contains("riscv64"), "{error}");
    assert_eq!(fixture.calls().len(), 1, "{:#?}", fixture.calls());
}

#[test]
fn a_copy_that_does_not_arrive_stops_before_the_check_and_the_run() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-copy-fail", "x86_64", "mkdir -p", 0, false);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    let error = host()
        .send(&fixture.tools, &binary)
        .expect_err("a copy that failed is not a copy");
    assert!(error.contains("did not arrive"), "{error}");
    // The failure names the child's own words, so an operator can act on the target's reason rather than on ours.
    assert!(error.contains("asked to fail"), "{error}");
    assert_eq!(fixture.calls().len(), 2, "{:#?}", fixture.calls());
}

#[test]
fn a_check_that_cannot_run_aborts_before_anything_executes() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-check-fail", "x86_64", "sha256sum", 0, false);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    let error = host()
        .send(&fixture.tools, &binary)
        .expect_err("an unmeasured copy is not a verified copy");
    assert!(error.contains("nothing there has run"), "{error}");
    let calls = fixture.calls();
    assert_eq!(calls.len(), 3, "{calls:#?}");
    assert!(
        !calls.iter().any(|call| call.contains("server deploy")),
        "the deploy ran over an unverified copy: {calls:#?}"
    );
}

#[test]
fn bytes_that_changed_after_the_digest_was_taken_are_refused() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-corrupt", "x86_64", "", 0, false);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    // One byte appended between the digest and the copy: the target's own `sha256sum` is what notices, and the local
    // side refuses on the comparison rather than on trust.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("the fixture is writable");
    file.write_all(b"x").expect("the fixture is writable");
    drop(file);

    let error = host()
        .send(&fixture.tools, &binary)
        .expect_err("one byte more is not the same file");
    assert!(error.contains("not the bytes that were sent"), "{error}");
    assert!(error.contains(&binary.digest), "{error}");
    let calls = fixture.calls();
    assert_eq!(calls.len(), 3, "{calls:#?}");
    assert!(
        !calls.iter().any(|call| call.contains("server deploy")),
        "the deploy ran over a copy that did not verify: {calls:#?}"
    );
}

#[test]
fn a_digest_that_is_not_a_digest_is_a_sentence() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-not-a-digest", "x86_64", "", 0, false);
    // A `sha256sum` that answers something else entirely: the comparison is only worth anything if an unreadable
    // answer is refused rather than compared.
    let lying = shim(
        &fixture.dir,
        "sha256sum",
        "echo 'sha256sum: cannot open the file'",
    );
    let mut tools = fixture.tools.clone();
    tools.sha256sum = lying;
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    let error = host()
        .send(&tools, &binary)
        .expect_err("a non-digest cannot verify anything");
    assert!(error.contains("cannot be verified"), "{error}");
}

#[test]
fn a_program_that_cannot_start_is_a_sentence_about_the_program() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-no-ssh", "x86_64", "", 0, false);
    let mut tools = fixture.tools.clone();
    tools.ssh = fixture.dir.file("no-such-ssh");
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    let error = host()
        .send(&tools, &binary)
        .expect_err("a missing ssh is a sentence, not a panic");
    assert!(error.contains("could not be run"), "{error}");
    assert!(error.contains("architecture"), "{error}");
}

#[test]
fn the_run_childs_exit_code_is_the_targets_answer_rather_than_an_error() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-run-fails", "x86_64", "", 1, false);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    let remote = host();
    let sent = remote
        .send(&fixture.tools, &binary)
        .expect("the copy arrives");
    let status = remote
        .run(&fixture.tools, &sent, &plan())
        .expect("a child that starts and fails is not a failure to ask");
    assert_eq!(status.code(), Some(1));
}

#[test]
fn a_file_that_is_not_a_binary_is_refused_with_its_path() {
    let _guard = one_at_a_time();
    let dir = TempDir::new("remote-not-elf");
    let script = dir.write("holosmith", "#!/bin/sh\n");
    let error = Binary::read(Some(&script)).expect_err("a shell script has no ELF header");
    assert!(error.contains("not an ELF binary"), "{error}");
    assert!(error.contains(&script.display().to_string()), "{error}");
}

#[test]
fn the_binary_this_process_is_running_is_an_elf_the_reader_can_name() {
    let _guard = one_at_a_time();
    // The one real artifact this test can read without a fixture: the test binary's own header. It proves the
    // reader against a real ELF rather than against a header the test wrote to match the reader.
    let binary = Binary::read(None).expect("this test binary is an ELF binary");
    assert!(matches!(binary.machine, Machine::X86_64 | Machine::Aarch64));
    assert!(binary.size > 0);
    assert_eq!(binary.digest.len(), 64);
    assert!(binary.path.exists());
}

#[test]
fn the_captured_children_name_the_host_and_the_tools_they_use() {
    let _guard = one_at_a_time();
    let fixture = Fixture::new("remote-argv", "x86_64", "", 0, false);
    let path = binary_file(&fixture.dir, 62);
    let binary = Binary::read(Some(&path)).expect("the fixture has an ELF header");
    let _ = host().send(&fixture.tools, &binary);
    let calls = fixture.calls();
    // Every captured child carries the target as its last argument before the command, and the connect bound.
    for call in &calls[..3] {
        assert!(call.contains("me@box"), "{call}");
        assert!(
            call.contains(&format!("ConnectTimeout={}", remote::CONNECT_TIMEOUT)),
            "{call}"
        );
    }
    assert!(calls[0].ends_with("uname -m"), "{}", calls[0]);
    // The expansion is quoted, so a home directory with a space in it stays one word.
    assert!(calls[1].contains(remote::COPY_TOKEN_DIR), "{}", calls[1]);
}

#[test]
fn a_plan_carries_a_hub_and_its_own_paths_verbatim() {
    let _guard = one_at_a_time();
    let hub = Plan {
        kind: Kind::Hub,
        dry_run: false,
        config: Some(PathBuf::from("holosmith.toml")),
        root: Some(PathBuf::from("tree")),
    };
    // Both are relative and stay relative: over ssh they name the **target's** paths, so making them absolute
    // against this machine's working directory would name a tree nobody asked about.
    assert_eq!(
        hub.arguments(),
        vec![
            "hub",
            "deploy",
            "--config",
            "holosmith.toml",
            "--root",
            "tree"
        ]
    );
    // And a value the target's shell would otherwise re-parse travels as one word.
    let spaced = Plan {
        kind: Kind::Server,
        dry_run: false,
        config: Some(PathBuf::from("/srv/My Tree/holosmith.toml")),
        root: Some(PathBuf::from("/srv/tree")),
    };
    assert_eq!(
        spaced.arguments(),
        vec![
            "server",
            "deploy",
            "--config",
            "'/srv/My Tree/holosmith.toml'",
            "--root",
            "/srv/tree"
        ]
    );
}
