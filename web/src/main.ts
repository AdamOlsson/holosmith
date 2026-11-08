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
  DIRTY_MERGE_REASON,
  FORCED_DELETE_LABEL,
  HISTORY_PAGE_LIMIT,
  HISTORY_TRUNCATED,
  NOTHING_COMMITTED,
  RESOLVE_ASKED,
  RESOLVE_LABEL,
  RESOLVE_PROMPT,
  STRATEGIES,
  TRANSCRIPT_EVENTS,
  DAY,
  Transcript,
  UNSAVED_WORK_NOTICE,
  affordances,
  budgetNotice,
  ciWarning,
  expiryText,
  conflictLine,
  countsLine,
  createFailure,
  deletedNotice,
  diffPath,
  fileLine,
  historyPath,
  mergeBlocked,
  mergedNotice,
  networkText,
  parseFrame,
  preselectedTarget,
  previewOffered,
  previewUrl,
  publishQuestion,
  publishVisible,
  publishedNotice,
  parseTruncated,
  sessionIdFromPath,
  sheetFailure,
  stateText,
  stoppedDetail,
  streamPath,
  targetOptions,
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
  PER_MACHINE_SENTENCE,
  SETTINGS_PATH,
  SUBSCRIBE_PATH,
  installSentence,
  isIos,
  machineState,
  navLink,
  pushActionLabel,
  pushActionable,
  pushState,
  subscriptionBody,
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

const LIVE_STATES: ReadonlySet<SessionState> = new Set<SessionState>([
  "starting",
  "idle",
  "running",
]);

const MINUTE = 60;
const HOUR = 60 * MINUTE;

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
// activity, then the provisioning step (while starting) or what the stop means (stopped).
// Every node is built with `createElement` and filled with `textContent`: the payload carries
// operator- and agent-controlled text and none of it is parsed as markup.
function sessionRow(session: Session, ttlDays: number): HTMLLIElement {
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
        ? expiryText(stoppedAt, stoppedTtlDays(row.closest("#session-list")), now)
        : null;
    const detail = stoppedDetail(frame.to, frame.reason, expires);
    retention.hidden = frame.to !== "stopped";
    retention.textContent = detail;
    retention.title = detail;
  }
}

