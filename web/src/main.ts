// The app shell: two screens that share one header and one stream. `/` is the machine's health, the
// session list and the new-session form; `/sessions/<id>` is one session with its live transcript
// (HS-014). Every value either screen shows came from a response or from a stream frame, so a
// reconnect has nothing to reconcile and there is no cache to invalidate (ADR-0017). The one value
// the list keeps that is not on screen is the pending Idempotency-Key a retry has to repeat.
//
// The transcript's rules — what a frame means, one block per message id, append-or-replace — live in
// `transcript.ts` and are tested without a DOM (ADR-0018). This file only turns those changes into
// nodes, which is why every delta is one appended text node and nothing is ever re-rendered.

import {
  COMMIT_ASKED,
  COMMIT_LABEL,
  COMMIT_PROMPT,
  COMPUTERS_INTRO,
  COUNTS_UNKNOWN,
  DIRTY_MERGE_REASON,
  FORCED_DELETE_LABEL,
  GROUP_WAITING,
  HISTORY_PAGE_LIMIT,
  HISTORY_TRUNCATED,
  HOST_ADD_LABEL,
  HOST_FORCE_LABEL,
  HOST_REMOVE_LABEL,
  LIVE_STATES,
  LOCAL_HOST,
  NOTHING_COMMITTED,
  NO_CREATE_TARGET,
  RESOLVE_ASKED,
  RESOLVE_LABEL,
  RESOLVE_PROMPT,
  STRATEGIES,
  TRANSCRIPT_EVENTS,
  Transcript,
  UNSAVED_WORK_NOTICE,
  AWAY_DETAIL,
  affordances,
  awaySentence,
  isAwayCode,
  apiPath,
  budgetNotice,
  ciWarning,
  createFailure,
  createTargets,
  expiryText,
  conflictLine,
  countsLine,
  deletedNotice,
  diffPath,
  fileLine,
  forwards,
  forwardingNote,
  historyPath,
  hostFailure,
  hostForceOffered,
  hostLabel,
  hostRef,
  hostRemoveQuestion,
  isLocalRow,
  lastCheckText,
  mergeBlocked,
  mergedNotice,
  networkText,
  ownLabel,
  parseFrame,
  preselectedTarget,
  previewShown,
  previewUrl,
  publishQuestion,
  publishVisible,
  publishedNotice,
  parseTruncated,
  reachText,
  registryMissing,
  relativeTime,
  sessionActionPath,
  sessionCounts,
  sessionCountsText,
  sessionHref,
  sessionIdFromPath,
  sessionPath,
  sheetFailure,
  skewNote,
  startLabel,
  stateText,
  stoppedDetail,
  STREAM_LOST,
  streamPath,
  targetOptions,
  uptimeText,
  type Change,
  type Changes,
  type DeltaKind,
  type HistoryPage,
  type Kept,
  type Merged,
  type Published,
  type SessionState,
  type Spend,
} from "./transcript";
import {
  COMPUTERS_PATH,
  PER_MACHINE_SENTENCE,
  SETTINGS_PATH,
  installSentence,
  isIos,
  machineState,
  machineSubscription,
  navLinks,
  pushActionLabel,
  pushActionable,
  pushState,
  subscriptionBody,
  subscriptionRoute,
  type NavLink,
  type PushFacts,
  type PushState,
} from "./push";

type Health = {
  name: string;
  version: string;
  role: string;
  uptime_s: number;
  // The machine's Web Push pair's public half, base64url, or `null` when it has none (HS-067, ADR-0072 D1). The
  // settings screen hands it straight to `pushManager.subscribe`, which is the only use it has.
  application_server_key: string | null;
  // What this machine calls itself in a notification's payload (HS-074): the `machine_name` config key, `local` for a
  // machine that has no name of its own. The settings screen labels its own row with it and the create form's first
  // option too, so the string the tap's path is built from is one the operator can see.
  machine_name: string;
  // The origin a session's preview is served on (HS-041): the listener's own origin, never the control one.
  preview_origin: string;
};

type ErrorEnvelope = {
  code: string;
  message: string;
};

// The four provisioning steps, in order (ADR-0015).
type Step = "worktree" | "network" | "container" | "pi";

// One list element: the columns this screen renders out of the row's fifteen.
type Session = {
  id: string;
  project: string;
  model: string;
  status: SessionState;
  status_reason: string | null;
  last_active_at: number;
  // When the state became `stopped`, and `null` in every other state (ADR-0016's one writer keeps the two in
  // step). The list draws the days it has left from this and the machine's window (HS-064).
  stopped_at: number | null;
};

// One session's row as the session screen reads it: the same fifteen columns, of which the list only
// draws six. `thinking` and `network` are what the create was accepted with, so the screen can say
// how the agent is configured without a second endpoint.
type SessionDetail = Session & {
  thinking: string;
  network: boolean;
};

// The one node per kind that a delta appends to and a snapshot replaces.
type BlockNodes = Record<DeltaKind, HTMLElement>;

// One message's block on screen and the nodes inside it. The element is kept as well as the nodes
// because the usage line is appended to the block itself.
type MessageBlock = { element: HTMLElement; nodes: BlockNodes };

// One tool call's block: its name and, once it ends, its output.
type ToolBlock = {
  element: HTMLElement;
  name: HTMLElement;
  // What the tool was asked for, as the frame's summary (HS-033). Shown because "I want to see what the agent
  // ran" is the story's own sentence, and a block that named the tool and hid the command would not answer it.
  arguments_: HTMLElement;
  // The tool's verdict and duration, and the link to its full output (HS-033).
  meta: HTMLElement;
  output: HTMLElement;
  full: HTMLAnchorElement;
  // One anchor per picture the result carried (HS-035). They are links rather than inline images: the bytes
  // are a file, and drawing it on a phone is a screen decision nothing owns yet (ADR-0043).
  images: HTMLElement;
};

// The key the create carries. `crypto.randomUUID` is the platform's own UUID, but it exists only in
// a secure context, and the control listener is plain HTTP unless it sits behind `tailscale serve`.
// The fallback is the same CSPRNG (`getRandomValues` works everywhere), formatted as a v4 UUID by
// hand: six lines rather than a dead button on the dev instance the operator actually opens.
function newKey(): string {
  if (typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

// The list's body: the rows and the machine setting the checkbox starts at (ADR-0017).
type SessionList = {
  sessions: Session[];
  network_default: boolean;
  // The machine's stopped-session window in days (HS-064): the row carries `stopped_at`, this carries the
  // number, and the page adds them. One clock — the browser's — draws the countdown.
  stopped_ttl_days: number;
};

// The payload of a `session.status` event (ADR-0016). `from === to` is a provisioning step report.
type StatusFrame = {
  from: SessionState;
  to: SessionState;
  step: Step | null;
  reason: string | null;
};

// The six states are `SessionState` in `transcript.ts`: one spelling, shared by the list, the screen
// and the stream.

// A response or the ADR-0008 envelope that refused it, so one path renders either.
type Answer<T> =
  | { ok: true; value: T }
  | { ok: false; code: string; message: string };

// The app shell's one node, and the only element every render starts from.
const app = document.querySelector<HTMLElement>("#app");

// The open stream per row, and every open stream: a row that leaves a live state lets go of its
// own, and the window closing lets go of them all.
const rowSources = new WeakMap<HTMLLIElement, EventSource>();
const openSources = new Set<EventSource>();

function make<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text = "",
): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  if (className !== "") {
    element.className = className;
  }
  element.textContent = text;
  return element;
}

function textInput(name: string, value: string): HTMLInputElement {
  const input = document.createElement("input");
  input.type = "text";
  input.name = name;
  input.value = value;
  input.autocomplete = "off";
  return input;
}

// One labelled control. `HTMLElement` rather than `HTMLInputElement` since HS-074: the create form's machine is a
// `<select>`, and a field helper that only took an input would have had the picker drawn outside its own label.
function field(label: string, input: HTMLElement): HTMLLabelElement {
  const element = make("label", "field");
  element.append(make("span", "field-name", label), input);
  return element;
}

// "3 min ago", coarse on purpose, is `transcript.ts`'s `relativeTime`: the Computers screen's last-check line
// asked for it too (HS-071), and a sentence written twice is the pair that drifts.

// One row: the project and the state with its reason, then the id, the model and the last
// activity, then the provisioning step (while starting) or what the stop means (stopped).
// Every node is built with `createElement` and filled with `textContent`: the payload carries
// operator- and agent-controlled text and none of it is parsed as markup.
//
// `host` is the machine that owns the row (HS-071): it is the row's link's first segment, the machine every request
// for this row is made against, and the stream's own path. A row is the *peer's* row, forwarded byte for byte
// (ADR-0074), so the machine it lives on is the page's fact and not a column of the answer.
function sessionRow(session: Session, ttlDays: number, host: string): HTMLLIElement {
  const row = make("li", "session");
  row.dataset.session = session.id;
  row.dataset.host = host;

  const head = document.createElement("a");
  head.className = "session-head session-link";
  head.href = sessionHref(host, session.id);
  head.append(
    make("span", "project", session.project),
    make("span", "state", stateText(session.status, session.status_reason)),
  );

  const meta = make("div", "session-meta");
  meta.append(
    make("span", "id", session.id),
    make("span", "model", session.model),
    make("span", "activity", `active ${relativeTime(session.last_active_at, secondsNow())}`),
  );

  const step = make("span", "step");
  // What a stopped row says: the sentence its reason earns (an eviction lost nothing, and the badge alone would
  // read like a loss — HS-020) and the days it has left before the daily job reaches it (HS-064). One span,
  // because both are "what this row's state means", and one function, because the frame path writes it too.
  const detail = stoppedDetail(
    session.status,
    session.status_reason,
    expiryText(session.stopped_at, ttlDays, secondsNow()),
  );
  const retention = make("span", "retention", detail);
  retention.title = detail;
  retention.hidden = session.status !== "stopped";
  row.append(head, meta, step, retention);
  syncStream(row, session.status);
  return row;
}

// The machine's stopped-session window in days, as the list carried it (HS-064).
//
// Read from the DOM rather than kept in a module variable, because `main.ts` keeps no state beyond the DOM (the
// same rule that makes a row's id a `data-` attribute): the list element carries the number, and every reader —
// a row built at load time, a row a create prepends, a row a frame patches — asks the same element. `0` when the
// list is not there or the answer carried nothing, and `expiryText` says nothing for a non-positive window.
function stoppedTtlDays(list: HTMLElement | null): number {
  const raw = list?.dataset.stoppedTtlDays;
  if (raw === undefined) {
    return 0;
  }
  const days = Number(raw);
  return Number.isFinite(days) ? days : 0;
}

function secondsNow(): number {
  return Math.floor(Date.now() / 1000);
}

// One source exactly while the row is live, and none otherwise: the row that comes back to a live
// state (HS-018's resume) opens one again. The server replays a stream's history first — the newest
// messages, since HS-061 — so a frame applied later always overwrites an earlier one and the last
// frame wins.
function syncStream(row: HTMLLIElement, state: SessionState): void {
  const open = rowSources.get(row);
  if (LIVE_STATES.has(state)) {
    if (open === undefined) {
      openSource(row);
    }
    return;
  }
  if (open !== undefined) {
    open.close();
    rowSources.delete(row);
    openSources.delete(open);
  }
}

function openSource(row: HTMLLIElement): void {
  const id = row.dataset.session;
  const host = row.dataset.host;
  if (id === undefined || host === undefined) {
    return;
  }
  // The create's answer carries `events` and a list element does not; both spell this path, so one
  // rule opens a stream for a row however it arrived. The row's own machine is where the stream is (HS-071):
  // a remote row's frames come through the hub's forwarding route, which relays the peer's stream event for
  // event (ADR-0074).
  const source = new EventSource(streamPath(host, id));
  rowSources.set(row, source);
  openSources.add(source);
  source.addEventListener("session.status", (event) => {
    const frame = parseStatusFrame((event as MessageEvent<string>).data);
    if (frame === null) {
      return;
    }
    applyStatus(row, frame);
    syncStream(row, frame.to);
  });
}

function parseStatusFrame(data: string): StatusFrame | null {
  try {
    return JSON.parse(data) as StatusFrame;
  } catch {
    return null;
  }
}

