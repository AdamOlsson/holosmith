// The spec's "build.rs never runs host-side" rule (item 10) governs agent-written
// repo code during promotion: promotion builds run in the builder container. This
// trusted workspace script runs in that same builder, never on the host.
use std::path::PathBuf;
use std::process::Command;

include!("build_support/web_embed.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support/web_embed.rs");
    println!("cargo:rerun-if-changed=../../pins/pi.txt");
    // Without this line a new migration file is silently left out of the binary (ADR-0009).
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-env-changed=HOLOSMITH_GIT_TAG");
    println!("cargo:rerun-if-env-changed=HOLOSMITH_WEB_DIST");
    println!("cargo:rerun-if-env-changed=HOLOSMITH_REQUIRE_WEB");
    println!(
        "cargo:rustc-env=HOLOSMITH_TARGET={}",
        std::env::var("TARGET").expect("cargo sets TARGET for every build script")
    );
    println!("cargo:rustc-env=HOLOSMITH_GIT_TAG={}", git_tag());
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

/// `HOLOSMITH_GIT_TAG` wins (scripts/build.sh sets it, and builder containers have no
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
