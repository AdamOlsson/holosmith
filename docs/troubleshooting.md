# Troubleshooting

Read this when something refuses, or when the unit will not stay up. [Back to README](../README.md).

## Is it serving?

```bash
systemctl --user status holosmith.service --no-pager
curl -fsS http://127.0.0.1:8080/api/health
```

`active (running)` and a JSON body is the machine working. `activating (auto-restart)` means the preflight refused:
`holosmith doctor --quick` and `journalctl --user -u holosmith -n 30` say why, and so does a failed deploy step.

A report is one line per step: `1. preflight ✓` … `6. health ✓`, then `url:` and
`next:`. The mark is the verdict — `✓` done or nothing to do, `–` this half has no work for the step, `!` still
yours to run, `✗` failed — a step that needs something keeps the commands you run, and a failed one adds a `fix:` line
with the command that puts it right. Add `--verbose` to `install`, `server deploy` or `hub deploy` for every step's
full sentence, the findings it counted, the install tree and the tally; that is the shape to read when you are
diagnosing the machine rather than setting it up. What a run prints *while* it works — `install: [3/6 images] …` — goes
to stderr, so `holosmith install > out` keeps the report alone.

## When something refuses

| What you see | What it means |
| --- | --- |
| `403 bad_host` | the `Host` sent is not in `allowed_hosts`: add the exact name. `localhost` and `127.0.0.1` always load, `[::1]` does not |
| `403 bad_origin` | a write whose `Origin` is not `control_origin`, character for character, port included — a scheme or port change needs a restart |
| `403 bad_client` | the request carried neither `X-Holosmith-Client: 1` nor a JSON content type: a stale tab, a script or `curl`, not the app |
| `403 bad_owner` | `owner` is set and no matching login header arrived: unset it, or point the proxy at the right login |
| `409 cap_reached` | `max_live_sessions` reached with nothing idle to evict: stop or delete a session |
| `507 disk_floor` | free space below `min_free_gb` on the data root: free space or lower the key, then restart |
| `422 git_not_directory` | the project has no `.git` directory, has submodules, or sets `extensions.worktreeConfig` |
| unit restart-loops on `perms` | something under the data root is not owned by its owner — a `sudo` wrote it. Fix it with the command below |
| session `error` at the container step | `docker ps`, `holosmith logs --session <id>` and `docker image ls` name it |
| a tool call fails at `mcp-proxy:8090` | the MCP proxy is not running (`holosmith status` says so, `doctor --full` names it) or the session has Internet access off: the proxy is joined by the egress proxy's condition, and `key set <tool>` is what stores the key it spends. A `fetch failed` for a server named `linear` is a repository's own `.pi/mcp.json`: it points at the public host, which a session cannot reach — the machine's server is `holosmith-linear` and a project's entry never replaces it |

## The `perms` restart loop

Something under the data root is not owned by the data root's owner — a file from an older install, or one a `sudo`
wrote before this build refused it. The next boot's `ExecStartPre` refuses, so the unit restarts forever:

```bash
sudo chown "$(id -u):$(id -g)" ~/.local/share/holosmith/data/secrets/keys.toml
```

## A session that never leaves `error`

```bash
docker ps
holosmith logs --session <id>
docker image ls
```

The product images are `holosmith-proxy:<version>` and `holosmith-base:<version>`. This project's own image is a
project's, so it is `hs-holosmith:<digest>` — the digest over `docker/Dockerfile` and the base image's tag — and
`scripts/project-image.sh tag` prints it.