// Patch one row from one frame, in arrival order. A step report (`from === to`) names the step now
// running and changes nothing else; a transition is the new state, its reason, and the end of the
// step line.
function applyStatus(row: HTMLLIElement, frame: StatusFrame): void {
  const step = row.querySelector<HTMLElement>(".step");
  if (frame.from === frame.to) {
    if (step !== null && frame.step !== null) {
      step.textContent = `${frame.step}…`;
    }
    return;
  }
  const state = row.querySelector<HTMLElement>(".state");
  if (state !== null) {
    state.textContent = stateText(frame.to, frame.reason);
  }
  if (step !== null) {
    step.textContent = "";
  }
  const retention = row.querySelector<HTMLElement>(".retention");
  if (retention !== null) {
    // The frame is the only path by which a row *becomes* stopped while the operator is looking at
    // it — an eviction lands exactly this way — so the detail line is written here too, not only
    // where the row was built (HS-020). One function, so the two paths cannot say different things.
    //
    // A frame carries no `stopped_at`: the transition that produced it just wrote the clock, so this
    // instant *is* the row's stop as far as the page can tell, and the countdown starts from a number no
    // fresher answer could improve on (HS-064).
    //
    // Two numbers that happen to be equal, named apart: the transition just wrote `stopped_at`, so the stop
    // instant is the frame's arrival, while `now` is what the countdown is measured against.
    const stoppedAt = secondsNow();
    const now = stoppedAt;
    const expires =
      frame.to === "stopped"
        ? expiryText(stoppedAt, stoppedTtlDays(row.closest(".session-list")), now)
        : null;
    const detail = stoppedDetail(frame.to, frame.reason, expires);
    retention.hidden = frame.to !== "stopped";
    retention.textContent = detail;
    retention.title = detail;
  }
}

// One machine's sessions, in the payload the sessions list answers with: a group is one machine's answer, the
// refusal that stood in for it, or `null` while its answer is still in flight (HS-071). The `host` is the machine's
// registry name, whose own answer carries the stopped window the rows count down against — the **peer's**
// `stopped_ttl_days` and not this machine's, which is what makes a countdown a fact about the machine that owns the
// row.
type SessionGroup = {
  host: string;
  answer: Answer<SessionList> | null;
};

// The list, and the one way a machine's section is filled once its answer arrives.
//
// The groups are drawn **before** the reads land, each with a word saying it is reading, and each section is filled
// by its own answer: the reads run in parallel and a machine that is away answers at the hop's bound (ADR-0074's
// 5 s, plus the connect), so waiting for all of them would be a blank home screen for the slowest machine — where the
// Computers screen's cards are right to degrade one at a time (ADR-0076 D1). One writer per section: `fillGroup` is
// called once for each host, from whichever answer arrives.
type SessionsList = { element: HTMLElement; fill: (host: string, answer: Answer<SessionList>) => void };

function sessionList(groups: SessionGroup[], note: string | null): SessionsList {
  const section = make("section", "sessions");
  section.append(make("h2", "", "Sessions"));
  const bodies = new Map<string, HTMLElement>();
  for (const group of groups) {
    const machine = make("section", "machine-group");
    machine.dataset.host = group.host;
    const body = make("div", "machine-body");
    machine.append(make("h3", "machine-name", hostLabel(group.host)), body);
    bodies.set(group.host, body);
    section.append(machine);
    if (group.answer === null) {
      body.append(make("p", "waiting", GROUP_WAITING));
    } else {
      fillGroup(body, group.host, group.answer);
    }
  }
  if (note !== null) {
    // A machine that forwards nothing cannot read another machine's rows, and the seed is copied to every machine —
    // so the screen says why those machines are missing rather than drawing a section per row that would answer
    // `404 not_found` (ADR-0076 D3).
    section.append(make("p", "notice", note));
  }
  return {
    element: section,
    fill: (host, answer) => {
      const body = bodies.get(host);
      if (body !== undefined) {
        fillGroup(body, host, answer);
      }
    },
  };
}

// One machine's body: its rows, or the sentence that stood in for them.
function fillGroup(body: HTMLElement, host: string, answer: Answer<SessionList>): void {
  if (!answer.ok) {
    // The refusal's own sentence, in the place the rows would have been: an empty list would read as a machine with
    // no sessions, which is a claim nothing measured (ADR-0076 D3). The sentence alone and not through `hostFailure`:
    // it already names the machine and what went wrong, and the code in front of it would be a word to look up about
    // a machine that is simply away.
    body.replaceChildren(make("p", "notice", answer.message));
    return;
  }
  const rows = answer.value.sessions;
  const ttlDays = answer.value.stopped_ttl_days;
  const list = make("ul", "session-list");
  // The one place the window enters the page (HS-064): a row's own columns do not carry the machine's setting, and
  // the frame path has no request to ask, so the element is the junction. Since HS-071 there is one such element per
  // machine, and the row's `.closest()` finds its own — which is why the selector is a class and not the single
  // `#session-list` id it was.
  list.dataset.stoppedTtlDays = String(ttlDays);
  list.dataset.host = host;
  if (rows.length === 0) {
    list.append(make("li", "empty", "No sessions yet."));
  }
  for (const session of rows) {
    list.append(sessionRow(session, ttlDays, host));
  }
  body.replaceChildren(list);
}

function addSession(session: Session, host: string): void {
  // A session a create just made belongs to **the machine the form named** (HS-074): the form posts to
  // `/api/hosts/<host>/sessions`, and the answer is that machine's own row, so the row is drawn in that machine's
  // group rather than in this machine's. A group that is not on screen is a row with nowhere to land, and the
  // operator's own reload is the honest answer.
  const list = document.querySelector<HTMLUListElement>(`.session-list[data-host="${host}"]`);
  if (list === null) {
    return;
  }
  // One row per id: a retry can be answered with a session the list already holds, and two rows
  // for one session would mean two streams applying every frame twice.
  const existing = list.querySelector<HTMLElement>(`[data-session="${session.id}"]`);
  if (existing !== null) {
    return;
  }
  list.querySelector<HTMLElement>(".empty")?.remove();
  // Prepended because the repository lists newest activity first and a create is the newest. The window comes
  // off the list's own dataset: a create's answer carries the row and not the machine's setting (HS-064).
  list.prepend(sessionRow(session, stoppedTtlDays(list), host));
}

// The new-session form. `networkDefault` is the machine setting the list carried: the checkbox
// starts where the config says and the operator may still change it for this session.
//
// Since HS-074 the form also says **which machine** the session runs on, and it says so in the request rather than
// in the body: the post goes to `POST /api/hosts/<host>/sessions`, so a hub forwards it to the machine the operator
// picked and a machine that runs no sessions still answers `403 sessions_disabled` for its own path. `targets` is
// `createTargets`' answer — this machine first when it can create, then every reachable peer.
function createSection(networkDefault: boolean, targets: string[], ownName: string): HTMLElement {
  const section = make("section", "create");
  const form = document.createElement("form");
  const project = textInput("project", "holosmith");
  const model = textInput("model", "claude-sonnet-4.5");
  const thinking = textInput("thinking", "high");
  const network = document.createElement("input");
  network.type = "checkbox";
  network.name = "network";
  network.checked = networkDefault;
  const internet = make("label", "checkbox");
  internet.append(network, make("span", "", "Internet access"));
  const button = make("button", "", "Start session");
  button.type = "submit";
  const error = make("p", "error");
  error.hidden = true;

  // The machine, as a control of its own. A `<select>` even when there is one machine: the form must say where the
  // session runs either way, and a form whose label vanishes when a second machine is registered is a form whose
  // answer changed without the operator doing anything.
  const machine = document.createElement("select");
  machine.name = "machine";
  for (const host of targets) {
    const option = document.createElement("option");
    option.value = host;
    option.textContent = host === LOCAL_HOST ? ownLabel(ownName) : hostLabel(host);
    machine.append(option);
  }
  // Before the click, not only after it: the button names the machine the picker holds, so the one thing about this
  // form that can point at the wrong computer is on the button (item 11's "the UI shows which machine it will run
  // on"). A create's answer then puts the row in that machine's own group with its own `hostLabel` badge (HS-071).
  const refreshLabel = (): void => {
    button.textContent = startLabel(machine.value);
  };
  machine.addEventListener("change", refreshLabel);
  refreshLabel();

  // The key of the submission in flight or last refused. A retry has to repeat the same request,
  // so a refusal keeps the key; a 202 means the next press is a new intent, so it is dropped
  // (HS-008's rule, ADR-0017).
  let pendingKey: string | null = null;

  const fail = (code: string, message: string): void => {
    // A project's refusal is a sentence about the repository and the operator reads it as one: the code
    // in front of it would be a word to act on and this is not one (HS-026, `createFailure`).
    error.textContent = createFailure(code, message);
    error.hidden = false;
  };

  const submit = async (key: string): Promise<void> => {
    button.disabled = true;
    error.hidden = true;
    try {
      const target = machine.value;
      const response = await fetch(apiPath(target, "sessions"), {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-holosmith-client": "1",
          "idempotency-key": key,
        },
        body: JSON.stringify({
          project: project.value,
          model: model.value,
          thinking: thinking.value,
          network: network.checked,
        }),
      });
      if (!response.ok) {
        const envelope = (await response.json()) as ErrorEnvelope;
        fail(envelope.code, envelope.message);
        return;
      }
      const created = (await response.json()) as Session;
      // The row is on screen before its stream has said anything: the 202's row is `starting`, and it lands in the
      // group of the machine the request named — the peer's own row, the peer's own group.
      addSession(created, target);
      // Only now is the submission finished: a body that did not parse is a retry of this key, not
      // a new intent, so the key survives it.
      pendingKey = null;
    } catch (failure) {
      fail("unreachable", String(failure));
    } finally {
      button.disabled = false;
    }
  };

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (button.disabled) {
      return;
    }
    if (pendingKey === null) {
      try {
        pendingKey = newKey();
      } catch (failure) {
        // A browser with no CSPRNG cannot make a request the server will accept, and a dead button
        // explains nothing: say which failure it is.
        fail("unreachable", `this page cannot create a key: ${String(failure)}`);
        return;
      }
    }
    void submit(pendingKey);
  });

  form.append(
    field("machine", machine),
    field("project", project),
    field("model", model),
    field("thinking", thinking),
    internet,
    button,
  );
  section.append(make("h2", "", "New session"), form, error);
  return section;
}

// What a machine with nothing to offer draws instead of a form: a hub whose registry has no reachable peer, or a
// machine whose only candidate is itself and a `hub` role. A control whose one choice can only be refused is the
// affordance `createOffered`'s rule exists to withhold, one step out.
function noCreateSection(): HTMLElement {
  const section = make("section", "create");
  section.append(make("h2", "", "New session"), make("p", "empty", NO_CREATE_TARGET));
  return section;
}

// The header every screen shares: the machine's own facts, and the links between the screens. Which links exist is
// `push.ts`'s `navLinks` rule — one per screen other than this one — so a fourth screen is a row in `SCREENS` and
// not a branch here, and `case_pwa` sees the class in the served bundle. The links carry a **class** and no id
// since HS-071: three screens mean two links on every page, and an id two elements share is not an id.
function header(health: Health, navs: NavLink[]): HTMLElement {
  const element = make("header", "machine");
  const nav_ = make("nav", "machine-nav");
  nav_.id = "nav";
  for (const nav of navs) {
    const link = make("a", "nav-link", nav.label);
    link.href = nav.href;
    nav_.append(link);
  }
  element.append(
    make("h1", "", health.name),
    make("p", "meta", `${health.version} · ${health.role} · up ${health.uptime_s}s`),
    nav_,
  );
  return element;
}

// ---- the Computers screen (HS-071, ADR-0076) -------------------------------------------------

// One registry row as `GET /api/hosts` answers it: the struct's field order, with `preview`, `version` and
// `last_error` nullable because a machine that never answered has none of them.
type HostRow = {
  name: string;
  url: string;
  preview: string | null;
  reach: string;
  version: string | null;
  last_error: string | null;
  last_checked_at: number;
};

type HostList = { hosts: HostRow[] };

// The two facts a card cannot read out of the registry: what the machine is running **now** and what it holds
// **now**. Both are one forwarded read each (ADR-0074), and both are fetched together so N machines cost two
// hops each rather than four.
//
// This machine's own health is the answer the page already made, so its card costs one read and not two: asking
// itself `/api/health` over loopback to learn a value it holds would be the round trip that adds nothing.
async function readMachine(
  host: string,
  own: Health,
): Promise<[Answer<Health>, Answer<SessionList>]> {
  const health =
    host === LOCAL_HOST
      ? Promise.resolve<Answer<Health>>({ ok: true, value: own })
      : getJson<Health>(apiPath(host, "health"));
  return Promise.all([health, getJson<SessionList>(apiPath(host, "sessions"))]);
}

