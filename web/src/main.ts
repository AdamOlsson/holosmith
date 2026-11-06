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
  FORCED_DELETE_LABEL,
  HISTORY_PAGE_LIMIT,
  HISTORY_TRUNCATED,
  NO_DETAIL,
  TRANSCRIPT_EVENTS,
  Transcript,
  UNSAVED_WORK_NOTICE,
  affordances,
  budgetNotice,
  createFailure,
  deletedNotice,
  historyPath,
  networkText,
  parseFrame,
  previewOffered,
  previewUrl,
  parseTruncated,
  sessionIdFromPath,
  stateText,
  stoppedDetail,
  streamPath,
  type Change,
  type DeltaKind,
  type HistoryPage,
  type Kept,
  type SessionState,
  type Spend,
} from "./transcript";

type Health = {
  name: string;
  version: string;
  role: string;
  uptime_s: number;
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

// HS-064's placeholder: a stopped row's detail line once auto-delete gives it a number. It is also the
// title of that span while the span holds a reason's sentence instead.
const RETAINED = "Days remaining until auto-delete arrives with HS-064";

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
  // The retention placeholder (HS-064's days-remaining line) and, while there is one, the sentence a
  // stopped session's reason earns: an eviction lost nothing, and the badge alone would read like a
  // loss (HS-020). One span, because both are "what this row's state means", never both at once.
  const detail = stoppedDetail(session.status, session.status_reason);
  const retention = make("span", "retention", detail);
  retention.title = detail === NO_DETAIL ? RETAINED : detail;
  retention.hidden = session.status !== "stopped";
  row.append(head, meta, step, retention);
  syncStream(row, session.status);
  return row;
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
    const detail = stoppedDetail(frame.to, frame.reason);
    retention.hidden = frame.to !== "stopped";
    retention.textContent = detail;
    retention.title = detail === NO_DETAIL ? RETAINED : detail;
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
      const frame = parseFrame(eventType, (event as MessageEvent<string>).data);
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
          say(UNSAVED_WORK_NOTICE);
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

  // The one place a state change patches the screen: the badge, the preview link, the composer's actions
  // and the delete affordance. Two writers of one state is the blocker HS-020's review found, and HS-043's
  // affordance is the fourth reader — so it is one function, and `session.status` goes through it too.
  function showState(state: SessionState, reason: string | null): void {
    badge.textContent = stateText(state, reason);
    preview.hidden = !previewOffered(state);
    composer.update(state);
    deleted.update(state);
  }
  composer.update(session.status);
  deleted.update(session.status);

  const section = make("section", "session-screen");
  section.id = "session";
  section.dataset.session = session.id;
  section.append(back, head, meta, earlier, transcript, composer.element, deleted.element);
  app.replaceChildren(header(health), section);
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
