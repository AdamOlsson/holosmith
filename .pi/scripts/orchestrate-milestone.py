#!/usr/bin/env python3
"""Drain one Linear milestone of the Holosmith project, one ticket at a time.

For each open, unblocked issue in the named milestone this runs

    pi -p --approve "/orchestrate <TICKET> [notes]"

from the repository root and waits for that process to exit before it looks at
the next issue. What counts as done is Linear's answer, not pi's exit status: a
ticket is finished when its state type is `completed`, and a ticket that is
still open after its run is a stop, not something to retry.

The issue list comes from the Linear MCP server pi is already logged in to
(`https://mcp.linear.app/mcp`), using the OAuth token pi stores in
`mcp-auth.json`, so the script needs no key of its own. An expired session is a
sentence asking for `pi mcp login linear`, never a silent empty plan.

Only the Holosmith project (P-HOL-2) is read, and only through that server.

Exits 0 when nothing in the milestone is left open, 1 when the run stopped with
work left, and 2 on a setup or query problem.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import TextIO

PROJECT = "Holosmith"
MCP_SERVER_PREFIX = "mcp__linear|"
MCP_PROTOCOL_VERSION = "2025-06-18"

#: Linear state types that are the end of the road for a milestone item.
TERMINAL = frozenset({"completed", "canceled", "duplicate"})

#: Fields the MCP server accepts on `list_issues` / `get_issue`.
LIST_FIELDS = ["id", "title", "status", "statusType", "projectMilestone", "priority", "createdAt", "url"]
BLOCKER_FIELDS = ["id", "status", "statusType"]


class SetupError(Exception):
    """Something about this machine or the invocation is wrong; exit 2."""


class LinearError(Exception):
    """The Linear MCP server refused or could not answer."""


class AuthRefused(LinearError):
    """HTTP 401: the access token this request carried is not accepted."""


# --------------------------------------------------------------------------
# Linear, over the MCP streamable-HTTP endpoint
# --------------------------------------------------------------------------


def pi_agent_dir() -> Path:
    override = os.environ.get("PI_CODING_AGENT_DIR")
    return Path(override).expanduser() if override else Path.home() / ".pi" / "agent"


def load_linear_session(auth_file: Path) -> tuple[str, str]:
    """Return (url, access token) for the Linear MCP server from pi's store."""
    path = auth_file
    try:
        store = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SetupError(
            f"no pi MCP store at {path}: log in once with `pi mcp login linear`, then re-run"
        ) from None
    except (OSError, ValueError) as error:
        raise SetupError(f"cannot read {path}: {error}") from None
    if not isinstance(store, dict):
        raise SetupError(f"{path} is not a JSON object")
    for key, entry in store.items():
        if not key.startswith(MCP_SERVER_PREFIX) or not isinstance(entry, dict):
            continue
        token = (entry.get("tokens") or {}).get("access_token")
        url = entry.get("serverUrl") or key[len(MCP_SERVER_PREFIX) :]
        if token and url:
            # The endpoint is a value from a file, so only a web origin is
            # accepted: urllib would happily read a `file://` URL.
            if not str(url).startswith(("https://", "http://")):
                raise SetupError(f"{path} names a non-HTTP Linear endpoint ({url})")
            return str(url), str(token)
    raise SetupError(
        f"{path} holds no access token for the Linear MCP server: "
        "log in once with `pi mcp login linear`, then re-run"
    )


