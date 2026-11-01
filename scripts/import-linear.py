#!/usr/bin/env python3
"""Import docs/stories/*.md into the Holosmith Linear project as issues.

The markdown in `docs/stories/` is the source of truth; Linear is a projection of
it. Re-running is safe: any story whose HS-id already exists in the project is
skipped, so this can be used to seed or extend, never to duplicate.

Auth comes from the pi MCP OAuth store (`~/.pi/agent/mcp-auth.json`) for the
Linear MCP server, so this must run somewhere pi has already authenticated
`linear`. No token is ever printed.

Usage:
    python3 scripts/import-linear.py --dry-run
    python3 scripts/import-linear.py
"""
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from typing import Any

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROJECT = 'Holosmith'          # scoped by AGENTS.md: no other project is in scope
TEAM = 'PER'                   # team Personal
AUTH = os.path.expanduser('~/.pi/agent/mcp-auth.json')
AUTH_KEY = 'mcp__linear|https://mcp.linear.app/mcp'
DRY = '--dry-run' in sys.argv

PHASES = [
    ('phase-0-walking-skeleton.md', 'Phase 0 — Walking skeleton'),
    ('phase-1-session-lifecycle-stub.md', 'Phase 1 — Session lifecycle (stub backend)'),
    ('phase-1b-docker-backend.md', 'Phase 1b — Docker backend'),
    ('phase-2-deployable-mvp.md', 'Phase 2 — Deployable MVP'),
    ('phase-3-multiple-machines.md', 'Phase 3 — Multiple machines'),
    ('phase-4-self-development.md', 'Phase 4 — Self-development'),
]
SIZES = {'XS': 'B1B1B1', 'S': '5E6AD2', 'M': 'F2994A', 'L': 'EB5757'}

_id = 0


def load_auth():
    """Read the Linear MCP OAuth credentials, failing with a usable message."""
    try:
        with open(AUTH) as fh:
            store = json.load(fh)
    except FileNotFoundError:
        sys.exit(f'No MCP credentials at {AUTH}. Authenticate `linear` in pi first.')
    except json.JSONDecodeError as ex:
        sys.exit(f'{AUTH} is not valid JSON ({ex}). Re-authenticate `linear` in pi.')
    entry = store.get(AUTH_KEY)
    if not entry:
        sys.exit(f'{AUTH} has no entry for the Linear MCP server. Authenticate `linear` in pi.')
    token = (entry.get('tokens') or {}).get('access_token')
    if not token:
        sys.exit('No Linear access token in the MCP store. Authenticate `linear` in pi.')
    expires_ms = entry.get('tokensExpireAt')
    if expires_ms and time.time() * 1000 > expires_ms:
        sys.exit('The Linear access token has expired. Re-authenticate `linear` in pi.')
    return token, entry['serverUrl']


def _rpc(method, params=None):
    """One JSON-RPC round trip against the Linear MCP server."""
    global _id
    token, url = load_auth()
    _id += 1
    payload = {'jsonrpc': '2.0', 'id': _id, 'method': method, 'params': params or {}}
    req = urllib.request.Request(
        url, data=json.dumps(payload).encode(), method='POST',
        headers={'Authorization': 'Bearer ' + token,
                 'Content-Type': 'application/json',
                 'Accept': 'application/json, text/event-stream',
                 'MCP-Protocol-Version': '2025-06-18'})
    try:
        raw = urllib.request.urlopen(req, timeout=120).read().decode('utf8', 'replace')
    except urllib.error.HTTPError as ex:
        detail = ex.read()[:500].decode('utf8', 'replace')
        raise RuntimeError(f'HTTP {ex.code}: {detail}') from ex
    except urllib.error.URLError as ex:
        raise RuntimeError(f'Cannot reach {url}: {ex.reason}') from ex
    events = []
    for line in raw.splitlines():
        if line.startswith('data: '):
            try:
                events.append(json.loads(line[6:]))
            except json.JSONDecodeError:
                pass
    if not events:
        try:
            events = [json.loads(raw)]
        except json.JSONDecodeError as ex:
            raise RuntimeError(f'Unparsable MCP response: {raw[:300]}') from ex
    return events[-1]


def call(tool, args, tries=4) -> Any:
    """Call one MCP tool, retrying transient failures. Returns the JSON payload."""
    for attempt in range(tries):
        try:
            res = _rpc('tools/call', {'name': tool, 'arguments': args})
            if 'error' in res:
                raise RuntimeError(json.dumps(res['error'])[:500])
            result = res['result']
            text = ''.join(c.get('text', '') for c in result.get('content', []))
            if result.get('isError'):
                raise RuntimeError(text[:500])
            return json.loads(text) if text.strip().startswith(('{', '[')) else text
        except Exception:
            if attempt == tries - 1:
                raise
            time.sleep(1.5 * (attempt + 1))


