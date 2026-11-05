//! The network self-test against a real daemon (HS-036, spec item 2).
//!
//! The three *decisions* are unit tests in `selftest.rs`, driven by a fake [`Runner`], because a machine cannot
//! be made to leak on demand. What only a daemon can answer is the other half: that the probe, with the real
//! image and the real `docker run` vector, **passes on an internal network and finds the host on the default
//! bridge**. That is the measurement this story is named for, so it is asserted against the machine rather than
//! described, and it skips — one printed line, no failure — where there is no daemon or no image, because a
//! machine without Docker is a supported machine (a hub) and `cargo test` must stay green there.
//!
//! Both halves matter: a probe that always said "nothing reached" would pass every unit test and prove nothing,
//! and the bridge run here is what makes "the probe can see a leak" a fact.

use std::process::Command;

use holosmith::docker::Docker;
use holosmith::selftest::{self, Outcome, PUBLIC, REACHED, Runner};

/// The image the probe runs in: the one sessions run, because a probe that used a different image would be
/// measuring a different machine.
fn image() -> String {
    std::env::var("HOLOSMITH_TEST_IMAGE").unwrap_or_else(|_| "pi-agent:local".to_string())
}

/// The daemon answers, or these tests say so once and return.
fn ready() -> bool {
    let daemon = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !daemon {
        eprintln!(
            "SKIP tests/selftest.rs: no Docker daemon answers (the gate's case is case_network_selftest)"
        );
    }
    daemon
}

#[test]
fn an_internal_network_reaches_nothing_while_the_default_bridge_reaches_the_host() {
    // The whole story in one test: the same probe, the same image, two networks. The internal one is the
    // production path (`selftest::run` creates it, runs the probe and removes it); the bridge one is the
    // control, and it must *find* something, or "passed" would mean nothing.
    if !ready() {
        return;
    }
    let docker = Docker::new(None);
    let image = image();

    let outcome = selftest::run(&docker, &image);
    eprintln!("selftest on an internal network: {outcome}");
    match &outcome {
        Outcome::Passed => {}
        Outcome::Failed(sentence) => panic!(
            "this machine's internal network reaches something, so it would run sessions that are not \
             isolated: {sentence}"
        ),
        Outcome::Unknown(sentence) => {
            // No image on this machine is the ordinary reason, and it is not this test's subject.
            eprintln!("SKIP tests/selftest.rs: the probe could not run ({sentence})");
            return;
        }
    }
    assert!(
        !docker.succeeds(&["network", "inspect", selftest::NETWORK]),
        "the temporary network outlived the test"
    );

    // The control: the same probe on the daemon's default bridge. The host's own 2375 answers with a reset on
    // a normal machine, which the probe counts as *reached* — that rule is why this arm is worth running.
    let targets = docker.targets();
    assert!(
        targets.len() >= 2,
        "the host target is what the bridge control is for: {targets:?}"
    );
    let bridged = docker
        .attempt(
            &[
                "run".to_string(),
                "--rm".to_string(),
                "--network".to_string(),
                "bridge".to_string(),
                image,
                "node".to_string(),
                "-e".to_string(),
                selftest::PROBE.to_string(),
            ]
            .into_iter()
            .chain(targets.iter().cloned())
            .collect::<Vec<String>>(),
            holosmith::selftest::PROBE_TIMEOUT,
        )
        .expect("the probe can be asked")
        .expect("the probe finishes");
    eprintln!(
        "selftest on the default bridge: exit {} reached {:?}",
        bridged.code, bridged.reached
    );
    if bridged.code != REACHED {
        // A daemon on a machine with no route out (and nothing listening on 2375) cannot be a control: there is
        // no leak here to find. That is a machine fact, not a pass — say so and stop, rather than turning a
        // supported configuration into a red test.
        eprintln!(
            "SKIP tests/selftest.rs: the default bridge reaches nothing either ({}), so this machine cannot \
             exercise the control",
            bridged.reached
        );
        return;
    }
    assert!(
        bridged.reached.contains(":2375") || bridged.reached.contains(PUBLIC),
        "the bridge control named nothing: {}",
        bridged.reached
    );
}