// One machine's card: what the registry knows (its name, its origin, its last check, and what that check found)
// and what it answers now (its role, its version, its uptime, and how many sessions it holds).
//
// `row` is `null` for this page's own machine when the registry does not name it — a machine's own row is a row
// like any other, but the page is served by it and does not need one to describe it (ADR-0076 D3).
function machineCard(
  own: Health,
  host: string,
  url: string,
  row: HostRow | null,
  refresh: () => void,
): HTMLLIElement {
  const card = make("li", "computer");
  card.dataset.host = host;

  const head = make("div", "computer-head");
  head.append(
    make("span", "computer-name", hostLabel(host)),
    make("span", "computer-url", url),
  );

  const facts = make("div", "computer-facts");
  const role = make("span", "computer-role");
  const version = make("span", "computer-version");
  const uptime = make("span", "computer-uptime");
  const reach = make("span", "computer-reach", row === null ? "" : reachText(row.reach));
  const checked = make("span", "computer-checked");
  const counts = make("span", "computer-counts", COUNTS_UNKNOWN);
  facts.append(role, version, uptime, reach, checked, counts);
  if (row === null) {
    // A machine the registry does not name has no probe behind it — but it *was* just asked: this page's own health
    // answer is that check (ADR-0076 D1), so `reachable` and "checked just now" are the same words the rows use
    // rather than a second way of saying "the page is being served by it".
    reach.textContent = reachText("reachable");
    checked.textContent = lastCheckText(secondsNow(), secondsNow());
  } else {
    checked.textContent = lastCheckText(row.last_checked_at, secondsNow());
    // The probe's own sentence, on hover: the row's `reach` is as old as the boot (ADR-0073), and the fresh answer
    // is the read below. A title is where a stale fact belongs — beside the word, not instead of the current one.
    if (row.last_error !== null) {
      reach.title = row.last_error;
    }
  }

  // The one sentence area: the skew about a version that differs, or the reason a read was refused. One area
  // because they are one question — "is there something about this machine I have to act on" — and two lines of
  // refusal for one unreachable machine is noise (the health hop and the sessions hop fail together).
  const sentence = make("p", "computer-sentence");
  sentence.hidden = true;
  const say = (text: string | null): void => {
    sentence.textContent = text ?? "";
    sentence.hidden = text === null;
  };

  const section = document.createDocumentFragment();
  section.append(head, facts, sentence);

  // Removal is a request to **this** machine's registry, never a forwarded one: a registry is each machine's own
  // and the hub's route refuses to relay `hosts/…` (ADR-0074). It exists only when there is a row to delete.
  if (row !== null) {
    const start = make("button", "secondary", HOST_REMOVE_LABEL);
    start.type = "button";
    const confirm = make("div", "confirm");
    confirm.hidden = true;
    const question = make("p", "question", hostRemoveQuestion(host));
    const yes = make("button", "danger", HOST_REMOVE_LABEL);
    yes.type = "button";
    const cancel = make("button", "secondary", "Cancel");
    cancel.type = "button";
    // The retry a `409 host_busy` earns: the row names live sessions, and `?force=true` is the operator saying
    // they meant it (`deleteSection`'s `unsaved_work` arm, one registry out).
    const forced = make("button", "danger", HOST_FORCE_LABEL);
    forced.type = "button";
    forced.hidden = true;
    confirm.append(question, yes, cancel);
    const notice = make("p", "notice");
    notice.hidden = true;

    const send = async (force: boolean): Promise<void> => {
      yes.disabled = true;
      forced.disabled = true;
      try {
        const path = apiPath(LOCAL_HOST, `hosts/${encodeURIComponent(host)}`);
        const response = await fetch(`${path}${force ? "?force=true" : ""}`, {
          method: "DELETE",
          headers: { "content-type": "application/json", "x-holosmith-client": "1" },
        });
        if (response.ok) {
          refresh();
          return;
        }
        const envelope = (await response.json()) as ErrorEnvelope;
        // The API's own sentence, verbatim: it is the one HS-072's CLI will show too (ADR-0076 D6).
        notice.textContent = hostFailure(envelope.code, envelope.message);
        notice.hidden = false;
        if (hostForceOffered(envelope.code)) {
          confirm.hidden = true;
          forced.hidden = false;
        }
      } catch (failure) {
        notice.textContent = `unreachable: ${String(failure)}`;
        notice.hidden = false;
      } finally {
        yes.disabled = false;
        forced.disabled = false;
      }
    };

    start.addEventListener("click", () => {
      confirm.hidden = false;
      start.hidden = true;
      notice.hidden = true;
    });
    cancel.addEventListener("click", () => {
      confirm.hidden = true;
      start.hidden = false;
    });
    yes.addEventListener("click", () => {
      void send(false);
    });
    forced.addEventListener("click", () => {
      void send(true);
    });
    section.append(start, confirm, forced, notice);
  }

  card.append(section);

  // This machine's own facts are the ones the page already fetched, so its card is complete before it is drawn —
  // except for its session count, which is one local read.
  if (row === null || host === LOCAL_HOST) {
    role.textContent = own.role;
    version.textContent = own.version;
    uptime.textContent = uptimeText(own.uptime_s);
  }

  void (async () => {
    if (host !== LOCAL_HOST && !forwards(own.role)) {
      // The same fact the sessions list carries: a machine that forwards nothing has no route to a peer's health or
      // sessions, so the card says why instead of drawing two `404 not_found` sentences about an endpoint that does
      // not exist here (ADR-0076 D3). What the *registry* knows — the origin, the reach, the last check — is drawn
      // above, because that is this machine's own row and needs no hop.
      say(forwardingNote(own.role));
      return;
    }
    const [health, sessions] = await readMachine(host, own);
    if (health.ok) {
      role.textContent = health.value.role;
      version.textContent = health.value.version;
      uptime.textContent = uptimeText(health.value.uptime_s);
      // The skew is about the API a call can use, and it is about the machine we just asked — never the registry's
      // boot-time `version`, which a redeploy makes stale (ADR-0076 D5).
      say(skewNote(health.value.version, own.version));
    } else {
      // The machine did not answer: the registry's boot-time version is the only one there is (a redeploy makes it
      // stale, which is why it is the fallback and not the first answer), and the sentence is about this read
      // rather than about a skew nothing measured.
      if (row !== null && row.version !== null) {
        version.textContent = row.version;
      }
      // The hop's `message` alone, not through `hostFailure`: that function keeps a code in front of a sentence the
      // operator has to act on (`bad_body`, `unsupported`), and this sentence already says which machine was asked
      // and what went wrong. A code here would be a word to look up about a machine that is simply away.
      say(health.message);
    }
    if (sessions.ok) {
      counts.textContent = sessionCountsText(sessionCounts(sessions.value.sessions));
    } else if (health.ok) {
      // One sentence per failed machine, and the count says that nothing measured it rather than `0`.
      say(sessions.message);
    }
  })();

  return card;
}

// The screen: this machine first, then every registry row that is not this machine, then the form that registers
// one. `refresh` redraws the whole screen, which is what an add or a remove means — the page holds no state, so a
// fresh answer is the whole of updating it (ADR-0017).
function computersSection(health: Health, rows: HostRow[]): HTMLElement {
  const section = make("section", "computers");
  section.id = "computers";
  section.append(
    make("h2", "", "Computers"),
    make("p", "computers-intro", COMPUTERS_INTRO),
  );

  const list = make("ul", "computer-list");
  list.id = "computer-list";
  const refresh = (): void => {
    void show();
  };
  const local = rows.find((row) => isLocalRow(row.url, location.origin)) ?? null;
  list.append(machineCard(health, LOCAL_HOST, location.origin, local, refresh));
  for (const row of rows) {
    if (isLocalRow(row.url, location.origin)) {
      continue;
    }
    list.append(machineCard(health, row.name, row.url, row, refresh));
  }
  section.append(list);

  // The registration form: the two fields `POST /api/hosts` takes, and its own refusal area. The API owns every rule
  // — the name's alphabet, the URL's host and port, and whether the machine answers as a Holosmith — so this form
  // validates nothing and prints the sentence it is given (ADR-0076 D6).
  const form = document.createElement("form");
  form.id = "computers-add";
  const name = textInput("name", "");
  name.id = "computers-name";
  name.placeholder = "workshop";
  const url = textInput("url", "");
  url.id = "computers-url";
  url.placeholder = "https://workshop.tail1a2b.ts.net";
  const submit = make("button", "", HOST_ADD_LABEL);
  submit.type = "submit";
  submit.id = "computers-submit";
  const error = make("p", "error");
  error.id = "computers-error";
  error.hidden = true;
  form.append(field("name", name), field("url", url), submit);
  form.addEventListener("submit", (event) => {
    // A form with no action is a GET of this path with the fields in the query string: a reload that throws away
    // the screen (HS-014's review). The submit is swallowed whether or not there is anything to send.
    event.preventDefault();
    if (submit.disabled) {
      return;
    }
    submit.disabled = true;
    error.hidden = true;
    void (async () => {
      try {
        const response = await fetch(apiPath(LOCAL_HOST, "hosts"), {
          method: "POST",
          headers: { "content-type": "application/json", "x-holosmith-client": "1" },
          body: JSON.stringify({ name: name.value, url: url.value }),
        });
        if (!response.ok) {
          const envelope = (await response.json()) as ErrorEnvelope;
          error.textContent = hostFailure(envelope.code, envelope.message);
          error.hidden = false;
          return;
        }
        // Registered: the row (and the probe's verdict on it) is an answer the server already has, so the whole
        // screen is redrawn rather than the new card being drawn from a guess.
        refresh();
      } catch (failure) {
        error.textContent = `unreachable: ${String(failure)}`;
        error.hidden = false;
      } finally {
        submit.disabled = false;
      }
    })();
  });
  section.append(make("h2", "", "Add a machine"), form, error);
  return section;
}

// ---- the session screen (HS-014) ----------------------------------------------------------

// One kind's node inside one message: a delta appends a text node to it, a snapshot replaces its
// children. `BlockNodes` holds the pair, keyed by `DeltaKind`; `article[data-message]` is the block
// they both live in.
function kindNode(className: string): HTMLElement {
  const element = make("div", className);
  // An empty kind's node is not shown at all, so a text-only message has no blank "thinking" band.
  element.hidden = true;
  return element;
}

function messageBlock(id: string): MessageBlock {
  const element = make("article", "message");
  element.dataset.message = id;
  const thinking = kindNode("thinking");
  const text = kindNode("text");
  element.append(thinking, text);
  return { element, nodes: { thinking, text } };
}

// One tool call's block: the tool's name, then its output. Both are `textContent` — the payload
// carries agent-controlled text and none of it is parsed as markup.
function toolBlock(id: string): ToolBlock {
  const element = make("article", "tool");
  element.dataset.tool = id;
  const name = make("div", "tool-name");
  // The arguments summary, set by `tool.start` and never replaced: the frame is the whole of what the log
  // kept, so there is nothing a later frame could add.
  const arguments_ = kindNode("tool-arguments");
  // What the tool said about itself and how long it took (HS-033). Both start empty and are filled by the
  // `tool.end` frame; a frame written before those keys existed leaves them empty, so a replayed old row
  // draws exactly what it always did.
  const meta = make("div", "tool-meta");
  const output = kindNode("tool-output");
  // The full output is a file, not a payload: this is the one link that fetches it (`output_ref`, a path
  // the server computed, so it is never text from the agent).
  const full = make("a", "tool-full", "full output");
  full.hidden = true;
  // The pictures the result carried, one link each. Filled by `tool.end` and never replaced afterwards: a
  // call's result is one frame, so there is nothing a later one could add.
  const images = make("div", "tool-images");
  images.hidden = true;
  element.append(name, arguments_, meta, output, full, images);
  return { element, name, arguments_, meta, output, full, images };
}

// Append one piece of output to a kind's node. This is the whole of the 10k-event rule: a text node
// is added and nothing already on screen is read back, so the cost of a delta does not grow with the
// length of the message it lands in (ADR-0018).
function appendPiece(node: HTMLElement, piece: string): void {
  if (piece === "") {
    return;
  }
  node.appendChild(document.createTextNode(piece));
  node.hidden = false;
}

// Replace a kind's whole content, which is what a snapshot means: an absent kind empties the node
// on screen rather than leaving a stale half of it there — and an emptied one is hidden again.
function replaceKind(node: HTMLElement, value: string): void {
  node.replaceChildren();
  node.hidden = value === "";
  appendPiece(node, value);
}

