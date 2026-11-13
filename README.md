# Holosmith

Holosmith runs coding agents in containers on a machine you own, and lets you drive them from a browser — the same
page on a laptop and on a phone. Each agent works in its own git worktree, on its own Docker network, with its own
transcript: you watch it, steer it, stop it, read the diff, and land the result. Provider keys stay on the host — a
session's container talks to a key proxy and never holds a real key.

It is one static binary, and the same binary is a **server** (runs sessions), a **hub** (forwards to several servers,
so one page drives several machines), or both. SQLite is bundled and the web app is built in.

## Dependencies

What a **server** needs. A hub runs no sessions: it needs systemd and space for its own database, and `holosmith
hub deploy` asks neither Docker nor git question.

| Dependency | Why |
| --- | --- |
| Linux with systemd | `deploy` installs the tree and starts `holosmith.service`, which a boot starts |
| Docker Engine | every session is a container on its own `--internal` network, with two proxies beside it |
| git 2.38 or newer | a session is a worktree plus an `agent/<id>` branch; merges use `git merge-tree --write-tree` |
| a provider key | `anthropic`, `openai`, `openrouter` and nine more — a name it does not know is refused with the full list |
| free space | `min_free_gb` (default **20** GB) on the data root: checked before a boot and before a create |
| openssl | the deploy generates the Web Push key pair with it, and the notification sender uses it |

Optional, for reaching the machine from other devices:

| Dependency | Why |
| --- | --- |
| Tailscale | the tailnet path: an HTTPS name, phone access from anywhere, machine-to-machine forwarding |

Nothing else is required: no Python or Node runtime, no database server, no reverse proxy for the local path.
Building from source needs Rust 1.99+ (edition 2024), Node.js and npm for `web/`, and the musl targets for release
artifacts — `scripts/build.sh` builds the artifact and `scripts/verify.sh` is the gate.

## Getting started

### On this machine

One account, one directory, one unit — and the deploy runs as the account, never as root. Put the binary at a path
that will still be there after a reboot: the systemd unit names the absolute path it was deployed from.

```bash
sudo useradd --system --create-home --home-dir /var/lib/holosmith --shell /usr/sbin/nologin holosmith
sudo usermod -aG docker holosmith                        # the one group it needs: the daemon's socket
sudo install -d -o holosmith -g holosmith -m 0700 /var/lib/holosmith

sudo -u holosmith holosmith server deploy               # the tree, two images, the unit, in six steps

sudo -u holosmith holosmith key set anthropic           # paste the key, then Ctrl-D

sudo -u holosmith git clone https://github.com/you/project /var/lib/holosmith/projects/project
```

`/var/lib/holosmith` is the machine's one directory: `config.toml`, `data/` (database, secrets, sessions),
`releases/` and `current` (what systemd runs), and `projects/`. It is also `holosmith`'s home, so the service user's
git credentials live inside it — which is what `publish` and `merge` use.

Then open **<http://localhost:8080>**, fill in **New session** — the project field is the directory name under
`/var/lib/holosmith/projects` (`project` here; the placeholder is `holosmith`) — pick a model, and press **Start
session**. The first deploy builds `holosmith-proxy` and `agent-image`, which takes minutes; every later deploy is a
read, and `--dry-run` on any of them prints the same six steps and writes nothing at all.

Three things about that first run:

- **The deploy prints what root owns; it never runs it.** Step 2 reads the service user and prints the `useradd`/
  `usermod` it would take, and step 5 writes the unit into the root and prints the three commands that install and
  start it — `sudo install -m 0644 /var/lib/holosmith/holosmith.service /etc/systemd/system/`,
  `sudo systemctl daemon-reload`, `sudo systemctl enable --now holosmith.service`. Run them, run the deploy again, and
  step 6 waits for `/api/health` and reports `doctor --full`. A machine whose unit was never installed is caught
  there, by an endpoint that does not answer.
- **One sudoers line, once**, so a release can restart the unit and confirm itself live:

  ```bash
  echo 'holosmith ALL=(root) NOPASSWD: /usr/bin/systemctl restart holosmith.service' \
    | sudo tee /etc/sudoers.d/holosmith >/dev/null
  ```

  Without it `upgrade` still switches the release; it says the restart is yours rather than claiming a health check
  it did not make.
