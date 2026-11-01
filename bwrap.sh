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

MOAT=(
    --ro-bind   $HOME/.config/agent-moat/skills/code-review     $AGENT_HOME/.agents/skills/code-review
    --ro-bind   $HOME/.config/agent-moat/skills/grill-with-docs $AGENT_HOME/.agents/skills/grill-with-docs
    --ro-bind   $HOME/.config/agent-moat/skills/domain-modeling $AGENT_HOME/.agents/skills/domain-modeling
    --ro-bind   $HOME/.config/agent-moat/skills/grilling        $AGENT_HOME/.agents/skills/grilling
)

ENVVARS=(
  --setenv HOME  "$AGENT_HOME"
  --setenv TMPDIR /tmp
  --setenv PATH  "$AGENT_HOME/android-sdk/platform-tools/adb:$AGENT_HOME/.dotnet:$AGENT_HOME/.pi:$AGENT_HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"
)

exec bwrap \
  "${BASE[@]}" "${NETWORK[@]}" "${ENVVARS[@]}" \
  "${PI[@]}" \
  "${MOAT[@]}" \
  --bind "$PWD" "$PWD" \
  --chdir "$PWD" \
  /bin/bash "$@" 2>&1