// One change from the transcript model, applied to the DOM. Blocks are found in the map the stream
// fills, never by querying the document, so no frame costs a walk over the transcript. The container is
// a `ParentNode` rather than an element because a page of older history is folded into a detached
// fragment and inserted in one go (HS-061).
function applyChange(
  change: Change,
  container: ParentNode,
  blocks: Map<string, MessageBlock>,
  tools: Map<string, ToolBlock>,
  host: string,
): void {
  if (change.op === "boundary") {
    // A turn's number comes from the `turn.start` events seen so far. A `turn.end` with no start
    // before it — reachable once HS-061 truncates a replay to recent messages — says "a turn settled"
    // rather than "turn 0 settled".
    let label = `turn ${change.turn} settled`;
    if (change.boundary === "start") {
      label = `turn ${change.turn}`;
    } else if (change.turn === 0) {
      label = "a turn settled";
    }
    container.append(make("div", "turn-boundary", label));
    return;
  }
  if (change.op === "message") {
    const block = messageBlock(change.messageId);
    blocks.set(change.messageId, block);
    container.append(block.element);
    return;
  }
  if (change.op === "tool") {
    const block = toolBlock(change.toolId);
    tools.set(change.toolId, block);
    container.append(block.element);
    return;
  }
  // A steer is one block drawn from one frame: no id, nothing a later frame updates, and its text is
  // set once rather than appended to (ADR-0021). The composer's guidance appears where it landed in
  // the turn, which is what "the transcript shows what was delivered and when" means.
  if (change.op === "steer") {
    const block = make("article", "steer");
    block.append(make("div", "steer-label", "You"), make("div", "steer-text", change.text));
    container.append(block);
    return;
  }
  // Something the turn could not do (HS-035): an extension asked a question a session cannot answer, so it was
  // cancelled and the transcript says which dialog and why. One block per frame like a steer — no id, nothing
  // a later frame updates — and every node is `createElement` + `textContent`, because the reason is text the
  // agent's own extension caused.
  if (change.op === "error") {
    const block = make("article", "turn-error");
    block.dataset.method = change.method;
    block.append(
      make("div", "turn-error-label", "Cancelled"),
      make("div", "turn-error-text", change.reason),
    );
    container.append(block);
    return;
  }
  // A machine policy ended the session (HS-063): the per-session disk soft cap is the first, and the block says
  // what the server measured. One block per frame, no id, nothing a later frame updates — and the code is kept
  // as a data attribute rather than shown, because `disk_quota` is a word for a client and the sentence is for
  // the operator (the same split the create form's refusal makes).
  if (change.op === "session-error") {
    const block = make("article", "session-error");
    block.dataset.code = change.code;
    block.append(
      make("div", "session-error-label", "Stopped by the machine"),
      make("div", "session-error-text", change.message),
    );
    container.append(block);
    return;
  }
  if (change.op === "tool-start" || change.op === "tool-end") {
    const block = tools.get(change.toolId);
    if (block === undefined) {
      // The model creates a tool block before it changes one, so this cannot happen on a stream it
      // folded itself.
      return;
    }
    if (change.op === "tool-start") {
      block.name.textContent = change.name;
      block.arguments_.textContent = change.arguments;
      block.arguments_.hidden = change.arguments === "";
      return;
    }
    // Appended when it arrives and never re-rendered: output can be large, and one assignment is
    // the whole of the cost.
    block.output.textContent = change.output;
    block.output.hidden = change.output === "";
    // `ok · 0.4s`, or just the part the frame carried: an older row has neither.
    const parts: string[] = [];
    if (change.status !== null) {
      parts.push(change.status);
    }
    if (change.durationMs !== null) {
      parts.push(`${(change.durationMs / 1000).toFixed(1)}s`);
    }
    block.meta.textContent = parts.join(" · ");
    if (change.outputRef !== null) {
      // The peer computed this path for itself (`/api/sessions/<id>/tool-output/<tool>`), so it is re-rooted at the
      // machine that wrote it: a forwarded frame is the peer's bytes and the hub's own route for that path is a 404
      // (HS-071, ADR-0076 D2).
      block.full.setAttribute("href", hostRef(host, change.outputRef));
      block.full.hidden = false;
    }
    // The pictures: one `<a>` per image, labelled with the block's index and its mime, so an operator can
    // open the bytes a tool returned without the transcript carrying a byte of them. Built with
    // `createElement` + `textContent` like every other node, and the href is the path the *server* computed.
    block.images.replaceChildren(
      ...change.images.map((image) => {
        const link = make("a", "tool-image", `image ${image.index} · ${image.mime}`);
        link.setAttribute("href", hostRef(host, image.outputRef));
        return link;
      }),
    );
    block.images.hidden = change.images.length === 0;
    return;
  }
  const block = blocks.get(change.messageId);
  if (block === undefined) {
    // The model creates a block before it changes one, so this cannot happen on a stream it folded
    // itself; a frame from somewhere else renders nothing rather than a heading with no message.
    return;
  }
  if (change.op === "delta") {
    appendPiece(block.nodes[change.delta], change.text);
    return;
  }
  if (change.op === "usage") {
    block.element.append(make("div", "usage", `in ${change.input} · out ${change.output}`));
    return;
  }
  replaceKind(block.nodes.text, change.text);
  replaceKind(block.nodes.thinking, change.thinking);
}

// The composer: one box of guidance and the three things that can be done with it. Prompting is the
// `idle` row's trigger and steering and aborting are a running turn's, so the buttons exist in one
// state each (`affordances`). All three post for real: the prompt (HS-015), the abort (HS-016) and the
// steer (HS-017), which is the one whose frame the page also draws into the transcript.
type Composer = { element: HTMLElement; update: (state: SessionState) => void };