class Mcp:
    """The two JSON-RPC calls this script needs, over streamable HTTP.

    The token is read from pi's store at start and re-read when a request is
    refused: a run of this script lasts hours, and pi (in the child process it
    starts) is what refreshes that token while it goes. Holding only the token
    read at startup would 401 every request after pi's first refresh.
    """

    def __init__(self, auth_file: Path) -> None:
        self.auth_file = auth_file
        self.url, self.token = load_linear_session(auth_file)
        self.session_id: str | None = None
        self.next_id = 0

    def _reload(self) -> bool:
        """Pick up a token pi has refreshed since this process started."""
        previous = self.token
        self.url, self.token = load_linear_session(self.auth_file)
        return self.token != previous

    def _post(self, message: dict) -> tuple[str, str]:
        headers = {
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
            "authorization": f"Bearer {self.token}",
            "mcp-protocol-version": MCP_PROTOCOL_VERSION,
        }
        if self.session_id:
            headers["mcp-session-id"] = self.session_id
        request = urllib.request.Request(
            self.url,
            data=json.dumps(message).encode("utf-8"),
            headers=headers,
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                self.session_id = response.headers.get("mcp-session-id") or self.session_id
                return response.headers.get_content_type(), response.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as error:
            if error.code == 401:
                raise AuthRefused(
                    "the Linear MCP session was refused (HTTP 401): "
                    "re-authorise with `pi mcp login linear`, then re-run"
                ) from None
            detail = error.read().decode("utf-8", "replace").strip()[:400]
            raise LinearError(f"Linear MCP answered HTTP {error.code}: {detail}") from None
        except urllib.error.URLError as error:
            raise LinearError(f"cannot reach {self.url}: {error.reason}") from None

    @staticmethod
    def _sse_payloads(body: str) -> list[str]:
        """The `data:` payloads of an SSE body, one string per event."""
        payloads: list[str] = []
        buffered: list[str] = []
        for line in body.splitlines():
            if not line:
                if buffered:
                    payloads.append("\n".join(buffered))
                    buffered = []
                continue
            if line.startswith(":"):
                continue
            if line.startswith("data:"):
                value = line[5:]
                buffered.append(value[1:] if value.startswith(" ") else value)
        if buffered:
            payloads.append("\n".join(buffered))
        return payloads

    def rpc(self, method: str, params: dict | None, expect_response: bool = True) -> dict | None:
        self.next_id += 1
        message: dict = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            message["params"] = params
        try:
            content_type, body = self._post(message)
        except AuthRefused:
            if not self._reload():
                raise
            content_type, body = self._post(message)
        if not expect_response:
            return None
        payloads = self._sse_payloads(body) if content_type == "text/event-stream" else [body]
        seen: list[dict] = []
        for payload in payloads:
            try:
                decoded = json.loads(payload)
            except ValueError:
                continue
            for item in decoded if isinstance(decoded, list) else [decoded]:
                if not isinstance(item, dict):
                    continue
                seen.append(item)
                if item.get("id") != self.next_id:
                    continue
                if "error" in item:
                    raise LinearError(f"{method}: {item['error'].get('message', item['error'])}")
                return item.get("result") or {}
        raise LinearError(f"the Linear MCP server sent no response to {method} ({len(seen)} messages read)")

    def hello(self) -> None:
        self.rpc(
            "initialize",
            {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "orchestrate-milestone", "version": "1"},
            },
        )
        self.rpc("notifications/initialized", None, expect_response=False)

    def tool(self, name: str, arguments: dict) -> dict:
        """Call one MCP tool and decode the JSON document it returns as text."""
        result = self.rpc("tools/call", {"name": name, "arguments": arguments}) or {}
        blocks = result.get("content") or []
        texts = [block.get("text", "") for block in blocks if isinstance(block, dict) and block.get("type") == "text"]
        body = "\n".join(text for text in texts if text).strip()
        if not body:
            raise LinearError(f"{name} answered no text")
        try:
            return json.loads(body)
        except ValueError:
            return {"text": body}


# --------------------------------------------------------------------------
# Milestone reading
# --------------------------------------------------------------------------


