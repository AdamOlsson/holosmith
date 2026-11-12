# The notification key rides on health, and the worker is a built file

This is item 11's browser half. /ADR-0071 built the wire — the two routes, the `push_subscriptions` table,
the four `kind` words, the 20-second threshold, the stream rule, and a VAPID-signed RFC 8291 message whose plaintext
is `{host, session_id, kind}` and never content. What was missing was the app the wire talks to: no manifest, no
icon, no service worker, no `pushManager` call, no settings screen, and — the one thing that cannot be worked around
— **no way for a browser to learn this machine's application server key**, which `pushManager.subscribe` needs and
which nothing served.

So this ADR decides five things and records two that it deliberately did not do: where the key rides, where the
settings screen lives, where the worker's rules live, what the tap may open, and where the icons come from.

## D1 — the application server key is a sixth `/api/health` key, read at the moment of asking

`GET /api/health` gains `application_server_key: string | null` — the Web Push pair's **public** half, base64url
(87 characters), or `null` when this machine has no readable `<data>/secrets/vapid.toml`. It is last, so the five
keys before it keep the order `tests/http_contract.rs` asserts.

The three arms and why this one:

| Arm | Cost |
| --- | --- |
| **a sixth health key** (chosen) | the payload's test and "the machine's five keys" prose move with it; the page makes the request it already makes |
| a new `GET /api/push/key` | a route spec item 11's API table does not list, and a second request before the screen can draw |
| served into the shell by `web/src/web.rs` | the shell is an embedded static file, so this is templating a value into it — which breaks `case_app_shell`'s byte-for-byte comparison and puts a dynamic value in a static asset |

`preview_origin` is the precedent, not an analogy: put the origin a session's preview lives on into this
payload for exactly this reason (ADR-0051), and `main.ts` already fetches health before either screen draws, so the
key costs no request at all. ADR-0067 D4 rejected health for the *changes sheet's* two lists because those are one
session's facts; a machine's signing key is the machine's, which is what this payload is.

**It is public by design** — it is the `k=` of every JWT this machine signs — so serving it is not a leak. The
private half never leaves `<data>/secrets`, and a test asserts the answer does not contain it.

**Read at the moment of asking, not cached at boot.** `vapid::public_key(&data_root)` is the reader, beside the
file it reads; the answer changes when the file changes, which is what makes `server deploy` on an already-serving
machine honest at the next page load. `Pusher::pair` remains the reader that *tells apart* "no file" from "a file
that is not a pair", because its caller has a log line to spend on the difference; this one answers `None` to both,
since a browser must not be handed a string that is not a key.

`Health::of` keeps its signature and calls that reader, so the handler does not move. The unit test builds a
`TempDir` config rather than `Config::default()`, because the default data root is the *machine's* — a machine that
had deployed would make the sixth key a fact about the host instead of about the test.

## D2 — the settings screen is a third path, `/settings`

Not a section of the Sessions screen, which is ADR-0067's shape for the Changes sheet. The difference is what the
thing belongs to: a sheet describes **one session** and is correctly unreachable without one, while a subscription
belongs to **the machine** — and the state a machine is in when an operator first installs the app is a table with
no sessions in it. A section there would be a screen you cannot reach exactly when you need it.

`/settings` costs no server work: the fallback answers the app shell for every non-`/api` path (ADR-0007), so the
address survives a reload with no route, no rewrite and no second `sessionIdFromPath`. `main.ts`'s `show()` gains
one branch ahead of the list's fetch, and the header gains one link whose href and label are `navLink(pathname)`'s
rule — `/settings` returns to the list, every other path offers it.

The screen's "state per machine" is one row today, because there is one control origin; the row names the origin
and whether this browser is subscribed, and a line under it says why a second machine will be a second
subscription (item 11's last bullet: each machine's keys are independent). those stories own the cross-machine half.

## D3 — the iOS sentence is a rule, and there are two of them

Item 11's third acceptance bullet asks that iOS's "must be installed to the home screen" requirement be **in the
UI**. `installSentence({installed, ios})` answers `IOS_INSTALL_SENTENCE` on iOS when the app is not installed,
`INSTALL_HINT` on any other browser that is not installed, and `null` once it really is installed (where the row
above already says `subscribed`).

