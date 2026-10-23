"""Real PTY smoke test. Run after cargo build; no model calls or third-party packages."""
import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time
import tomllib

BINARY = Path(os.environ.get('AI_LEAD_BINARY', Path(__file__).resolve().parents[1] / 'target/debug/ai-lead'))

class Terminal:
    def __init__(self, cwd, env=None):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
        self.process = subprocess.Popen([str(BINARY)], cwd=cwd, stdin=slave, stdout=slave, stderr=slave, start_new_session=True, env=env)
        os.close(slave)
        self.output = ''
    def send(self, data):
        os.write(self.master, data)
    def wait(self, text, timeout=10):
        deadline = time.monotonic() + timeout
        while text not in self.output and time.monotonic() < deadline:
            if select.select([self.master], [], [], .02)[0]:
                try:
                    self.output += os.read(self.master, 65536).decode('utf8', errors='replace')
                except OSError:
                    break
        assert text in self.output, f'Missing terminal text: {text}'
    def close(self):
        try:
            deadline = time.monotonic() + 4
            while self.process.poll() is None and time.monotonic() < deadline:
                if select.select([self.master], [], [], .02)[0]:
                    try:
                        self.output += os.read(self.master, 65536).decode('utf8', errors='replace')
                    except OSError:
                        break
            self.process.wait(timeout=1)
            assert self.process.returncode == 0
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            os.close(self.master)

with tempfile.TemporaryDirectory() as folder:
    root = Path(folder)
    terminal = Terminal(folder)
    terminal.wait('configure this project')
    assert not (root / '.ai-lead').exists()
    terminal.send(b'\x1b')
    terminal.close()
    assert not (root / '.ai-lead').exists(), 'Cancel must not initialize a project'

    terminal = Terminal(folder)
    terminal.wait('configure this project')
    terminal.send(b'\r')
    terminal.wait('agent setup')
    config = root / '.ai-lead/team.toml'
    assert tomllib.loads(config.read_text())['setup_pending'] is True
    terminal.send(b'\x1b')
    terminal.close()

    terminal = Terminal(folder)
    terminal.wait('configure this project')
    terminal.send(b'\r')
    terminal.wait('agent setup')
    terminal.send(b'\x13')
    terminal.wait('AI LEAD / new session')
    terminal.wait('Live / Full access')
    assert tomllib.loads(config.read_text())['setup_pending'] is False
    for path in ['contexts/lead.md', 'skills/lead.md']:
        assert (root / '.ai-lead' / path).is_file()
    terminal.send(b'\x1bOQ')  # F2: Live write -> Demo, never call a real provider.
    terminal.wait('Demo / no model calls')
    terminal.send(b'Build a demo project\r')
    terminal.wait('reports /')
    terminal.wait('Approved', timeout=15)
    terminal.send(b'p')
    terminal.wait('PAUSED')
    terminal.send(b'mpq')
    terminal.close()
    assert (root / '.ai-lead/demo-memory/state.json').is_file()
    sessions = list((root / '.ai-lead/runs').iterdir())
    assert len(sessions) == 1
    assert (sessions[0] / 'events.jsonl').is_file()
    before = config.read_bytes()
    terminal = Terminal(folder)
    terminal.wait('AI LEAD / new session')
    terminal.send(b'\x1b')
    terminal.close()
    assert config.read_bytes() == before

    # Exercise consent with a local CLI fixture, never a paid model.
    bin_dir = root / 'bin'
    bin_dir.mkdir()
    provider = bin_dir / 'codex'
    provider.write_text((Path(__file__).parent / 'fixtures/provider.sh').read_text())
    provider.chmod(0o755)
    env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ.get('PATH', ''))
    terminal = Terminal(folder, env)
    terminal.wait('AI LEAD / new session')
    terminal.send(b'Build integration\r')
    terminal.wait('Type ALLOW')
    assert not (root / '.ai-lead/mock-calls.log').exists()
    terminal.send(b'\x1b')
    terminal.close()
    assert not (root / '.ai-lead/permissions.json').exists()

    terminal = Terminal(folder, env)
    terminal.wait('AI LEAD / new session')
    terminal.send(b'Build integration\r')
    terminal.wait('Type ALLOW')
    terminal.send(b'ALLOW\r')
    terminal.wait('Approved')
    terminal.send(b'q')
    terminal.close()
    assert (root / '.ai-lead/permissions.json').is_file()

    terminal = Terminal(folder, env)
    terminal.wait('AI LEAD / new session')
    terminal.send(b'Build integration\r')
    terminal.wait('Approved')
    assert 'Type ALLOW' not in terminal.output
    terminal.send(b'q')
    terminal.close()
    subprocess.run([str(BINARY), '--project', folder, 'permissions', 'restricted'], check=True, capture_output=True)
    assert not (root / '.ai-lead/permissions.json').exists()
    print('PTY: current-directory setup, cancellation, pending setup, task launch, live dashboard, persistence, reopening, consent denial/acceptance/reuse and revocation OK')