function sessionList(sessions: Session[], ttlDays: number): HTMLElement {
  const section = make("section", "sessions");
  section.append(make("h2", "", "Sessions"));
  const list = make("ul", "session-list");
  list.id = "session-list";
  // The one place the window enters the page (HS-064): a row's own columns do not carry the machine's setting,
  // and the frame path has no request to ask, so the element is the junction.
  list.dataset.stoppedTtlDays = String(ttlDays);
  if (sessions.length === 0) {
    list.append(make("li", "empty", "No sessions yet."));
  }
  for (const session of sessions) {
    list.append(sessionRow(session, ttlDays));
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
  // Prepended because the repository lists newest activity first and a create is the newest. The window comes
  // off the list's own dataset: a create's answer carries the row and not the machine's setting (HS-064).
  list.prepend(sessionRow(session, stoppedTtlDays(list)));
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
    // A project's refusal is a sentence about the repository and the operator reads it as one: the code
    // in front of it would be a word to act on and this is not one (HS-026, `createFailure`).
    error.textContent = createFailure(code, message);
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

// The header every screen shares: the machine's own facts, and the one link between the screens. The link's
// href and label are `push.ts`'s `navLink` rule, so `/settings` returns to the list and every other path offers
// the settings — one place decides, and `case_pwa` sees the ids in the served bundle.
function header(health: Health, nav: NavLink): HTMLElement {
  const element = make("header", "machine");
  const link = make("a", "nav-link", nav.label);
  link.id = "nav-link";
  link.href = nav.href;
  const nav_ = make("nav", "machine-nav");
  nav_.id = "nav";
  nav_.append(link);
  element.append(
    make("h1", "", health.name),
    make("p", "meta", `${health.version} · ${health.role} · up ${health.uptime_s}s`),
    nav_,
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
      block.full.setAttribute("href", change.outputRef);
      block.full.hidden = false;
    }
    // The pictures: one `<a>` per image, labelled with the block's index and its mime, so an operator can
    // open the bytes a tool returned without the transcript carrying a byte of them. Built with
    // `createElement` + `textContent` like every other node, and the href is the path the *server* computed.
    block.images.replaceChildren(
      ...change.images.map((image) => {
        const link = make("a", "tool-image", `image ${image.index} · ${image.mime}`);
        link.setAttribute("href", image.outputRef);
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
function composerSection(id: string, showState: (state: SessionState, reason: string | null) => void): Composer {
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
      const response = await fetch(sessionPath(id), { headers: { accept: "application/json" } });
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

  const post = async (
    path: string,
    body?: string,
    mayResume = true,
    retried = false,
  ): Promise<PostResult> => {
    const response = await fetch(`${sessionPath(id)}${path}`, {
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
    const resumed = await fetch(`${sessionPath(id)}/resume`, {
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
    return post(path, body, mayResume, true);
  };

  // Steering is a real request (HS-017): the 202 means the running turn took the guidance, and the
  // frame it answers with puts the steer's own block in the transcript. The one refusal worth a
  // sentence is `session_idle`: a turn that ended while the operator was typing means the guidance is
  // a prompt, and the Prompt button is already on screen for exactly that state.
  const steerTurn = async (): Promise<void> => {
    steer.disabled = true;
    try {
      const sent = guidance.value;
      const { envelope, row } = await post("/steer", JSON.stringify({ guidance: sent }));
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
      const { envelope, row } = await post("/abort", undefined, false);
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
      const { envelope, row } = await post("/resume", undefined, false);
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
      const { envelope, row } = await post("/prompt", JSON.stringify({ prompt: sent }));
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
};

function openSessionStream(id: string, target: StreamTarget): void {
  const { transcript, placeholder, earlier, showState, showSpend } = target;
  const model = new Transcript();
  const blocks = new Map<string, MessageBlock>();
  const tools = new Map<string, ToolBlock>();
  const source = new EventSource(streamPath(id));
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
        const answer = await getJson<HistoryPage>(historyPath(id, before, HISTORY_PAGE_LIMIT));
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
            applyChange(change, fragment, olderBlocks, olderTools);
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
      for (const change of model.apply(frame)) {
        applyChange(change, transcript, blocks, tools);
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
      const response = await fetch(diffPath(id, path), { headers: { accept: "text/plain" } });
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
    const answer = await getJson<Changes>(`${sessionPath(id)}/changes`);
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
      const response = await fetch(`${sessionPath(id)}/merge`, {
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
      const response = await fetch(`${sessionPath(id)}/publish`, {
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
      const response = await fetch(`${sessionPath(id)}/prompt`, {
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
      const response = await fetch(`${sessionPath(id)}${force ? "?force=true" : ""}`, {
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
  preview.href = previewUrl(health.preview_origin, session.id);
  preview.textContent = "preview ↗";
  preview.rel = "noreferrer";
  preview.target = "_blank";

  const meta = make("div", "session-meta");
  meta.append(
    make("span", "id", session.id),
    make("span", "model", session.model),
    make("span", "level", `thinking ${session.thinking}`),
    make("span", "network", networkText(session.network)),
    make("span", "activity", `active ${relativeTime(session.last_active_at, secondsNow())}`),
    budget,
    preview,
  );
  preview.hidden = !previewOffered(session.status);
  showSpend({ settled: 0, usage: false });

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

  const composer = composerSection(session.id, (state, reason) => showState(state, reason));
  const deleted = deleteSection(session.id, (state, reason) => showState(state, reason));
  const changes = changesSection(session.id, session.status, (state, reason) =>
    showState(state, reason),
  );

  // The one place a state change patches the screen: the badge, the preview link, the composer's actions,
  // the delete affordance and the Changes sheet. Two writers of one state is the blocker HS-020's review
  // found, and HS-043's affordance is the fourth reader and HS-058's sheet the fifth — so it is one function,
  // and `session.status` goes through it too. The sheet is the one reader that *acts* on the transition: a turn
  // ending is what moves the branch, so it re-reads.
  function showState(state: SessionState, reason: string | null): void {
    badge.textContent = stateText(state, reason);
    preview.hidden = !previewOffered(state);
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
    earlier,
    transcript,
    composer.element,
    deleted.element,
  );
  app.replaceChildren(header(health, navLink(location.pathname)), section);
  openSessionStream(session.id, {
    transcript,
    placeholder,
    earlier,
    showState,
    showSpend,
  });
}

function sessionPath(id: string): string {
  return `/api/sessions/${encodeURIComponent(id)}`;
}

// ---- the settings screen (HS-067, ADR-0072 D2/D6) -------------------------------------------

// The machine's notifications: one row per machine the app is served from (one today, because there is one
// control origin), the sentence this browser earns, and the one control that sentence allows.
//
// Every word is `push.ts`'s — the page only decides *when* to ask, never *what* to say. The facts are read from
// the platform each time the screen is (re)drawn rather than kept: a permission the operator changed in the
// browser's settings is a fact this page cannot be told about, and a subscription can be rotated by the browser
// without asking anybody.
function settingsSection(health: Health): HTMLElement {
  const section = make("section", "settings");
  section.id = "settings";

  const machine = make("p", "settings-machine");
  machine.id = "settings-machine";

  const sentence = make("p", "settings-state");
  sentence.id = "settings-state";

  const action = make("button", "settings-action");
  action.id = "settings-action";
  action.type = "button";

  const detail = make("p", "settings-detail");
  detail.id = "settings-detail";
  detail.hidden = true;

  const install = make("p", "settings-install");
  install.id = "settings-install";
  install.hidden = true;

  const perMachine = make("p", "settings-per-machine", PER_MACHINE_SENTENCE);
  perMachine.id = "settings-per-machine";

  const notice = make("p", "notice");
  notice.id = "settings-notice";
  notice.hidden = true;

  // The facts as last read, and whether a request of ours is in flight. `busy` exists because the screen is
  // redrawn after every action: without it a re-render re-enables the button mid-request and a second tap is a
  // second subscribe (ADR-0067's lesson, in a second screen).
  let facts: PushFacts = {
    supported: false,
    permission: "default",
    hasKey: health.application_server_key !== null,
    subscribed: false,
    installed: false,
  };
  let busy = false;
  let pending: PushState | null = null;

  function draw(): void {
    const decided = pushState(facts);
    pending = decided;
    machine.textContent = machineState(location.origin, facts.subscribed);
    machine.dataset.origin = location.origin;
    machine.dataset.subscribed = String(facts.subscribed);
    sentence.textContent = decided.sentence;
    detail.textContent = decided.detail ?? "";
    detail.hidden = decided.detail === null;
    action.textContent = busy ? "Working…" : pushActionLabel(decided.action);
    action.hidden = !pushActionable(decided.action);
    action.disabled = busy;
    // The iOS requirement is item 11's third bullet, and it is stated here rather than in a document. `null`
    // once the app really is installed, where the row above already says so.
    const installText = installSentence({
      installed: facts.installed,
      ios: isIos(navigator.userAgent, navigator.maxTouchPoints),
    });
    install.textContent = installText ?? "";
    install.hidden = installText === null;
  }

  // The platform's answers, read fresh. `supported` covers the three things a subscribe needs at once: a secure
  // origin (an insecure one has no `serviceWorker` at all), a browser with a `PushManager`, and a `Notification`
  // to ask for permission with.
  async function gather(): Promise<PushFacts> {
    const supported =
      window.isSecureContext &&
      "serviceWorker" in navigator &&
      "PushManager" in window &&
      "Notification" in window;
    // SAFETY: `navigator.standalone` is iOS Safari's own non-standard flag, which no lib declares. It is
    // `undefined` everywhere else, which the comparison reads as "not installed".
    const standalone = (navigator as unknown as { standalone?: boolean }).standalone === true;
    const installed = standalone || window.matchMedia("(display-mode: standalone)").matches;
    let subscribed = false;
    if (supported) {
      try {
        const registration = await navigator.serviceWorker.getRegistration();
        subscribed =
          registration !== undefined &&
          (await registration.pushManager.getSubscription()) !== null;
      } catch {
        // A registration this page cannot read is a browser that has no subscription as far as this screen can
        // tell; the sentence says `not subscribed`, which is the safe half of the two.
        subscribed = false;
      }
    }
    return {
      supported,
      permission: supported ? Notification.permission : "default",
      hasKey: health.application_server_key !== null,
      subscribed,
      installed,
    };
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

  function fail(code: string, message: string): void {
    notice.textContent = `${code}: ${message}`;
    notice.hidden = false;
  }

  async function subscribe(): Promise<void> {
    notice.hidden = true;
    const key = health.application_server_key;
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
    const response = await fetch(SUBSCRIBE_PATH, {
      method: "POST",
      headers: { "content-type": "application/json", "x-holosmith-client": "1" },
      body: JSON.stringify(body),
    });
    if (!response.ok) {
      // The row did not land, so the local subscription is not one this machine can deliver to: undo it rather
      // than leave a screen that says `subscribed` about a subscription no row knows.
      await subscription.unsubscribe();
      const envelope = (await response.json()) as ErrorEnvelope;
      fail(envelope.code, envelope.message);
      return;
    }
  }

  async function unsubscribe(): Promise<void> {
    notice.hidden = true;
    const registration = await navigator.serviceWorker.getRegistration();
    const subscription =
      registration === undefined ? null : await registration.pushManager.getSubscription();
    if (subscription === null) {
      return;
    }
    const query = new URLSearchParams({ endpoint: subscription.endpoint }).toString();
    const response = await fetch(`${SUBSCRIBE_PATH}?${query}`, {
      method: "DELETE",
      // A mutating request needs this header even with no body (ADR-0010); `404` is the row already gone, which
      // is the answer the operator wanted.
      headers: { "content-type": "application/json", "x-holosmith-client": "1" },
    });
    if (!response.ok && response.status !== 404) {
      const envelope = (await response.json()) as ErrorEnvelope;
      fail(envelope.code, envelope.message);
      return;
    }
    await subscription.unsubscribe();
  }

  action.addEventListener("click", () => {
    const decided = pending;
    if (busy || decided === null || !pushActionable(decided.action)) {
      return;
    }
    busy = true;
    draw();
    const run = decided.action === "subscribe" ? subscribe : unsubscribe;
    run()
      .catch((error: unknown) => {
        fail("unreachable", String(error));
      })
      .finally(() => {
        busy = false;
        void gather().then((read) => {
          facts = read;
          draw();
        });
      });
  });

  // The first draw is the unsupported shape until the platform has answered, so the button is never offered on a
  // guess.
  draw();
  void gather().then((read) => {
    facts = read;
    draw();
  });

  section.append(
    make("h2", "", "Notifications"),
    machine,
    sentence,
    detail,
    action,
    install,
    perMachine,
    notice,
  );
  return section;
}

function render(health: Health, list: SessionList): void {
  if (app === null) {
    return;
  }
  app.replaceChildren(
    header(health, navLink(location.pathname)),
    sessionList(list.sessions, list.stopped_ttl_days),
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
  // `/settings` is the third screen and the machine's own (HS-067, ADR-0072): it needs no request but the health
  // this function already made, and it draws one row per machine the app is served from — one today.
  if (location.pathname === SETTINGS_PATH) {
    app?.replaceChildren(
      header(health.value, navLink(location.pathname)),
      settingsSection(health.value),
    );
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