def parse_stories():
    """Turn the phase documents into issue payloads."""
    stories = []
    for fname, phase in PHASES:
        path = os.path.join(REPO, 'docs/stories', fname)
        try:
            with open(path) as fh:
                txt = fh.read()
        except OSError as ex:
            sys.exit(f'Cannot read {path}: {ex}')
        for block in re.split(r'^### ', txt, flags=re.M)[1:]:
            head, _, body = block.partition('\n')
            m = re.match(r'(HS-\d+) — (.+)', head)
            if not m:
                sys.exit(f'{fname}: story heading not in "HS-NNN — title" form: {head!r}')
            sid, title = m.group(1), m.group(2).strip()
            story = re.search(r'^\*\*As\*\* .*$', body, flags=re.M)
            size = re.search(r'\*\*Size:\*\* (\w+)', body)
            spec = re.search(r'\*\*Spec:\*\* (.*)$', body, flags=re.M)
            demo = re.search(r'\*\*Demo:\*\* (.*)$', body, flags=re.M)
            if story is None or size is None or spec is None or demo is None:
                missing = [n for n, hit in (('As', story), ('Size', size),
                                            ('Spec', spec), ('Demo', demo)) if hit is None]
                sys.exit(f'{fname}: {sid} is missing required field(s): {", ".join(missing)}')
            story_text = story.group(0).strip()
            size_text = size.group(1)
            blocker = re.search(r'\*\*Blocked by:\*\* (.*?) · \*\*Spec:\*\*', body)
            blocked = re.findall(r'HS-\d+', blocker.group(1)) if blocker else []
            spec_text = spec.group(1).strip()
            demo_text = demo.group(1).strip()
            acceptance = re.findall(r'^- \[ \] (.*)$', body, flags=re.M)
            notes = []
            nm = re.search(r'^\*\*Notes\*\*\n((?:- .*\n?)+)', body, flags=re.M)
            if nm:
                notes = [line[2:].strip() for line in nm.group(1).strip().splitlines()]

            lines = [story_text, '', f'**Demo:** {demo_text}', '', '**Acceptance**']
            lines += [f'- [ ] {a}' for a in acceptance]
            if notes:
                lines += ['', '**Notes**'] + [f'- {n}' for n in notes]
            lines += ['', '---',
                      f'**Phase:** {phase} · **Size:** {size_text}',
                      f'**Spec:** {spec_text}',
                      f'**Source:** `docs/stories/{fname}`']
            stories.append(dict(id=sid, title=title, size=size_text, blocked=blocked,
                                phase=phase, desc='\n'.join(lines)))
    return stories


def main():
    stories = parse_stories()
    print(f'parsed {len(stories)} stories from docs')

    existing = call('list_issues', {'project': PROJECT, 'limit': 250})
    ident = {}
    for issue in existing.get('issues', []):
        m = re.match(r'(HS-\d+)', issue.get('title', ''))
        if m:
            ident[m.group(1)] = issue['id']
    print(f'already in Linear: {len(ident)}')

    todo = [s for s in stories if s['id'] not in ident]
    print(f'to create: {len(todo)}')
    if DRY:
        for s in todo[:3]:
            print('-' * 60)
            print(s['id'], '|', s['title'], '|', s['size'], '| blocked by', s['blocked'])
            print(s['desc'][:600])
        return

    milestones = {m['name']: m['id'] for m in
                  call('list_milestones', {'project': PROJECT}).get('milestones', [])}
    for _, phase in PHASES:
        if phase not in milestones:
            milestones[phase] = call('save_milestone', {'project': PROJECT, 'name': phase})['id']
            print('+ milestone', phase)

    labels = {l['name'] for l in
              call('list_issue_labels', {'team': TEAM, 'limit': 100}).get('labels', [])}
    for size, colour in SIZES.items():
        if f'size-{size}' not in labels:
            call('save_issue_label', {'name': f'size-{size}', 'color': colour})
            print('+ label', f'size-{size}')

    for n, story in enumerate(todo, 1):
        args = {'title': f"{story['id']} — {story['title']}",
                'description': story['desc'],
                'team': TEAM,
                'project': PROJECT,
                'milestone': milestones[story['phase']],
                'addLabels': [f"size-{story['size']}"]}
        deps = [ident[b] for b in story['blocked'] if b in ident]
        if deps:
            args['blockedBy'] = deps
        created = call('save_issue', args)
        ident[story['id']] = created['id']
        print(f"  {story['id']} -> {created['id']}  ({n}/{len(todo)})")
    print(f'created {len(todo)} issues in project {PROJECT}')


if __name__ == '__main__':
    main()
