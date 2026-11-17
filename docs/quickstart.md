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
df -h ~/.local/share/holosmith 2>/dev/null || df -h ~
```

You need: Docker answering, git 2.38+, the binary at a **stable path** (the unit names the path it was deployed
from, so do not run a copy out of a build directory), a tailnet that reaches the phone, and at least as much free
space as `min_free_gb` below. Nothing here runs `sudo` for you, and nothing here needs it: an install is the account's
own, and the two things that are root's — joining the daemon's group, and on some machines setting linger — are
**commands it prints** for you to run.

One value is substituted throughout: this node's MagicDNS name. Get it with
`tailscale status --json | jq -r .Self.DNSName` and strip the trailing dot; this guide writes
`huawei-laptop.tailnet.internal` wherever it appears, including inside the TOML.

## 1. Install

One command, as your own account — no `sudo`, no prompt, and nothing to create by hand:

```bash
holosmith install
```

The one file you write first is your config (§2), when the defaults do not fit this machine; everything else is this
command, and running it again changes nothing. It:

- **refuses before writing anything** if this account is not in the `docker` group. The daemon's socket is
  `root:docker` with mode 0660, so a session could not run, and the fix is one command the output ends with:

  ```bash
  sudo usermod -aG docker $USER        # then log out and back in: a group reaches a login only at the next one
  ```

- runs the deploy for the half this machine's config asks for — `server` and `both` install the server's, `hub` the
  hub's;
- enables `~/.config/systemd/user/holosmith.service` through your own systemd manager, and starts it;
- sets `loginctl enable-linger`, so the daemon starts at boot rather than only while you are logged in. On a machine
  whose polkit refuses that, the run says so, prints `sudo loginctl enable-linger $USER`, and carries on — the machine
  serves either way;
- waits for `/api/health`, then prints the control URL and the one next step.

The state is `~/.local/share/holosmith` (one directory to back up and one to delete) and the config is
`~/.config/holosmith.toml`, the only file in an install you edit. `--dry-run` prints each step's plan and changes
nothing.

## 2. Write the config

The install renders this file from the config it **loads**, so write it first and it survives every re-run;
`min_free_gb` is the one value that must match this machine, because `doctor` refuses below it and a `serve` that
refuses is a unit that restart-loops.

```bash
mkdir -p ~/.config && tee ~/.config/holosmith.toml >/dev/null <<'EOF'
role = "server"
machine_name = "laptop"

control_listen = "127.0.0.1:8080"
preview_listen = "127.0.0.1:8081"
control_origin = "http://huawei-laptop.tailnet.internal:8080"
preview_origin = "http://huawei-laptop.tailnet.internal:8081"
allowed_hosts  = ["huawei-laptop.tailnet.internal"]

min_free_gb = 10
session_disk_soft_gb = 5
idle_stop_min = 30
turn_timeout_min = 45

[egress]
allow = ["crates.io", "index.crates.io", "static.crates.io", "registry.npmjs.org", "github.com"]
EOF
holosmith config validate    # "config valid: /home/you/.config/holosmith.toml"
```

`data_root` and `projects_root` are deliberately absent: they are the install's own —
`~/.local/share/holosmith/data` and `~/.local/share/holosmith/projects`, under whatever root `--root` names — and the
file it writes out carries them.

Three notes on that file: `allowed_hosts` and both origins carry the **MagicDNS name**, not `localhost` — the page is
loaded from the phone, and the guards compare the `Host` and `Origin` headers with these strings. `owner` is
deliberately unset: it is a check on the login header a proxy in front supplies, and until you have one it would only
get in the way. `[egress] allow` is what a session may reach; with the list empty a session has no Internet at all.
Provider traffic does not use this list — it goes through the key proxy, which is why no provider host appears above.

## 3. The deploy underneath

`holosmith install` runs `holosmith server deploy` (or `hub deploy`) for you, and that is the lower-level command when
you want the steps without the start: it checks the current state first and writes the tree, the config and the unit,
and prints the two `systemctl --user` commands as the operator's rather than running them.

```bash
holosmith server deploy --dry-run   # the plan; changes nothing
holosmith server deploy             # the real run: six steps
```

The first real run against a fresh tree ends with its last step failed: nothing serves yet, because a deploy writes
the unit and does not start it. That is the step `holosmith install` finishes, and a `deploy` on a machine that is
serving reads:

```text
1. preflight ✓
2. files ✓
3. images ✓ (built 2 images: holosmith-proxy:0.1.0, agent-image:0.1.0)
4. proxies ✓ (2 actions)
5. service ✓ (1 action)
    still yours to run: systemctl --user daemon-reload; systemctl --user enable --now holosmith.service
