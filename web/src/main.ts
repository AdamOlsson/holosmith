// The app shell: the machine's health, the session list and the new-session form. Every value it
// shows came from a response or from a `session.status` frame, so a reconnect has nothing to
// reconcile and there is no cache to invalidate (ADR-0017). The one value it keeps that is not on
// screen is the pending Idempotency-Key a retry has to repeat.

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

// The six states, spelled as the API spells them (spec item 8).
type SessionState = "starting" | "idle" | "running" | "stopped" | "error" | "deleting";

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

  const head = make("div", "session-head");
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
  const source = new EventSource(`/api/sessions/${id}/events`);
  rowSources.set(row, source);
  openSources.add(source);
  source.addEventListener("session.status", (event) => {
    const frame = parseFrame((event as MessageEvent<string>).data);
    if (frame === null) {
      return;
    }
    applyStatus(row, frame);
    syncStream(row, frame.to);
  });
}

function parseFrame(data: string): StatusFrame | null {
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