Two sentences rather than one: the requirement is iOS's, and a desktop browser's offer is a convenience — printing
the iOS requirement at a desktop would be a sentence about a browser the operator is not using. `installed` is
`matchMedia("(display-mode: standalone)")` **or** iOS's own non-standard `navigator.standalone`, and `ios` is
`/iPhone|iPad|iPod/` in the user agent **plus** the iPad since iPadOS 13, which reports a Macintosh agent and is
only told apart by `maxTouchPoints > 1`. Both detections are pure functions with node tests, because the rule "which
sentence does this browser earn" is a rule.

## D4 — the worker is a real asset at `/sw.js`, built from `web/src/sw.ts`

A service worker must be a file of its own at a **fixed** URL (`/sw.js`, scope `/`): the fallback's shell is a page
every browser refuses to register, silently. Three arms:

| Arm | Why not |
| --- | --- |
| `web/public/sw.js`, plain | it would have to restate the four sentences, the id rule and the flattened body — a second source of truth for copy `npm test` is supposed to own |
| a second Rollup input in the app's build | Vite hashes the filename, and `/sw.js` is the whole contract |
| **`build.lib` in a second config** (chosen) | one documented Vite shape: `entry: src/sw.ts`, `formats: ["es"]`, `fileName: () => "sw.js"`, `emptyOutDir: false`, `publicDir: false` |

So `web/src/sw.ts` `import`s the rules from `web/src/push.ts` — one home, typechecked by `tsc`, tested by
`npm test`, and bundled into the served file. `minify` is off on purpose: the served bytes are what `case_pwa`
compares with this tree's `web/dist/sw.js`, and a reader of the artifact can see the sentences and the id rule.
The bundle has no `export` statement (the worker's only import is bundled in), so it registers as a classic
worker script — no `{type: "module"}` and therefore no browser-version floor beyond push itself.

**There is no `fetch` handler, deliberately.** A cache is a second source of truth for a page whose whole design is
that every value came from a response or a frame (ADR-0017); a stale transcript is worse than a wait. The worker
handles exactly three events: `push`, `notificationclick`, `pushsubscriptionchange`, plus `install`/`activate` to
take over at once (nothing is cached, so there is no old state to replace).

The worker's global scope is typed in that file rather than pulled from `lib.webworker`, which cannot share a
TypeScript program with the page's `lib.dom` — the two declare the same names. The one type assertion carries the
invariant in a comment, because that is what it is.

## D5 — what the notification says, and what a `kind` this build does not know earns

`{host, session_id, kind}` is all there is (ADR-0071 priced that deliberately), so the four words are four
sentences: `turn_finished` → "a turn finished", `error` → "a session failed", `turn_timeout` → "a turn timed out",
`budget_exceeded` → "a turn ran out of its budget". The title is the payload's host (`Holosmith · local`), the tag is
`holosmith-<id>` so two sessions do not replace each other on a lock screen, and `data` carries the session id so
the tap has something to open.

**A word this build does not know is a notification, never an exception.** A throw in a `push` handler ends that
subscription's delivery silently — the browser drops it — so `notificationText` falls back to one generic sentence
and the tap still opens the session. The same rule governs a body that is not this payload: `notificationPayload`
answers `null`, the handler returns, and nothing is guessed at.

## D6 — the tap may only open a path built from a session id

`notificationclick` closes the notification, then focuses a window already showing that session, else
`clients.openWindow(notificationPath(id))`. The id comes from a payload a push service delivered, so it is
untrusted on its way into a URL: `notificationPath` returns `/sessions/<id>` for exactly sixteen lowercase hex
characters (`sessions::is_id`) and `/` for anything else. `openWindow` would otherwise open arbitrary text on this
app's own origin — a link this machine did not author.

`applicationServerKey` is passed as the **base64url string** the API allows (`BufferSource | string`), which is why
no hand-written base64url decoder exists on either side of the call — the browser's own decoder is the one that has
to agree with the browser's own encoder, and `subscriptionBody` only has to lift `{keys: {p256dh, auth}}` into the
route's two top-level fields.

## D7 — `pushsubscriptionchange` is one attempt, and then the screen is the truth

A browser rotates a subscription without asking. The event may carry `newSubscription`; the worker flattens and
`POST`s it, once. It does **not** call `subscribe` again (that needs the application-server key, which a worker
would have to fetch, and a `pushManager` call whose failure is silent), it does **not** retry (a worker has no
deadline to spend), and it does not store the old endpoint (nothing reads one). When the event carries no
subscription, or the `POST` is refused, the worker does nothing: the settings screen reads `getSubscription()` and
that is where "not subscribed" comes from.

## D8 — the icons are checked-in PNGs, generated once outside the build

The repository had no binary asset and no rasteriser, and an installable app needs three: a 192 `any`, a 512 `any`
and a 512 `maskable` (Chrome will not offer an install prompt without both sizes, and Android's adaptive mask crops
a non-`maskable` icon into a circle by guesswork). iOS does not read the manifest's `icons` for the home screen at
all — it reads `<link rel="apple-touch-icon">` and will not take an SVG — so a fourth file, 180×180, has a
`<link>` of its own in the shell.

They were made **once**, by `/tmp`'s throwaway python3 + `zlib` script (a rounded blue field and the white `H` of
`public/favicon.svg`; the maskable one full-bleed, with the glyph inside the safe zone), and committed as bytes. No
rasteriser joins the build, no dependency is added, and `web/test/push.test.ts` reads every PNG's signature and
`IHDR` to assert it is an 8-bit RGBA image of the size the manifest claims — which is a shape no shell `grep` could
measure.

