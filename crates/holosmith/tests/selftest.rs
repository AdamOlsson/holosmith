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
use holosmith::selftest::{self, Outcome, PROXY_TARGET, PUBLIC, REACHED, Runner};

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

/// HS-038's off case, measured the only way it can be: **the same network**, before and after something answers on
/// the proxy's alias.
///
/// The unit tests decide what a report means; this test is what makes the report possible at all. It is the pair
/// that matters — a probe that reported `0` on a network where a listener *is* attached would make the off case a
/// claim rather than a measurement — and one network rather than two, so "the second network was different" is not
/// an explanation for the difference.
///
/// The listener is a stand-in, not the machine's live `hs_egress`: joining the real proxy to a test network would
/// hand a running session a route it did not ask for (the landmine HS-037's tests record). What is being measured
/// is the question the boot asks — *does something answer on `egress-proxy:3128` from an internal network* — and a
/// container listening on 3128 under that alias answers it exactly as the proxy does.
#[test]
fn a_listener_on_the_proxy_alias_is_what_the_off_case_looks_for() {
    if !ready() {
        return;
    }
    let docker = Docker::new(None);
    let image = image();
    if !docker.succeeds(&["image", "inspect", &image]) {
        eprintln!("SKIP tests/selftest.rs: no image {image}");
        return;
    }
    // Names of its own: a session's are `agents_<id>` and the machine's proxy is `hs_egress`.
    let network = "holosmith-selftest-offcase";
    let standin = "holosmith-selftest-standin";
    let _ = docker.remove_container(standin);
    let _ = docker.remove_network(network);
    docker
        .create_internal_network(network)
        .expect("the test network can be created");

    let targets = vec![PROXY_TARGET.to_string()];
    let probe = |docker: &Docker| {
        docker
            .attempt(
                &[
                    "run".to_string(),
                    "--rm".to_string(),
                    "--network".to_string(),
                    network.to_string(),
                    image.clone(),
                    "node".to_string(),
                    "-e".to_string(),
                    selftest::PROBE.to_string(),
                ]
                .into_iter()
                .chain(targets.iter().cloned())
                .collect::<Vec<String>>(),
                selftest::PROBE_TIMEOUT,
            )
            .expect("the probe can be asked")
            .expect("the probe finishes")
    };

    // Nothing is attached: the alias does not resolve, and that is the pass `selftest::run` reports today.
    let bare = probe(&docker);
    eprintln!(
        "off case on {network}: exit {} reached {:?}",
        bare.code, bare.reached
    );
    assert_eq!(
        bare.code, 0,
        "nothing should answer on a network with no proxy attached, and the probe said {}",
        bare.reached
    );

    // The same network with a listener on the alias: now the probe must find it, or the check cannot fire and the
    // off case is a claim.
    let started = docker
        .run_detached(
            &[
                "run".to_string(),
                "--detach".to_string(),
                "--rm".to_string(),
                "--name".to_string(),
                standin.to_string(),
                "--network".to_string(),
                network.to_string(),
                "--network-alias".to_string(),
                holosmith::container::PROXY_ALIAS.to_string(),
                image.clone(),
                "node".to_string(),
                "-e".to_string(),
                // One connection, one refusal, then on to the next: the probe only needs an answer.
                "require('net').createServer((s) => { s.on('error', () => {}); \
             s.end('HTTP/1.1 403 Forbidden\\r\\n\\r\\n'); }).listen(3128)"
                    .to_string(),
            ],
            std::time::Duration::from_secs(60),
        )
        .expect("the stand-in can be started");
    assert!(
        started.is_some(),
        "the stand-in listener did not start within its bound"
    );

    let attached = probe(&docker);
    eprintln!(
        "off case with a listener on the alias: exit {} reached {:?}",
        attached.code, attached.reached
    );
    assert_eq!(
        attached.code, REACHED,
        "a listener on the alias must be a finding, or the off case proves nothing: {}",
        attached.reached
    );
    assert!(
        attached.reached.contains(PROXY_TARGET),
        "the finding names the address that answered: {}",
        attached.reached
    );
    // And the decision turns that report into the proxy's own sentence, not the bridge's.
    let Outcome::Failed(sentence) = selftest::decide(&attached, &targets) else {
        panic!("the report must refuse the boot");
    };
    assert!(
        sentence.contains(holosmith::container::PROXY_NAME),
        "the sentence names the container to look at: {sentence}"
    );

    docker.remove_container(standin);
    docker.remove_network(network);
    assert!(
        !docker.succeeds(&["network", "inspect", network]),
        "the test's network must not outlive it"
    );
}
