// The Service screen's rules (ADR-0110): what this machine is running, the commit a promotion would make live, the
// one button, and the job's lines while it runs.
//
// `main.ts` turns these answers into nodes and decides nothing, and `web/test/service.test.ts` is the only gate that
// can see a sentence — `npm test`, with no browser, so every branch here is a pure function of plain values. The clock
// is not read here either: a rule that needs "now" takes it as an argument (`transcript.ts`'s `expiryText`), which is
// what makes a report's dates testable.
//
// The screen exists for **one wait**: a press is not a request that answers, it is a job of about 45 minutes whose
// progress is a log file, so the page's work is making that wait legible — the candidate the button would send, the
// job's own last lines while it runs, and the ending when it stops. A screen rather than a card on the Computers
// screen (which owns "what is this machine running" for every machine) because a job's lines do not fit in a card,
// and this machine's card carries one sentence and a link here instead.
//
// Two things are **not** here: the checkout's own refusal sentences (`candidate::Refusal`'s, which the route answers
// with and which a press would answer with — one spelling of "why there is no candidate", and its repair is a
// command the operator runs on the machine) and the report's shape (`crate::pass::Report`, which the route relays as
// the file's own bytes).
//
// No state, no I/O, no DOM.

// The two routes this screen reads, and the one it posts to — which are two paths, because a press and its job's
// record are one route and the candidate is another. Spelled once, here: `push.ts` keeps the screens' own paths, and
// a path spelled twice is a read that 404s in one place.
export const UPDATE_PATH = "/api/update";
export const CANDIDATE_PATH = "/api/update/candidate";

// How often the page re-reads the job while one is under way. Three seconds because the job's own record is a file
// and a gate prints a line every few seconds: faster is a poll with nothing new in it, slower reads as a page that
// stopped.
export const POLL_INTERVAL_MS = 3000;

// How many of the log's last lines the screen shows. More than a phone screen and fewer than the route's hundred: the
// lines are what a run is doing, and the whole log is the file's, at the path the job names.
export const JOB_LINES_SHOWN = 12;

// The commit a promotion would make live, as `GET /api/update/candidate` answers it. `source` is the checkout it was
// read from, rendered as a string by the route: it is the machine's fact and the page only prints it.
export type Candidate = { source: string; commit: string; tag: string };

// What the page knows about the candidate: the commit a press would send, or the refusal and the sentence that says
// why there is none. One value rather than a candidate and a refusal that could disagree.
export type CandidateFacts =
  | { found: true; candidate: Candidate }
  | { found: false; code: string; sentence: string };

// The gate's own ending, as the report carries it (`crate::pass::GateReport`). `ending` is the report's word —
// `verified`, `refused`, `stale crate set`, `could not verify` — and `exit_code` is `null` when a signal killed it.
export type Gate = {
  command: string;
  image: string;
  exit_code: number | null;
  ending: string;
  skips: number;
};

// One job, as `GET /api/update` answers it: the report file's own object plus the log's last lines. Only the fields
// this screen draws are declared — the report has more (`candidate`, `artifact`, `steps`), and a type that named them
// would be a second spelling of a shape the file owns.
//
// `since` is `number | null` because a report the **previous** release's job wrote carries no `since` at all: that
// run's start is unknown to this page, and a client that defaulted it would render the previous run as this one.
export type Report = {
  state: string;
  since?: number | null;
  gate: Gate | null;
  detail: string | null;
  log?: { path: string; lines: string[] };
};

// ---- the sentences -----------------------------------------------------------------------------

// The button's one word.
export const PROMOTE_LABEL = "Promote";

// What the button will do, said **before** the press: 45 minutes with no warning is the wrong surprise, and the
// press is not a request that answers.
export const PROMOTE_WAIT =
  "pressing this sends `master`'s commit, runs the tree's own gate on it in a container, and makes it live if the gate passes — about 45 minutes, and this page shows the job's lines while it runs";

