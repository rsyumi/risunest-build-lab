"""Loopback WebDAV server and inputs for the Mac external storage backup round trip."""
import json
import os
import secrets
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

PHASES = ('external-storage', 'external-storage-restart')
DEPENDENCIES = {'external-storage-restart': 'external-storage'}
ROOT_FOLDER = 'RisuNest'


class ExternalStorageSession:
    def __init__(self, rclone, artifacts):
        self.rclone = Path(rclone).resolve(strict=True)
        self.artifacts = artifacts
        base = Path(os.environ.get('RUNNER_TEMP') or tempfile.gettempdir()).resolve(strict=True)
        # The served directory is a fresh physical path; /var is a symlink on macOS.
        self.root = Path(tempfile.mkdtemp(prefix='risunest-webdav-', dir=base)).resolve(strict=True)
        self.user = f'synthetic-{secrets.token_hex(4)}'
        self.password = secrets.token_urlsafe(24)
        if os.environ.get('GITHUB_ACTIONS') == 'true':
            print(f'::add-mask::{self.password}', flush=True)
        self.markers = {'before': f'webdav-before-{secrets.token_hex(6)}', 'after': f'webdav-after-{secrets.token_hex(6)}'}
        self.process = None
        self.log = None
        self.endpoint = None

    def emit(self, payload):
        print(f'external-env: {json.dumps(payload, sort_keys=True)}', flush=True)

    def start(self):
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            port = probe.getsockname()[1]
        self.endpoint = f'http://127.0.0.1:{port}'
        self.log = (self.artifacts / 'webdav-server.log').open('w')
        # rclone reads flags from RCLONE_<FLAG>, which keeps the password off the command line.
        environment = {**os.environ, 'RCLONE_USER': self.user, 'RCLONE_PASS': self.password}
        (self.root / 'served').mkdir()
        self.process = subprocess.Popen([str(self.rclone), 'serve', 'webdav', str(self.root / 'served'),
                                         '--addr', f'127.0.0.1:{port}', '--config', str(self.root / 'rclone.conf')],
                                        env=environment, stdout=self.log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError(f'WebDAV server exited {self.process.returncode} before listening')
            try:
                urllib.request.urlopen(self.endpoint + '/', timeout=5)
                raise RuntimeError('WebDAV server accepted an unauthenticated request')
            except urllib.error.HTTPError as error:
                if error.code == 401:
                    self.emit({'stage': 'server-reachable', 'passed': True, 'status': 401})
                    return
                raise RuntimeError(f'unexpected WebDAV response {error.code}') from None
            except (urllib.error.URLError, ConnectionError, TimeoutError):
                time.sleep(0.3)
        raise RuntimeError('WebDAV server did not listen')

    def before(self, phase):
        if phase == 'external-storage':
            return {'webdav': {'endpoint': self.endpoint, 'accountId': self.user, 'password': self.password,
                               'root': ROOT_FOLDER},
                    'before': self.markers['before'], 'after': self.markers['after']}
        # A relaunch uses the stored connection and secret, never fresh credentials.
        return {'before': self.markers['before']}

    def summarize(self):
        served = self.root / 'served'
        files = [path for path in served.rglob('*') if path.is_file()] if served.exists() else []
        shape = {'folders': sorted({path.relative_to(served).parts[0] for path in files}),
                 'files': len(files), 'bytes': sum(path.stat().st_size for path in files)}
        (self.artifacts / 'webdav-store.json').write_text(json.dumps(shape, indent=2))
        self.emit({'stage': 'remote-store', **shape})
        return shape

    def close(self):
        try:
            self.summarize()
        finally:
            if self.process is not None and self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=10)
            if self.log is not None:
                self.log.close()
            shutil.rmtree(self.root, ignore_errors=True)
