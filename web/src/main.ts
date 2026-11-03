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
  TRANSCRIPT_EVENTS,
  Transcript,
  affordances,
  parseFrame,
  sessionIdFromPath,
  streamPath,
  type Change,
  type DeltaKind,
  type SessionState,
} from "./transcript";

type Health = {
  name: string;
  version: string;
  role: string;
  uptime_s: number;
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

const LIVE_STATES: ReadonlySet<SessionState> = new Set<SessionState>([
  "starting",
  "idle",
  "running",
]);

const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

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

function field(label: string, input: HTMLInputElement): HTMLLabelElement {
  const element = make("label", "field");
  element.append(make("span", "field-name", label), input);
  return element;
}

function stateText(state: SessionState, reason: string | null): string {
  return reason === null ? state : `${state} · ${reason}`;
}

// "3 min ago", coarse on purpose. It reads `last_active_at` from the row and never from a frame:
// no frame carries a time, so the text is as fresh as the answer that drew it.
function relativeTime(at: number, now: number): string {
  const elapsed = now - at;
  if (elapsed < MINUTE) {
    return "just now";
  }
  if (elapsed < HOUR) {
    return `${Math.floor(elapsed / MINUTE)} min ago`;
  }
  if (elapsed < DAY) {
    return `${Math.floor(elapsed / HOUR)} h ago`;
  }
  return `${Math.floor(elapsed / DAY)} d ago`;
}

// One row: the project and the state with its reason, then the id, the model and the last
// activity, then the provisioning step (while starting) or the retention placeholder (stopped).
// Every node is built with `createElement` and filled with `textContent`: the payload carries
// operator- and agent-controlled text and none of it is parsed as markup.
function sessionRow(session: Session): HTMLLIElement {
  const row = make("li", "session");
  row.dataset.session = session.id;

  const head = document.createElement("a");
  head.className = "session-head session-link";
  head.href = `/sessions/${session.id}`;
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
  const retention = make("span", "retention", "—");
  retention.title = "Days remaining until auto-delete arrives with HS-064";
  retention.hidden = session.status !== "stopped";

  row.append(head, meta, step, retention);
  syncStream(row, session.status);
  return row;
}

function secondsNow(): number {
  return Math.floor(Date.now() / 1000);
}

// One source exactly while the row is live, and none otherwise: the row that comes back to a live
// state (HS-018's resume) opens one again. The server replays a stream from the beginning, so a
// frame applied later always overwrites an earlier one and the last frame wins.
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
  if (id === undefined) {
    return;
  }
  // The create's answer carries `events` and a list element does not; both spell this path, so one
  // rule opens a stream for a row however it arrived.
  const source = new EventSource(streamPath(id));
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
    retention.hidden = frame.to !== "stopped";
  }
}

function sessionList(sessions: Session[]): HTMLElement {
  const section = make("section", "sessions");
  section.append(make("h2", "", "Sessions"));
  const list = make("ul", "session-list");
  list.id = "session-list";
  if (sessions.length === 0) {
    list.append(make("li", "empty", "No sessions yet."));
  }
  for (const session of sessions) {
    list.append(sessionRow(session));
  }
  section.append(list);
  return section;
}

function addSession(session: Session): void {
  const list = document.querySelector<HTMLUListElement>("#session-list");
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
  // Prepended because the repository lists newest activity first and a create is the newest.
  list.prepend(sessionRow(session));
}

