# Quickstart: one machine, your phone as the browser

The shortest path from an installed binary to a session you can drive from your phone: this machine serves, the
tailnet carries the traffic, the phone opens a page. No hub, no second machine.

**What this does not give you:** plain `http://` on a tailnet address is not a secure context, so the app installs to
no home screen and push notifications do nothing. Everything else — creating a session, prompting, watching the
stream, diffs, merges — works. For the installable app and notifications, put TLS in front; see
[After the quickstart](#after-the-quickstart).

**This is the plain-HTTP variant, and it is what a Headscale control plane forces**, because Headscale issues no node
certificates. If your tailnet does issue them, the README's *Over a tailnet* section is the shorter path: the same
`deploy` configures `tailscale serve --https` on 443 and 8444, and the app is an `https://` name.

## Before you start

```bash
holosmith version                 # this build
docker info >/dev/null && echo docker ok
tailscale status | head -1        # your node, and its MagicDNS name
df -h /var/lib 2>/dev/null || df -h /
```

You need: Docker answering, git 2.38+, the binary at a **stable path** (the unit names the path it was deployed
from, so do not run a copy out of a build directory), a tailnet that reaches the phone, and at least as much free
space as `min_free_gb` below. The deploy never runs `sudo` itself: creating the account, making the root and
installing the unit are **commands it prints for you to run**, and the section below is that list (ADR-0086).

One value is substituted throughout: this node's MagicDNS name. Get it with
`tailscale status --json | jq -r .Self.DNSName` and strip the trailing dot; this guide writes
`huawei-laptop.tailnet.internal` wherever it appears, including inside the TOML.

## 1. The machine's halves: one account, one root, one unit

Everything below is root's, so you run it. The deploy is what tells you the exact lines (its step 2 and step 5 print
them), but this is the shape:

```bash
# 1. the service user, with the root as its home and no login shell
sudo useradd --system --create-home --home-dir /var/lib/holosmith --shell /usr/sbin/nologin holosmith
# 2. the one group it needs: the daemon's socket
sudo usermod -aG docker holosmith
# 3. the root itself, 0700, the service user's
sudo install -d -o holosmith -g holosmith -m 0700 /var/lib/holosmith
```

`/var/lib/holosmith` is now one directory holding `config.toml`, `data/` (the database, secrets, sessions),
`releases/` and `current` (what systemd runs), and `projects/`. It is also `holosmith`'s home, so `~holosmith` and
the root are the same path.

## 2. Write the config

The deploy renders this file from the config it **loads**, so write it before the first deploy (as the service user —
the root is 0700) and it survives every re-run. `min_free_gb` is the one value that must match this machine: `doctor`
refuses below it, and a `serve` that refuses is a unit that restart-loops.

```bash
sudo -u holosmith tee /var/lib/holosmith/config.toml >/dev/null <<'EOF'
role = "server"
machine_name = "laptop"

control_listen = "127.0.0.1:8080"
preview_listen = "127.0.0.1:8081"
control_origin = "http://huawei-laptop.tailnet.internal:8080"
preview_origin = "http://huawei-laptop.tailnet.internal:8081"
allowed_hosts  = ["huawei-laptop.tailnet.internal"]

data_root = "/var/lib/holosmith/data"
projects_root = "/var/lib/holosmith/projects"

min_free_gb = 10
session_disk_soft_gb = 5
idle_stop_min = 30
turn_timeout_min = 45

[egress]
allow = ["crates.io", "index.crates.io", "static.crates.io", "registry.npmjs.org", "github.com"]
EOF
sudo -u holosmith holosmith config validate    # "config valid: /var/lib/holosmith/config.toml"
```

Three notes on that file: `allowed_hosts` and both origins carry the **MagicDNS name**, not `localhost` — the page is
loaded from the phone, and the guards compare the `Host` and `Origin` headers with these strings. `owner` is
deliberately unset: it is a check on the login header a proxy in front supplies, and until you have one it would only
get in the way. `[egress] allow` is what a session may reach; with the list empty a session has no Internet at all.
Provider traffic does not use this list — it goes through the key proxy, which is why no provider host appears above.