- **Tailscale is not part of this.** The deploy no longer configures `tailscale serve`: the listeners are
  `127.0.0.1:8080` and `127.0.0.1:8081`, and putting them on a tailnet is the operator's own step (below). A machine
  whose Tailscale is installed but logged *out* is still refused in the preflight, and the fix is `tailscale up`.
  If the run stops anywhere else, the failing step names its reason and `holosmith doctor --quick` says the same
  thing.
- **Type `localhost`, not `127.0.0.1`.** The page's origin is `control_origin`, `http://localhost:8080` by default,
  and every request that changes something must come from exactly that origin. `127.0.0.1` is a different origin and
  is answered `403 bad_origin`.
- **A session has no Internet by default.** `[egress] allow = []` means the agent can reach its provider through the
  key proxy and nothing else, so `cargo build` or `npm install` fails. Add the hosts a project needs
  (`sudo holosmith config edit`) and restart:

  ```toml
  [egress]
  allow = ["crates.io", "index.crates.io", "static.crates.io", "registry.npmjs.org", "github.com"]
  ```

  ```bash
  sudo systemctl restart holosmith
  ```

Day two is the same verbs: `sudo holosmith status`, `sudo holosmith logs -f`, `sudo holosmith doctor --full`. A
config edit is a restart, not a redeploy — though a redeploy re-reads the file, so edits survive one.

### From your phone, on the same wifi

Two things change: the service listens on more than loopback, and it answers the address the phone types. Take the
machine's LAN address (`ip -4 -o addr show scope global`), then `sudo holosmith config edit` for these five keys:

```toml
control_listen = "0.0.0.0:8080"
preview_listen = "0.0.0.0:8081"
control_origin = "http://192.168.1.42:8080"
preview_origin = "http://192.168.1.42:8081"
allowed_hosts  = ["192.168.1.42"]
```

`sudo systemctl restart holosmith`, open `http://192.168.1.42:8080` on the phone, and open the same port on the
machine's firewall if it has one (`sudo ufw allow 8080/tcp`; 8081 too if you want the agent's dev-server previews on
the phone).

- **`allowed_hosts` accepts a list, `control_origin` does not.** The host is how a page is allowed to load — it holds
  the LAN address *and* still answers `localhost` — but only one origin may change anything, so the address you type
  on the phone must be the one in `control_origin`, port included.
- **A LAN address that changes is a config edit.** Give the machine a DHCP reservation, or use the name it answers on,
  rather than chasing the address.
- **Plain HTTP over the wifi is not a secure context.** Everything works — sessions, prompts, the stream, diffs,
  merges, previews — but the page does not install to a home screen and push notifications do not arrive. Browser
  storage and clipboard also have narrower rules. The tailnet section is how to get HTTPS; for your own certificate,
  `docs/config.md` has the shape.
- **Listening on the LAN is a LAN-wide decision.** The host and origin checks stop *another web page* from driving the
  machine; they do not stop someone on the wifi who types the address. Every session on this machine is then theirs
  to prompt. The tailnet is the way to stay off the shared network.

### Over a tailnet

Tailscale gives the machine a stable HTTPS name that the phone reaches from anywhere, and `deploy` configures it. What
changes is that the app is served on **443** and previews on **8444**, on that name — so the origins are the name, not
the loopback port.

1. `tailscale up` if this node is not logged in, and read the name: `tailscale status --json | jq -r .Self.DNSName`
   answers `laptop.tailnet.ts.net.` — drop the trailing dot.
2. Put the name in the config **before** the deploy, so the deploy writes the file and configures `tailscale serve`
   for the same origins (`sudo holosmith config edit`; if you already deployed with the defaults, this edit plus the
   two commands below is the whole move, and without it the tailnet page loads and every action is refused).
   `control_listen` stays on loopback, because that is where `tailscale serve` connects from:

   ```toml
   control_listen = "127.0.0.1:8080"
   preview_listen = "127.0.0.1:8081"
   control_origin = "https://laptop.tailnet.ts.net"
   preview_origin = "https://laptop.tailnet.ts.net:8444"
   allowed_hosts  = ["laptop.tailnet.ts.net"]
   ```

