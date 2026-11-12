//! `holosmith serve` as the operator runs it.
//!
//! Every test drives `CARGO_BIN_EXE_holosmith`: the startup lines, the exit codes, the signals
//! and the files left behind are the real ones. `kill` is a process, not a syscall, so the
//! test needs no `unsafe`.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

mod support;
use support::TempDir;

/// The config every serving test reads, unless it needs another one.
///
/// It names a data root inside the directory: without one, `serve` would create and migrate the
/// default `/var/lib/holosmith`, so the tests would write machine state and fail wherever that
/// path is not writable (ADR-0009). It names the **stub** backend for the same reason: a Docker run
/// runs the preflight first and refuses on a machine without a daemon, and these tests are
/// about the banner, the signals, the data root and the health payload, not about which backend a
/// machine would pick — `the_default_backend_is_docker_and_the_log_line_says_so` is the one that asks
/// that, and it gives itself a daemon. `serve`-specific, so it stays here rather than in `support`.
fn config(dir: &TempDir) -> PathBuf {
    dir.write(
        "config.toml",
        &format!(
            "backend = \"stub\"\ncontrol_listen = \"127.0.0.1:0\"\npreview_listen = \"127.0.0.1:0\"\n\
             data_root = \"{}\"\n",
            dir.file("data").display()
        ),
    )
}

/// A socket that answers one `GET /_ping` the way a real daemon does, and its path.
///
/// The tests that must boot a **Docker** run need a machine that passes the preflight, and the daemon is the
/// only check this sandbox can satisfy without trusting the host (`docker_ping_socket` is the fixture
/// `tests/preflight.rs` also builds for its own cases). One connection, one answer, then the thread ends:
/// `serve` asks once.
fn docker_ping_socket(dir: &TempDir) -> PathBuf {
    use std::io::{Read, Write};
    let socket = dir.file("docker.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the test socket binds");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\nOK");
        }
    });
    socket
}

/// A `serve` process, started and waited for until it says where it listens.
struct Server {
    child: Child,
    port: u16,
    stderr: PathBuf,
    _dir: TempDir,
}

