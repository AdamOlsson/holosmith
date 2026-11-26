// The spec's "build.rs never runs host-side" rule (item 10) governs agent-written
// repo code during promotion: promotion builds run in a container. This trusted
// workspace script runs in that same container, never on the host.
use std::path::PathBuf;
use std::process::Command;

include!("build_support/web_embed.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support/web_embed.rs");
    println!("cargo:rerun-if-changed=../../pins/pi.txt");
    // Both pins are embedded by `src/pins.rs`, so a bumped one is a different binary.
    println!("cargo:rerun-if-changed=../../pins/rust.txt");
    // The version is a file outside the manifests, for the reason `version()` gives.
    println!("cargo:rerun-if-changed=../../VERSION");
    // Without this line a new migration file is silently left out of the binary (ADR-0009).
    println!("cargo:rerun-if-changed=migrations");
    // The two Dockerfiles `src/images.rs` embeds: a reworded one is a different binary.
    println!("cargo:rerun-if-changed=images");
    println!("cargo:rerun-if-env-changed=HOLOSMITH_GIT_TAG");
    println!("cargo:rerun-if-env-changed=HOLOSMITH_WEB_DIST");
    println!("cargo:rerun-if-env-changed=HOLOSMITH_REQUIRE_WEB");
    println!(
        "cargo:rustc-env=HOLOSMITH_TARGET={}",
        std::env::var("TARGET").expect("cargo sets TARGET for every build script")
    );
    println!("cargo:rustc-env=HOLOSMITH_GIT_TAG={}", git_tag());
    println!("cargo:rustc-env=HOLOSMITH_VERSION={}", version());
    let web_dir = write_web_dir(web_dir());
    println!("cargo:rustc-env=HOLOSMITH_WEB_DIR={}", web_dir.display());
}

/// Write the `include_dir!` invocation for the chosen directory into `OUT_DIR`.
///
/// `include_dir!` takes a string literal, not an `env!`, so the literal is generated: this is
/// the one place the embedded path is spelled, and `src/web.rs` `include!`s the result.
fn write_web_dir(dir: PathBuf) -> PathBuf {
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts"))
            .join("web_app.rs");
    let literal = format!("{:?}", dir.to_string_lossy());
    let source = format!(
        "// Written by build.rs: the app directory chosen by build_support/web_embed.rs.\n\
         static APP: include_dir::Dir<'static> = include_dir::include_dir!({literal});\n"
    );
    if let Err(error) = std::fs::write(&out, source) {
        println!("cargo::error=cannot write {}: {error}", out.display());
        std::process::exit(1);
    }
    out
}

/// The directory the binary embeds: the built app, or a placeholder written into `OUT_DIR`
/// (ADR-0007). `HOLOSMITH_REQUIRE_WEB=1` turns a missing app into a build failure instead of
/// a warning, which is what `scripts/build.sh` wants and `cargo test` does not.
fn web_dir() -> PathBuf {
    let dist = match std::env::var_os("HOLOSMITH_WEB_DIST") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
        )
        .join("../../web/dist"),
    };
    let dist = dist.canonicalize().unwrap_or(dist);
    println!("cargo:rerun-if-changed={}", dist.display());

    let require_web = std::env::var("HOLOSMITH_REQUIRE_WEB").as_deref() == Ok("1");
    match decide(dist.join("index.html").is_file(), require_web) {
        Decision::Embed => dist,
        Decision::Placeholder => {
            println!(
                "cargo:warning=the web app was not built: {} has no index.html. Run \
                 scripts/build-web.sh, or set HOLOSMITH_WEB_DIST. This binary serves a \
                 placeholder page.",
                dist.display()
            );
            write_placeholder()
        }
        Decision::Refuse => {
            let message = format!(
                "HOLOSMITH_REQUIRE_WEB=1 but the web app was not built: {} has no \
                 index.html. Run scripts/build-web.sh first.",
                dist.display()
            );
            println!("cargo::error={message}");
            std::process::exit(1);
        }
    }
}

/// Write the placeholder into `OUT_DIR`, where nothing but this build reads it.
fn write_placeholder() -> PathBuf {
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts"))
            .join("web-placeholder");
    if let Err(error) = std::fs::create_dir_all(&out) {
        println!("cargo::error=cannot create {}: {error}", out.display());
        std::process::exit(1);
    }
    let page = out.join("index.html");
    if let Err(error) = std::fs::write(&page, placeholder_index_html()) {
        println!("cargo::error=cannot write {}: {error}", page.display());
        std::process::exit(1);
    }
    out
}

/// The version this build reports, read out of `VERSION` at the repository root — the one copy of it.
///
/// `Cargo.toml` carries `0.0.0` as a placeholder instead, and that is load-bearing rather than tidy: a real version
/// there lands in `Cargo.lock`, whose digest `scripts/check-crate-set.sh` compares with the crate set the agent image
/// vendors — so a bump would read as a stale image and become the one edit this tree cannot promote. A file of its
/// own is in nobody's digest.
///
/// The two refusals are the two ways a value here would reach a tag that cannot exist: no file at all (a source copy
/// that dropped it), and a file holding anything but one token (a newline would reach `version --json` and
/// `agent-image:<version>`). Both stop the build, because the alternative is a binary that reports a version nothing
/// agrees with.
fn version() -> String {
    let path = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("cargo sets CARGO_MANIFEST_DIR for every build script"),
    )
    .join("../../VERSION");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        println!(
            "cargo::error={} cannot be read ({error}): it is the one copy of the version this build reports",
            path.display()
        );
        std::process::exit(1);
    });
    let version = text.trim();
    if version.is_empty() || version.split_whitespace().count() != 1 {
        println!(
            "cargo::error={} must hold one version on one line, and holds {text:?}",
            path.display()
        );
        std::process::exit(1);
    }
    version.to_string()
}

/// `HOLOSMITH_GIT_TAG` wins (scripts/build.sh sets it, and a container build has no
/// `.git` to ask); otherwise ask git; a tree with no git answers `unknown` and the
/// build still succeeds.
fn git_tag() -> String {
    if let Ok(from_env) = std::env::var("HOLOSMITH_GIT_TAG") {
        let from_env = from_env.trim();
        if !from_env.is_empty() {
            return from_env.to_string();
        }
    }
    let described = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|tag| !tag.is_empty());
    described.unwrap_or_else(|| "unknown".to_string())
}