// Why the button is off. Three of them are the page's own facts — a machine that refuses the route at all, one that is
// draining, one with a job already under way — and the rest are the route's sentences, passed through below.
export const PROMOTE_HUB =
  "this machine is a hub: it has no release tree and no Docker to run the gate in, so a promotion is asked of a server";
export const PROMOTE_DRAINING =
  "this machine is draining for a restart, so it takes no new work and starts no promotion until it comes back";
export const PROMOTE_RUNNING =
  "a promotion is already under way on this machine: the page shows its lines until it ends";

// The job's block above its lines: a press that has been accepted and whose job has not written anything yet, and a
// job under way. An ended job reads as its ending, so it has no heading of its own.
export const JOB_ACCEPTED =
  "the promotion was accepted; the job writes its first line when its unit starts";
export const JOB_RUNNING = "a promotion is running on this machine: its last lines are below";

// What each ending means for the machine, and one sentence per state (`crate::pass::State`). `running` has no
// sentence here: it has not ended.
const ENDINGS: Readonly<Record<string, string>> = {
  live: "the commit is live: this machine now answers as the process the promotion started",
  refused: "the promotion was refused, and nothing on this machine was asked to move",
  "could-not-verify": "the gate could not verify the tree, so nothing was promoted",
  failed:
    "the promotion ran and the commit did not become live, so the previous release is the one running",
};

// The one sentence a state this build does not know earns: a report is a file, and a state from a newer release is a
// word to print rather than a throw.
const UNKNOWN_ENDING = "the job ended as";

// The Computers screen's one sentence about this machine's own service, and the link that says more. On **this**
// machine's card only: a peer's card is a peer's, and a promotion is asked of the machine it would restart.
export const SERVICE_CARD_SENTENCE =
  "what this machine is running, and the commit a promotion would make live, are on its service screen";
export const SERVICE_LINK_LABEL = "Service";

// One sentence for a refusal the page has to show, in the API's own words — `no_candidate` and `update_disabled` are
// a press's answers too, and each names the repair (`holosmith config edit`, the config key and the restart). A code
// a page has nothing to add to keeps its word in front, as `hostFailure` does: `unreachable` and `internal` are the
// operator's next question, not a sentence to read twice.
export function refusalSentence(refusal: { code: string; sentence: string }): string {
  const known = refusal.code === "no_candidate" || refusal.code === "update_disabled";
  return known ? refusal.sentence : `${refusal.code}: ${refusal.sentence}`;
}

// ---- what the page says ------------------------------------------------------------------------

// The version this machine is running: the health answer's own, which is the release `serve` is a process of.
export function liveText(version: string): string {
  return `the live version is ${version}`;
}

// The machine's drain, or `null` when it is taking work. A draining machine is a state the server's health answer
// carries and nothing else on the page draws.
export function drainText(draining: boolean): string | null {
  return draining
    ? "this machine is draining for a restart: its sessions are stopping, and it comes back when the restart does"
    : null;
}

// The candidate: `master`'s commit and the tag it describes itself with, or the refusal's own sentence. A candidate
// is only ever a **clean** checkout on `master` (`candidate::resolve` refuses anything else), so "the tree is clean"
// is a fact of the value's existence rather than a second check: a dirty tree is a refusal, with the files named.
export function candidateText(facts: CandidateFacts): string {
  if (!facts.found) {
    return refusalSentence(facts);
  }
  return `\`master\` is ${facts.candidate.commit} (${facts.candidate.tag}), and the tree is clean`;
}

// Whether the page offers the button, and the one sentence that says why not — the composer's `canPrompt` rule, one
// screen over: the control is drawn and disabled rather than hidden, because the reason is what the operator needs.
//
// The order is the machine's own facts first (a hub promotes nothing whatever it says, then a job already running,
// then the drain), and the candidate last: a machine that could not start a promotion at all is the sentence that
// answers the question, and "there is no candidate" about a hub would be a smaller fact than the reason.
export function promoteOffered(facts: {
  role: string;
  draining: boolean;
  running: boolean;
  candidate: CandidateFacts;
}): { allowed: boolean; sentence: string | null } {
  if (facts.role === "hub") {
    return { allowed: false, sentence: PROMOTE_HUB };
  }
  if (facts.running) {
    return { allowed: false, sentence: PROMOTE_RUNNING };
  }
  if (facts.draining) {
    return { allowed: false, sentence: PROMOTE_DRAINING };
  }
  if (!facts.candidate.found) {
    return { allowed: false, sentence: refusalSentence(facts.candidate) };
  }
  return { allowed: true, sentence: null };
}