class Milestone:
    def __init__(self, mcp: Mcp, name: str) -> None:
        self.mcp = mcp
        self.name = name
        self.id: str = ""
        self.issues: list[dict] = []
        self.blockers: dict[str, dict] = {}

    # -- milestones ------------------------------------------------------

    def resolve(self) -> None:
        listing = self.mcp.tool("list_milestones", {"project": PROJECT})
        milestones = [m for m in listing.get("milestones") or [] if isinstance(m, dict)]
        if not milestones:
            raise SetupError(f"project {PROJECT} has no milestones")
        matches = [m for m in milestones if m.get("name") == self.name]
        if not matches:
            matches = [m for m in milestones if (m.get("name") or "").lower() == self.name.lower()]
        if not matches:
            matches = [m for m in milestones if self.name.lower() in (m.get("name") or "").lower()]
        if len(matches) != 1:
            names = "\n".join(f"  {m.get('name')}" for m in milestones)
            if not matches:
                raise SetupError(f"no milestone of {PROJECT} matches {self.name!r}. Milestones:\n{names}")
            raise SetupError(f"{self.name!r} matches {len(matches)} milestones; name one exactly:\n{names}")
        self.id = str(matches[0].get("id") or "")
        self.name = str(matches[0].get("name") or self.name)
        if not self.id:
            raise SetupError(f"milestone {self.name!r} has no id")

    # -- issues ----------------------------------------------------------

    def load_issues(self) -> None:
        issues: list[dict] = []
        cursor: str | None = None
        while True:
            arguments: dict = {"project": PROJECT, "limit": 250, "fields": LIST_FIELDS}
            if cursor:
                arguments["cursor"] = cursor
            page = self.mcp.tool("list_issues", arguments)
            for issue in page.get("issues") or []:
                if not isinstance(issue, dict):
                    continue
                milestone = issue.get("projectMilestone") or {}
                if milestone.get("id") == self.id:
                    issues.append(issue)
            cursor = page.get("cursor") if page.get("hasNextPage") else None
            if not cursor:
                break
        self.issues = issues

    def open_issues(self) -> list[dict]:
        return [i for i in self.issues if i.get("statusType") not in TERMINAL]

    def closure(self) -> list[dict]:
        return [i for i in self.issues if i.get("statusType") in {"canceled", "duplicate"}]

    # -- blockers --------------------------------------------------------

    def blocked_by(self, issue: dict) -> list[dict]:
        """The issues that still block `issue`, each with a fresh state."""
        identifier = str(issue.get("id"))
        detail = self.mcp.tool("get_issue", {"id": identifier, "includeRelations": True, "fields": ["id", "relations"]})
        relations = detail.get("relations") or {}
        open_blockers = []
        for blocker in relations.get("blockedBy") or []:
            if not isinstance(blocker, dict) or not blocker.get("id"):
                continue
            state = self.blocker_state(str(blocker["id"]))
            if state.get("statusType") in TERMINAL:
                continue
            open_blockers.append(state)
        return open_blockers

    def blocker_state(self, identifier: str) -> dict:
        cached = self.blockers.get(identifier)
        if cached and cached.get("statusType") in TERMINAL:
            return cached
        detail = self.mcp.tool("get_issue", {"id": identifier, "fields": BLOCKER_FIELDS})
        state = {
            "id": detail.get("id") or identifier,
            "status": detail.get("status") or "?",
            "statusType": detail.get("statusType") or "",
        }
        self.blockers[identifier] = state
        return state

    def plan(self) -> list[dict]:
        """Open issues in run order, each annotated with its open blockers."""
        ordered = sorted(
            self.open_issues(),
            key=lambda issue: (
                ((issue.get("priority") or {}).get("value") or 0) or 99,
                issue.get("createdAt") or "",
                issue.get("id") or "",
            ),
        )
        return [{"issue": issue, "blocked_by": self.blocked_by(issue)} for issue in ordered]


def describe(issue: dict) -> str:
    priority = (issue.get("priority") or {}).get("name") or "No priority"
    status = issue.get("status") or "?"
    title = issue.get("title") or ""
    return f"{issue.get('id')}  [{status} · {priority}]  {title}"


def blockers_line(blocked_by: list[dict]) -> str:
    if not blocked_by:
        return "runnable"
    parts = [f"{b['id']} ({b['status']})" for b in blocked_by]
    return "blocked by " + ", ".join(parts)


def compact(issue: dict) -> str:
    return f"{issue.get('id')}  [{issue.get('status') or '?'}]"


# --------------------------------------------------------------------------
# Running pi
# --------------------------------------------------------------------------


def repo_root() -> Path:
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError) as error:
        raise SetupError(f"not inside a git repository ({error})") from None
    root = Path(out)
    prompt = root / ".pi" / "prompts" / "orchestrate.md"
    if not prompt.is_file():
        raise SetupError(f"no orchestrate prompt at {prompt}: run this from the Holosmith repository")
    return root