// `showState` patches the screen's badge from a row the prompt's answer carried, so a 202 says
// `running` before the stream has said anything.
// `host` is the machine the session lives on: every request below is made against it (HS-071), so one closure
// carries it rather than each of the six `fetch` calls spelling the prefix.
function composerSection(
  host: string,
  id: string,
  showState: (state: SessionState, reason: string | null) => void,
): Composer {
  const section = make("section", "composer");
  const form = document.createElement("form");
  const guidance = document.createElement("textarea");
  guidance.name = "guidance";
  guidance.rows = 3;
  guidance.placeholder = "Tell the agent what to do…";
  const label = make("label", "field");
  label.append(make("span", "field-name", "Guidance"), guidance);

  const prompt = make("button", "", "Prompt");
  prompt.type = "submit";
  const steer = make("button", "secondary", "Steer");
  steer.type = "button";
  const abort = make("button", "danger", "Abort");
  abort.type = "button";
  const resume = make("button", "secondary", "Resume");
  resume.type = "button";
  const actions = make("div", "actions");
  actions.append(prompt, steer, abort, resume);

  const notice = make("p", "notice");
  notice.hidden = true;
  const say = (message: string): void => {
    notice.textContent = message;
    notice.hidden = false;
  };

  // The two states the transition table has a `resume` row for, so the two refusals a resume can fix
  // (HS-018). Everything else is the operator's to act on.
  const RESUMABLE = new Set(["session_stopped", "session_error"]);

  // Wait until the row says `idle`, which is what a retried prompt or steer needs after a resume's
  // 202 put the session in `starting`. The stream moves the badge; this poll only bounds the wait, so
  // a session that never comes back leaves the retry unsent instead of looping.
  const waitIdle = async (): Promise<boolean> => {
    for (let attempt = 0; attempt < 40; attempt += 1) {
      const response = await fetch(sessionPath(host, id), { headers: { accept: "application/json" } });
      if (response.ok) {
        const row = (await response.json()) as Session;
        showState(row.status, row.status_reason);
        update(row.status);
        if (row.status === "idle") {
          return true;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    return false;
  };

  // One POST, with the story's resume-and-retry rule: a `session_stopped` or `session_error` refusal
  // resumes the session once, waits for it to be idle, and sends the same request once more. Anything
  // else — and a retry that is refused again — is answered to the caller.
  type PostResult = { envelope: ErrorEnvelope | null; row: Session | null };

  // `action` is the route's own word (`prompt`, `abort`, `steer`, `resume`): `sessionActionPath` is the one spelling of
  // a session's action on the machine that owns it (HS-071).
  const post = async (
    action: string,
    body?: string,
    mayResume = true,
    retried = false,
  ): Promise<PostResult> => {
    const response = await fetch(sessionActionPath(host, id, action), {
      method: "POST",
      headers: { "content-type": "application/json", "x-holosmith-client": "1" },
      body,
    });
    if (response.ok) {
      // Every one of these answers the row it acted on, so the badge can follow the 202 rather than
      // waiting for the stream to catch up (HS-015's rule, kept for all four).
      return { envelope: null, row: (await response.json()) as Session };
    }
    const envelope = (await response.json()) as ErrorEnvelope;
    if (!mayResume || retried || !RESUMABLE.has(envelope.code)) {
      return { envelope, row: null };
    }
    const resumed = await fetch(sessionActionPath(host, id, "resume"), {
      method: "POST",
      headers: { "content-type": "application/json", "x-holosmith-client": "1" },
    });
    if (!resumed.ok) {
      return { envelope: (await resumed.json()) as ErrorEnvelope, row: null };
    }
    if (!(await waitIdle())) {
      // Not an envelope: no API answer carries a code for "the client gave up waiting", and inventing
      // one would put a word in the operator's error matching that no server can ever send. The
      // callers' `catch` already reports this as `unreachable`.
      throw new Error("this session did not come back");
    }
    return post(action, body, mayResume, true);
  };

  // Steering is a real request (HS-017): the 202 means the running turn took the guidance, and the
  // frame it answers with puts the steer's own block in the transcript. The one refusal worth a
  // sentence is `session_idle`: a turn that ended while the operator was typing means the guidance is
  // a prompt, and the Prompt button is already on screen for exactly that state.
  const steerTurn = async (): Promise<void> => {
    steer.disabled = true;
    try {
      const sent = guidance.value;
      const { envelope, row } = await post("steer", JSON.stringify({ guidance: sent }));
      if (envelope !== null) {
        say(
          envelope.code === "session_idle"
            ? "This turn has ended; send it as a prompt instead."
            : `${envelope.code}: ${envelope.message}`,
        );
        return;
      }
      if (row !== null) {
        showState(row.status, row.status_reason);
      }
      if (guidance.value === sent) {
        guidance.value = "";
      }
      notice.hidden = true;
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    } finally {
      steer.disabled = false;
    }
  };
  steer.addEventListener("click", () => {
    void steerTurn();
  });
  // Aborting is a real request (HS-016): the 202 says the ladder accepted it and the turn ends
  // off-request, so the badge and the buttons follow the stream rather than this answer. No body is
  // sent — there is nothing to say — but the content type goes with it, because the guards read it
  // on every request that changes something (ADR-0010).
  const abortTurn = async (): Promise<void> => {
    abort.disabled = true;
    try {
      // `mayResume: false`: a resume ends with the row `idle` and abort needs it `running`, so a
      // retried abort could only answer `session_idle` — after a provisioning run nobody asked for.
      const { envelope, row } = await post("abort", undefined, false);
      if (envelope !== null) {
        say(`${envelope.code}: ${envelope.message}`);
        return;
      }
      if (row !== null) {
        showState(row.status, row.status_reason);
      }
      say("Abort accepted: the turn is stopping.");
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    } finally {
      abort.disabled = false;
    }
  };
  abort.addEventListener("click", () => {
    void abortTurn();
  });

  // Resuming is the one action that brings a session back (HS-018): the 202 promises `starting`, the
  // provisioner walks it back to `idle`, and the badge follows the stream. `mayResume` is false here
  // because this *is* the resume — a refused one has nothing left to retry.
  const resumeSession = async (): Promise<void> => {
    resume.disabled = true;
    try {
      const { envelope, row } = await post("resume", undefined, false);
      if (envelope !== null) {
        say(`${envelope.code}: ${envelope.message}`);
        return;
      }
      if (row !== null) {
        showState(row.status, row.status_reason);
        update(row.status);
      }
      notice.hidden = true;
      if (!(await waitIdle())) {
        say("The resume was accepted, but this session has not come back yet.");
      }
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    } finally {
      resume.disabled = false;
    }
  };
  resume.addEventListener("click", () => {
    void resumeSession();
  });

  // True while the badge on screen offers a prompt, so a submit that arrives while the session is
  // running (a second tap before the frame lands) sends nothing.
  let canPrompt = false;

  const send = async (): Promise<void> => {
    prompt.disabled = true;
    try {
      // The prompt, through the shared POST: a `session_stopped` refusal resumes and retries it once
      // (HS-018). The 202 carries the row, now `running`, so the badge and the buttons follow it and
      // the box is emptied, so the next prompt starts from nothing.
      const sent = guidance.value;
      const { envelope, row } = await post("prompt", JSON.stringify({ prompt: sent }));
      if (envelope !== null) {
        // The ADR-0008 envelope: the code is what the operator acts on, and the message says why.
        say(`${envelope.code}: ${envelope.message}`);
        return;
      }
      if (row !== null) {
        showState(row.status, row.status_reason);
        update(row.status);
      }
      // Only what was sent is cleared: a resume-and-retry can take ten seconds, and anything typed in
      // the meantime is the operator's next prompt, not something to throw away.
      if (guidance.value === sent) {
        guidance.value = "";
      }
      notice.hidden = true;
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    } finally {
      prompt.disabled = false;
    }
  };

  // The form has no action, so its default submit is a GET of the current path with the guidance in
  // the query string: a full reload that discards the transcript and the notice. The form swallows
  // its own submit whether or not there is a prompt to send.
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (canPrompt && !prompt.disabled) {
      void send();
    }
  });

  form.append(label, actions);
  section.append(form, notice);

  // True while the line on screen is the state's own ("nothing can be sent"), so a real notice is
  // not cleared by an update that has an action to offer.
  let stateNotice = false;
  const update = (state: SessionState): void => {
    const shown = affordances(state);
    canPrompt = shown.prompt;
    prompt.hidden = !shown.prompt;
    steer.hidden = !shown.steer;
    abort.hidden = !shown.abort;
    resume.hidden = !shown.resume;
    // A state that offers no action says so once, instead of showing an empty button row.
    if (!shown.prompt && !shown.steer && !shown.abort && !shown.resume) {
      // Only two states have no action now, and each has a real reason rather than a story number.
      const wait = state === "starting" ? "it finishes provisioning" : "it is deleted";
      notice.textContent = `This session is ${state}; nothing can be sent until ${wait}.`;
      notice.hidden = false;
      stateNotice = true;
    } else if (stateNotice) {
      notice.hidden = true;
      stateNotice = false;
    }
  };
  return { element: section, update };
}

// The session's stream, folded into the transcript. The browser owns `Last-Event-ID` and resends the
// last seq it saw (ADR-0014), so a reconnect resumes after what is already on screen and the page
// needs no cursor of its own.
//
// The one cursor it does keep is the truncation frame's (HS-061): a stream opened with no resume point
// carries only the newest `history_window` messages, so the frame names where its replay started and
// the "Load earlier" button walks down from there one page per click. Paging is a button and not
// infinite scroll because the story says so, and because one click is one request.
// The nodes the stream writes into, bundled so the stream's signature says what it needs rather than
// counting it: the transcript's container and its placeholder, the row's state badge, the "Load earlier"
// button, and the two pure rules the page applies after every frame.
type StreamTarget = {
  transcript: HTMLElement;
  placeholder: HTMLElement;
  earlier: HTMLButtonElement;
  // The one way a `session.status` frame patches the screen: the badge, the preview link, the composer's
  // actions and the delete affordance (HS-043 put a fourth reader on it, and a fifth copy of "where a state
  // is written" is what HS-020's blocker was).
  showState: (state: SessionState, reason: string | null) => void;
  showSpend: (spend: Spend) => void;
  // The line that says the machine stopped answering under an open page (HS-074), hidden until it does.
  lost: HTMLElement;
};

function openSessionStream(host: string, id: string, target: StreamTarget): void {
  const { transcript, placeholder, earlier, showState, showSpend, lost } = target;
  const model = new Transcript();
  const blocks = new Map<string, MessageBlock>();
  const tools = new Map<string, ToolBlock>();
  const source = new EventSource(streamPath(host, id));
  openSources.add(source);
  // Where the next older page ends, or `null` while the replay reaches the beginning of the log.
  let cursor: number | null = null;

  source.addEventListener(HISTORY_TRUNCATED, (event) => {
    const frame = parseTruncated((event as MessageEvent<string>).data);
    if (frame === null) {
      return;
    }
    // Before any transcript frame, which is the order the server sends them in: the replay's first turn
    // is `turns_before` turns into the log, not turn 1.
    model.setTurnBase(frame.turnsBefore);
    cursor = frame.beforeSeq;
    earlier.hidden = false;
  });

  // One page per click, folded through a model of its own and inserted in front of everything on
  // screen. A page is strictly older than the cursor the stream was opened at, so its blocks are new
  // ids and no live block is touched — and the fold happens in a detached fragment, so the document is
  // written once.
  earlier.addEventListener("click", () => {
    if (cursor === null) {
      return;
    }
    const before = cursor;
    earlier.disabled = true;
    void (async () => {
      try {
        const answer = await getJson<HistoryPage>(historyPath(host, id, before, HISTORY_PAGE_LIMIT));
        if (!answer.ok) {
          // The button stays, and stays enabled: a page that failed is worth another try, and the
          // cursor has not moved.
          return;
        }
        const older = new Transcript();
        older.setTurnBase(answer.value.turns_before);
        const fragment = document.createDocumentFragment();
        const olderBlocks = new Map<string, MessageBlock>();
        const olderTools = new Map<string, ToolBlock>();
        for (const stored of answer.value.events) {
          const frame = parseFrame(stored.type, stored.payload);
          if (frame === null) {
            continue;
          }
          for (const change of older.apply(frame)) {
            applyChange(change, fragment, olderBlocks, olderTools, host);
          }
        }
        transcript.prepend(fragment);
        if (placeholder.isConnected) {
          placeholder.remove();
        }
        cursor = answer.value.before_seq;
        earlier.hidden = !answer.value.has_more;
      } finally {
        earlier.disabled = false;
      }
    })();
  });

  // A connection that fails is not a frame: `EventSource` dispatches a plain `Event` named `error` and retries on
  // its own, and a machine that went away leaves the transcript below as the last thing it sent. The sentence says
  // exactly that rather than letting a stale transcript read as live (ADR-0017); the next frame it does receive
  // takes the line away again, because a stream that came back is the machine answering.
  source.addEventListener("error", (event) => {
    if (!(event instanceof MessageEvent)) {
      lost.textContent = STREAM_LOST;
      lost.hidden = false;
    }
  });
  source.addEventListener("open", () => {
    lost.hidden = true;
  });

  for (const eventType of TRANSCRIPT_EVENTS) {
    source.addEventListener(eventType, (event) => {
      // `error` is the one name the platform uses as well as the server: an `EventSource` dispatches a plain
      // `Event` of that type when its own connection fails, and that is not a transcript frame (HS-063's kind is
      // a stored event, and a stored event always arrives as a `MessageEvent`). One guard here rather than a
      // special case per kind, because the next server-chosen name may collide too.
      if (!(event instanceof MessageEvent)) {
        return;
      }
      const frame = parseFrame(eventType, event.data as string);
      if (frame === null) {
        return;
      }
      if (placeholder.isConnected) {
        placeholder.remove();
      }
      // A frame is the machine answering, whatever the last connection error said.
      lost.hidden = true;
      for (const change of model.apply(frame)) {
        applyChange(change, transcript, blocks, tools, host);
      }
      // One pure rule over what the stream has said about cost, applied after every frame: the
      // sentence is the same one the screen builds with (HS-023).
      showSpend(model.spend());
    });
  }
  source.addEventListener("session.status", (event) => {
    const frame = parseStatusFrame((event as MessageEvent<string>).data);
    if (frame === null) {
      return;
    }
    if (frame.from !== frame.to) {
      showState(frame.to, frame.reason);
    }
  });
}

// The Changes sheet (HS-058, spec item 12's UI): item 12's screen, folded away behind one button.
//
// `deleteSection` is the shape — a section that draws itself from an answer and is patched by the one state
// callback the screen already has (ADR-0023's precedent) — with three differences the story adds: it re-reads
// when the row's state changes (a turn ending is what moves a branch), it has three actions rather than one, and
// every sentence it shows is a rule in `transcript.ts`, because `npm test` is the only gate that can see one.
//
// Two closure variables hold what the server said — the last answer and the last row state — so the disabled
// reason and the pickers are derived from the answer the body rendered rather than read back out of the DOM. They
// are the same class of value as the composer's `canPrompt`: derived from the last update, with no lifetime of
// their own (ADR-0017).
type ChangesSheet = {
  element: HTMLElement;
  update: (state: SessionState) => void;
  // A `session.status` transition happened, so the branch may have moved: re-read if the sheet is open.
  refresh: () => void;
};

// The headers every mutating request carries (the guards read the content type as well).
const JSON_HEADERS = { "content-type": "application/json", "x-holosmith-client": "1" };

// A refusal read from a body that may not be JSON at all — the diff route answers `text/plain` when it answers.
function envelopeOf(text: string): ErrorEnvelope | null {
  try {
    const body = JSON.parse(text) as Partial<ErrorEnvelope>;
    return typeof body.code === "string" && typeof body.message === "string"
      ? { code: body.code, message: body.message }
      : null;
  } catch {
    return null;
  }
}

// One refusal as the notice area renders it, whichever shape the body has: an envelope, or the plain text a route
// that failed before it could answer JSON left behind. Four requests below need exactly this, which is why it is a
// function rather than four copies of the same three lines.
function refusalLine(status: number, text: string): string {
  const envelope = envelopeOf(text);
  return envelope === null ? sheetFailure(String(status), text) : sheetFailure(envelope.code, envelope.message);
}

function changesSection(
  host: string,
  id: string,
  initial: SessionState,
  showState: (state: SessionState, reason: string | null) => void,
): ChangesSheet {
  const section = make("section", "changes");
  section.id = "changes";
  const toggle = make("button", "changes-toggle", "Changes");
  toggle.type = "button";
  toggle.id = "changes-toggle";

  const body = make("div", "changes-body");
  body.id = "changes-body";
  body.hidden = true;

  const notice = make("p", "notice");
  notice.id = "changes-notice";
  notice.hidden = true;

  const counts = make("p", "changes-counts");
  counts.id = "changes-counts";
  // ADR-0066 D11's sentence, shown beside the counts whenever the diff touches CI — and again in the confirm
  // step, from the same constant, because the confirm is what the operator agrees to.
  const warning = make("p", "warning");
  warning.id = "changes-warning";
  warning.hidden = true;

  const files = make("ul", "changes-files");
  files.id = "changes-files";
  const diff = make("pre", "changes-diff");
  diff.id = "changes-diff";
  diff.hidden = true;

  const targetField = make("label", "field");
  const target = document.createElement("select");
  target.id = "changes-target";
  targetField.append(make("span", "field-name", "Target"), target);
  const strategyField = make("label", "field");
  const strategy = document.createElement("select");
  strategy.id = "changes-strategy";
  for (const word of STRATEGIES) {
    const option = document.createElement("option");
    option.value = word;
    option.textContent = word;
    strategy.append(option);
  }
  strategy.value = STRATEGIES[0];
  strategyField.append(make("span", "field-name", "Strategy"), strategy);

  const merge = make("button", "", "Merge");
  merge.type = "button";
  merge.id = "changes-merge";
  // The disabled reason, in the same notice area every other answer on the page uses.
  const reason = make("p", "notice");
  reason.id = "changes-reason";
  reason.hidden = true;
  // The shortcut a `409 dirty_worktree` earns: the prompt ADR-0065 D4 named, sent to the running agent.
  const commit = make("button", "secondary", COMMIT_LABEL);
  commit.type = "button";
  commit.id = "changes-commit";
  commit.hidden = true;

  // Rule 3's state: either the read says the merge conflicts (its reason names the paths) or a `409 conflict`
  // came back with the paths in its additive key. One area, because they are one state.
  const conflict = make("div", "changes-conflict");
  conflict.id = "changes-conflict";
  conflict.hidden = true;
  const conflictText = make("p", "question");
  conflictText.id = "changes-conflict-text";
  const conflictPaths = make("p", "notice");
  conflictPaths.id = "changes-conflict-paths";
  const resolve = make("button", "secondary", RESOLVE_LABEL);
  resolve.type = "button";
  resolve.id = "changes-resolve";
  conflict.append(conflictText, conflictPaths, resolve);

  // The publish half: a button only when a remote is configured, a remote picker when several are, and a confirm
  // step whose paragraph carries the CI warning (ADR-0066 D11). `confirm: true` is the request's own opt-in.
  const publish = make("div", "changes-publish");
  publish.id = "changes-publish";
  publish.hidden = true;
  const publishStart = make("button", "", "Publish");
  publishStart.type = "button";
  publishStart.id = "changes-publish-start";
  const confirm = make("div", "confirm");
  confirm.id = "changes-confirm";
  confirm.hidden = true;
  const question = make("p", "question");
  question.id = "changes-question";
  const remoteField = make("label", "field");
  const remote = document.createElement("select");
  remote.id = "changes-remote";
  remoteField.append(make("span", "field-name", "Remote"), remote);
  const yes = make("button", "", "Publish");
  yes.type = "button";
  yes.id = "changes-publish-yes";
  const no = make("button", "secondary", "Cancel");
  no.type = "button";
  no.id = "changes-publish-cancel";
  confirm.append(question, remoteField, yes, no);
  publish.append(publishStart, confirm);

  const actions = make("div", "changes-actions");
  actions.append(targetField, strategyField, merge, reason, commit);

  body.append(notice, counts, warning, files, diff, actions, conflict, publish);
  section.append(toggle, body);

  // The last answer, the last row state, and the three transient facts the body's own actions set. All of them
  // are "what the server said last", never a cache of the repository.
  let last: Changes | null = null;
  let state = initial;
  let selected: string | null = null;
  let opened = false;
  // True while one of the sheet's own requests is in flight, so a second tap sends nothing.
  let busy = false;
  // A `409 conflict`'s two facts, kept only until the next read says what is true now.
  let conflictMessage: string | null = null;
  let conflicts: string[] | null = null;

  const say = (message: string): void => {
    notice.textContent = message;
    notice.hidden = false;
  };
  const clearNotice = (): void => {
    notice.hidden = true;
  };

  // One select's options, rebuilt only when the list changed: a re-render that replaced them would move the
  // operator's own pick back to the preselection under their finger.
  const fillOptions = (select: HTMLSelectElement, options: string[]): void => {
    const have = Array.from(select.options, (option) => option.value);
    if (have.length !== options.length || have.some((value, index) => value !== options[index])) {
      select.replaceChildren();
      for (const value of options) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = value;
        select.append(option);
      }
    }
  };

  const render = (): void => {
    const answer = last;
    // Every shape the body can take is decided here, from `answer`, `state` and `busy`: an element that is not
    // re-decided keeps last render's state, which is exactly how a button stays enabled after its refusal.
    merge.disabled = true;
    reason.hidden = true;
    commit.disabled = busy;
    target.disabled = true;
    conflict.hidden = true;
    publish.hidden = true;
    if (answer === null) {
      counts.textContent = "";
      warning.hidden = true;
      files.replaceChildren();
      diff.hidden = true;
      return;
    }
    counts.textContent = countsLine(answer);
    const ci = ciWarning(answer.touches_ci);
    warning.textContent = ci ?? "";
    warning.hidden = ci === null;

    files.replaceChildren();
    if (answer.files.length === 0) {
      files.append(make("li", "empty", NOTHING_COMMITTED));
    }
    for (const file of answer.files) {
      const item = make("li", "change");
      const button = make("button", "change-file", fileLine(file));
      button.type = "button";
      button.dataset.path = file.path;
      if (file.path === selected) {
        button.classList.add("selected");
      }
      button.addEventListener("click", () => {
        selected = file.path;
        void loadDiff(file.path);
        render();
      });
      item.append(button);
      files.append(item);
    }

    const targets = targetOptions(answer);
    fillOptions(target, targets);
    const chosen = preselectedTarget(answer);
    if (!targets.includes(target.value) && chosen !== null) {
      target.value = chosen;
    }
    target.disabled = busy || targets.length === 0;
    strategy.disabled = busy;

    const blocked = mergeBlocked(state, answer);
    merge.disabled = busy || blocked !== null;
    if (blocked !== null) {
      reason.textContent = blocked;
      reason.hidden = false;
    }
    commit.hidden = blocked !== DIRTY_MERGE_REASON;

    // The conflict area is drawn from the read or from the last `409 conflict`. `render()` never *clears* the
    // refusal: a render also happens on a state frame and in a request's `finally`, where the answer on screen is
    // still the one from before the tap — and a stale `mergeable: true` would then wipe the paths the refusal just
    // named. A fresh answer clears it in `load()`, because the agent's commit is what a fixed conflict looks like.
    const paths = conflictLine(conflicts);
    const said =
      conflictMessage ?? (answer.mergeable === false ? answer.mergeable_reason : null);
    conflict.hidden = said === null && paths === null;
    conflictText.textContent = said ?? "";
    conflictText.hidden = said === null;
    conflictPaths.textContent = paths ?? "";
    conflictPaths.hidden = paths === null;
    resolve.disabled = busy;

    publish.hidden = !publishVisible(answer);
    if (publish.hidden) {
      confirm.hidden = true;
      publishStart.hidden = false;
    } else {
      fillOptions(remote, answer.choices.remotes);
      if (!answer.choices.remotes.includes(remote.value)) {
        remote.value = answer.choices.remotes[0] ?? "";
      }
      publishStart.disabled = busy;
      yes.disabled = busy;
      remote.disabled = busy;
      if (!confirm.hidden) {
        question.textContent = publishQuestion(answer.branch, remote.value, answer.touches_ci);
      }
    }
  };

  const loadDiff = async (path: string): Promise<void> => {
    diff.textContent = "";
    diff.hidden = true;
    try {
      const response = await fetch(diffPath(host, id, path), { headers: { accept: "text/plain" } });
      const text = await response.text();
      if (!response.ok) {
        say(refusalLine(response.status, text));
        return;
      }
      // The body as the route wrote it: a patch, with the cap's marker line already inside it. Never `innerHTML`
      // with server data, and never re-measured — the marker names what git printed.
      diff.textContent = text;
      diff.hidden = false;
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    }
  };

  const load = async (): Promise<void> => {
    const answer = await getJson<Changes>(sessionActionPath(host, id, "changes"));
    if (!answer.ok) {
      last = null;
      render();
      say(sheetFailure(answer.code, answer.message));
      return;
    }
    // A new answer is the authority, and a clean one clears a refusal's words: the agent's commit is what a fixed
    // conflict looks like, and this is the only place that can tell.
    if (answer.value.mergeable === true) {
      conflicts = null;
      conflictMessage = null;
    }
    last = answer.value;
    render();
    if (selected === null) {
      return;
    }
    // A branch that moved has a new patch for the same path, and a path that left the list has no patch at all.
    if (answer.value.files.some((file) => file.path === selected)) {
      void loadDiff(selected);
    } else {
      selected = null;
      diff.textContent = "";
      diff.hidden = true;
    }
  };

  // The merge, with its own `fetch`: rule 6's 60 seconds belongs to the hub (HS-069), and a blanket timeout on
  // the shared helper would cut the delete's own `abort_grace` wait (ADR-0065 D8, ADR-0066 D12).
  const sendMerge = async (): Promise<void> => {
    busy = true;
    clearNotice();
    render();
    try {
      const response = await fetch(sessionActionPath(host, id, "merge"), {
        method: "POST",
        headers: JSON_HEADERS,
        body: JSON.stringify({ target: target.value, strategy: strategy.value }),
      });
      const text = await response.text();
      if (!response.ok) {
        // The one refusal with a payload: rule 3's paths travel in the envelope's additive `conflicts` key, and a
        // phone needs the list (ADR-0008's fixed sentence cannot carry it).
        const envelope = envelopeOf(text);
        if (envelope?.code === "conflict") {
          conflictMessage = envelope.message;
          const payload = JSON.parse(text) as { conflicts?: unknown };
          conflicts = Array.isArray(payload.conflicts)
            ? payload.conflicts.filter((path): path is string => typeof path === "string")
            : [];
        }
        say(refusalLine(response.status, text));
        // A refusal is also news about the branch: `dirty_worktree` means the answer on screen is stale, and
        // `session_running` means a turn just started. Re-read so the disabled reason converges on what the
        // refusal named rather than staying a button that would refuse again.
        await load();
        return;
      }
      const merged = JSON.parse(text) as Merged;
      say(mergedNotice(merged));
      conflicts = null;
      conflictMessage = null;
      selected = null;
      diff.textContent = "";
      diff.hidden = true;
      // The target moved, so the counts and the branch's own list have changed: the answer on screen is a
      // pre-merge one and the sentence above says what landed.
      await load();
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    } finally {
      busy = false;
      render();
    }
  };

  const sendPublish = async (): Promise<void> => {
    busy = true;
    clearNotice();
    render();
    try {
      const response = await fetch(sessionActionPath(host, id, "publish"), {
        method: "POST",
        headers: JSON_HEADERS,
        // `confirm` is the request's opt-in, not a second button: the operator's second tap *is* the confirmation.
        body: JSON.stringify({ remote: remote.value, confirm: true }),
      });
      const text = await response.text();
      if (!response.ok) {
        say(refusalLine(response.status, text));
        return;
      }
      say(publishedNotice(JSON.parse(text) as Published));
      confirm.hidden = true;
      publishStart.hidden = false;
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    } finally {
      busy = false;
      render();
    }
  };

  // The two prompts. Neither resumes the session: the composer owns that ladder (HS-018), and a prompt sent to a
  // stopped session answers `409 session_stopped`, which is the sentence an operator needs — resume it first.
  const sendPrompt = async (prompt: string, done: string): Promise<void> => {
    busy = true;
    clearNotice();
    render();
    try {
      const response = await fetch(sessionActionPath(host, id, "prompt"), {
        method: "POST",
        headers: JSON_HEADERS,
        body: JSON.stringify({ prompt }),
      });
      const text = await response.text();
      const envelope = envelopeOf(text);
      if (!response.ok || envelope !== null) {
        say(refusalLine(response.status, text));
        return;
      }
      // The 202 is the row, now `running`: the badge follows it and the merge is disabled by the turn it names.
      const row = JSON.parse(text) as Session;
      showState(row.status, row.status_reason);
      conflicts = null;
      conflictMessage = null;
      say(done);
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    } finally {
      busy = false;
      render();
    }
  };

  toggle.addEventListener("click", () => {
    opened = !opened;
    body.hidden = !opened;
    if (opened) {
      clearNotice();
      void load();
    }
  });
  merge.addEventListener("click", () => {
    void sendMerge();
  });
  commit.addEventListener("click", () => {
    void sendPrompt(COMMIT_PROMPT, COMMIT_ASKED);
  });
  resolve.addEventListener("click", () => {
    void sendPrompt(RESOLVE_PROMPT, RESOLVE_ASKED);
  });
  publishStart.addEventListener("click", () => {
    publishStart.hidden = true;
    confirm.hidden = false;
    if (last !== null) {
      question.textContent = publishQuestion(last.branch, remote.value, last.touches_ci);
    }
  });
  remote.addEventListener("change", () => {
    if (last !== null) {
      question.textContent = publishQuestion(last.branch, remote.value, last.touches_ci);
    }
  });
  no.addEventListener("click", () => {
    confirm.hidden = true;
    publishStart.hidden = false;
  });
  yes.addEventListener("click", () => {
    void sendPublish();
  });

  render();
  return {
    element: section,
    update: (next: SessionState) => {
      state = next;
      render();
    },
    refresh: () => {
      if (opened) {
        void load();
      }
    },
  };
}

