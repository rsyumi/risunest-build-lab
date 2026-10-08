"""Loopback WebDAV server and inputs for the Mac external storage and backup file round trips."""
import json
import os
import hashlib
import secrets
import shutil
import socket
import ssl
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

PHASES = ('external-storage', 'external-storage-restart')
SYNC_PHASES = ('external-sync', 'external-sync-restart')
FILE_PHASES = ('backup-file', 'backup-file-restart')
GROUPS = (PHASES, SYNC_PHASES, FILE_PHASES)
DEPENDENCIES = {'external-storage-restart': 'external-storage', 'external-sync-restart': 'external-sync',
                'backup-file-restart': 'backup-file'}
ROOT_FOLDER = 'RisuNest'
SYSTEM_KEYCHAIN = '/Library/Keychains/System.keychain'


def issue_loopback_certificate(directory):
    """Writes a fresh CA and a 127.0.0.1 server certificate it signs; the subjects are unique per run."""
    tag = secrets.token_hex(4)
    (directory / 'ca.cnf').write_text(
        '[req]\ndistinguished_name=dn\nx509_extensions=v3_ca\nprompt=no\n'
        f'[dn]\nCN=RisuNest synthetic loopback CA {tag}\n'
        '[v3_ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n')
    (directory / 'server.ext').write_text(
        'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\n'
        'extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n'
        'subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n')

    def openssl(*arguments):
        result = subprocess.run(['openssl', *arguments], cwd=directory, capture_output=True, text=True)
        if result.returncode != 0:
            raise RuntimeError(f'openssl {arguments[0]} failed: {result.stderr.strip()}')
    openssl('req', '-x509', '-new', '-newkey', 'rsa:2048', '-nodes', '-sha256', '-days', '30',
            '-keyout', 'ca.key', '-out', 'ca.pem', '-config', 'ca.cnf')
    openssl('req', '-new', '-newkey', 'rsa:2048', '-nodes', '-sha256', '-keyout', 'server.key',
            '-out', 'server.csr', '-subj', f'/CN=127.0.0.1 {tag}')
    openssl('x509', '-req', '-in', 'server.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key',
            '-set_serial', str(secrets.randbits(63)), '-days', '30', '-sha256',
            '-extfile', 'server.ext', '-out', 'server.pem')
    (directory / 'ca.key').unlink()
    return directory / 'ca.pem', directory / 'server.pem', directory / 'server.key'


def file_markers():
    return {'before': f'file-before-{secrets.token_hex(6)}', 'after': f'file-after-{secrets.token_hex(6)}'}


class ExternalStorageSession:
    def __init__(self, rclone, artifacts, scheme='https'):
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
        self.scheme = scheme
        self.process = None
        self.log = None
        self.endpoint = None
        self.trusted = None
        self.probe_context = None

    def emit(self, payload):
        print(f'external-env: {json.dumps(payload, sort_keys=True)}', flush=True)

    def start(self):
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            port = probe.getsockname()[1]
        self.endpoint = f'{self.scheme}://127.0.0.1:{port}'
        tls = []
        if self.scheme == 'https':
            certificates = self.root / 'tls'
            certificates.mkdir()
            authority, certificate, key = issue_loopback_certificate(certificates)
            # The app verifies the server through the system trust store, so the run's CA is trusted there.
            subprocess.run(['sudo', 'security', 'add-trusted-cert', '-d', '-r', 'trustRoot', '-p', 'ssl',
                            '-k', SYSTEM_KEYCHAIN, str(authority)], check=True, timeout=120)
            self.trusted = authority
            self.probe_context = ssl.create_default_context(cafile=str(authority))
            tls = ['--cert', str(certificate), '--key', str(key)]
        self.log = (self.artifacts / 'webdav-server.log').open('w')
        # rclone reads flags from RCLONE_<FLAG>, which keeps the password off the command line.
        environment = {**os.environ, 'RCLONE_USER': self.user, 'RCLONE_PASS': self.password}
        (self.root / 'served').mkdir()
        self.process = subprocess.Popen([str(self.rclone), 'serve', 'webdav', str(self.root / 'served'),
                                         '--addr', f'127.0.0.1:{port}', '--config', str(self.root / 'rclone.conf'), *tls],
                                        env=environment, stdout=self.log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError(f'WebDAV server exited {self.process.returncode} before listening')
            try:
                urllib.request.urlopen(self.endpoint + '/', timeout=5, context=self.probe_context)
                raise RuntimeError('WebDAV server accepted an unauthenticated request')
            except urllib.error.HTTPError as error:
                if error.code == 401:
                    self.emit({'stage': 'server-reachable', 'passed': True, 'status': 401, 'scheme': self.scheme})
                    return
                raise RuntimeError(f'unexpected WebDAV response {error.code}') from None
            except (urllib.error.URLError, ConnectionError, TimeoutError):
                time.sleep(0.3)
        raise RuntimeError('WebDAV server did not listen')

    def before(self, phase):
        if phase in ('external-storage', 'external-sync'):
            return {'webdav': {'endpoint': self.endpoint, 'accountId': self.user, 'password': self.password,
                               'root': ROOT_FOLDER},
                    'before': self.markers['before'], 'after': self.markers['after']}
        # A relaunch uses the stored connection and secret, never fresh credentials.
        if phase == 'external-sync-restart':
            return {'before': self.markers['before'], 'after': self.markers['after']}
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
            if self.trusted is not None:
                digest = hashlib.sha1(ssl.PEM_cert_to_DER_cert(self.trusted.read_text())).hexdigest().upper()
                for command in (['sudo', 'security', 'remove-trusted-cert', '-d', str(self.trusted)],
                                ['sudo', 'security', 'delete-certificate', '-Z', digest, SYSTEM_KEYCHAIN]):
                    try:
                        subprocess.run(command, check=False, timeout=60)
                    except subprocess.TimeoutExpired:
                        pass
            shutil.rmtree(self.root, ignore_errors=True)
