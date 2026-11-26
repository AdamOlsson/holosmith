# Requirements

What a machine needs before it runs sessions, and what an install writes. Read this if you are preparing a machine,
or wondering why one dependency is there. [Back to README](../README.md).

What a **server** needs. A hub runs no sessions: it needs systemd and space for its own database, and `holosmith
install` on a `role = "hub"` config asks neither Docker nor git question.

| Dependency | Why |
| --- | --- |
| Linux with systemd (a promotion's job needs **240 or newer**) | `holosmith install` enables `holosmith.service` in your user manager, and linger is what starts it at boot; a promotion's job asks for `StandardOutput=append:` and `StandardError=append:`, which is systemd 240's spelling — a 239 does everything else, sessions included |
| Docker Engine | every session is a container on its own `--internal` network, with three proxies beside it: `hs_egress`, `hs_key` and `hs_mcp` |
| git 2.38 or newer | a session is a worktree plus an `agent/<id>` branch; merges use `git merge-tree --write-tree` |
| a provider key | `opencode-go` (the default), `anthropic`, `openai` and nine more — a name it does not know is refused with the full list |
| free space | `min_free_gb` (default **20** GB) on the data root: checked before a boot and before a create |
| openssl | the deploy generates the Web Push key pair with it, and the notification sender uses it |

Optional, for reaching the machine from other devices:

| Dependency | Why |
| --- | --- |
| Tailscale | the tailnet path: an HTTPS name, phone access from anywhere, machine-to-machine forwarding |

Nothing else is required: no Python or Node runtime, no database server, no reverse proxy for the local path.

## What an install does

`holosmith install` is the whole first run, and it is the command to know:

- **refuses before writing anything** when the account is not in the `docker` group, with the `usermod` as the last
  line — a machine whose account cannot open the socket could not run a session, so it is a refusal and not a
  half-finished install;
- runs the deploy for the half the config's `role` asks for: `server` and `both` install the server's, `hub` the
  hub's. The defaults — `role = "both"`, both listeners on loopback, `http://localhost:8080` as the control
  origin — need no config file at all;
- enables `~/.config/systemd/user/holosmith.service` through **your own** systemd manager and starts it;
- sets `loginctl enable-linger`, so the daemon starts at boot rather than only while you are logged in. On a machine
  whose polkit refuses that, the run says so, prints `sudo loginctl enable-linger $USER`, and carries on — the
  machine serves either way;
- waits for `/api/health`, then prints the control URL and the one next step.

Re-running changes nothing, and `holosmith install --dry-run` prints each step's plan and writes nothing. The first
real run builds `holosmith-proxy` and `agent-image` from the embedded recipes, which takes minutes; every later run
is a read. The run copies the binary it is running into the install tree's release directory and points the unit at
that copy, so where the download landed stops mattering — when this binary's tag can name a release directory; the
step's line says why, and names the file it ran instead, when it cannot.

The state is one directory, `~/.local/share/holosmith`, and the config is one file,
`~/.config/holosmith.toml`. `holosmith config show` prints the resolved config with every default,
`config validate` checks an edit and `config edit` opens it; `docs/config.md` documents every key. `holosmith key
list` prints which providers and tools have a key, never the values.

`min_free_gb` (default 20 GB) is the one value to fit to the machine — `doctor` refuses below it, and a refusing
`serve` is a unit that restart-loops.

## The provider key

`holosmith key set <provider>` reads the key from stdin only, never argv, and writes it to
`<data>/secrets/keys.toml` with mode 0600. On a terminal it asks for the key and hides it as you type, so Enter
is what finishes it; `printf '…' | holosmith key set <provider>` reads the same value to EOF, for a script. Run it
as yourself: the file's owner must be the data root's owner, or
`doctor`'s `perms` finding fails and the next boot's `ExecStartPre` refuses to start the unit. No restart is needed —
the key proxy reads the store per request, and the container only ever holds a per-session dummy.

A tool's credential lives in the same file: `holosmith key set <tool-id>` stores one — `linear` is the tool this build
knows — and `holosmith key revoke <tool-id>` forgets it. The MCP proxy (`hs_mcp`, reached by a session at
`http://mcp-proxy:8090/<tool>/<endpoint>` with the session's dummy key) spends it, and it is joined to a session's
network only when that session allows Internet access, so a tool's host is never reachable with the checkbox off
(ADR-0098, ADR-0099). [Rotating a tool key](keys.md) is the runbook for that credential, including the
`doctor --full` line that says whether the host accepts the stored one.

## Install versus deploy

`install` is `server deploy` (or `hub deploy`) plus the start: the deploy writes the tree, config, unit and proxies and
*prints* the two `systemctl --user` commands instead of running them. Use it for the steps without the start —
`--dry-run` plans the six, `--ssh user@host` runs it on another machine.

## Building from source

Rust 1.100+ (edition 2024) on `PATH` — `pins/rust.txt` holds the version, and `scripts/build.sh` refuses a compiler that
is not it — and Node.js and npm for `web/`. The one musl cross compiler the shipped target needs,
`x86_64-linux-musl-gcc`, is the tree's rather than the machine's: the script takes it from `PATH` when it is there,
then from this tree's `toolchains/`, and fetches the tarball `agent.Dockerfile` pins — checked against the recipe's
SHA-256 before anything is extracted — into `toolchains/` when neither has one (ADR-0120). SQLite is compiled from
source, so the C toolchain crosses too. `scripts/build.sh` builds the web app first and then the static artifact, which
lands in `dist/<triple>/holosmith` with its digest beside it; `scripts/verify.sh` is the gate. Put the artifact in a
directory on your `PATH`; the README's quick start uses `~/.local/bin`. `AGENTS.md` has the repository rules.