## 3. Deploy

Run it as the service user: the tree is written as its owner, which is why it needs no `sudo` of its own.

```bash
sudo -u holosmith holosmith server deploy --dry-run   # the plan; changes nothing
sudo -u holosmith holosmith server deploy             # the real run: six steps
```

The first real run stops at step 5 and prints the commands that finish the job. Run them, then run it again:

```bash
sudo install -m 0644 /var/lib/holosmith/holosmith.service /etc/systemd/system/holosmith.service
sudo systemctl daemon-reload
sudo systemctl enable --now holosmith.service
```

A second `deploy` then runs to the end:

```text
deploy: run
deploy: into /var/lib/holosmith
1. preflight: satisfied: …
2. files: satisfied: …     # the config, the data root, the secrets and the keys
3. images: changed: …      # builds holosmith-proxy and agent-image, minutes on a first run
4. proxies: changed: …     # starts hs_key and hs_egress
5. service: satisfied: …   # the unit is in the root; still root's to run: the install above
6. health: satisfied: …    # `/api/health` answers and `doctor --full` finds nothing failing
deploy: 6 steps: 4 satisfied, 2 changed, 0 skipped, 0 failed
```

Verify it is serving:

```bash
systemctl status holosmith.service --no-pager
curl -fsS http://127.0.0.1:8080/api/health
```

`active (running)` plus a JSON body is the machine working. A unit that is `activating (auto-restart)` means the
preflight refused — read `sudo holosmith doctor --quick` and `journalctl -u holosmith -n 30`.

One more line, once, so a release can restart the unit and confirm itself live (ADR-0086 D4):

```bash
echo 'holosmith ALL=(root) NOPASSWD: /usr/bin/systemctl restart holosmith.service' \
  | sudo tee /etc/sudoers.d/holosmith >/dev/null
```

## 4. Put it on the tailnet (plain HTTP)

So the phone can reach it without exposing anything to the LAN, and without touching `control_listen`:

```bash
sudo tailscale serve --bg --yes --http=8080 http://127.0.0.1:8080   # control listener
sudo tailscale serve --bg --yes --http=8081 http://127.0.0.1:8081   # preview listener
tailscale serve status                                              # both entries, with their targets
```

Check it from the phone's browser before going further:

```text
http://huawei-laptop.tailnet.internal:8080/api/health
```

If that does not answer, the tailnet is the problem, not Holosmith: check that the phone's Tailscale app lists this
node as connected. Both `serve` entries live in tailscaled's own config and survive a reboot.

## 5. The provider key

One key, on stdin only, written to `/var/lib/holosmith/data/secrets/keys.toml` (0600). Run it **as the service
user**: the file's owner must be the data root's owner, or `doctor`'s `perms` finding fails and the next boot's
`ExecStartPre` refuses to start the unit.

```bash
sudo -u holosmith holosmith key set anthropic < ~/anthropic.key
sudo -u holosmith holosmith key list          # names only, never values
```

Twelve provider ids exist — `anthropic`, `openai`, `openrouter`, `deepseek`, `mistral`, `groq`, `xai`, `fireworks`,
`together`, `cerebras`, `nvidia`, `opencode-go` — and a name this build does not know is refused with that list. No
restart is needed: the key proxy reads the store per request.

## 6. A project to work in

A "project" is a directory **directly inside `projects_root`** that holds a `.git` directory; its name is what the
new-session form asks for. Worktrees are made under `/var/lib/holosmith/projects/.worktrees/<id>`.

```bash
sudo -u holosmith git clone <repository-url> /var/lib/holosmith/projects/<project>
sudo -u holosmith git -C /var/lib/holosmith/projects/<project> status --short
```

