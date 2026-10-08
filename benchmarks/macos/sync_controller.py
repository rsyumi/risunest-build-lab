"""Loopback Sync server, device registrations and profile swaps for the Mac Sync round trip."""
import base64
import json
import os
import re
import secrets
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

PHASES = ('sync-publish', 'sync-receive', 'sync-pull')
DEPENDENCIES = {'sync-receive': 'sync-publish', 'sync-pull': 'sync-receive'}
KEYCHAIN_SERVICE = 'io.github.rsyumi.risunest.server-sync'
PROBE_SERVICE = 'io.github.rsyumi.risunest.harness-keychain-probe'
REGISTRATION_PREFIX = 'risunestlocal://sync-server/register#'
LIBRARY = Path.home() / 'Library'
# Preferences stay in place: cfprefsd caches them, and the product keeps no profile state there.
PROFILE_ROOTS = ['Application Support', 'Caches', 'WebKit', 'HTTPStorages', 'Logs', 'Saved Application State']
ID = re.compile(r'[A-Za-z0-9_-]{1,128}')
TOKEN = re.compile(r'[0-9a-fA-F]{64}')
READY = re.compile(r'sync listener ready: 127\.0\.0\.1:([0-9]+) \(local development\)')


class SyncEnvironmentError(RuntimeError):
    pass


def profile_entries():
    entries = set()
    for root in PROFILE_ROOTS:
        directory = LIBRARY / root
        if directory.is_dir():
            entries.update((root, entry.name) for entry in directory.iterdir() if 'risunest' in entry.name.lower())
    return entries