## D9 — what no gate can see, and how the gate still measures this

No gate may need a browser (ADR-0046), so the split is: **the files' bytes and the served answers are the gate's,
the rules are `npm test`'s, and the tap is the demo's.**

`case_pwa` boots the artifact on its own data root with a hand-written pair (so it needs no `openssl`, `node` or
`python3`, and prints **no `SKIP` line at all** — a skip here would fire exactly when the feature was deleted) and
asserts: `GET /` is `web/dist/index.html` byte for byte and names the manifest and the apple-touch icon;
`/manifest.webmanifest` is this tree's file with `application/manifest+json` and the install's fields;
`/sw.js` is `web/dist/sw.js` byte for byte, served as JavaScript, **not** the shell, and carries the four sentences,
the three events and the id rule; each icon is its file's bytes as `image/png`; `/settings` is the shell; the
served **bundle** carries the screen's ten element ids, the iOS sentence, the per-machine sentence and two state
sentences; and `/api/health` carries the pair's `public_key` byte for byte, and `null` once the file is removed.

`web/test/push.test.ts` adds 25 tests: the four sentences and the unknown-kind fallback, the title and tag, the id
alphabet, the path rule, the payload's three keys and twelve non-shapes, the subscription body's flattening and
seven refusals, the five control arms and their order, the labels, the iOS sentence's three arms, the user-agent
detection, the per-machine row, the header's link, the manifest, and every icon's bytes.

The demo drives the four things neither can: **Chromium's own install judgement** (`Page.getInstallabilityErrors`
answers `[in-incognito]` — the automation profile and nothing about the app, which is the measurement that says a
cache-less worker still installs), a real `push` event delivered to the registered worker, the notification it draws,
and the tap's `clients.openWindow("/sessions/<id>")`. It does **not** install the app on a device, and it cannot
click a system notification: a synthetic dispatch carries no user activation, so `openWindow` is recorded rather than
opened — the acceptance's tap is the handler's call, measured, with the window that a real tap opens stated as the
one thing left to a phone.

## What this ADR does not decide

Per-session subscriptions (item 11's payload carries the session, and a subscription that named one is a
registration no browser makes); a notification's content (ADR-0071's plaintext is three keys); a caching worker
(above); the hub forwarding a subscription or a push; and any change to the wire — the two
routes take exactly `{endpoint, p256dh, auth}` and are unchanged.