The clone must be the service user's, because `publish` and `merge` run on the host as that user and use **its**
git credentials (`/var/lib/holosmith/.git-credentials`, or a deploy key under its home — which is the root).

A missing `.git`, a submodule (`.gitmodules` or `.git/modules`), or `extensions.worktreeConfig` set is refused at
create time with a `422` — those are the three shapes that cannot be mounted safely.

## 7. From the phone

1. Open `http://huawei-laptop.tailnet.internal:8080`.
2. **Create a session** — the form asks for:
   - **project**: the directory name from step 5 (the placeholder is `holosmith`);
   - **model** and **thinking**: what the agent runs, with suggestions to edit;
   - **Internet access**: starts from `network_default`, which is `false` here — tick it for a session that needs
     cargo or npm;
   - **machine**: `local` while this is the only machine; a hub lists the machines it can forward to.
3. The session walks `starting → idle`. Its container, network and worktree appear in that order.
4. **Prompt it.** The transcript streams; a turn moves `idle → running → idle`. Steer or abort from the same screen.
5. When the turn ends, open the session's **changes** view to read the diff of `agent/<id>` against the base.
6. **Merge** into one of `merge.targets` (`main`/`master` here); the host does the merge, never the container.
7. A dev server the agent started is at `http://huawei-laptop.tailnet.internal:8081/local/<session-id>/` — the session
   screen links it, which is safer than typing it.

## 8. Day two

```bash
sudo -u holosmith holosmith status       # role, version, uptime, session counts, proxies
sudo holosmith logs -f                   # journalctl -u holosmith.service (the journal is root's, or the
                                         # `systemd-journal` group's)
sudo -u holosmith holosmith doctor --full
sudo systemctl restart holosmith         # after any config edit
sudo -u holosmith holosmith server deploy   # idempotent: the second run changes nothing
```

## When something refuses

| What you see | What it means |
|---|---|
| `403 bad_host` | The `Host` the browser sent is not in `allowed_hosts`. Add the exact MagicDNS name; `localhost` and `127.0.0.1` are always allowed, `[::1]` is not. |
| `403 bad_origin` | A `POST`/`DELETE` whose `Origin` is not `control_origin`. The two strings must match exactly, port included, and a scheme or port change needs a restart. |
| `403 bad_client` | The request carried neither `X-Holosmith-Client: 1` nor `Content-Type: application/json` — this is not the app posting: a stale tab, a script, or `curl`. |
| `403 bad_owner` | `owner` is set and the request carried no matching login header, or two of them. Unset it, or point the proxy at the right login. |
| `409 cap_reached` | `max_live_sessions` reached and nothing is idle to evict. Stop or delete a session. |
| `507 disk_floor` | Free space below `min_free_gb` on the data root. Free space or lower the key, then restart. |
| `422 git_not_directory` | The project has no `.git` directory, or has submodules, or sets `extensions.worktreeConfig`. |
| Unit restart-loops on `perms` | Something under the data root is not owned by the data root's owner — usually a `key set` run as root. `sudo chown holosmith:holosmith /var/lib/holosmith/data/secrets/keys.toml`. |
| Session `error` at the container step | `docker ps` and `holosmith logs --session <id>`; the images are `agent-image` and `holosmith-proxy` (`sudo docker image ls`). |

## After the quickstart

- **The installable app and push notifications.** Both need a secure context. Terminate TLS in front of
  `127.0.0.1:8080` (Caddy or nginx, bound to the tailnet address, with a real domain and DNS-01 or with an internal CA
  trusted on the phone), then set `control_origin`, `preview_origin` and `allowed_hosts` to those `https://` names and
  restart. `docs/config.md` has the exact block.
- **A second machine.** `hub deploy` on the always-on box, `hub host add <name> <url>` for this one, and
  `allowed_hubs = ["<hub node name>"]` here.
- **Every key, rule and default.** `docs/config.md`. Build, release, upgrade: `docs/build.md`.
