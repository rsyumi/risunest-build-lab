"""External WebKitWebDriver runner. Only starts an isolated synthetic benchmark build."""
import argparse
import hashlib
import json
import importlib.util
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import urllib.error

parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--phase', choices=['persistence', 'reload', 'regex', 'appearance-seed', 'appearance-app'], required=True)
parser.add_argument('--app-theme', choices=['light', 'dark'])
parser.add_argument('--system-theme', choices=['light', 'dark'])
parser.add_argument('--capture-size')
parser.add_argument('--display-owner-pid', type=int)
parser.add_argument('--capture-codec-experiment', action='store_true',
                    help='Run diagnostic-only six-second FFV1/NUT and stream-copy/NUT controls')
parser.add_argument('--capture-codec-control', choices=['ffv1', 'copy'], help=argparse.SUPPRESS)
args = parser.parse_args()
if args.capture_codec_experiment and args.capture_codec_control:
    parser.error('Select the paired experiment without an individual control')
if args.capture_codec_experiment or args.capture_codec_control:
    if args.phase != 'appearance-app' or args.app_theme != 'light' or args.system_theme != 'light':
        parser.error('Codec diagnostics require appearance-app with light app and system themes')
    if not args.capture_size or not args.display_owner_pid:
        parser.error('Codec diagnostics require capture size and the owned Xvfb PID')
if args.phase.startswith('appearance-') and (not args.app_theme or not args.system_theme):
    raise RuntimeError('Appearance phase requires app and system theme labels')
out = Path(args.output).resolve()
out.mkdir(parents=True, exist_ok=True)
if args.capture_codec_experiment:
    if any(out.iterdir()):
        raise RuntimeError('Paired codec diagnostic requires an empty output directory')
    observations = []
    for codec in ['ffv1', 'copy']:
        directory = out / codec
        directory.mkdir()
        outcome = {'diagnosticOnly': True, 'codec': codec, 'container': 'nut',
                   'captureSeconds': 6, 'runnerPassed': False,
                   'visualReview': 'required', 'contentBackgroundPass': None, 'titlebarPass': None}
        command = [sys.executable, str(Path(__file__).resolve()), '--binary', args.binary,
                   '--output', str(directory), '--app-theme', args.app_theme,
                   '--system-theme', args.system_theme, '--capture-size', args.capture_size,
                   '--display-owner-pid', str(args.display_owner_pid)]
        try:
            with (directory / 'seed-runner.log').open('w') as log:
                seed = subprocess.run([*command, '--phase', 'appearance-seed'], stdout=log, stderr=subprocess.STDOUT)
            outcome['seedReturncode'] = seed.returncode
            if seed.returncode == 0:
                with (directory / 'app-runner.log').open('w') as log:
                    app = subprocess.run([*command, '--phase', 'appearance-app', '--capture-codec-control', codec],
                                         stdout=log, stderr=subprocess.STDOUT)
                outcome['appReturncode'] = app.returncode
                outcome['runnerPassed'] = app.returncode == 0
        except OSError as error:
            outcome['preparationError'] = {'type': type(error).__name__}
        label = f'appearance-{args.system_theme}-{args.app_theme}'
        for name, key in [('appearance-app-runtime-observation.json', 'runtimeObservation'),
                          (f'{label}.json', 'capture')]:
            path = directory / name
            try:
                if path.exists():
                    outcome[key] = json.loads(path.read_text())
            except (OSError, ValueError) as error:
                outcome.setdefault('artifactErrors', []).append({'artifact': name, 'type': type(error).__name__})
                outcome['runnerPassed'] = False
        diagnostics = directory / 'appearance-app-diagnostics.jsonl'
        try:
            if diagnostics.exists():
                events = [json.loads(line) for line in diagnostics.read_text().splitlines() if line]
                outcome['errors'] = [event for event in events if event['event'] in
                                     {'primary-error', 'cleanup-capture-error'}]
        except (OSError, ValueError) as error:
            outcome.setdefault('artifactErrors', []).append({'artifact': diagnostics.name, 'type': type(error).__name__})
            outcome['runnerPassed'] = False
        observations.append(outcome)
        (out / 'capture-codec-experiment.json').write_text(json.dumps({
            'diagnosticOnly': True, 'systemTheme': args.system_theme, 'appTheme': args.app_theme,
            'observations': observations}, indent=2))
    print(json.dumps({'diagnosticOnly': True, 'captureCodecExperiment': observations}), flush=True)
    if not all(item['runnerPassed'] for item in observations):
        raise RuntimeError('Paired capture diagnostic failed one or more controls')
    sys.exit(0)