def slugify(name: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
    return slug or "milestone"


def take_lock(root: Path) -> TextIO:
    """Hold an exclusive lock so two loops cannot drive the same tree."""
    path = root / "target" / "orchestrate-milestone.lock"
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = path.open("w")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as error:
        handle.close()
        if error.errno in (errno.EACCES, errno.EAGAIN):
            raise SetupError(f"another orchestrate-milestone run holds {path}") from None
        raise
    handle.write(f"{os.getpid()}\n")
    handle.flush()
    return handle


class Runner:
    def __init__(self, root: Path, log_dir: Path, notes: str, pi_bin: str) -> None:
        self.root = root
        self.log_dir = log_dir
        self.notes = notes
        self.pi_bin = pi_bin
        self.child: subprocess.Popen[str] | None = None

    def command(self, identifier: str) -> list[str]:
        prompt = f"/orchestrate {identifier}"
        if self.notes:
            prompt = f"{prompt} {self.notes}"
        return [
            self.pi_bin,
            "-p",
            # Trust this repository's project files for the run: it is how
            # .pi/prompts/orchestrate.md loads at all.
            "--approve",
            "-n",
            f"orchestrate {identifier}",
            prompt,
        ]

    def run(self, identifier: str) -> tuple[int, Path]:
        self.log_dir.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        log_path = self.log_dir / f"{identifier}-{stamp}.log"
        command = self.command(identifier)
        log = log_path.open("w", encoding="utf-8")
        log.write(f"# {time.strftime('%Y-%m-%d %H:%M:%SZ', time.gmtime())} {' '.join(command)}\n")
        log.flush()
        try:
            process = subprocess.Popen(
                command,
                cwd=str(self.root),
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                bufsize=1,
                # Its own session, so an interrupt can reach pi's whole tree
                # with one signal instead of leaving a grandchild behind.
                start_new_session=True,
            )
            self.child = process
            for line in process.stdout or ():
                log.write(line)
                log.flush()
                print(f"    | {line.rstrip()}", flush=True)
            status = process.wait()
            self.child = None
        finally:
            log.close()
        return status, log_path

    def stop(self) -> None:
        child = self.child
        if child is None:
            return
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(child.pid, sig)
            except OSError:
                try:
                    child.send_signal(sig)
                except OSError:
                    return
            try:
                child.wait(timeout=15)
                return
            except subprocess.TimeoutExpired:
                continue


# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="orchestrate-milestone",
        description=(
            "Run `/orchestrate <TICKET>` on every open, unblocked issue of one Holosmith "
            "milestone, one at a time, until the milestone is done."
        ),
        epilog=(
            "The issue list is read from the Linear MCP server pi is logged in to "
            "(`pi mcp login linear`). A ticket counts as done when Linear says `Done`; a run "
            "that leaves its ticket open stops the loop with work left unless --keep-going, "
            "and no ticket is attempted twice in one run. Every pi run is `pi -p --approve`, "
            "since that is what loads .pi/prompts/orchestrate.md."
        ),
    )
    parser.add_argument("milestone", nargs="?", help=f"a milestone name (or part of one) of the {PROJECT} project")
    parser.add_argument(
        "--notes",
        default="",
        help="extra instructions to append to every ticket id, as the /orchestrate prompt's ${@:2}",
    )
    parser.add_argument("--dry-run", action="store_true", help="print the order and stop")
    parser.add_argument(
        "--keep-going",
        action="store_true",
        help="carry on with the next unblocked ticket when a run leaves its ticket open",
    )
    parser.add_argument("--max-issues", type=int, default=0, metavar="N", help="stop after N tickets (0 = no limit)")
    parser.add_argument(
        "--log-dir",
        metavar="DIR",
        help="where each run's log goes (default: target/orchestrate-milestone/<milestone>)",
    )
    parser.add_argument(
        "--auth-file", metavar="PATH", help="pi's mcp-auth.json (default: <pi agent dir>/mcp-auth.json)"
    )
    parser.add_argument(
        "--pi-bin",
        default=os.environ.get("PI_BIN", "pi"),
        metavar="PATH",
        help="the pi executable to run (default: $PI_BIN or `pi`)",
    )
    return parser.parse_args(argv)