// The session screen's destructive affordance (HS-043, spec item 9).
//
// ADR-0023 deferred it because a destructive request needs a confirmation step and a way past a refused
// snapshot; the story's fourth bullet asks the UI to show the recovery command, so the screen owns both.
// The notice after the 202 is the *answer's* `kept` read through `deletedNotice`, so the page shows the
// command the server carried rather than spelling a second one.

type DeleteSection = { element: HTMLElement; update: (state: SessionState) => void };

function deleteSection(
  host: string,
  id: string,
  showState: (state: SessionState, reason: string | null) => void,
): DeleteSection {
  const section = make("section", "danger-zone");
  const start = make("button", "danger", "Delete session");
  start.type = "button";

  const confirm = make("div", "confirm");
  confirm.hidden = true;
  const question = make(
    "p",
    "question",
    "Delete this session? Its container and its worktree are removed; uncommitted work is snapshotted first.",
  );
  const yes = make("button", "danger", "Delete");
  yes.type = "button";
  const cancel = make("button", "secondary", "Cancel");
  cancel.type = "button";
  confirm.append(question, yes, cancel);

  const notice = make("p", "notice");
  notice.hidden = true;
  // The second request a `409 unsaved_work` earns: the envelope's message names `?force=true`, which is a
  // query string rather than a button, so the screen offers the button and this sentence says what it does.
  const forced = make("button", "danger", FORCED_DELETE_LABEL);
  forced.type = "button";
  forced.hidden = true;
  section.append(start, confirm, notice, forced);

  const say = (message: string): void => {
    notice.textContent = message;
    notice.hidden = false;
  };

  // One DELETE, with the two headers every mutating request carries (the guard reads the content type as
  // well) and the spec's one spelling of force. The 202's body is the row plus `kept`; a refusal is
  // ADR-0008's envelope.
  const send = async (force: boolean): Promise<void> => {
    yes.disabled = true;
    forced.disabled = true;
    try {
      const response = await fetch(`${sessionPath(host, id)}${force ? "?force=true" : ""}`, {
        method: "DELETE",
        headers: { "content-type": "application/json", "x-holosmith-client": "1" },
      });
      const body = (await response.json()) as Partial<Session> &
        Partial<ErrorEnvelope> & { kept?: Kept | null };
      if (!response.ok) {
        if (body.code === "unsaved_work") {
          // The question is answered: the same request without force will only earn this 409 again, so the
          // confirm step goes and the one button that can go ahead takes its place.
          say(UNSAVED_WORK_NOTICE);
          confirm.hidden = true;
          forced.hidden = false;
          return;
        }
        say(`${body.code ?? response.status}: ${body.message ?? "the delete did not answer"}`);
        return;
      }
      // Accepted: the row is `deleting`, the affordance has nothing left to ask, and the answer is what the
      // teardown will leave in the repository.
      confirm.hidden = true;
      start.hidden = true;
      forced.hidden = true;
      showState("deleting", null);
      say(deletedNotice(body.kept ?? null));
    } catch (failure) {
      say(`unreachable: ${String(failure)}`);
    } finally {
      yes.disabled = false;
      forced.disabled = false;
    }
  };

  start.addEventListener("click", () => {
    confirm.hidden = false;
    start.hidden = true;
    notice.hidden = true;
    forced.hidden = true;
  });
  cancel.addEventListener("click", () => {
    confirm.hidden = true;
    start.hidden = false;
  });
  yes.addEventListener("click", () => {
    void send(false);
  });
  forced.addEventListener("click", () => {
    void send(true);
  });

  // A session whose delete was already accepted has nothing left to ask about, so the affordance follows
  // the state the badge does rather than a flag of its own.
  const update = (state: SessionState): void => {
    if (state === "deleting") {
      start.hidden = true;
      confirm.hidden = true;
    }
  };

  return { element: section, update };
}