marker = out / 'synthetic-profile.txt'
if marker.exists():
    profile = Path(marker.read_text().strip())
    if profile.parent != Path('/var/tmp') or not profile.name.startswith('risunest-linux-bench-') or profile.stat().st_uid != os.getuid():
        raise RuntimeError('Invalid synthetic profile')
else:
    profile = Path(tempfile.mkdtemp(prefix='risunest-linux-bench-', dir='/var/tmp'))
    marker.write_text(str(profile))
for key, leaf in [('XDG_DATA_HOME', 'data'), ('XDG_CONFIG_HOME', 'config'), ('XDG_CACHE_HOME', 'cache')]:
    location = profile / leaf
    location.mkdir(exist_ok=True)
    os.environ[key] = str(location)
os.environ['TAURI_WEBVIEW_AUTOMATION'] = 'true'
os.environ['GDK_BACKEND'] = 'x11'
with socket.socket() as probe:
    probe.bind(('127.0.0.1', 0))
    port = probe.getsockname()[1]
base = f'http://127.0.0.1:{port}'
log = (out / f'{args.phase}-driver.log').open('w')
driver = subprocess.Popen(['WebKitWebDriver', '--host=127.0.0.1', f'--port={port}'], stdout=log, stderr=log)
session = None
capture = None
appearance_capture = None
stop = threading.Event()
memory = []
diagnostic_path = out / f'{args.phase}-diagnostics.jsonl'
receipt_path = out / 'owned-process-receipt.json'
profile_id = hashlib.sha256(str(profile).encode()).hexdigest()
owned_processes = {}
primary_error = None
current_stage = 'driver-start'

def diagnostic(event, **details):
    record = {'diagnosticOnly': True, 'phase': args.phase, 'event': event,
              'monotonicSeconds': time.monotonic(), 'profileId': profile_id, **details}
    try:
        with diagnostic_path.open('a') as destination:
            destination.write(json.dumps(record) + '\n')
    except OSError as error:
        print(json.dumps({'diagnosticWriteError': type(error).__name__}), flush=True)

def process_identity(pid):
    try:
        text = Path(f'/proc/{pid}/stat').read_text()
        fields = text[text.rfind(')') + 2:].split()
        executable = Path(os.readlink(f'/proc/{pid}/exe'))
        known_names = {Path(args.binary).name, 'WebKitWebDriver', 'WebKitWebProcess',
                       'WebKitNetworkProcess', 'bwrap', 'xdg-dbus-proxy'}
        return {'pid': pid, 'ppid': int(fields[1]), 'state': fields[0],
                'starttime': int(fields[19]),
                'exe': executable.name if executable.name in known_names else 'other',
                'isBenchmarkBinary': executable == Path(args.binary).resolve()}
    except (OSError, ValueError, IndexError):
        return None

def process_snapshot(event):
    try:
        candidates = sorted({driver.pid, *descendants(driver.pid), *owned_processes})
        identities = []
        for pid in candidates[:64]:
            identity = process_identity(pid)
            expected_start = owned_processes.get(pid)
            if identity is None:
                identities.append({'pid': pid, 'starttime': expected_start, 'status': 'exited'})
            elif expected_start is not None and identity['starttime'] != expected_start:
                identities.append({'pid': pid, 'starttime': expected_start, 'status': 'pid-reused'})
            else:
                owned_processes[pid] = identity['starttime']
                identities.append(identity)
        diagnostic(event, driverReturncode=driver.poll(), processes=identities,
                   omittedProcesses=max(0, len(candidates) - 64))
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        diagnostic('process-snapshot-error', sourceEvent=event, errorType=type(error).__name__)

