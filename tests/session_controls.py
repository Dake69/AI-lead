"""Real terminal test for live model changes and durable follow-ups; local fake provider only."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib

# Reuse the PTY helper without executing the separate onboarding scenarios.
exec((Path(__file__).parent / 'terminal_flow.py').read_text().split('with tempfile.TemporaryDirectory()')[0])

def wait_file(path, predicate=lambda text: bool(text), timeout=12):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        active = globals().get('terminal')
        if active and select.select([active.master], [], [], .01)[0]:
            try:
                active.output += os.read(active.master, 65536).decode('utf8', errors='replace')
            except OSError:
                pass
        if path.exists() and predicate(path.read_text()):
            return
        time.sleep(.02)
    raise AssertionError(f'PTY tail: {globals().get("terminal").output[-6000:]}\nTimed out waiting for {path}: {path.read_text() if path.exists() else "missing"}')

PROVIDER = r'''#!/bin/sh
set -eu
if [ "${1-}" = app-server ]; then
    read -r init
    printf '%s\n' '{"id":1,"result":{}}'
    read -r initialized
    read -r request
    printf '%s\n' '{"id":2,"result":{"data":[{"model":"backend-next","displayName":"Server Craft","description":"Backend specialist fixture"},{"model":"lead-next","displayName":"Lead Genius","description":"Coordinator fixture","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"xhigh"}]}],"nextCursor":null}}'
    while read -r unused; do :; done
    exit 0
fi
out=''
model=default
prior=''
for arg in "$@"; do
    if [ "$prior" = --output-last-message ]; then out="$arg"; fi
    if [ "$prior" = --model ]; then model="$arg"; fi
    prior="$arg"
done
prompt=$(cat)
role=lead
case "$prompt" in *'You are backend. Lead:'*) role=backend ;; esac
root=.ai-lead/controls
mkdir -p "$root"
printf '%s:%s\n' "$role" "$model" >> "$root/calls"
count=0
if [ -f "$root/$role-count" ]; then count=$(cat "$root/$role-count"); fi
count=$((count + 1))
printf '%s\n' "$count" > "$root/$role-count"
if [ "$role" = lead ]; then
    action=delegate
    assignments='[{"agent":"backend","task":"Implement backend scope only","files":["server/"],"depends_on":[]}]'
    summary=DELEGATED
    case "$count" in
        2) case "$prompt" in *LIVE_USER_FOLLOWUP*) ;; *) echo 'live user input missing' >&2; exit 1 ;; esac ;;
        3) action=finish; assignments='[]'; summary=FIRST_DELIVERY ;;
        4) case "$prompt" in *AFTER_DELIVERY_FOLLOWUP*) ;; *) echo 'post-delivery user input missing' >&2; exit 1 ;; esac ;;
        5) action=finish; assignments='[]'; summary=SECOND_DELIVERY ;;
    esac
    result="{\"action\":\"$action\",\"summary\":\"$summary\",\"assignments\":$assignments,\"messages\":[],\"brief\":\"Stable server-only scope\"}"
else
    if [ "$count" -eq 1 ]; then
        : > "$root/backend-started"
        remaining=1000
        while [ ! -f "$root/release-backend" ]; do
            remaining=$((remaining - 1)); test "$remaining" -gt 0 || exit 1
            sleep .02
        done
    fi
    result="{\"summary\":\"Backend delivery $count\",\"messages\":[]}"
fi
printf '%s\n' "$result" > "$out"
printf '{"type":"turn.completed"}\n'
'''

with tempfile.TemporaryDirectory(prefix='ai-lead-controls-') as folder:
    root = Path(folder)
    subprocess.run([str(BINARY), '--project', folder, 'init'], check=True, capture_output=True)
    path = root / '.ai-lead/team.toml'
    path.write_text(path.read_text().replace('setup_pending = true', 'setup_pending = false').replace('permissions = "full-access"', 'permissions = "restricted"'))
    bin_dir = root / 'bin'
    bin_dir.mkdir()
    provider = bin_dir / 'codex'
    provider.write_text(PROVIDER)
    provider.chmod(0o755)
    env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ.get('PATH', ''))
    terminal = Terminal(folder, env)
    try:
        terminal.wait('AI LEAD / new session')
        terminal.send(b'Build the server\r')
        wait_file(root / '.ai-lead/controls/backend-started', lambda _: True)
        terminal.wait('Message to lead')
        terminal.wait('Model / provider')
        terminal.send(b'iLIVE_USER_FOLLOWUP: add validation\r')
        user_path = next((root / '.ai-lead/runs').glob('*/team')) / 'user.jsonl'
        wait_file(user_path, lambda text: 'LIVE_USER_FOLLOWUP' in text)
        # Select backend, open its model directly, then switch to lead inside the editor.
        terminal.send(b'\t\t\t\x1bOQ')
        terminal.wait('models / providers')
        terminal.wait('Craft')
        terminal.send(b'Server Craft\r\x13')
        wait_file(path, lambda text: 'model = "backend-next"' in text)
        terminal.send(b'\x1b[1;3A' * 3 + b'\rLead Genius\r\t\rExtra high\r\x13')
        wait_file(path, lambda text: 'model = "lead-next"' in text)
        saved = tomllib.loads(path.read_text())
        assert saved['leader']['model'] == 'lead-next'
        assert saved['leader']['reasoning'] == 'xhigh'
        assert next(a for a in saved['agents'] if a['name'] == 'backend')['model'] == 'backend-next'
        # Saving applies before the editor is closed, while coordinator keeps running.
        (root / '.ai-lead/controls/release-backend').touch()
        wait_file(root / '.ai-lead/controls/calls', lambda text: 'backend:backend-next' in text)
        terminal.send(b'\x1b')
        terminal.wait('Settings saved')
        terminal.wait('FIRST_DELIVERY', timeout=18)
        # A completed session stays open and accepts another prompt, in the same workflow.
        terminal.send(b'iAFTER_DELIVERY_FOLLOWUP: add pagination\r')
        terminal.wait('SECOND_DELIVERY', timeout=18)
        calls = (root / '.ai-lead/controls/calls').read_text().splitlines()
        assert calls == ['lead:default', 'backend:default', 'lead:lead-next', 'backend:backend-next', 'lead:lead-next', 'lead:lead-next', 'backend:backend-next', 'lead:lead-next'], calls
        state = json.loads((root / '.ai-lead/memory/state.json').read_text())
        assert len(state['workflows']) == 1
        workflow = state['workflows'][state['active']]
        assert workflow['status'] == 'complete'
        messages = [item['message'] for item in workflow['transcript'] if item.get('from') == 'user']
        assert len(messages) == 2, messages
        assert 'LIVE_USER_FOLLOWUP' in messages[0] and 'AFTER_DELIVERY_FOLLOWUP' in messages[1]
        terminal.send(b'q')
        terminal.close()
    except BaseException:
        print(terminal.output[-3000:])
        try:
            terminal.send(b'\x03\x03')
            terminal.close()
        except BaseException:
            if terminal.process.poll() is None:
                terminal.process.kill()
                terminal.process.wait()
        raise
    print('PTY: per-agent live model editing, next-call models, in-flight follow-up, post-delivery continuation, one persistent workflow OK')