// The session screen: one row's worth of the list, the transcript, and the composer. The state badge
// is drawn from the answer and then patched by `session.status`, so a reconnect has nothing to
// reconcile; the row that is not live still opens a stream, because this is the screen the operator
// asked for by name.
// `host` is the machine the session lives on, from the address (HS-071): the preview link's first segment names
// it, every request the screen makes is made against it, and the meta line says whose session this is — the list's
// grouping is gone on a screen that shows one row.
// What a session's page draws when the machine that owns it did not answer (HS-074).
//
// Not a bare notice: the operator came here from the list, so the screen keeps the way back and the name of the
// machine whose session it is, and says the machine's own sentence — `hostFailure` drops the code of the two
// upstream words, because "upstream_unreachable: workshop could not be reached" reads as a word to look up about a
// machine that is simply away (HS-071's rule, on the one screen that did not have it yet).
//
// Nothing is invented: no cached transcript, no counts, no false `idle`. The one affordance is a reload, which is a
// new read of the same address — the sentence the machine would answer if it came back.
function awayScreen(health: Health, host: string, code: string, message: string): void {
  if (app === null) {
    return;
  }
  const section = make("section", "session-screen");
  section.id = "session-away";
  section.dataset.host = host;

  const back = document.createElement("a");
  back.className = "back";
  back.href = "/";
  back.textContent = "← All sessions";

  const retry = document.createElement("a");
  retry.className = "secondary";
  retry.href = location.pathname;
  retry.textContent = "Try again";

  const head = make("div", "session-head");
  // `state` is the badge's look; `machine-state` says which fact this badge is (a machine's reach, not a session's
  // state) — `stateText`'s slot on the normal screen is the session's, and the two must not read as one vocabulary.
  head.append(
    make("span", "project", hostLabel(host)),
    make("span", "state machine-state", "unreachable"),
  );

  section.append(
    back,
    head,
    make("p", "error", awaySentence(code, message)),
    make("p", "notice", AWAY_DETAIL),
    retry,
  );
  app.replaceChildren(header(health, navLinks(location.pathname)), section);
}

function sessionScreen(health: Health, host: string, session: SessionDetail): void {
  if (app === null) {
    return;
  }
  const back = document.createElement("a");
  back.className = "back";
  back.href = "/";
  back.textContent = "← All sessions";

  const head = make("div", "session-head");
  const badge = make("span", "state", stateText(session.status, session.status_reason));
  head.append(make("span", "project", session.project), badge);

  // What the stream says about the cost of the turns, and nothing else: the totals themselves are
  // HS-034's. The span is drawn by the same function the stream patches it with, so an operator who
  // reloads sees what a live one saw (HS-020's rule about a sentence in two places).
  const budget = make("span", "budget");
  const showSpend = (spend: Spend): void => {
    const notice = budgetNotice(spend);
    budget.textContent = notice ?? "";
    budget.hidden = notice === null;
  };

  // The link to what the agent built, on the preview listener's own origin (HS-041, spec item 3). It is drawn
  // from the state, so a session whose container is gone does not offer a page nobody is serving — and it is
  // patched by the same callback that patches the badge, so a session that stops stops offering it.
  const preview = document.createElement("a");
  preview.className = "preview";
  preview.href = previewUrl(health.preview_origin, host, session.id);
  preview.textContent = "preview ↗";
  preview.rel = "noreferrer";
  preview.target = "_blank";

  const meta = make("div", "session-meta");
  meta.append(
    make("span", "host", hostLabel(host)),
    make("span", "id", session.id),
    make("span", "model", session.model),
    make("span", "level", `thinking ${session.thinking}`),
    make("span", "network", networkText(session.network)),
    make("span", "activity", `active ${relativeTime(session.last_active_at, secondsNow())}`),
    budget,
    preview,
  );
  preview.hidden = !previewShown(session.status, host, health.role);
  showSpend({ settled: 0, usage: false });

  const lost = make("p", "notice");
  lost.id = "stream-lost";
  lost.textContent = STREAM_LOST;
  lost.hidden = true;

  const transcript = make("div", "transcript");
  transcript.id = "transcript";
  const placeholder = make(
    "p",
    "empty",
    "Nothing yet: the agent's text, its thinking, its tool calls and the turn boundaries appear here.",
  );
  transcript.append(placeholder);

  // "Load earlier": hidden until the stream says its replay was truncated, so a short session never
  // shows a button that would do nothing (HS-061).
  const earlier = make("button", "secondary earlier", "Load earlier");
  earlier.type = "button";
  earlier.hidden = true;

  const composer = composerSection(host, session.id, (state, reason) => showState(state, reason));
  const deleted = deleteSection(host, session.id, (state, reason) => showState(state, reason));
  const changes = changesSection(host, session.id, session.status, (state, reason) =>
    showState(state, reason),
  );

  // The one place a state change patches the screen: the badge, the preview link, the composer's actions,
  // the delete affordance and the Changes sheet. Two writers of one state is the blocker HS-020's review
  // found, and HS-043's affordance is the fourth reader and HS-058's sheet the fifth — so it is one function,
  // and `session.status` goes through it too. The sheet is the one reader that *acts* on the transition: a turn
  // ending is what moves the branch, so it re-reads.
  function showState(state: SessionState, reason: string | null): void {
    badge.textContent = stateText(state, reason);
    preview.hidden = !previewShown(state, host, health.role);
    composer.update(state);
    deleted.update(state);
    changes.update(state);
    changes.refresh();
  }
  composer.update(session.status);
  deleted.update(session.status);

  const section = make("section", "session-screen");
  section.id = "session";
  section.dataset.session = session.id;
  section.append(
    back,
    head,
    meta,
    changes.element,
    lost,
    earlier,
    transcript,
    composer.element,
    deleted.element,
  );
  app.replaceChildren(header(health, navLinks(location.pathname)), section);
  openSessionStream(host, session.id, {
    transcript,
    placeholder,
    earlier,
    showState,
    showSpend,
    lost,
  });
}

// ---- the settings screen (HS-067, HS-074; ADR-0072 D2/D6, ADR-0079) --------------------------

// One machine's row: the words `push.ts` decided and the nodes they are drawn into.
type MachineRow = {
  host: string;
  // This machine's application server key, as the last read answered it. `null` is a machine with no pair, and
  // `undefined` is a machine that has not answered yet — two facts a sentence must not confuse (HS-074).
  key: string | null | undefined;
  name: HTMLElement;
  sentence: HTMLElement;
  detail: HTMLElement;
  action: HTMLButtonElement;
  decided: PushState | null;
};

// The machine's notifications: **one row per machine** the app can subscribe on (HS-074, item 11's last bullet —
// "each machine has its own keys, so the web app subscribes on every machine it uses (through the hub)").
//
// Every word is `push.ts`'s, and every row asks that machine's own questions: its `application_server_key` through
// the hop, and whether *this browser* holds a subscription for that key. A machine that did not answer is a
// sentence and no control — both facts a row would show are things only that machine can answer (HS-071's rule
// about `false` being a claim, in a second screen).
//
// The facts are read from the platform each time the screen is (re)drawn rather than kept: a permission the
// operator changed in the browser's settings is a fact this page cannot be told about, and a subscription can be
// rotated by the browser without asking anybody.
function settingsSection(health: Health, hosts: HostRow[], origin: string): HTMLElement {
  const section = make("section", "settings");
  section.id = "settings";

  const perMachine = make("p", "settings-per-machine", PER_MACHINE_SENTENCE);
  perMachine.id = "settings-per-machine";

  const notice = make("p", "notice");
  notice.id = "settings-notice";
  notice.hidden = true;

  const install = make("p", "settings-install");
  install.id = "settings-install";
  install.hidden = true;

  const list = make("ul", "settings-machines");
  list.id = "settings-machines";

  // This machine first, then every registry row that is not this machine's own row — and a machine that forwards
  // nothing has no peer rows at all (its own `/api/hosts/<name>/…` is a `404 not_found`, the sentence the list's
  // `forwardingNote` already draws), so a `server` shows its own row and says nothing about the others.
  const peers = forwards(health.role)
    ? hosts.filter((row) => !isLocalRow(row.url, origin)).map((row) => row.name)
    : [];
  const machines: string[] = [LOCAL_HOST, ...peers];

  const rows: MachineRow[] = machines.map((host, index) => {
    const item = make("li", "settings-machine");
    item.dataset.host = host;
    const name = make("p", "settings-machine-name");
    const sentence = make("p", "settings-state");
    const detail = make("p", "settings-detail");
    detail.hidden = true;
    const action = make("button", "settings-action");
    action.type = "button";
    // The first row keeps the ids this screen has carried since HS-067 (the local machine's), and a **class** is
    // what the rest share: an id two elements carry is not an id (HS-071's `nav-link`). `case_pwa` reads these
    // four ids out of the served bundle, so the local row is where they stay.
    if (index === 0) {
      name.id = "settings-machine";
      sentence.id = "settings-state";
      detail.id = "settings-detail";
      action.id = "settings-action";
    }
    item.append(name, sentence, detail, action);
    list.append(item);
    return { host, key: undefined, name, sentence, detail, action, decided: null };
  });

  // What a machine's row is called: this machine's own name (`ownLabel`) for the local row, and the registry's name for
  // a peer — the string the tap's path is built from in both cases.
  function ownLabelFor(host: string): string {
    return host === LOCAL_HOST ? ownLabel(health.machine_name) : hostLabel(host);
  }

  // Only one of this screen's controls is ever in flight: a second tap while one subscribe is still asking the
  // browser for permission is the second subscribe ADR-0067's lesson is about.
  let busy = false;

  function draw(): void {
    for (const row of rows) {
      const decided = row.decided;
      // Which word the row's own state earns is `push.ts`'s rule, and `null` while nothing has been read: a row that
      // has not asked its machine yet shows its name and `reading…`, never `not subscribed` (the `false`-is-a-claim
      // rule HS-071's `—` keeps, one screen over).
      const state = decided === null ? null : machineSubscription(decided.action);
      row.name.textContent =
        state === null
          ? hostLabel(row.host)
          : machineState(ownLabelFor(row.host), state);
      row.name.dataset.subscribed = String(state === "subscribed");
      row.sentence.textContent = decided?.sentence ?? GROUP_WAITING;
      row.detail.textContent = decided?.detail ?? "";
      row.detail.hidden = decided?.detail == null;
      row.action.textContent = busy ? "Working…" : decided === null ? "" : pushActionLabel(decided.action);
      row.action.hidden = decided === null || !pushActionable(decided.action);
      row.action.disabled = busy;
    }
    // The iOS requirement is item 11's third bullet, and it is stated here rather than in a document. `null` once
    // the app really is installed, where the rows above say so.
    const installText = installSentence({
      installed: installedNow(),
      ios: isIos(navigator.userAgent, navigator.maxTouchPoints),
    });
    install.textContent = installText ?? "";
    install.hidden = installText === null;
  }

  function fail(code: string, message: string): void {
    notice.textContent = `${code}: ${message}`;
    notice.hidden = false;
  }

  // Whether the page is running as the installed app (`display-mode: standalone`, or iOS's own flag).
  function installedNow(): boolean {
    // SAFETY: `navigator.standalone` is iOS Safari's own non-standard flag, which no lib declares. It is
    // `undefined` everywhere else, which the comparison reads as "not installed".
    const standalone = (navigator as unknown as { standalone?: boolean }).standalone === true;
    return standalone || window.matchMedia("(display-mode: standalone)").matches;
  }

  // The worker's registration, or `undefined`. `navigator.serviceWorker.ready` never settles when the registration
  // failed, so the wait is bounded: a settings screen that waits forever is a screen that cannot be acted on, and
  // this page's rule is that a sentence describes its own request (ADR-0008).
  async function registrationFor(): Promise<ServiceWorkerRegistration | undefined> {
    if (!("serviceWorker" in navigator)) {
      return undefined;
    }
    const timeout = new Promise<undefined>((resolve) => {
      window.setTimeout(() => resolve(undefined), 5000);
    });
    return Promise.race([navigator.serviceWorker.ready, timeout]);
  }

  // This browser's subscription for **one machine's key**, or `null`.
  //
  // One service-worker registration can hold one subscription per application server key, and the option is how the
  // browser is asked for the one that belongs to this machine. A browser that refuses the option form (or throws)
  // answers `null` — the safe half again: the row says `not subscribed` about a machine this page could not ask,
  // and the subscribe control is the one that can fix it.
  async function subscriptionFor(key: string | null): Promise<PushSubscription | null> {
    if (key === null || !("serviceWorker" in navigator) || !("PushManager" in window)) {
      return null;
    }
    const registration = await registrationFor();
    if (registration === undefined) {
      return null;
    }
    // SAFETY: `getSubscription` takes the machine's key as `PushSubscriptionOptionsInit`, which is how a
    // registration that holds several subscriptions names the one it is asked about. The library's declaration in
    // this tree's TypeScript still says `getSubscription()`, so the argument is stated here rather than dropped: the
    // call without it answers the browser's *default* subscription, which may belong to another machine.
    const manager = registration.pushManager as PushManager & {
      getSubscription(options?: { applicationServerKey?: string }): Promise<PushSubscription | null>;
    };
    try {
      return await manager.getSubscription({ applicationServerKey: key });
    } catch {
      return null;
    }
  }

  // One machine's facts: what the platform can do, what that machine answered, and what this browser holds.
  async function gather(row: MachineRow): Promise<PushFacts> {
    const supported =
      window.isSecureContext &&
      "serviceWorker" in navigator &&
      "PushManager" in window &&
      "Notification" in window;
    // This machine's own health is the answer the page already made; a peer's is one forwarded read, the same one
    // the Computers screen makes. A machine that did not answer has no key to read, and its refusal is the row's
    // sentence — `hostFailure` drops the code of the two upstream ones, which is the operator-facing word for "away".
    const read =
      row.host === LOCAL_HOST
        ? ({ ok: true, value: health } as const)
        : await getJson<Health>(apiPath(row.host, "health"));
    const away = read.ok ? null : awaySentence(read.code, read.message);
    row.key = read.ok ? read.value.application_server_key : undefined;
    const subscribed =
      supported && read.ok && row.key ? (await subscriptionFor(row.key ?? null)) !== null : false;
    return {
      supported,
      permission: supported ? Notification.permission : "default",
      hasKey: read.ok && row.key !== null && row.key !== undefined,
      subscribed,
      installed: installedNow(),
      away,
    };
  }

  async function drawMachine(row: MachineRow): Promise<void> {
    row.decided = pushState(await gather(row));
    draw();
  }

  async function subscribe(row: MachineRow): Promise<void> {
    notice.hidden = true;
    const key = row.key ?? null;
    if (key === null) {
      return;
    }
    const permission = await Notification.requestPermission();
    if (permission !== "granted") {
      return;
    }
    const registration = await registrationFor();
    if (registration === undefined) {
      fail("no_worker", "this app's service worker is not running, so nothing could be subscribed");
      return;
    }
    const subscription = await registration.pushManager.subscribe({
      userVisibleOnly: true,
      // The string form the API allows: the browser decodes it, so no hand-written base64url decoder exists on
      // either side of this call (ADR-0072 D6).
      applicationServerKey: key,
    });
    const body = subscriptionBody(subscription.toJSON());
    if (body === null) {
      await subscription.unsubscribe();
      fail("bad_subscription", "this browser's subscription carried no p256dh/auth pair");
      return;
    }
    // That machine's own route, through the hub: `local` is this origin's own path, a peer is
    // `/api/hosts/<name>/push/subscribe`, which is HS-069's forwarding route reaching the peer's own
    // `POST /api/push/subscribe`. The row is stored **there**, sealed to **that** machine's pair.
    const response = await fetch(subscriptionRoute(row.host), {
      method: "POST",
      headers: JSON_HEADERS,
      body: JSON.stringify(body),
    });
    if (!response.ok) {
      // The row did not land, so the local subscription is not one this machine can deliver to: undo it rather
      // than leave a screen that says `subscribed` about a subscription no row knows.
      await subscription.unsubscribe();
      const envelope = (await response.json()) as ErrorEnvelope;
      fail(envelope.code, envelope.message);
    }
  }

  // The one delete: **every** machine this app subscribed to (item 11's acceptance, HS-074).
  //
  // One `DELETE` per machine, against each machine's own route with that machine's own endpoint — the endpoints
  // differ, because each subscribe call is the push service minting a subscription for a different application
  // server key. `404` is the answer the operator wanted (the row is already gone) and is the rule the local
  // unsubscribe has kept since HS-067; anything else is that row's own sentence, so a machine that is away says so
  // and the machines that answered are still cleared.
  async function unsubscribeEverywhere(): Promise<void> {
    notice.hidden = true;
    const failures: string[] = [];
    for (const row of rows) {
      const key = row.key ?? null;
      const subscription = await subscriptionFor(key);
      if (subscription === null) {
        continue;
      }
      const query = new URLSearchParams({ endpoint: subscription.endpoint }).toString();
      const response = await fetch(`${subscriptionRoute(row.host)}?${query}`, {
        method: "DELETE",
        // A mutating request carries these even with no body (ADR-0010).
        headers: JSON_HEADERS,
      });
      if (!response.ok && response.status !== 404) {
        const envelope = (await response.json()) as ErrorEnvelope;
        failures.push(`${hostLabel(row.host)}: ${hostFailure(envelope.code, envelope.message)}`);
        continue;
      }
      await subscription.unsubscribe();
    }
    if (failures.length > 0) {
      fail("not_everywhere", failures.join(" · "));
    }
  }

  function act(row: MachineRow): void {
    const decided = row.decided;
    if (busy || decided === null || !pushActionable(decided.action)) {
      return;
    }
    busy = true;
    draw();
    const run = decided.action === "subscribe" ? subscribe(row) : unsubscribeEverywhere();
    run
      .catch((error: unknown) => {
        fail("unreachable", String(error));
      })
      .finally(() => {
        busy = false;
        void Promise.all(rows.map(drawMachine));
      });
  }

  for (const row of rows) {
    row.action.addEventListener("click", () => act(row));
  }

  // The first draw is a name and `reading…` on every row until the platform has answered, so neither the control nor
  // the word on it is offered on a guess.
  draw();
  void Promise.all(rows.map(drawMachine));

  section.append(
    make("h2", "", "Notifications"),
    perMachine,
    list,
    install,
    notice,
  );
  return section;
}

