# Phase 3 — Multiple machines

**Goal:** one URL that reaches every machine, with sessions, previews and notifications working through it.

**Exit criteria:** a hub machine with no Docker lists two servers, forwards session API calls and previews (including WebSockets) to the right one, accepts forwarded calls only from registered hubs, and a second machine can be deployed over SSH from the CLI.

---

### HS-068 — Host registry, `POST /api/hosts` and the hosts seed

**As** the operator **I want** to register machines once and have them agree **so that** my phone sees every machine without manual sync.

- **Size:** M · **Blocked by:** HS-046 · **Spec:** item 4 (host registration), item 16 (hosts seed)
- **Demo:** add a host over the API; copy one `hosts.toml` to every machine and restart; every machine lists the same hosts.

**Acceptance**
- [ ] `POST /api/hosts` validates: URL is http(s), host is a MagicDNS name or an address in 100.64.0.0/10, port is in the allowed range, redirects are not followed.
- [ ] The `/api/health` reply must carry the Holosmith name and version fields; anything else is rejected with a distinct code.
- [ ] `hosts.toml` is read at startup and merged into the hosts table, the file winning on conflicts.
- [ ] A host that fails health is marked unreachable with the last error, not removed.
- [ ] Removing a host is refused while it has live sessions, unless forced.

---

### HS-069 — Hub request and preview forwarding

**As** the operator **I want** one origin that reaches every machine **so that** I do not juggle URLs.

- **Size:** L · **Blocked by:** HS-068, HS-041 · **Spec:** item 3, item 16, Roles
- **Demo:** from the hub, open a session on a remote server and its preview, and use a WebSocket-backed dev server through it.

**Acceptance**
- [ ] The hub forwards API calls to the owning server and streams events back unchanged.
- [ ] The hub's preview listener forwards `/<host>/<id>/*` to that machine's preview listener over the Tailnet, including WebSocket upgrades.
- [ ] Local sessions use the host name `local`; remote ones use the registered name.
- [ ] Default forwarding timeout is 5 s; merge and publish use 60 s.
- [ ] A hub-only machine (no Docker) serves the API and registry and lists only registered servers; it creates no sessions.
- [ ] A remote machine going down mid-stream produces a clear client-visible error, not a hang.

---

### HS-070 — Machine-to-machine trust (`allowed_hubs`, V11)

**As** the operator **I want** only my hub to be able to forward to my servers **so that** a stray tailnet node cannot drive them.

- **Size:** M · **Blocked by:** HS-069 · **Spec:** item 4, V11
- **Demo:** the hub is accepted and a different tailnet node is rejected with a logged reason.

**Acceptance**
- [ ] A machine accepts forwarded calls only from peers listed in `allowed_hubs`.
- [ ] Peer identity is verified with the Tailscale LocalAPI whois of the remote address — not by a header the caller supplies.
- [ ] Browser-facing guards (Host, Origin, content type, client header) apply at the hub; forwarded calls carry the verified identity.
- [ ] **V11 gate:** the whois check accepts the hub and rejects another tailnet node; on failure the documented fallback is ACLs only, recorded as a risk.
- [ ] Rejections are logged with the peer address and reason, and never leak configuration details.

---

### HS-071 — Hub UI: Computers screen

**As** the operator **I want** to see my machines and their state **so that** I know where work is running.

- **Size:** M · **Blocked by:** HS-070 · **Spec:** Roles, item 4
- **Demo:** the Computers screen lists two machines with health, version, live session counts and last-seen times, and can add and remove one.

**Acceptance**
- [ ] Lists name, URL, role, version, reachability, last health check and live/stopped session counts.
- [ ] Add and remove are available with the same validation as `POST /api/hosts`, showing the specific rejection reason.
- [ ] Version skew between machines is visible, since it affects which API a call can use.
- [ ] Sessions screen groups by machine and shows which machine owns each session.
- [ ] A hub-only machine shows no "new session" affordance for itself.

---

### HS-072 — `hub deploy` and `hub host` CLI

**As** the operator **I want** to deploy and manage a hub from the CLI **so that** the hub is as scriptable as a server.

- **Size:** M · **Blocked by:** HS-071 · **Spec:** CLI, What `server deploy` does, Commands table
- **Demo:** `holosmith hub deploy` on a Docker-less machine gives a working hub; `hub host add/list/remove` match the API.

**Acceptance**
- [ ] `hub deploy` runs steps 1 (without Docker and git checks), 2 (without the docker group), 5, 6 and 7; it builds no images and no proxies.
- [ ] `holosmith hub host add <name> <url>`, `list` and `remove <name>` perform the same checks as `POST /api/hosts`.
- [ ] `--dry-run` works for `hub deploy`.
- [ ] Running `server deploy` and `hub deploy` on the same machine yields role `both`, and the API is unchanged.
- [ ] `status` on a hub reports the health of every registered machine.

---

### HS-073 — `--ssh` deploys

**As** the operator **I want** to install on a remote machine from my laptop **so that** I do not SSH in by hand.

- **Size:** M · **Blocked by:** HS-072 · **Spec:** What `server deploy` does (--ssh), Build and release
- **Demo:** `holosmith server deploy --ssh me@box --dry-run` lists steps remotely, then a real run installs and prints the URL.

**Acceptance**
- [ ] Copies the same binary, verifies its SHA-256 after the copy, and runs the same deploy there with sudo, streaming output.
- [ ] Refuses when the target's CPU architecture differs, unless `--binary` supplies a matching build.
- [ ] The CLI downloads nothing from the internet.
- [ ] A failed verification aborts before executing anything on the target.
- [ ] `--dry-run` over SSH performs the preflight only.

---

### HS-074 — Cross-machine sessions and notifications

**As** the operator **I want** one session list and one notification stream across machines **so that** the hub is the only thing I open.

- **Size:** M · **Blocked by:** HS-073, HS-066 · **Spec:** item 11, Roles
- **Demo:** a turn finishing on machine B notifies my phone while I am looking at machine A's sessions.

**Acceptance**
- [ ] Each machine keeps its own VAPID keys and subscriptions; the app subscribes per machine it uses through the hub.
- [ ] Notification payload `{host, session_id, kind}` opens the right machine's session through the hub.
- [ ] Creating a session through the hub sets its machine explicitly, and the UI shows which machine it will run on.
- [ ] A machine that is unreachable degrades to a clear state per session rather than an empty list.
- [ ] Deleting a subscription removes it from every machine the app subscribed to.