6. health ✓
role: both
url: http://workshop.example.ts.net:8443
next: holosmith key set <provider>
next: holosmith hub host add <name> <url>
```

The six steps are the config (2), the two images (3), the two proxies (4), the unit (5) and `/api/health` (6). Each is
one line and the mark is its verdict: `✓` done or nothing to do, `–` skipped because this half of the machine has no
work for it, `!` still yours to run, `✗` failed. A step that wrote something says what in parentheses, a step that
needs something keeps the line you run. A failure keeps its whole sentence and adds one indented `fix:` line — the
command that puts it right, which is the step's own repair where it knows one and the command that just ran
otherwise. `--verbose` prints the older shape instead: every step's full sentence, the install tree and a tally, which
is what to reach for when you are diagnosing the machine rather than setting it up.

Verify it is serving:

```bash
systemctl --user status holosmith.service --no-pager
curl -fsS http://127.0.0.1:8080/api/health
```

`active (running)` plus a JSON body is the machine working. A unit that is `activating (auto-restart)` means the
preflight refused — read `holosmith doctor --quick` and `journalctl --user -u holosmith -n 30`.

The unit is started, stopped and restarted with `systemctl --user`, which is this account's own. A **promotion** is no
exception: `upgrade` and `rollback` ask the same user manager for the restart, so there is no `/etc/sudoers.d` line in
an install and nothing to set for a release to confirm itself live.

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

One key, on stdin only, written to `~/.local/share/holosmith/data/secrets/keys.toml` (0600). Run it as yourself:
the file's owner must be the data root's owner, which is your own account, or `doctor`'s `perms` finding fails and
the next boot's `ExecStartPre` refuses to start the unit.

```bash
holosmith key set anthropic < ~/anthropic.key
holosmith key list          # names only, never values
```

Twelve provider ids exist — `anthropic`, `openai`, `openrouter`, `deepseek`, `mistral`, `groq`, `xai`, `fireworks`,
`together`, `cerebras`, `nvidia`, `opencode-go` — and a name this build does not know is refused with that list. No
restart is needed: the key proxy reads the store per request.

## 6. A project to work in

A "project" is a directory **directly inside `projects_root`** that holds a `.git` directory; its name is what the
new-session form asks for. Worktrees are made under `~/.local/share/holosmith/projects/.worktrees/<id>`.

```bash
git clone <repository-url> ~/.local/share/holosmith/projects/<project>
git -C ~/.local/share/holosmith/projects/<project> status --short
```

The clone must be yours, because `publish` and `merge` run on the host as your own account and use **your** git
credentials (`~/.git-credentials`, or a deploy key under your home).

A missing `.git`, a submodule (`.gitmodules` or `.git/modules`), or `extensions.worktreeConfig` set is refused at
create time with a `422` — those are the three shapes that cannot be mounted safely.

## 7. From the phone

1. Open `http://huawei-laptop.tailnet.internal:8080`.
2. **Create a session** — the form asks for:
   - **project**: the directory name from step 6 (the placeholder is `holosmith`);
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
holosmith status                     # role, version, uptime, session counts, proxies
holosmith logs -f                    # journalctl --user -u holosmith.service
holosmith doctor --full
systemctl --user restart holosmith   # after any config edit
holosmith install                    # idempotent: a second run changes nothing
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
| Unit restart-loops on `perms` | Something under the data root is not owned by the data root's owner — a file from an older install, or one a `sudo` wrote before this build refused it. `sudo chown "$(id -u):$(id -g)" ~/.local/share/holosmith/data/secrets/keys.toml`. |
| Session `error` at the container step | `docker ps` and `holosmith logs --session <id>`; the images are `agent-image` and `holosmith-proxy` (`docker image ls`). |

## After the quickstart

- **The installable app and push notifications.** Both need a secure context. Terminate TLS in front of
  `127.0.0.1:8080` (Caddy or nginx, bound to the tailnet address, with a real domain and DNS-01 or with an internal CA
  trusted on the phone), then set `control_origin`, `preview_origin` and `allowed_hosts` to those `https://` names and
  restart. `docs/config.md` has the exact block.
- **A second machine.** `hub deploy` on the always-on box, `hub host add <name> <url>` for this one, and
  `allowed_hubs = ["<hub node name>"]` here.
- **Every key, rule and default.** `docs/config.md`. Build, release, upgrade: `AGENTS.md` and the ADRs.