def auth_path(args: argparse.Namespace) -> Path:
    if args.auth_file:
        return Path(args.auth_file).expanduser()
    return pi_agent_dir() / "mcp-auth.json"


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        root = repo_root()
        mcp = Mcp(auth_path(args))
        mcp.hello()
        whoami = mcp.tool("whoami", {})
        workspace = whoami.get("workspace") or {}
        workspace_name = workspace.get("name") if isinstance(workspace, dict) else None
        milestone = Milestone(mcp, args.milestone or "")
        if not args.milestone:
            listing = mcp.tool("list_milestones", {"project": PROJECT})
            print(f"{PROJECT} milestones ({workspace_name or 'Linear'}):")
            for entry in listing.get("milestones") or []:
                print(f"  {entry.get('name')}  [{entry.get('progress', '?')}% done]")
            print("\nName one to run it.", file=sys.stderr)
            return 2
        milestone.resolve()
        milestone.load_issues()
        plan = milestone.plan()
    except (SetupError, LinearError) as error:
        print(f"orchestrate-milestone: {error}", file=sys.stderr)
        return 2

    runnable = [step for step in plan if not step["blocked_by"]]
    print(f"milestone: {milestone.name}")
    print(f"  {len(milestone.issues)} issues, {len(milestone.open_issues())} open, {len(runnable)} runnable")
    for step in plan:
        marker = " " if not step["blocked_by"] else "·"
        print(f"  {marker} {describe(step['issue'])}")
        if step["blocked_by"]:
            print(f"      {blockers_line(step['blocked_by'])}")
    print(flush=True)

    if args.dry_run:
        return 0
    if not runnable:
        return report(milestone, plan, [], [], 0)
    try:
        lock = take_lock(root)
    except SetupError as error:
        print(f"orchestrate-milestone: {error}", file=sys.stderr)
        return 2

    log_dir = (
        Path(args.log_dir).expanduser()
        if args.log_dir
        else root / "target" / "orchestrate-milestone" / slugify(milestone.name)
    )
    runner = Runner(root, log_dir, args.notes.strip(), args.pi_bin)
    attempted: list[str] = []
    done: list[str] = []
    failed: list[tuple[str, str]] = []
    status = 0
    notes = f" {args.notes.strip()}" if args.notes.strip() else ""

    def on_signal(signum: int, _frame: object) -> None:
        raise KeyboardInterrupt(f"signal {signum}")

    previous = {sig: signal.signal(sig, on_signal) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        while True:
            try:
                milestone.load_issues()
                plan = milestone.plan()
            except LinearError as error:
                print(f"orchestrate-milestone: {error}", file=sys.stderr)
                status = 1
                break
            candidates = [
                step["issue"] for step in plan if not step["blocked_by"] and step["issue"]["id"] not in attempted
            ]
            if not candidates:
                break
            issue = candidates[0]
            identifier = str(issue["id"])
            attempted.append(identifier)
            print(f"[{len(done) + len(failed) + 1}] {describe(issue)}", flush=True)
            print(f"    pi -p --approve \"/orchestrate {identifier}{notes}\"", flush=True)
            started = time.monotonic()
            code, log_path = runner.run(identifier)
            elapsed = int(time.monotonic() - started)
            try:
                state = milestone.mcp.tool("get_issue", {"id": identifier, "fields": BLOCKER_FIELDS})
            except LinearError as error:
                print(f"orchestrate-milestone: {error}", file=sys.stderr)
                status = 1
                break
            status_type = state.get("statusType") or ""
            label = state.get("status") or "?"
            if status_type == "completed":
                done.append(identifier)
                print(f"    {identifier}: {label} after {elapsed}s (log {log_path})", flush=True)
            elif status_type in {"canceled", "duplicate"}:
                done.append(identifier)
                print(f"    {identifier}: {label}, so out of scope (log {log_path})", flush=True)
            else:
                failed.append((identifier, str(label)))
                print(
                    f"    {identifier}: still {label} after {elapsed}s (pi exit {code}); "
                    f"log {log_path}",
                    flush=True,
                )
                if not args.keep_going:
                    status = 1
                    break
            if args.max_issues and len(done) + len(failed) >= args.max_issues:
                print(f"    stopping after {args.max_issues} ticket(s) (--max-issues)", flush=True)
                break
    except KeyboardInterrupt as interruption:
        runner.stop()
        status = 130
        print(
            f"\ninterrupted ({interruption}); a worktree and branch may still exist for the ticket above",
            file=sys.stderr,
        )
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        lock.close()

    try:
        milestone.load_issues()
        plan = milestone.plan()
    except LinearError as error:
        print(f"orchestrate-milestone: {error}", file=sys.stderr)
        status = status or 1
        return report(milestone, plan, done, failed, status, stale=True)
    return report(milestone, plan, done, failed, status)


def report(
    milestone: Milestone,
    plan: list[dict],
    done: list[str],
    failed: list[tuple[str, str]],
    status: int,
    stale: bool = False,
) -> int:
    open_now = milestone.open_issues()
    print()
    print(f"milestone: {milestone.name}")
    if done:
        print(f"  finished: {', '.join(done)}")
    if failed:
        for identifier, label in failed:
            print(f"  stopped on: {identifier} (still {label})")
    remaining = [step for step in plan if step["issue"]["id"] not in done]
    if not open_now:
        print("  every issue in the milestone is closed.")
        return status
    print(f"  {len(open_now)} still open:")
    for step in remaining:
        line = compact(step["issue"])
        if step["blocked_by"]:
            line = f"{line}  ({blockers_line(step['blocked_by'])})"
        print(f"    {line}")
    listed = {step["issue"]["id"] for step in plan}
    for issue in open_now:
        if issue["id"] not in listed and issue["id"] not in done:
            suffix = "  (state from the last read)" if stale else ""
            print(f"    {compact(issue)}{suffix}")
    if stale:
        print("  (Linear could not be re-read, so this list may be behind)")
    return status or 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