// The new-session form. `networkDefault` is the machine setting the list carried: the checkbox
// starts where the config says and the operator may still change it for this session.
function createSection(networkDefault: boolean): HTMLElement {
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

  // The key of the submission in flight or last refused. A retry has to repeat the same request,
  // so a refusal keeps the key; a 202 means the next press is a new intent, so it is dropped
  // (HS-008's rule, ADR-0017).
  let pendingKey: string | null = null;

  const fail = (code: string, message: string): void => {
    error.textContent = `${code}: ${message}`;
    error.hidden = false;
  };

  const submit = async (key: string): Promise<void> => {
    button.disabled = true;
    error.hidden = true;
    try {
      const response = await fetch("/api/sessions", {
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
      // The row is on screen before its stream has said anything: the 202's row is `starting`.
      addSession(created);
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
    field("project", project),
    field("model", model),
    field("thinking", thinking),
    internet,
    button,
  );
  section.append(make("h2", "", "New session"), form, error);
  return section;
}

function header(health: Health): HTMLElement {
  const element = make("header", "machine");
  element.append(
    make("h1", "", health.name),
    make("p", "meta", `${health.version} · ${health.role} · up ${health.uptime_s}s`),
  );
  return element;
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

function messageBlock(id: string): { element: HTMLElement; nodes: BlockNodes } {
  const element = make("article", "message");
  element.dataset.message = id;
  const thinking = kindNode("thinking");
  const text = kindNode("text");
  element.append(thinking, text);
  return { element, nodes: { thinking, text } };
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
// fills, never by querying the document, so no frame costs a walk over the transcript.
function applyChange(
  change: Change,
  container: HTMLElement,
  blocks: Map<string, BlockNodes>,
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
    blocks.set(change.messageId, block.nodes);
    container.append(block.element);
    return;
  }
  const nodes = blocks.get(change.messageId);
  if (nodes === undefined) {
    // The model creates a block before it changes one, so this cannot happen on a stream it folded
    // itself; a frame from somewhere else renders nothing rather than a heading with no message.
    return;
  }
  if (change.op === "delta") {
    appendPiece(nodes[change.delta], change.text);
    return;
  }
  replaceKind(nodes.text, change.text);
  replaceKind(nodes.thinking, change.thinking);
}

// The composer: one box of guidance and the three things that can be done with it. Prompting is the
// `idle` row's trigger and steering is a running turn's, so the buttons exist in one state each
// (`affordances`), and the box says which story wires the request there is no endpoint for yet:
// HS-015 prompts, HS-017 steers, HS-016 aborts.
type Composer = { element: HTMLElement; update: (state: SessionState) => void };

function composerSection(): Composer {
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
  const actions = make("div", "actions");
  actions.append(prompt, steer, abort);

  const notice = make("p", "notice");
  notice.hidden = true;
  const say = (message: string): void => {
    notice.textContent = message;
    notice.hidden = false;
  };

  // No request is invented: the endpoint a button would call is a later story's, and a button that
  // silently did nothing would be indistinguishable from a broken one.
  prompt.addEventListener("click", () => say("Prompting arrives with HS-015; nothing was sent."));
  steer.addEventListener("click", () => say("Steering arrives with HS-017; nothing was sent."));
  abort.addEventListener("click", () => say("Aborting arrives with HS-016; nothing was sent."));

  // The form has no action, so its default submit is a GET of the current path with the guidance in
  // the query string: a full reload that discards the transcript and the notice. HS-015 inherits a
  // form that submits nowhere rather than a page that forgets what it was showing.
  form.addEventListener("submit", (event) => {
    event.preventDefault();
  });

  form.append(label, actions);
  section.append(form, notice);

  // True while the line on screen is the state's own ("nothing can be sent"), so a real notice is
  // not cleared by an update that has an action to offer.
  let stateNotice = false;
  const update = (state: SessionState): void => {
    const shown = affordances(state);
    prompt.hidden = !shown.prompt;
    steer.hidden = !shown.steer;
    abort.hidden = !shown.abort;
    // A state that offers no action says so once, instead of showing an empty button row.
    if (!shown.prompt && !shown.steer && !shown.abort) {
      notice.textContent = `This session is ${state}; its next action arrives with a later story.`;
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
function openSessionStream(
  id: string,
  transcript: HTMLElement,
  placeholder: HTMLElement,
  badge: HTMLElement,
  updateComposer: (state: SessionState) => void,
): void {
  const model = new Transcript();
  const blocks = new Map<string, BlockNodes>();
  const source = new EventSource(streamPath(id));
  openSources.add(source);
  for (const eventType of TRANSCRIPT_EVENTS) {
    source.addEventListener(eventType, (event) => {
      const frame = parseFrame(eventType, (event as MessageEvent<string>).data);
      if (frame === null) {
        return;
      }
      if (placeholder.isConnected) {
        placeholder.remove();
      }
      for (const change of model.apply(frame)) {
        applyChange(change, transcript, blocks);
      }
    });
  }
  source.addEventListener("session.status", (event) => {
    const frame = parseStatusFrame((event as MessageEvent<string>).data);
    if (frame === null) {
      return;
    }
    if (frame.from !== frame.to) {
      badge.textContent = stateText(frame.to, frame.reason);
      updateComposer(frame.to);
    }
  });
}

// The session screen: one row's worth of the list, the transcript, and the composer. The state badge
// is drawn from the answer and then patched by `session.status`, so a reconnect has nothing to
// reconcile; the row that is not live still opens a stream, because this is the screen the operator
// asked for by name.
function sessionScreen(health: Health, session: SessionDetail): void {
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

  const meta = make("div", "session-meta");
  meta.append(
    make("span", "id", session.id),
    make("span", "model", session.model),
    make("span", "level", `thinking ${session.thinking}`),
    make("span", "network", session.network ? "internet on" : "internet off"),
    make("span", "activity", `active ${relativeTime(session.last_active_at, secondsNow())}`),
  );

  const transcript = make("div", "transcript");
  transcript.id = "transcript";
  const placeholder = make(
    "p",
    "empty",
    "Nothing yet: the agent's text, its thinking and the turn boundaries appear here.",
  );
  transcript.append(placeholder);

  const composer = composerSection();
  composer.update(session.status);

  const section = make("section", "session-screen");
  section.id = "session";
  section.dataset.session = session.id;
  section.append(back, head, meta, transcript, composer.element);
  app.replaceChildren(header(health), section);
  openSessionStream(session.id, transcript, placeholder, badge, composer.update);
}

function sessionPath(id: string): string {
  return `/api/sessions/${encodeURIComponent(id)}`;
}

function render(health: Health, list: SessionList): void {
  if (app === null) {
    return;
  }
  app.replaceChildren(
    header(health),
    sessionList(list.sessions),
    createSection(list.network_default),
  );
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
  const health = await getJson<Health>("/api/health");
  if (!health.ok) {
    renderFailure(health.code, health.message);
    return;
  }
  // The path decides the screen. `/sessions/<id>` is read from the address rather than from a
  // router: the server already answers the app shell for every non-`/api/` path (ADR-0007), so a
  // reload of one session's screen lands here with nothing installed (ADR-0017).
  const id = sessionIdFromPath(location.pathname);
  if (id !== null) {
    const session = await getJson<SessionDetail>(sessionPath(id));
    if (!session.ok) {
      renderFailure(session.code, session.message);
      return;
    }
    sessionScreen(health.value, session.value);
    return;
  }
  // The list after the health: a machine whose database is not open answers 500 here, and that
  // envelope is what the operator needs to see rather than a page that half drew.
  const list = await getJson<SessionList>("/api/sessions");
  if (!list.ok) {
    renderFailure(list.code, list.message);
    return;
  }
  render(health.value, list.value);
}

// Navigating away ends the streams: the server counts them (HS-066), and a source the page forgets
// would keep one open until the browser decides to drop it.
window.addEventListener("beforeunload", () => {
  for (const source of openSources) {
    source.close();
  }
  openSources.clear();
});

show().catch((error: unknown) => {
  renderFailure("unreachable", String(error));
});