impl Server {
    fn start(dir: TempDir, arguments: &[OsString]) -> Self {
        let stderr = dir.file("serve.err");
        // The admin socket goes under this test's own directory, for the reason `--data-dir` does: this sandbox
        // lets an unprivileged user write the machine's paths, and a boot that bound `/run/holosmith/drain.sock`
        // would be writing this machine's real admin socket. Two tests booting at once would then also
        // be racing for one socket.
        let admin = dir.file("run/holosmith");
        let child = Command::new(env!("CARGO_BIN_EXE_holosmith"))
            .args(arguments)
            .arg("--admin-dir")
            .arg(&admin)
            .env_remove("HOLOSMITH_LOG")
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr).expect("the log file is creatable"))
            .spawn()
            .expect("the binary starts");
        Self {
            child,
            port: 0,
            stderr,
            _dir: dir,
        }
    }

    /// Wait until the process reports its control listener, or fail with everything it said.
    fn wait_until_listening(mut self) -> Self {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let said = self.said();
            if let Some(address) = startup(&said, "control_listen") {
                self.port = address
                    .rsplit_once(':')
                    .and_then(|(_, port)| port.parse().ok())
                    .expect("the logged address ends in a port");
                return self;
            }
            if Instant::now() > deadline {
                panic!("serve never reported control_listen. It said:\n{said}");
            }
            sleep(Duration::from_millis(20));
        }
    }

    fn said(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// `kill -<signal>` and the exit code that followed.
    fn signal(&mut self, signal: &str) -> i32 {
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(self.child.id().to_string())
            .status()
            .expect("/bin/kill is on PATH");
        assert!(status.success(), "kill -{signal} failed");
        self.child
            .wait()
            .expect("the child is waitable")
            .code()
            .expect("the child exited normally")
    }

    /// One HTTP/1.1 request, answered in full because `Connection: close` ends the body.
    fn request(&self, request: &str) -> (u16, String, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("the server accepts");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a timeout is settable");
        stream
            .write_all(request.as_bytes())
            .expect("the request is written");
        let mut raw = String::new();
        stream
            .read_to_string(&mut raw)
            .expect("the response is read");
        let (head, body) = raw.split_once("\r\n\r\n").expect("a header block");
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("a status line starts with the code");
        (status, head.to_string(), body.to_string())
    }

    fn get(&self, path: &str) -> (u16, String, String) {
        self.request(&format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        ))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A preview port another process holds is one `warn` and a banner line, not a refusal (ADR-0051).
///
/// The control UI is the screen an operator needs when something else on the machine is holding a port, so a
/// preview that cannot bind must not take it down — and the banner must say what happened rather than claim a
/// listener this run does not have.
#[test]
fn a_preview_port_that_is_taken_is_a_warning_and_the_control_listener_still_serves() {
    let dir = TempDir::new("preview-taken");
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port is bindable");
    let taken = held.local_addr().expect("a bound address").port();
    let config = dir.write(
        "config.toml",
        &format!(
            "backend = \"stub\"\ncontrol_listen = \"127.0.0.1:0\"\n\
             preview_listen = \"127.0.0.1:{taken}\"\ndata_root = \"{}\"\n",
            dir.file("data").display()
        ),
    );
    let data = dir.file("data");
    let mut server = Server::start(
        dir,
        &build(&[
            "serve",
            "--config",
            config.to_str().expect("the temp path is UTF-8"),
            "--port",
            "0",
            "--data-dir",
            data.to_str().expect("the temp path is UTF-8"),
            "--backend",
            "stub",
        ]),
    )
    .wait_until_listening();
    let said = server.said();
    // The `warn` this case is about is a tracing line, not a banner line: the eight startup lines are still
    // eight, which is the pin `the_startup_lines_are_exact_and_name_the_bound_port` holds.
    let banner: Vec<&str> = said
        .lines()
        .filter(|line| !line.contains(" WARN "))
        .collect();
    assert_eq!(
        banner.len(),
        8,
        "a failed preview bind must not add a banner line: {said}"
    );
    assert!(
        said.contains("WARN holosmith::serve: the preview listener could not be bound"),
        "the failure must be said in words: {said}"
    );
    let line = said
        .lines()
        .find(|line| line.starts_with("preview_listen "))
        .expect("the banner always has a preview line");
    assert!(line.contains("unavailable"), "{line}");
    assert!(line.contains(&taken.to_string()), "{line}");
    // The control listener is unaffected, and the process still stops the way systemd expects.
    assert_eq!(server.get("/api/health").0, 200);
    assert_eq!(server.signal("TERM"), 0);
    drop(held);
}

/// The value on one `key value` startup line.
fn startup(said: &str, key: &str) -> Option<String> {
    said.lines()
        .find_map(|line| line.strip_prefix(&format!("{key} ")))
        .map(|value| value.to_string())
}

fn holosmith(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(arguments)
        .env_remove("HOLOSMITH_LOG")
        .output()
        .expect("the binary runs")
}

fn build(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn the_startup_lines_are_exact_and_name_the_bound_port() {
    let dir = TempDir::new("startup");
    let config = config(&dir);
    let data = dir.file("data");
    let mut server = Server::start(
        dir,
        &build(&[
            "serve",
            "--config",
            config.to_str().expect("the temp path is UTF-8"),
            "--port",
            "0",
            "--data-dir",
            data.to_str().expect("the temp path is UTF-8"),
            "--backend",
            "stub",
        ]),
    )
    .wait_until_listening();
    let said = server.said();
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines.len(), 8, "{said}");
    assert_eq!(lines[0], "log info");
    let version = holosmith::version::VersionInfo::current();
    assert_eq!(
        lines[1],
        format!(
            "holosmith {} target {} role both backend stub",
            version.version, version.target
        )
    );
    assert_eq!(lines[2], format!("config {}", config.display()));
    assert_eq!(lines[3], format!("data_root {}", data.display()));
    assert_eq!(
        lines[4],
        format!("control_listen 127.0.0.1:{}", server.port)
    );
    // `--port 0` picked the port, so the origin is the one from the bound port, not `:0`.
    assert_eq!(
        lines[5],
        format!("control_origin http://localhost:{}", server.port)
    );
    //  binds the preview listener, so its line is the address the kernel gave — like the control
    // line above, and for the same reason: `127.0.0.1:0` is what the config may say, and the banner is where
    // the real port is readable. Two listeners, two addresses, one line each, and still exactly eight.
    let preview = lines[6]
        .strip_prefix("preview_listen 127.0.0.1:")
        .expect("the preview line names a loopback address");
    let preview_port: u16 = preview.parse().expect("the preview port is a number");
    assert_ne!(
        preview_port, 0,
        "the banner printed the port that was asked for, not the one bound"
    );
    assert!(
        TcpStream::connect(("127.0.0.1", preview_port)).is_ok(),
        "the preview listener is not accepting on {preview_port}"
    );
    // The eighth line, appended after `preview_listen` (ADR-0009): the file the process
    // opened and the schema it recorded.
    assert_eq!(
        lines[7],
        format!(
            "database {} schema {}",
            data.join("app.db").display(),
            holosmith::schema::SCHEMA_VERSION
        )
    );
    assert!(
        data.join("app.db").is_file(),
        "serve created no database under {}",
        data.display()
    );
    assert_ne!(server.port, 0, "a bound port is a real port");
    assert_eq!(server.signal("TERM"), 0);
}

#[test]
fn the_role_in_the_config_file_reaches_the_health_payload() {
    let dir = TempDir::new("role");
    let config = dir.write(
        "config.toml",
        &format!(
            "role = \"hub\"\nbackend = \"stub\"\ncontrol_listen = \"127.0.0.1:0\"\n\
             data_root = \"{}\"\n",
            dir.file("data").display()
        ),
    );
    let mut server = Server::start(
        dir,
        &build(&["serve", "--config", config.to_str().expect("UTF-8")]),
    )
    .wait_until_listening();
    let (status, head, body) = server.get("/api/health");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: application/json"), "{head}");
    assert!(body.contains(r#""role":"hub""#), "{body}");
    assert_eq!(server.signal("TERM"), 0);
}

#[test]
fn the_backend_comes_from_the_config_file() {
    // `backend = "stub"` is selectable from the file *and* by `--backend`, and the flag
    // wins — the same precedence `--port` and `--data-dir` already follow (ADR-0006).
    let dir = TempDir::new("backend");
    let config = dir.write(
        "config.toml",
        &format!(
            "backend = \"stub\"\ncontrol_listen = \"127.0.0.1:0\"\ndata_root = \"{}\"\n",
            dir.file("data").display()
        ),
    );
    let mut server = Server::start(
        dir,
        &build(&["serve", "--config", config.to_str().expect("UTF-8")]),
    )
    .wait_until_listening();
    let said = server.said();
    assert!(
        said.contains(" role both backend stub\n"),
        "the file's backend did not reach the startup line: {said}"
    );
    assert_eq!(server.signal("TERM"), 0);
}

#[test]
fn a_flag_backend_wins_over_the_config_file() {
    // The override this asserts *is* the Docker backend, so the machine has to pass the preflight: a
    // socket that answers `_ping`, and `min_free_gb = 0` so the real `df` on this sandbox's small disk
    // cannot fail a test that is about which backend the flag chose. The `git` check still runs
    // against the host's own git, which is what a Docker run on this machine would use.
    let dir = TempDir::new("backend-flag");
    let socket = docker_ping_socket(&dir);
    let config = dir.write(
        "config.toml",
        &format!(
            "backend = \"stub\"\ncontrol_listen = \"127.0.0.1:0\"\ndata_root = \"{}\"\n\
             docker_host = \"unix://{}\"\nmin_free_gb = 1\n",
            dir.file("data").display(),
            socket.display()
        ),
    );
    let mut server = Server::start(
        dir,
        &build(&[
            "serve",
            "--config",
            config.to_str().expect("UTF-8"),
            "--backend",
            "docker",
        ]),
    )
    .wait_until_listening();
    let said = server.said();
    assert!(
        said.contains(" role both backend docker\n"),
        "--backend did not override the file: {said}"
    );
    assert_eq!(server.signal("TERM"), 0);
}

#[test]
fn the_default_backend_is_docker_and_the_log_line_says_so() {
    // ADR-0001: the stub is opt-in, so a machine that says nothing about the backend runs the
    // Docker one — and a Docker run checks the machine first, so this test brings a daemon
    // that answers `_ping` and sets `min_free_gb = 0` so the real `df` on this sandbox's small disk cannot
    // fail it. That the banner appears at all is therefore half the assertion: the preflight passed.
    let dir = TempDir::new("backend-default");
    let socket = docker_ping_socket(&dir);
    let config = dir.write(
        "config.toml",
        &format!(
            "control_listen = \"127.0.0.1:0\"\npreview_listen = \"127.0.0.1:8081\"\n\
             data_root = \"{}\"\ndocker_host = \"unix://{}\"\nmin_free_gb = 0\n",
            dir.file("data").display(),
            socket.display()
        ),
    );
    let mut server = Server::start(
        dir,
        &build(&["serve", "--config", config.to_str().expect("UTF-8")]),
    )
    .wait_until_listening();
    let said = server.said();
    assert!(
        said.contains(" role both backend docker\n"),
        "the default backend is not docker: {said}"
    );
    assert_eq!(server.signal("TERM"), 0);
}

#[test]
fn a_hub_is_not_checked_for_docker_because_it_runs_no_sessions() {
    // Spec item 10 §6: `hub deploy` runs the startup steps "without the Docker and git checks". A hub with
    // the default `backend = "docker"` and no daemon anywhere is the normal hub, so it must boot — and it
    // boots even though `min_free_gb` is the default 20 and this sandbox has 3.
    let dir = TempDir::new("hub-no-docker");
    let config = dir.write(
        "config.toml",
        &format!(
            "role = \"hub\"\ncontrol_listen = \"127.0.0.1:0\"\ndata_root = \"{}\"\n\
             docker_host = \"unix://{}/absent.sock\"\n",
            dir.file("data").display(),
            dir.path().display()
        ),
    );
    let mut server = Server::start(
        dir,
        &build(&["serve", "--config", config.to_str().expect("UTF-8")]),
    )
    .wait_until_listening();
    let said = server.said();
    assert!(
        said.contains(" role hub backend docker\n"),
        "a hub did not boot on the default backend: {said}"
    );
    assert_eq!(server.signal("TERM"), 0);
}

#[test]
fn sigterm_stops_the_process_with_exit_zero() {
    let dir = TempDir::new("sigterm");
    let config = config(&dir);
    let mut server = Server::start(
        dir,
        &build(&["serve", "--config", config.to_str().expect("UTF-8")]),
    )
    .wait_until_listening();
    assert_eq!(server.signal("TERM"), 0);
}

#[test]
fn sigint_stops_the_process_with_exit_zero() {
    let dir = TempDir::new("sigint");
    let config = config(&dir);
    let mut server = Server::start(
        dir,
        &build(&["serve", "--config", config.to_str().expect("UTF-8")]),
    )
    .wait_until_listening();
    assert_eq!(server.signal("INT"), 0);
}

#[test]
fn an_invalid_config_is_reported_and_never_served() {
    let dir = TempDir::new("invalid");
    let config = dir.write(
        "config.toml",
        "control_listen = \"127.0.0.1:0\"\nmax_live_sessions = 0\n",
    );
    let output = holosmith(&["serve", "--config", config.to_str().expect("UTF-8")]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("max_live_sessions: must be at least 1"),
        "{stderr}"
    );
    assert!(stderr.contains("1 problem in"), "{stderr}");
    assert!(
        !stderr.contains("control_listen "),
        "an invalid config must not bind: {stderr}"
    );
}

#[test]
fn a_config_file_the_operator_named_must_exist() {
    let output = holosmith(&["serve", "--config", "/nonexistent/holosmith/config.toml"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no such file"), "{stderr}");
}

#[test]
fn a_port_that_collides_with_the_preview_port_is_refused_in_the_configs_words() {
    let dir = TempDir::new("collision");
    let config = dir.write("config.toml", "preview_listen = \"127.0.0.1:9123\"\n");
    let output = holosmith(&[
        "serve",
        "--config",
        config.to_str().expect("UTF-8"),
        "--port",
        "9123",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.lines().next().unwrap_or_default(),
        format!(
            "{}: preview_listen: must differ from control_listen, both are 127.0.0.1:9123",
            config.display()
        )
    );
    assert!(!stderr.contains("control_listen 127"), "{stderr}");
}

#[test]
fn a_data_dir_the_file_must_not_name_is_refused_from_the_cli_too() {
    // The repro this story's first P1 names: `config validate` refuses this data_root, so
    // `serve --data-dir` must refuse it in the same words and never bind.
    let dir = TempDir::new("data-dir-overlap");
    let config = config(&dir);
    let output = holosmith(&[
        "serve",
        "--config",
        config.to_str().expect("UTF-8"),
        "--data-dir",
        "/srv/holosmith/projects",
        "--port",
        "0",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.lines().next().unwrap_or_default(),
        format!(
            "{}: projects_root: must differ from data_root: deleting data must never touch a \
             repository",
            config.display()
        )
    );
    assert!(!stderr.contains("control_listen 127"), "{stderr}");
}

#[test]
fn a_relative_data_dir_is_refused_from_the_cli_too() {
    let dir = TempDir::new("data-dir-relative");
    let config = config(&dir);
    let output = holosmith(&[
        "serve",
        "--config",
        config.to_str().expect("UTF-8"),
        "--data-dir",
        "relative/data",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.lines().next().unwrap_or_default(),
        format!("{}: data_root: must be an absolute path", config.display())
    );
}

#[test]
fn a_data_dir_that_is_not_utf8_can_be_served() {
    let dir = TempDir::new("osstring");
    let config = config(&dir);
    let mut data = dir.file("data").into_os_string().into_vec();
    data.push(0xff);
    let mut server = Server::start(
        dir,
        &[
            OsString::from("serve"),
            OsString::from("--config"),
            config.into_os_string(),
            OsString::from("--data-dir"),
            OsString::from_vec(data),
        ],
    )
    .wait_until_listening();
    assert_eq!(server.get("/api/health").0, 200);
    assert_eq!(server.signal("TERM"), 0);
}

#[test]
fn a_data_dir_that_cannot_be_created_is_reported_before_anything_binds() {
    // A regular file where the data root belongs: `mkdir -p` fails, so the start names the
    // path and exits 1 with no listener (ADR-0009).
    let dir = TempDir::new("data-dir-unwritable");
    let config = config(&dir);
    let blocker = dir.write("blocker", "not a directory\n");
    let data = blocker.join("data");
    let output = holosmith(&[
        "serve",
        "--config",
        config.to_str().expect("UTF-8"),
        "--data-dir",
        data.to_str().expect("UTF-8"),
        "--port",
        "0",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.lines().next().unwrap_or_default(),
        format!(
            "holosmith: serve: cannot create the data root {}: Not a directory (os error 20)",
            data.display()
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("control_listen "), "{stderr}");
}

#[test]
fn the_data_root_serve_creates_is_the_mode_the_deploy_writes_whatever_the_umask() {
    // `doctor`'s `perms` finding compares the install tree with the modes `server deploy` writes, and the
    // data root's is 0755. `create_dir_all` uses the process's umask — 0775 on a machine whose umask is 0002 — so a
    // root `serve` made itself would fail the *next* boot's `ExecStartPre` over a mode nothing changed. `umask` is
    // a shell builtin and this crate denies `unsafe`, so the permissive umask arrives through a shell.
    let dir = TempDir::new("data-root-mode");
    let config = config(&dir);
    let data = dir.file("data");
    let stderr = dir.file("serve.err");
    // `--admin-dir` for the reason `--data-dir` is here: a boot in a test must not bind the machine's own
    // `/run/holosmith/drain.sock`, and this one spawns `serve` without `Server::start`'s helper.
    let admin = dir.file("run/holosmith");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "umask 0002; exec '{}' serve --config '{}' --port 0 --admin-dir '{}'",
            env!("CARGO_BIN_EXE_holosmith"),
            config.display(),
            admin.display()
        ))
        .env_remove("HOLOSMITH_LOG")
        .stdout(Stdio::null())
        .stderr(fs::File::create(&stderr).expect("the log file is creatable"))
        .spawn()
        .expect("the binary starts through a shell");
    // The banner is printed after the root is created and its mode set, so waiting for it is what makes the read
    // below a fact rather than a race with the chmod.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut said = String::new();
    while Instant::now() < deadline {
        said = fs::read_to_string(&stderr).unwrap_or_default();
        if said.contains("control_listen ") {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    let mode = fs::metadata(&data)
        .expect("serve created the data root")
        .permissions()
        .mode()
        & 0o777;
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        mode, 0o755,
        "serve made the data root {mode:04o}, which is the umask's mode and not the deploy's: {said}"
    );
}

#[test]
fn the_binary_serves_the_app_from_a_directory_that_holds_only_the_binary() {
    let dir = TempDir::new("empty-dir");
    let binary = dir.file("holosmith");
    fs::copy(env!("CARGO_BIN_EXE_holosmith"), &binary).expect("the binary is copyable");

    let elsewhere = TempDir::new("empty-dir-state");
    let config = elsewhere.write(
        "config.toml",
        "backend = \"stub\"\ncontrol_listen = \"127.0.0.1:0\"\n",
    );
    let stderr = elsewhere.file("serve.err");
    // The admin socket stays under this test's own directory, because a `serve` in a test must never bind the
    // machine's `/run/holosmith/drain.sock`, and this spawn does not go through `Server::start`.
    let admin = elsewhere.file("run/holosmith");
    let mut child = Command::new(&binary)
        .args([
            "serve",
            "--config",
            config.to_str().expect("UTF-8"),
            "--admin-dir",
            admin.to_str().expect("UTF-8"),
            "--data-dir",
            elsewhere
                .file("data")
                .to_str()
                .expect("UTF-8")
                .to_string()
                .as_str(),
        ])
        .current_dir(dir.path())
        .env_remove("HOLOSMITH_LOG")
        .stdout(Stdio::null())
        .stderr(fs::File::create(&stderr).expect("the log file is creatable"))
        .spawn()
        .expect("the binary starts");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut said = String::new();
    while Instant::now() < deadline {
        said = fs::read_to_string(&stderr).unwrap_or_default();
        if said.contains("control_listen ") {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    let port: u16 = startup(&said, "control_listen")
        .expect("serve reports its control listener")
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .expect("the logged address ends in a port");

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("the server accepts");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .expect("the request is written");
    let mut page = String::new();
    stream.read_to_string(&mut page).expect("the page is read");
    let (head, body) = page
        .split_once("\r\n\r\n")
        .expect("a response header block");
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "the app shell is not served: {page}"
    );
    // Compared with the embedded page, not with a wildcard: the directory holds nothing but
    // the binary, so this fails if `serve` ever reads the app from the filesystem instead.
    assert_eq!(
        body,
        holosmith::web::index_html(),
        "the served shell is not the embedded one"
    );

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("the server accepts");
    stream
        .write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .expect("the request is written");
    let mut health = String::new();
    stream.read_to_string(&mut health).expect("health is read");
    assert!(health.contains(r#""name":"holosmith""#), "{health}");

    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("/bin/kill is on PATH");
    assert!(status.success());
    assert_eq!(
        child.wait().expect("waitable").code(),
        Some(0),
        "SIGTERM must exit 0"
    );

    let mut entries: Vec<String> = fs::read_dir(dir.path())
        .expect("the directory is readable")
        .map(|entry| {
            entry
                .expect("a readable entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    entries.sort();
    assert_eq!(entries, vec!["holosmith".to_string()], "{entries:?}");
}
