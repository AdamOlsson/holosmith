// The service worker (ADR-0072 D4): three events, no cache.
//
// It is **built**, not copied: `web/vite.sw.config.ts` compiles this file into `web/dist/sw.js` as one
// dependency-free script, so the rules it runs are the ones `web/src/push.ts` owns and `npm test` tests — the four
// sentences, the sixteen-hex id rule, the flattened subscription body. A hand-written `public/sw.js` would have
// had to restate every one of them.
//
// There is deliberately **no `fetch` handler**: a cache is a second source of truth for a page whose whole design
// is "every value on screen came from a response or a frame" (ADR-0017), and a stale transcript is worse than a
// wait. The worker exists for the three events below and nothing else.
//
// The global scope is typed here rather than pulled from `lib.webworker`, which cannot share a program with the
// page's `lib.dom` (the two declare the same names).

import {
  SUBSCRIBE_PATH,
  notificationData,
  notificationPath,
  notificationPayload,
  notificationTag,
  notificationText,
  notificationTitle,
  subscriptionBody,
} from "./push";

type Waitable = { waitUntil(promise: Promise<unknown>): void };

// What a JSON body can be, named once so the I/O boundary's value has a type of its own rather than `unknown`
// passed around. Everything that reads a body (`event.data.json()`, a subscription's `toJSON()`) answers this;
// `push.ts` is what turns it into the named domain types.
type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

type PushEventLike = Waitable & {
  data: { json(): JsonValue } | null;
};

type NotificationClickEventLike = Waitable & {
  notification: {
    data: unknown;
    close(): void;
  };
};

type PushSubscriptionChangeEventLike = Waitable & {
  // The browser's replacement subscription, when it hands one over. It may be `null`, and then there is nothing
  // this worker can do: getting a new one needs the application server key and a subscribe call whose failure is
  // silent, and the settings screen (which reads `getSubscription()`) is where that operator finds out.
  newSubscription: { toJSON(): JsonValue } | null;
};

type ClientLike = { url: string; focus(): Promise<unknown> };

type WorkerScope = {
  addEventListener(type: string, listener: (event: unknown) => void): void;
  skipWaiting(): Promise<void>;
  registration: {
    showNotification(title: string, options: Record<string, unknown>): Promise<void>;
  };
  clients: {
    claim(): Promise<void>;
    matchAll(options: { type: string; includeUncontrolled?: boolean }): Promise<ClientLike[]>;
    openWindow(url: string): Promise<ClientLike | null>;
  };
};

// SAFETY: this file is a service worker and is only ever loaded as one, so the global scope *is* a
// `ServiceWorkerGlobalScope`. TypeScript cannot know that — the page's program already has `lib.dom`, which
// declares `self` as a `Window` and cannot be combined with `lib.webworker` — so the invariant is stated here,
// where `WorkerScope` is exactly the subset of the worker's globals this file uses.
const scope = self as unknown as WorkerScope;

// A new worker takes over at once. Nothing is cached, so there is no old state to replace and no reason to wait
// for every tab to close.
scope.addEventListener("install", () => {
  void scope.skipWaiting();
});

scope.addEventListener("activate", (event) => {
  (event as Waitable).waitUntil(scope.clients.claim());
});

// One push → one notification. The browser has already decrypted the body, so `data.json()` is the payload
// ADR-0071 sealed: `{host, session_id, kind}` and nothing else.
scope.addEventListener("push", (event) => {
  const push = event as PushEventLike;
  const payload = readPayload(push);
  if (payload === null) {
    // Not this build's payload. Showing a notification with no way back would be worse than showing none, and
    // throwing would end this subscription's delivery silently.
    return;
  }
  push.waitUntil(
    scope.registration.showNotification(notificationTitle(payload.host), {
      body: notificationText(payload.kind),
      tag: notificationTag(payload.sessionId),
      // A lock-screen entry that opens the session, and the icon the app already ships. `badge` is the small
      // monochrome slot Android draws; the same file is the honest answer there.
      //
      // `data` carries the host as well as the id, and `push.ts`'s `notificationData` is what says so: a
      // session lives on exactly one machine, the tap's path is `/hosts/<host>/sessions/<id>` on the hub's origin, and
      // this worker has no page to ask which machine an id belongs to. Without the host every tap opened the *hub's*
      // own `/sessions/<id>`, which holds no such row — the story's one P0.
      data: notificationData(payload),
      icon: "/icons/icon-192.png",
      badge: "/icons/icon-192.png",
    }),
  );
});

// A tap opens the session it names — or focuses the window already showing it, which is what an operator who left
// the app open meant by tapping.
scope.addEventListener("notificationclick", (event) => {
  const click = event as NotificationClickEventLike;
  click.notification.close();
  const target = readTarget(click.notification.data);
  const path = notificationPath(target.host, target.sessionId);
  click.waitUntil(
    (async () => {
      const windows = await scope.clients.matchAll({ type: "window", includeUncontrolled: true });
      const destination = new URL(path, self.location.origin);
      for (const window of windows) {
        if (new URL(window.url, self.location.origin).pathname === destination.pathname) {
          await window.focus();
          return;
        }
      }
      await scope.clients.openWindow(destination.pathname);
    })(),
  );
});

// A rotated subscription, stored once. One attempt, no retry: a worker has no deadline to spend, and a row that
// did not land is a screen that says "not subscribed" — which is the truth.
scope.addEventListener("pushsubscriptionchange", (event) => {
  const change = event as PushSubscriptionChangeEventLike;
  const subscription = change.newSubscription;
  if (subscription === null) {
    return;
  }
  const body = subscriptionBody(subscription.toJSON());
  if (body === null) {
    return;
  }
  change.waitUntil(
    fetch(SUBSCRIBE_PATH, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-holosmith-client": "1",
      },
      body: JSON.stringify(body),
    }).catch(() => {
      // A refusal (403/422) or an unreachable machine leaves the old row: the next page load reads the truth.
    }),
  );
});

// The payload out of an event's `data`, or `null`. `json()` throws on a body that is not JSON, and a throw here
// is a push that vanishes; so the read is guarded.
function readPayload(event: PushEventLike): ReturnType<typeof notificationPayload> {
  if (event.data === null) {
    return null;
  }
  try {
    return notificationPayload(event.data.json());
  } catch {
    return null;
  }
}

// The two fields out of a notification's `data`, or empty strings — which `notificationPath` turns into the list.
//
// Not `notificationPayload`: that reads the **push** body, which the browser parsed out of the sealed record, and a
// notification's `data` is this worker's own copy of two of its fields. A missing field is the list rather than a
// throw, because a tap on a notification this build did not draw is still a tap.
function readTarget(data: unknown): { host: string; sessionId: string } {
  if (typeof data !== "object" || data === null) {
    return { host: "", sessionId: "" };
  }
  const record = data as Record<string, unknown>;
  const host = record["host"];
  const id = record["session_id"];
  return {
    host: typeof host === "string" ? host : "",
    sessionId: typeof id === "string" ? id : "",
  };
}