// The body a press posts: the commit **the page rendered**, not one read again. The route compares it against
// `master`'s now, so a commit that landed while the page was open is a `422` rather than a promotion nobody saw.
export function promoteBody(commit: string): string {
  return JSON.stringify({ commit });
}

// ---- the job -----------------------------------------------------------------------------------

// Whether the report the page holds is the run it pressed for.
//
// `since` is the `202`'s, and the report's is the instant the job's pass began, which is later — so a report written
// **before** the one the page accepted is the previous promotion's ending, and rendering it as this run's is the
// window `http/update.rs`'s header describes. A report with no `since` at all is a previous **release**'s job, which
// this page cannot date: it is not this run either.
export function isThisRun(since: number | null | undefined, accepted: number): boolean {
  return typeof since === "number" && since >= accepted;
}

// Whether a job is under way, from the report alone: `running` is the job's own first write, so this is true of a job
// this page never pressed (one a command or another phone started) and of a job whose process was killed — which
// leaves `running` in place until the next run writes its seed.
export function jobRunning(report: Report | null): boolean {
  return report !== null && report.state === "running";
}

// What the job's block says above its lines, or `null` when it has nothing to add: an ending is its own sentence.
export function jobHead(waiting: boolean, report: Report | null): string | null {
  if (waiting) {
    return JOB_ACCEPTED;
  }
  return jobRunning(report) ? JOB_RUNNING : null;
}

// The job's lines, newest last, capped at what a screen holds. A run that has printed nothing yet, or a job whose log
// this page could not read, is no lines rather than a sentence: the heading says what is happening.
export function jobLines(report: Report | null): string[] {
  if (report === null) {
    return [];
  }
  return (report.log?.lines ?? []).slice(-JOB_LINES_SHOWN);
}

// The gate's own ending, as one clause: how it ended, its status, and how many of its cases measured nothing — a
// `SKIP` line is a case that measured nothing, which is why the count is part of the ending and not a footnote
// (`crate::pass` says the same thing over the same number).
export function gateText(gate: Gate | null): string | null {
  if (gate === null) {
    return null;
  }
  const status = gate.exit_code === null ? "killed by a signal" : `exit ${gate.exit_code}`;
  const skipped = gate.skips === 1 ? "1 case skipped" : `${gate.skips} cases skipped`;
  return `the gate ended ${gate.ending} (${status}), ${skipped}`;
}

// What the job's ending says: the state's own sentence, then the gate's record when one ran. `null` while a job is
// under way — its lines are the answer then — and for a page that has never seen a job.
export function endingText(report: Report | null): string | null {
  if (report === null || report.state === "running") {
    return null;
  }
  const state = ENDINGS[report.state] ?? `${UNKNOWN_ENDING} ${report.state}`;
  const gate = gateText(report.gate);
  return gate === null ? state : `${state} — ${gate}`;
}

// The report's `detail`: the sentence the run wrote for its own ending — the gate's refusal with its last lines in
// it, or why the promotion failed. `null` when the report has none, which is every `live` ending.
export function detailText(report: Report | null): string | null {
  const detail = report?.detail ?? null;
  return detail === null || detail.trim() === "" ? null : detail;
}

// What the page asks for the candidate, from the route's answer — the two halves of one value, so a refusal cannot be
// held beside a candidate that is not there.
export function candidateFacts(
  answer: { ok: true; value: Candidate } | { ok: false; code: string; message: string },
): CandidateFacts {
  return answer.ok
    ? { found: true, candidate: answer.value }
    : { found: false, code: answer.code, sentence: answer.message };
}