// The sessions list: one group per machine, and the create form only where this machine can create.
//
// `own` is this machine's answer, which is also where the form's `network_default` comes from and the only place a
// create can post to (HS-071). `groups` carries every machine's rows; `health.role` decides the affordance.
function render(
  health: Health,
  own: SessionList,
  groups: SessionGroup[],
  note: string | null,
  targets: string[],
): SessionsList | null {
  if (app === null) {
    return null;
  }
  const list = sessionList(groups, note);
  const sections: HTMLElement[] = [header(health, navLinks(location.pathname)), list.element];
  // The form is drawn when there is somewhere to put a session: this machine (a role that runs sessions) or a peer
  // that said it was reachable. A hub with no reachable peer says why instead of offering a choice that can only be
  // refused (§1.5's question 2, `createOffered`'s rule one machine out).
  if (targets.length > 0) {
    sections.push(createSection(own.network_default, targets, health.machine_name));
  } else {
    sections.push(noCreateSection());
  }
  app.replaceChildren(...sections);
  return list;
}

function renderFailure(code: string, message: string): void {
  if (app === null) {
    return;
  }
  const notice = make("p", "error", `${code}: ${message}`);
  app.replaceChildren(notice);
}

async function getJson<T>(path: string): Promise<Answer<T>> {
  try {
    const response = await fetch(path, { headers: { accept: "application/json" } });
    if (!response.ok) {
      const envelope = (await response.json()) as ErrorEnvelope;
      return { ok: false, code: envelope.code, message: envelope.message };
    }
    return { ok: true, value: (await response.json()) as T };
  } catch (error) {
    return { ok: false, code: "unreachable", message: String(error) };
  }
}

async function show(): Promise<void> {
  const health = await getJson<Health>(apiPath(LOCAL_HOST, "health"));
  if (!health.ok) {
    renderFailure(health.code, health.message);
    return;
  }
  // The path decides the screen. `/sessions/<id>` is read from the address rather than from a
  // router: the server already answers the app shell for every non-`/api/` path (ADR-0007), so a
  // reload of one session's screen lands here with nothing installed (ADR-0017). Since HS-071 the address also
  // names **whose** session it is — `/hosts/<host>/sessions/<id>` — and every request below is made against it.
  const address = sessionIdFromPath(location.pathname);
  if (address !== null) {
    const session = await getJson<SessionDetail>(sessionPath(address.host, address.id));
    if (!session.ok) {
      // A machine that is away is a **state of this session**, not a page that failed to load (HS-074, item 11's
      // "degrades to a clear state per session rather than an empty list"): the hub's own `502 upstream_unreachable`
      // and `504 upstream_timeout` say the machine did not answer, and everything this page could offer —
      // the four composer actions, the delete, the Changes sheet — is a request to that same machine. So the screen
      // says whose session it is and what happened, and offers one thing that can now work: the same read again.
      // Anything else (a `404`, a refusal) keeps the plain notice, because the page has nothing to add to it.
      if (isAwayCode(session.code)) {
        awayScreen(health.value, address.host, session.code, session.message);
        return;
      }
      renderFailure(session.code, session.message);
      return;
    }
    sessionScreen(health.value, address.host, session.value);
    return;
  }
  // `/settings` is the third screen and the machine's own (HS-067, ADR-0072), and since HS-074 it is **one row per
  // machine**: the health this function already made is this machine's row, and the registry is what names the rest,
  // exactly as the Computers screen reads it. A registry that cannot be read is not a reason to draw nothing — this
  // machine's own row is still true, and the notice says the rest are missing rather than absent.
  if (location.pathname === SETTINGS_PATH) {
    const hosts = await getJson<HostList>(apiPath(LOCAL_HOST, "hosts"));
    app?.replaceChildren(
      header(health.value, navLinks(location.pathname)),
      settingsSection(health.value, hosts.ok ? hosts.value.hosts : [], location.origin),
    );
    if (!hosts.ok) {
      const notice = document.getElementById("settings-notice");
      if (notice !== null) {
        notice.textContent = registryMissing(awaySentence(hosts.code, hosts.message));
        notice.hidden = false;
      }
    }
    return;
  }
  // `/computers` is the fourth (HS-071): the machines this one may send a request to, and their state. It reads the
  // registry and then one health and one session list per machine, so it is a screen of its own rather than a
  // section of the list — a machine with no sessions still has a state, and a hub has no sessions at all.
  if (location.pathname === COMPUTERS_PATH) {
    const hosts = await getJson<HostList>(apiPath(LOCAL_HOST, "hosts"));
    if (!hosts.ok) {
      renderFailure(hosts.code, hosts.message);
      return;
    }
    app?.replaceChildren(
      header(health.value, navLinks(location.pathname)),
      computersSection(health.value, hosts.value.hosts),
    );
    return;
  }
  // The list after the health: a machine whose database is not open answers 500 here, and that
  // envelope is what the operator needs to see rather than a page that half drew.
  //
  // One request per machine (HS-071): this machine's own list, then each registry row's own — the peer's rows are the
  // peer's, forwarded byte for byte, so the page gets a `host` for each group rather than adding one to somebody
  // else's row (ADR-0074). A machine that cannot be reached is a group with its refusal's sentence, not a group
  // that vanished (ADR-0076 D3).
  const own = await getJson<SessionList>(apiPath(LOCAL_HOST, "sessions"));
  if (!own.ok) {
    renderFailure(own.code, own.message);
    return;
  }
  const hosts = await getJson<HostList>(apiPath(LOCAL_HOST, "hosts"));
  if (!hosts.ok) {
    renderFailure(hosts.code, hosts.message);
    return;
  }
  const skipped = hosts.value.hosts.filter((row) => !isLocalRow(row.url, location.origin));
  // A machine that forwards nothing cannot read another machine's rows: its own route for one is its `404 not_found`
  // (ADR-0074 registers the forwarding route only where a machine forwards), so those rows are reported as the reason
  // they are missing rather than as a section each (ADR-0076 D3).
  const readable = forwards(health.value.role);
  const note = readable || skipped.length === 0 ? null : forwardingNote(health.value.role);
  const remote = readable ? skipped : [];
  const list = render(
    health.value,
    own.value,
    [{ host: LOCAL_HOST, answer: own }, ...remote.map((row) => ({ host: row.name, answer: null }))],
    note,
    createTargets(health.value.role, hosts.value.hosts, location.origin),
  );
  if (list === null) {
    return;
  }
  // Each machine's group is filled by its own answer, so one machine that is away cannot hold the list back for the
  // hop's whole bound (ADR-0076 D3).
  for (const row of remote) {
    void getJson<SessionList>(apiPath(row.name, "sessions")).then((answer) => {
      list.fill(row.name, answer);
    });
  }
}

// Navigating away ends the streams: the server counts them (HS-066), and a source the page forgets
// would keep one open until the browser decides to drop it.
window.addEventListener("beforeunload", () => {
  for (const source of openSources) {
    source.close();
  }
  openSources.clear();
});

// The service worker, registered from every screen (HS-067, ADR-0072 D4). The manifest is what makes the app
// installable; the worker is what a push arrives at, and it is registered at load rather than from the settings
// screen so that an app opened from a home-screen icon — which lands on `/` or on a session — is already reachable
// when the notification comes. A registration that fails is not a reason to draw nothing: the settings screen reads
// `serviceWorker.supported` and says so, and this page still serves its sessions.
if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("/sw.js").catch((error: unknown) => {
    console.error("the service worker did not register:", error);
  });
}

show().catch((error: unknown) => {
  renderFailure("unreachable", String(error));
});
