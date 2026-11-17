//! What a run says **while** it works, as one line per fact, as against what it reports when it is over.
//!
//! A [`crate::deploy::Report`] is written after the last step, and that is right for a verdict and wrong for the two
//! silences an install has: step 3 builds two images, the first of which downloads pi, and the wait for `/api/health`
//! after the unit is enabled may take a minute. Neither prints anything until the whole run is done, so a first
//! install is minutes of a terminal that looks hung. So the run tells a **sink** what it is about to do and what a
//! build answered, and the sink decides where that goes: [`Silent`] for `deploy` — which has no one to tell, because
//! its report is the whole of what it prints — and [`Announce`] for an install an operator is watching.
//!
//! The two streams are the other half of that decision, and they are not the same stream (ADR-0092): progress is a
//! line as it happens and the report is one block at the end, so `holosmith install > out 2> err` leaves `out` the
//! verdict an operator or a gate reads and puts every line of "still working" on `err`.
//!
//! Two methods rather than one because the two facts are different: [`Progress::begin`] is a step about to be
//! evaluated, and [`Progress::note`] is one thing inside a step that is worth a line of its own (a build that starts,
//! the same build that ended). Both are called **before** the work they describe, both have a body that does nothing
//! so a sink says only what it cares about, and neither can fail: a sink is an observer, and an observer that could
//! refuse would be a second place the run's order is decided.

use std::io::Write as _;

/// What a run tells a sink about the work it is doing, one line per fact.
///
/// `label` is what the line is about, spelled the way the report spells it — `3/6 images` for one of the spec's six
/// steps, `unit` for one of install's own — so a reader can tie a line that arrived while waiting to the line that
/// arrives with the verdict. `phrase` is present tense and, where the work is slow, says how long it may take.
pub trait Progress {
    /// A step is about to be evaluated: after the skip answer and before the step's first read.
    ///
    /// A step the run has no work for emits nothing: a hub's images and proxies are `skipped`, which has already
    /// answered that there is nothing to wait for, and a begin line over that silence would be a lie about what it is.
    fn begin(&mut self, _label: &str, _phrase: &str) {}

    /// One fact inside a step: the beginning or the end of one image build, a wait whose bound is worth naming.
    fn note(&mut self, _label: &str, _phrase: &str) {}
}

/// The sink that says nothing, and the default: a plan has nothing to wait for, `deploy` has no one to tell, and a
/// test that asserts a [`crate::deploy::Report`] wants the report rather than a stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct Silent;

impl Progress for Silent {}

/// The prefix every progress line carries, and the reason it is one word: the verb whose silence this is.
const VERB: &str = "install";

/// The sink that prints: `install: [3/6 images] building the two images — …`, on **stderr**, one flushed line per
/// fact.
///
/// The prefix's verb is `install`'s and not a parameter, because it is the one verb that announces: `server deploy`
/// and `hub deploy` pass [`Silent`] (their report is the whole of what they print), and a plan passes it too.
///
/// `stderr` is the decision rather than a habit: the report on stdout is what a gate greps and an operator reads as
/// the verdict, and a progress line interleaved into it would be a byte that depends on how long a build took.
#[derive(Debug, Clone, Copy, Default)]
pub struct Announce;

impl Progress for Announce {
    fn begin(&mut self, label: &str, phrase: &str) {
        line(label, phrase);
    }

    fn note(&mut self, label: &str, phrase: &str) {
        line(label, phrase);
    }
}

/// One progress line, flushed: a build's last line is the reader's only evidence that the next one is coming, so a
/// line sitting in a buffer is the same as no line at all. Writing to a stream cannot fail in a way this run could act
/// on — a closed `stderr` is a reader that stopped reading, not a step that failed.
fn line(label: &str, phrase: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{VERB}: [{label}] {phrase}");
    let _ = stderr.flush();
}