class SyncSession:
    def __init__(self, binary, artifacts):
        self.artifacts = artifacts
        self.secrets = []
        self.process = None
        self.server_log = None
        base = Path(os.environ.get('RUNNER_TEMP') or tempfile.gettempdir()).resolve(strict=True)
        # The server refuses data paths with a symlink in them, and /var is one on macOS.
        self.root = Path(tempfile.mkdtemp(prefix='risunest-sync-', dir=base)).resolve(strict=True)
        self.baseline = profile_entries()
        self.binary = Path(binary).resolve(strict=True)
        self.markers = {'a': f'sync-marker-a-{secrets.token_hex(6)}', 'b': f'sync-marker-b-{secrets.token_hex(6)}'}
        self.registrations = {}
        self.events = []
        self.phase = None

    def emit(self, label, payload):
        self.events.append({'label': label, **payload})
        print(f'{label}: {json.dumps(payload, sort_keys=True)}', flush=True)

    def stage(self, name, run):
        try:
            result = run() or {}
        except Exception as error:
            self.emit('sync-env', {'phase': self.phase, 'stage': name, 'passed': False, 'error': type(error).__name__, 'message': self.scrub(str(error))[:300]})
            raise SyncEnvironmentError(f'{name}: {self.scrub(str(error))[:300]}') from None
        self.emit('sync-env', {'phase': self.phase, 'stage': name, 'passed': True, **result})
        return result

    def secret(self, value):
        self.secrets.append(value)
        if os.environ.get('GITHUB_ACTIONS') == 'true':
            print(f'::add-mask::{value}', flush=True)

    def scrub(self, text):
        for value in sorted(self.secrets, key=len, reverse=True):
            text = text.replace(value, '<redacted>')
        return text

    def admin(self, *command):
        result = subprocess.run([str(self.binary), *command, '--data-dir', str(self.root / 'server')],
                                capture_output=True, text=True, timeout=120)
        if result.returncode != 0:
            raise RuntimeError(f'{" ".join(command)} exited {result.returncode}: {self.scrub(result.stderr.strip())[-200:]}')
        return result.stdout

    def start(self):
        self.stage('keychain', self.keychain_probe)
        self.stage('server-init', self.init)
        self.stage('registrations', self.issue)
        self.stage('server-start', self.serve)
        self.stage('server-reachable', self.probe)

    def init(self):
        self.admin('init')
        return {}

    def keychain_probe(self):
        # Cleanup removes every item of the product service, so it must start with none.
        if subprocess.run(['security', 'find-generic-password', '-s', KEYCHAIN_SERVICE],
                          capture_output=True, timeout=30).returncode == 0:
            raise RuntimeError('the login keychain already holds RisuNest sync credentials')
        account = f'probe-{secrets.token_hex(4)}'
        added = subprocess.run(['security', 'add-generic-password', '-s', PROBE_SERVICE, '-a', account, '-w', 'synthetic'],
                               capture_output=True, text=True, timeout=30)
        if added.returncode != 0:
            raise RuntimeError(f'login keychain refused a generic password ({added.returncode})')
        subprocess.run(['security', 'delete-generic-password', '-s', PROBE_SERVICE, '-a', account],
                       capture_output=True, timeout=30)
        return {'writable': True}

    def issue(self):
        library = None
        # One registration is one device credential, so each profile gets its own.
        for device in ('a', 'b'):
            output = self.admin('device', 'add')
            try:
                credential = json.loads(output)
            except json.JSONDecodeError:
                raise RuntimeError('device add did not print a credential') from None
            if set(credential) != {'deviceId', 'libraryId', 'token'}:
                raise RuntimeError('device add printed unexpected fields')
            for key in ('deviceId', 'libraryId'):
                self.secret(credential[key])
            self.secret(credential['token'])
            if not (ID.fullmatch(credential['deviceId']) and ID.fullmatch(credential['libraryId'])
                    and TOKEN.fullmatch(credential['token'])):
                raise RuntimeError('device add printed a malformed credential')
            if library not in (None, credential['libraryId']):
                raise RuntimeError('device credentials name different libraries')
            library = credential['libraryId']
            self.registrations[device] = credential
        return {'devices': len(self.registrations)}

    def serve(self):
        self.server_log = (self.root / 'server.log').open('w')
        self.process = subprocess.Popen([str(self.binary), 'serve', '--data-dir', str(self.root / 'server'),
                                         '--listen', '127.0.0.1:0'], stdout=self.server_log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError(f'server exited {self.process.returncode} before listening')
            match = READY.search((self.root / 'server.log').read_text(errors='replace'))
            if match:
                self.endpoint = f'http://127.0.0.1:{match.group(1)}'
                for device, credential in self.registrations.items():
                    payload = json.dumps({'endpoint': self.endpoint, **credential}, separators=(',', ':'))
                    encoded = base64.urlsafe_b64encode(payload.encode()).decode().rstrip('=')
                    self.secret(encoded)
                    self.registrations[device] = REGISTRATION_PREFIX + encoded
                return {'listener': 'loopback'}
            time.sleep(0.2)
        raise RuntimeError('server did not report a listener')

    def probe(self):
        if self.process is None or self.process.poll() is not None:
            raise RuntimeError('server is not running')
        try:
            urllib.request.urlopen(self.endpoint + '/head', timeout=10)
        except urllib.error.HTTPError as error:
            try:
                code = json.loads(error.read()).get('error')
            except (ValueError, AttributeError):
                code = None
            if error.code == 401 and code == 'unauthorized':
                return {'status': 401}
            raise RuntimeError(f'unexpected response {error.code}') from None
        raise RuntimeError('unauthenticated request was accepted')

    def settle(self):
        # WebKit services of the quit app can outlive it for a moment and still write its storage.
        time.sleep(3)

    def stash(self, name):
        self.settle()
        moved = 0
        for root, entry in sorted(profile_entries() - self.baseline):
            target = self.root / 'profiles' / name / root / entry
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(LIBRARY / root / entry), str(target))
            moved += 1
        if not moved:
            raise RuntimeError(f'profile {name} left no data to move aside')
        if profile_entries() - self.baseline:
            raise RuntimeError('profile data reappeared while it was moved aside')
        return moved

    def discard(self):
        self.settle()
        removed = 0
        for root, entry in sorted(profile_entries() - self.baseline):
            path = LIBRARY / root / entry
            if path.is_dir() and not path.is_symlink():
                shutil.rmtree(path)
            else:
                path.unlink()
            removed += 1
        return removed

    def restore(self, name):
        if profile_entries() - self.baseline:
            raise RuntimeError('restoring onto a profile that still holds data')
        source = self.root / 'profiles' / name
        restored = 0
        for root in sorted(source.iterdir()):
            for entry in sorted(root.iterdir()):
                os.rename(entry, LIBRARY / root.name / entry.name)
                restored += 1
        shutil.rmtree(source)
        return restored

    def before(self, phase):
        """Prepares the profile for `phase` and returns the inputs the page reads."""
        self.phase = phase
        if phase == 'sync-publish':
            self.stage('profile', self.fresh)
            inputs = {'registration': self.registrations['a'], 'marker': self.markers['a']}
        elif phase == 'sync-receive':
            self.stage('profile', lambda: {'movedAside': self.stash('a'), 'fresh': True})
            inputs = {'registration': self.registrations['b'], 'expect': self.markers['a'], 'marker': self.markers['b']}
        else:
            self.stage('profile', lambda: {'removed': self.discard(), 'restored': self.restore('a')})
            inputs = {'expect': self.markers['b']}
        self.stage('server-reachable', self.probe)
        return inputs

    def fresh(self):
        if profile_entries() - self.baseline:
            raise RuntimeError('profile is not fresh')
        return {'fresh': True}

    def summarize(self, phase):
        report = self.artifacts / f'{phase}.jsonl'
        if not report.exists():
            self.emit('sync-env', {'phase': phase, 'stage': 'report', 'passed': False, 'message': 'no page report'})
            return None
        failure = None
        for line in report.read_text().splitlines():
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            stage, result = record.get('stage'), record.get('result') or {}
            if stage == 'failure':
                detail = result.get('detail') or {}
                label = detail.get('label') if detail.get('label') in ('sync-env', 'sync-result') else 'sync-result'
                failure = {'phase': phase, 'label': label, 'step': detail.get('step'), 'passed': False,
                           'message': self.scrub(str(result.get('message')))[:300],
                           'detail': {key: value for key, value in detail.items() if key not in ('label', 'step')}}
                self.emit(label, failure)
            elif isinstance(stage, str) and stage.startswith(('sync-env:', 'sync-result:')):
                label, step = stage.split(':', 1)
                self.emit(label, {'phase': phase, 'step': step, **{k: v for k, v in result.items() if k not in ('label', 'phase', 'step')}})
            elif stage == phase:
                self.emit('sync-result', {'phase': phase, 'step': 'complete', **{k: v for k, v in result.items() if k != 'label'}})
        return failure

    def close(self):
        outcome = {}
        if self.process is not None:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=10)
            outcome['serverExit'] = self.process.returncode
        if self.server_log is not None:
            self.server_log.close()
            log = self.root / 'server.log'
            (self.artifacts / 'sync-server.log').write_text(self.scrub(log.read_text(errors='replace')))
        try:
            outcome['profileEntriesRemoved'] = self.discard()
        except OSError as error:
            outcome['profileCleanup'] = type(error).__name__
        removed = 0
        for service in (KEYCHAIN_SERVICE, PROBE_SERVICE):
            for _ in range(16):
                if subprocess.run(['security', 'delete-generic-password', '-s', service],
                                  capture_output=True, timeout=30).returncode != 0:
                    break
                removed += 1
        outcome['keychainItemsRemoved'] = removed
        shutil.rmtree(self.root, ignore_errors=True)
        for path in self.artifacts.rglob('*'):
            if path.is_file() and path.suffix in ('.log', '.jsonl', '.json', '.txt'):
                text = path.read_text(errors='surrogateescape')
                scrubbed = self.scrub(text)
                if scrubbed != text:
                    path.write_text(scrubbed, errors='surrogateescape')
        self.emit('sync-env', {'stage': 'cleanup', 'passed': True, **outcome})
        (self.artifacts / 'sync.json').write_text(self.scrub(json.dumps(self.events, indent=2)))