3. `sudo systemctl restart holosmith` — the new origins are read at boot, and a deploy does not restart a unit that
   is already enabled.
4. `sudo tailscale serve --bg --yes --https=443 http://127.0.0.1:8080` and the same for `--https=8444` with
   `http://127.0.0.1:8081`, then `sudo -u holosmith holosmith server deploy` to check the machine is healthy. Both
   `serve` entries live in tailscaled's config and survive a reboot.
5. Open `https://laptop.tailnet.ts.net` from any device on the tailnet.

One thing to know:

- **`--https` needs a tailnet that issues certificates.** Headscale does not. Serve plain HTTP instead —
  `sudo tailscale serve --bg --yes --http=8080 http://127.0.0.1:8080`, the same for 8081 — and change the two origins
  to match, `control_origin = "http://laptop.tailnet.internal:8080"` and the preview one on 8081 with the same name
  in `allowed_hosts`. `docs/quickstart.md` is that path start to finish, including what HTTPS costs you (the
  installable app and push notifications).

Optional hardening: `owner = "<your tailnet login>"` makes the machine require that login on every request, which
`tailscale serve` supplies as a header. Direct LAN access carries no header and is unaffected.

## Architecture

```text
                    phone / browser
                          │  HTTPS (Tailscale) or plain HTTP on the LAN
                          ▼
                 ┌──────────────────┐
                 │  control listener │  API + embedded PWA
                 └────────┬─────────┘
                          │
        ┌─────────────────┼──────────────────┐
        │                 │                  │
   hub role          server role        preview listener
   registry +        sessions,          /<host>/<id>/*  (own origin)
   forwarding        events, SQLite
                          │
                    provisioner ──► Docker container (pi) on an internal network
                          │              │
                    git worktree         ├─ key proxy  (provider key never enters)
                    agent/<id>           └─ egress proxy (allow-listed HTTPS)
```

**One binary, three roles.** `serve` reads `role` from the config: `hub`, `server`, or `both`. A hub has no Docker and
no sessions; it serves the app, the host registry, and forwards API and preview requests. A server runs sessions and
owns the session API. Every role answers the same paths.

**Sessions are a state machine.** A create answers `202` and the provisioner walks `starting → idle` (or `error`),
creating the network, worktree, container and agent. Turns move `idle → running → idle`; abort, steer, resume,
delete, idle-stop, timeout, token budget and disk caps all apply transitions through one table. Crash recovery
reconciles rows and containers on boot.

**The transcript is an append-only event log.** One writer batches events into SQLite (`WAL`, `synchronous=FULL`) and
broadcasts after commit. Clients read replayable SSE streams and paginated history; tool output and images are files
under the session directory, not log payloads.

**Work happens on the host.** Each session has a git worktree and an `agent/<id>` branch. The server reads changes,
computes diffs, merges into a target branch, and optionally pushes; the container only edits files. The `.git` mount
is hardened read-only for hooks and config.

**Isolation is topology.** A session gets an `--internal` network. The only routes out are the key proxy (provider
calls, with a per-session dummy key) and, when Internet access is enabled, the egress proxy (CONNECT/443,
allow-list). Previews run on a separate listener and origin.

**Release and self-development.** The binary embeds the web app, migrations and the Docker build recipes.
`holosmith build` compiles in a pinned builder container; `smoke` proves a candidate against the real daemon with a
recorded turn; `upgrade` runs the review gate, smoke, image builds, drain, backup, release switch and health poll;
`rollback` walks the release tree one step back. Migrations are expand/contract, so switching back is safe.

## Where to look next

- `docs/quickstart.md` — one machine, the phone as the browser: config, deploy, a session, day two.
- `docs/config.md` — every key in `<root>/config.toml`: its type, default, rule and what it decides.
- `docs/build.md` — build, test, verify, deploy, release, and the by-hand credential checks.
- `GLOSSARY.md` — the vocabulary: what a session, a turn, a stream, a release and a drain mean here.
- `docs/adr/` — the decisions and why they were taken.
- `AGENTS.md` — the repository rules for agents working on the code.
