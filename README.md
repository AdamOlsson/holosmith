# Holosmith

Holosmith runs coding agents in isolated Docker containers on a machine you own, and you drive them from a browser —
the same page on a laptop and on a phone. Each agent gets its own git worktree, its own network and its own
transcript; you review the diff and land it. Provider keys never enter a container.

Requirements: Linux with systemd (240 or newer for a promotion's job), Docker, git 2.38 or newer, and a provider key —
[what each one is for](docs/requirements.md).

## Quick start

```bash
holosmith install                # the tree, the two images, the unit; waits for /api/health
holosmith key set opencode-go    # paste the key, then Enter
holosmith project add https://github.com/you/project
```

The last command clones the repository into the projects root and prints the name the new-session form takes
(`project`). Open <http://localhost:8080>, type that name in **New session**, press **Start session**.

If Docker says permission denied, your account is not in the `docker` group. Run this, log back in, and repeat the
install:

```bash
sudo usermod -aG docker $USER
```

No `sudo` is needed for the install itself: it belongs to your own account, and everything lives in
`~/.local/share/holosmith`.

## Next steps

- [The agent cannot reach the Internet](docs/egress.md) — off by default, though the toolchain, this tree's crates and
  the web app's packages are in the image; a new dependency is what an allow-list entry is for.
- [Use it from your phone](docs/remote-access.md) — the LAN, the tailnet, and where TLS comes from.
- [Drive several machines from one page](docs/hub.md) — a hub and its host registry.
- [Rotate a tool key](docs/keys.md) — store a new one and prove the host accepts it.
- [Something broke](docs/troubleshooting.md) — start with `holosmith doctor`.
- [Configuration](docs/config.md), [architecture](docs/architecture.md), [glossary](GLOSSARY.md),
  [decisions](docs/adr/).

## Everyday commands

```bash
holosmith status                     # role, version, uptime, sessions, proxies
holosmith logs -f                    # journalctl --user -u holosmith.service
holosmith doctor --full              # every check, including the network self-test
holosmith uninstall [--purge]        # unit, proxies, networks; --purge takes the data root too
```

## Building from source

Rust 1.100+, Node.js and npm for `web/`, and the musl cross linker on `PATH` — or none: `scripts/build.sh` takes it
from `PATH` first, then from this tree's `toolchains/`, and fetches the pinned tarball into `toolchains/` when neither
has one. It writes `dist/<triple>/holosmith` with the `.sha256` a promotion reads beside it, and
`scripts/build.sh --profile dev` writes a debug binary to `target/<triple>/dev/holosmith` without touching `dist/`.
`scripts/verify.sh` is the gate, and `AGENTS.md` has the repository rules. Put the artifact in
a directory on your `PATH` — the quick start uses `~/.local/bin`.
