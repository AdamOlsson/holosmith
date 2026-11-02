// The app shell: it renders what the health endpoint says and holds no state of its own.
// Sessions, events and turns arrive in later stories; ADR-0008's `code` is the one thing
// this file reads out of an error.

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

const app = document.querySelector<HTMLElement>("#app");

function row(label: string, value: string): HTMLElement {
  const element = document.createElement("div");
  element.className = "row";
  const key = document.createElement("span");
  key.className = "key";
  key.textContent = label;
  const content = document.createElement("span");
  content.className = "value";
  content.textContent = value;
  element.append(key, content);
  return element;
}

function render(health: Health): void {
  if (app === null) {
    return;
  }
  app.replaceChildren(
    Object.assign(document.createElement("h1"), { textContent: health.name }),
    row("version", health.version),
    row("role", health.role),
    row("uptime", `${health.uptime_s}s`),
  );
}

function renderFailure(code: string, message: string): void {
  if (app === null) {
    return;
  }
  const notice = document.createElement("p");
  notice.className = "error";
  notice.textContent = `${code}: ${message}`;
  app.replaceChildren(notice);
}

async function show(): Promise<void> {
  const response = await fetch("/api/health", {
    headers: { accept: "application/json" },
  });
  if (!response.ok) {
    const envelope = (await response.json()) as ErrorEnvelope;
    renderFailure(envelope.code, envelope.message);
    return;
  }
  render((await response.json()) as Health);
}

show().catch((error: unknown) => {
  renderFailure("unreachable", String(error));
});