def inspect_seed_receipt():
    if args.phase != 'appearance-app' or not receipt_path.exists():
        return
    try:
        receipt = json.loads(receipt_path.read_text())
        entries = receipt['processes']
        if receipt.get('scope') != 'owned-driver-descendants' or receipt.get('profileId') != profile_id or len(entries) > 64:
            diagnostic('seed-receipt-invalid')
            return
        observations = []
        for entry in entries:
            pid, starttime = entry['pid'], entry['starttime']
            if not isinstance(pid, int) or pid <= 0 or not isinstance(starttime, int) or starttime < 0:
                diagnostic('seed-receipt-invalid')
                return
            identity = process_identity(pid)
            status = 'exited' if identity is None else ('surviving' if identity['starttime'] == starttime else 'pid-reused')
            observations.append({**(identity if status == 'surviving' else {}),
                                 'pid': pid, 'starttime': starttime, 'status': status})
        diagnostic('seed-receipt-survival', processes=observations)
    except (OSError, ValueError, KeyError, TypeError) as error:
        diagnostic('seed-receipt-error', errorType=type(error).__name__)

def call(method, route, payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(base + route, data=body, method=method, headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            return json.load(response)['value']
    except urllib.error.HTTPError as error:
        raise RuntimeError(error.read().decode()) from error

def execute(script):
    return call('POST', f'/session/{session}/execute/sync', {'script': script, 'args': []})

def descendants(pid):
    try:
        children = set()
        for task in Path(f'/proc/{pid}/task').iterdir():
            try:
                children.update((task / 'children').read_text().split())
            except OSError:
                continue
    except OSError:
        return []
    result = []
    for child in children:
        result.append(int(child))
        result.extend(descendants(child))
    return result

def monitor():
    # Linux PSS avoids double-counting shared pages across WebKit subprocesses.
    while not stop.wait(0.2):
        totals = {'pssKiB': 0, 'ussKiB': 0, 'processes': 0}
        for pid in descendants(driver.pid):
            try:
                rows = Path(f'/proc/{pid}/smaps_rollup').read_text().splitlines()
                values = {row.split(':')[0]: int(row.split()[1]) for row in rows if row.split()[0].endswith(':')}
                totals['pssKiB'] += values.get('Pss', 0)
                totals['ussKiB'] += sum(values.get(key, 0) for key in ['Private_Clean', 'Private_Dirty', 'Private_Hugetlb'])
                totals['processes'] += 1
            except (OSError, ValueError):
                continue
        memory.append(totals)

try:
    diagnostic('driver-start', driverPid=driver.pid)
    inspect_seed_receipt()
    current_stage = 'driver-status'
    for attempt in range(100):
        try:
            call('GET', '/status')
            diagnostic('driver-status-ready')
            break
        except OSError:
            time.sleep(0.1)
    if args.phase == 'appearance-app':
        current_stage = 'capture-start'
        specification = importlib.util.spec_from_file_location('appearance_capture', Path(__file__).resolve().parents[1] / 'macos/run.py')
        appearance_capture = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(appearance_capture)
        capture = appearance_capture.start_appearance_capture(out,
            f'appearance-{args.system_theme}-{args.app_theme}', 'linux', os.environ.get('DISPLAY'),
            args.capture_size, args.display_owner_pid, experiment_codec=args.capture_codec_control)
        diagnostic('capture-started')
    current_stage = 'session-create'
    process_snapshot('session-create-enter')
    created = call('POST', '/session', {'capabilities': {'alwaysMatch': {'webkitgtk:browserOptions': {'binary': str(Path(args.binary).resolve()), 'args': []}}}})
    session = created['sessionId']
    process_snapshot('session-created')
    current_stage = 'window-rect'
    call('POST', f'/session/{session}/window/rect', {'width': 1280, 'height': 900})
    current_stage = 'tauri-ready'
    for attempt in range(100):
        if execute('return !!window.__TAURI_INTERNALS__'):
            diagnostic('tauri-ready')
            break
        time.sleep(0.1)
    current_stage = 'identity-path'
    identity = call('POST', f'/session/{session}/execute/async', {'script': '''const done=arguments[arguments.length-1]; Promise.all([window.__TAURI_INTERNALS__.invoke('plugin:app|identifier'),window.__TAURI_INTERNALS__.invoke('plugin:path|resolve_directory',{directory:14})]).then(done,()=>done(null));''', 'args': []})
    if not identity or identity[0] != 'io.github.rsyumi.risunest.linux.bench' or Path(identity[1]).resolve() != (profile / 'data' / identity[0]).resolve():
        raise RuntimeError('Synthetic identity/path gate failed')
    diagnostic('identity-path-verified')
    current_stage = 'benchmark-ready'
    for attempt in range(100):
        if execute('return !!window.__RISUNEST_LINUX_BENCHMARK__'):
            diagnostic('benchmark-ready')
            break
        time.sleep(0.1)
    thread = threading.Thread(target=monitor, daemon=True)
    thread.start()
    operation = (f"startupAppearance({str(args.phase == 'appearance-seed').lower()},{json.dumps(args.app_theme)})"
                 if args.phase.startswith('appearance-') else f"{args.phase}()")
    current_stage = 'operation'
    diagnostic('operation-enter')
    execute(f"window.__linuxResult=null; window.__RISUNEST_LINUX_BENCHMARK__.{operation}.then(result=>window.__linuxResult={{result}},error=>window.__linuxResult={{error:String(error)}})")
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        result = execute('return window.__linuxResult')
        if result is not None:
            break
        time.sleep(0.5)
    else:
        raise RuntimeError('Benchmark timed out')
    if 'error' in result:
        raise RuntimeError(result['error'])
    result = result['result']
    diagnostic('operation-resolved')
    if args.phase == 'appearance-app':
        if result['systemDark'] != (args.system_theme == 'dark'):
            raise RuntimeError('WebKit observed system appearance differs from the requested theme')
        (out / f'{args.phase}-runtime-observation.json').write_text(json.dumps({
            'diagnosticOnly': True, 'systemTheme': args.system_theme, 'appTheme': args.app_theme,
            'observation': result}, indent=2))
        diagnostic('appearance-runtime-observation-saved')
        current_stage = 'capture-finalize'
        completed_capture = capture
        capture = None
        result['capture'] = appearance_capture.finish_appearance_capture(completed_capture)
        diagnostic('capture-finalized')
        result['systemTheme'] = args.system_theme
    current_stage = 'result-finalize'
    stop.set()
    thread.join(timeout=2)
    if not memory or not any(item['processes'] for item in memory):
        raise RuntimeError('No process memory samples collected')
    result['memorySamples'] = memory
    result['environment'] = {'display': 'Xvfb/X11', 'backend': 'WebKitGTK', 'storage': 'synthetic /var/tmp', 'memoryScope': 'WebDriver descendants; sampled every 200ms, not per-mode attribution'}
    if args.phase == 'reload':
        previous = json.loads((out / 'persistence.json').read_text())
        if result['revision'] != previous['revision'] or result['finalHash'] != previous['finalHash']:
            raise RuntimeError('Restart hash/revision mismatch')
    output_name = f'{args.phase}-{args.system_theme}-{args.app_theme}' if args.phase.startswith('appearance-') else args.phase
    (out / f'{output_name}.json').write_text(json.dumps(result, indent=2))
    print(json.dumps({'phase': args.phase, 'passed': None if args.phase.startswith('appearance-') else True, 'visualReview': 'required' if args.phase.startswith('appearance-') else None, 'samples': len(result.get('samples', [])), 'memorySamples': len(memory)}))
except Exception as error:
    primary_error = error
    diagnostic('primary-error', stage=current_stage, errorType=type(error).__name__,
               category='capture' if current_stage == 'capture-finalize' else 'runtime')
    raise
finally:
    capture_error = None
    if capture is not None:
        try:
            appearance_capture.finish_appearance_capture(capture)
        except Exception as error:
            capture_error = error
            diagnostic('cleanup-capture-error', errorType=type(error).__name__)
    stop.set()
    process_snapshot('session-cleanup-enter')
    if args.phase == 'appearance-seed':
        try:
            receipt_path.write_text(json.dumps({'diagnosticOnly': True, 'scope': 'owned-driver-descendants',
                'profileId': profile_id, 'processes': [
                    {'pid': pid, 'starttime': starttime} for pid, starttime in sorted(owned_processes.items())[:64]]}, indent=2))
        except OSError as error:
            diagnostic('seed-receipt-write-error', errorType=type(error).__name__)
    if session:
        try:
            call('DELETE', f'/session/{session}')
        except Exception:
            pass
    process_snapshot('session-cleanup-return')
    driver.terminate()
    try:
        driver.wait(timeout=10)
    except subprocess.TimeoutExpired:
        driver.kill()
        driver.wait()
    process_snapshot('driver-cleanup-return')
    log.close()
    if capture_error is not None and primary_error is None:
        raise capture_error
