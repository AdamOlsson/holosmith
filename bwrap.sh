#!/usr/bin/env bash
set -euo pipefail

: "${OPENCODE_API_KEY:?OPENCODE_API_KEY is empty/unset - export it in the shell that runs this script}"
: "${HOME:?}"

AGENT_HOME=$HOME
AGENT_PATH=""

BASE=(
    --ro-bind /usr /usr
    --symlink usr/bin   /bin
    --symlink usr/lib   /lib
    --symlink usr/lib64 /lib64
    --symlink usr/sbin  /sbin
    --proc /proc --dev /dev --tmpfs /tmp
    --ro-bind-try /etc/passwd /etc/passwd 
    --ro-bind-try /etc/group /etc/group
    --unshare-all
    --die-with-parent
)

NETWORK=(
    --share-net
    --ro-bind-try /etc/resolv.conf     /etc/resolv.conf
    --ro-bind-try /etc/hosts           /etc/hosts
    --ro-bind-try /etc/nsswitch.conf   /etc/nsswitch.conf
    --ro-bind-try /etc/ssl             /etc/ssl
    --ro-bind-try /etc/pki             /etc/pki
    --ro-bind-try /etc/ca-certificates /etc/ca-certificates
)

PI=(
    --bind-try    "$HOME/.pi"         "$AGENT_HOME/.pi"
    # --ro-bind-try "$HOME/.agents"     "$AGENT_HOME/.agents"
    --ro-bind-try "$HOME/.local/bin"  "$AGENT_HOME/.local/bin"
    --setenv      PI_CODING_AGENT_DIR "$AGENT_HOME/.pi/agent"
    # --setenv      PATH                "$AGENT_HOME/.pi:$PATH"
)

# Playwright's Chromium, installed on the host by `npx playwright install chromium`.
# Run it with --no-sandbox: Chromium's own namespace sandbox cannot nest in this one.
BROWSER=(
    --ro-bind     "$HOME/.cache/ms-playwright" "$AGENT_HOME/.cache/ms-playwright"
    --bind        "$HOME/.cache/hs-browser"    "$AGENT_HOME/.cache/hs-browser"
    --ro-bind-try /etc/fonts                   /etc/fonts
    --setenv      PLAYWRIGHT_BROWSERS_PATH     "$AGENT_HOME/.cache/ms-playwright"
    --setenv      NODE_PATH                    "$AGENT_HOME/.cache/hs-browser/pw/node_modules"
    --setenv      XDG_CACHE_HOME               "$AGENT_HOME/.cache"
)

# `git push` over SSH: the config, the pinned host keys and the global identity come in
# read-only; the key itself does not. The host's ssh-agent socket is bound instead, so
# the sandbox can ask it to sign and never sees the private key.
GIT=(
    --ro-bind-try "$HOME/.ssh/config"      "$AGENT_HOME/.ssh/config"
    --ro-bind-try "$HOME/.ssh/known_hosts" "$AGENT_HOME/.ssh/known_hosts"
    --ro-bind-try "$HOME/.gitconfig"       "$AGENT_HOME/.gitconfig"
    --ro-bind-try /etc/ssh/ssh_config      /etc/ssh/ssh_config
    --ro-bind-try /etc/ssh/ssh_config.d    /etc/ssh/ssh_config.d
)

if [ -S "${SSH_AUTH_SOCK:-/nonexistent}" ]; then
  GIT+=(
    --bind   "$SSH_AUTH_SOCK" /run/ssh-agent.sock
    --setenv SSH_AUTH_SOCK    /run/ssh-agent.sock
  )
fi

# The Docker daemon: the host's socket is bound in at its canonical path, so `docker` inside the
# sandbox drives the real daemon (`/usr` already carries the CLI and its cli-plugins). This is
# host root by another name - anything that can reach the socket can start a container with `/`
# mounted - so it is granted deliberately, like the ssh-agent socket above. A machine without a
# daemon is the normal hub, so a missing socket is not an error. The socket is group-owned by
# `docker` on the host; the sandbox's supplementary groups are unmapped (they read as `nogroup`
# in `id`) but the kernel still matches the gids, so no group juggling is needed here.
DOCKER=()
for sock in /run/docker.sock "/run/user/$UID/docker.sock"; do
  if [ -S "$sock" ]; then
    DOCKER+=(--bind "$sock" /var/run/docker.sock)
    break
  fi
done

MOAT=(
    --ro-bind   $HOME/.config/agent-moat/skills/code-review     $AGENT_HOME/.agents/skills/code-review
    --ro-bind   $HOME/.config/agent-moat/skills/grill-with-docs $AGENT_HOME/.agents/skills/grill-with-docs
    --ro-bind   $HOME/.config/agent-moat/skills/domain-modeling $AGENT_HOME/.agents/skills/domain-modeling
    --ro-bind   $HOME/.config/agent-moat/skills/grilling        $AGENT_HOME/.agents/skills/grilling
)

mkdir -p "$HOME/.cache/hs-browser"
[ -d "$HOME/.cache/ms-playwright" ] || {
  echo "bwrap.sh: no browser to bind - run \`npx playwright install chromium\` on the host" >&2
  exit 1
}

ENVVARS=(
  --setenv HOME  "$AGENT_HOME"
  --setenv TMPDIR /tmp
  --setenv PATH  "$AGENT_HOME/android-sdk/platform-tools/adb:$AGENT_HOME/.dotnet:$AGENT_HOME/.pi:$AGENT_HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"
)

exec bwrap \
  "${BASE[@]}" "${NETWORK[@]}" "${ENVVARS[@]}" \
  "${PI[@]}" \
  "${BROWSER[@]}" \
  "${GIT[@]}" \
  "${DOCKER[@]}" \
  "${MOAT[@]}" \
  --bind "$PWD" "$PWD" \
  --chdir "$PWD" \
  /bin/bash "$@" 2>&1
