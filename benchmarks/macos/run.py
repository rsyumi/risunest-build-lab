"""Drive only the isolated agent app, using real LaunchServices open/reopen events."""
import argparse
import json
import os
import re
import shutil
import sys
from pathlib import Path
import subprocess
import tempfile
import time


def records(path):
    # A report can be large; the reader may observe an unfinished final write.
    return [json.loads(line) for line in path.read_text().splitlines(keepends=True) if line.endswith('\n')] if path.exists() else []


def memory_sample(pid):
    # RSS is a process-residency observation, not physical footprint or private memory.
    output = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,rss=,comm='], text=True)
    rows = []
    for line in output.splitlines():
        fields = line.split(None, 3)
        if len(fields) == 4:
            rows.append((int(fields[0]), int(fields[1]), int(fields[2]), fields[3]))
    selected = {pid}
    for _ in range(8):
        selected.update(row[0] for row in rows if row[1] in selected)
    # WKWebView services can be launched by launchd. Keep a separate un-attributed
    # system-wide observation instead of claiming those all belong to this app.
    own = [{'pid': p, 'rssKiB': rss} for p, _, rss, _ in rows if p in selected]
    webkit = [{'pid': p, 'rssKiB': rss} for p, _, rss, name in rows if 'WebKit' in name]
    return {'time': time.time(), 'appTree': own, 'systemWebKit': webkit}


def start_appearance_capture(directory, label, platform, capture_input, capture_size=None, display_owner_pid=None, experiment_codec=None):
    if experiment_codec is not None and (platform != 'macos' or experiment_codec not in {'ffv1', 'copy'}):
        raise RuntimeError('Paired capture experiment supports only Mac ffv1 and copy controls')
    if not shutil.which('ffmpeg') or not shutil.which('ffprobe'):
        raise RuntimeError('Appearance capture requires ffmpeg and ffprobe')
    if platform == 'macos':
        if os.environ.get('GITHUB_ACTIONS') != 'true':
            raise RuntimeError('Mac whole-display capture is restricted to the fresh hosted CI desktop')
        if not re.fullmatch(r'Capture screen [0-9]+:none', capture_input or ''):
            raise RuntimeError('Select an enumerated Capture screen device without audio')
        source = ['-f', 'avfoundation', '-framerate', '60', '-i', capture_input]
        desktop = {'kind': 'hosted macOS desktop'}
    else:
        display = os.environ.get('DISPLAY', '')
        if not display or capture_input != display or not display_owner_pid:
            raise RuntimeError('Linux capture needs DISPLAY and its owned Xvfb PID')
        process_root = Path('/proc') / str(display_owner_pid)
        command = (process_root / 'cmdline').read_bytes().split(b'\0')
        if (process_root.stat().st_uid != os.getuid() or not command
                or Path(os.fsdecode(command[0])).name != 'Xvfb'
                or display.split('.')[0].encode() not in command):
            raise RuntimeError('Capture display is not the owned synthetic Xvfb process')
        wm = subprocess.check_output(['xprop', '-root', '_NET_SUPPORTING_WM_CHECK'], text=True)
        match = re.search(r'0x[0-9a-fA-F]+', wm)
        if not match or int(match.group(), 16) == 0:
            raise RuntimeError('A named window manager is required for title-bar observations')
        wm_name = subprocess.check_output(['xprop', '-id', match.group(), '_NET_WM_NAME'], text=True).strip()
        if ' = ' not in wm_name:
            raise RuntimeError('Window manager name unavailable')
        if not re.fullmatch(r'[1-9][0-9]*x[1-9][0-9]*', capture_size or ''):
            raise RuntimeError('Explicit capture size required')
        source = ['-f', 'x11grab', '-framerate', '60', '-video_size', capture_size, '-i', display]
        desktop = {'kind': 'owned Xvfb/X11', 'windowManager': wm_name, 'gtkTheme': os.environ.get('GTK_THEME')}
    video = directory / f'{label}.{"nut" if experiment_codec else "mkv"}'
    progress = directory / f'{label}-capture-progress.txt'
    if video.exists() or progress.exists():
        raise RuntimeError('Refusing to overwrite appearance capture')
    log = (directory / f'{label}-capture.log').open('w')
    started = time.monotonic()
    experiment_options = ['-nostdin', '-benchmark', '-t', '6', '-f', 'nut'] if experiment_codec else []
    process = subprocess.Popen(['ffmpeg', '-hide_banner', '-loglevel', 'info' if experiment_codec else 'warning', '-n', *source,
        '-an', '-c:v', experiment_codec or 'ffv1', '-fps_mode', 'passthrough', '-progress', str(progress),
        *experiment_options, str(video)],
        stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)
    try:
        deadline = started + 20
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError('Screen capture exited before application launch')
            frames = re.findall(r'^frame=(\d+)$', progress.read_text() if progress.exists() else '', re.M)
            if frames and int(frames[-1]) >= 2:
                return {'process': process, 'log': log, 'video': video, 'started': started,
                        'launchOffsetSeconds': time.monotonic() - started, 'desktop': desktop,
                        'experimentCodec': experiment_codec}
            time.sleep(0.05)
        raise RuntimeError('No prelaunch frames received from screen capture')
    except BaseException:
        process.terminate()
        process.wait(timeout=10)
        log.close()
        raise


def finish_appearance_capture(capture):
    process = capture['process']
    try:
        process.communicate(None if capture.get('experimentCodec') else b'q\n', timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)
        raise RuntimeError('Screen capture did not stop')
    finally:
        capture['log'].close()
    if process.returncode != 0:
        raise RuntimeError('Screen capture failed')
    probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-select_streams', 'v:0',
        '-show_frames', '-show_entries', 'frame=best_effort_timestamp_time', '-of', 'json', str(capture['video'])], text=True))
    times = [float(frame['best_effort_timestamp_time']) for frame in probe['frames']]
    if capture.get('experimentCodec'):
        capture['video'].with_suffix('.frame-timestamps.json').write_text(json.dumps(probe, indent=2))
    gaps = [end - start for start, end in zip(times, times[1:])]
    result = {'requestedFps': 60, 'frames': len(times), 'maximumFrameGapSeconds': max(gaps, default=None),
              'launchOffsetSeconds': capture['launchOffsetSeconds'], 'desktop': capture['desktop'],
              'visualReview': 'required', 'contentBackgroundPass': None, 'titlebarPass': None,
              'scope': 'cold process launch, filesystem caches not reset'}
    capture['video'].with_suffix('.json').write_text(json.dumps(result, indent=2))
    if len(times) < 60 or not gaps or max(gaps) > 0.04:
        raise RuntimeError('Capture cadence insufficient to assess a one-frame startup flash')
    return result


def run_phase(app, phase, artifacts, fixtures):
    report = artifacts / f'{phase}.jsonl'
    environment = {**os.environ, 'RISUNEST_MACOS_PHASE': phase, 'RISUNEST_MACOS_REPORT': str(report)}
    binary = app / 'Contents/MacOS/risunest-macos-bench'
    samples = []
    handled = set()
    (artifacts / f'{phase}-baseline-memory.json').write_text(json.dumps(memory_sample(-1), indent=2))
    with (artifacts / f'{phase}.log').open('w') as log:
        process = subprocess.Popen([str(binary)], env=environment, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 600
            while process.poll() is None:
                for record in records(report):
                    stage = record['stage']
                    if stage == 'failure':
                        raise RuntimeError(json.dumps(record))
                    if stage in handled:
                        continue
                    handled.add(stage)
                    print(f'{phase}: {stage}', flush=True)
                    if stage == 'closed':
                        subprocess.run(['open', '-a', str(app)], check=True)
                    if stage == 'reopened':
                        subprocess.run(['open', '-a', str(app), *map(str, fixtures)], check=True)
                    if stage == 'app':
                        subprocess.run(['screencapture', '-x', str(artifacts / 'app.png')], check=False)
                samples.append(memory_sample(process.pid))
                if time.monotonic() > deadline:
                    raise TimeoutError(f'{phase}: native app timeout, stages={sorted(handled)}')
                time.sleep(0.5)
            if process.returncode != 0:
                raise RuntimeError(f'{phase}: app exited {process.returncode}')
            result = records(report)
            required = {phase: {phase}}[phase] if phase.startswith('appearance-') else {'termination-probe': {'termination-probe-cancel', 'termination-probe-reload', 'termination-probe-approved'}, 'contracts': {'persistence', 'regex', 'tokenizer', 'reload', 'closed', 'reopened', 'finder', 'quit-cancelled', 'quit-saved'}, 'restart': {'restart'}, 'app': {'app'}, 'app-restart': {'app-restart'}, 'streaming': {'streaming'}}[phase]
            stages = {entry['stage'] for entry in result}
            if not required <= stages or 'failure' in stages:
                raise RuntimeError(f'{phase}: incomplete results {stages}')
            return result
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)
            (artifacts / f'{phase}-memory.json').write_text(json.dumps(samples, indent=2))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--appearance', action='store_true')
    parser.add_argument('--capture-codec-experiment', action='store_true')
    parser.add_argument('--capture-input')
    parser.add_argument('--system-theme', choices=['light', 'dark'])
    args = parser.parse_args()
    if args.appearance and args.capture_codec_experiment:
        parser.error('Select appearance capture or the paired codec experiment')
    if args.capture_codec_experiment and args.system_theme != 'light':
        parser.error('Paired capture experiment requires --system-theme light')
    app = args.app.resolve(strict=True)
    identifier = subprocess.check_output(['/usr/libexec/PlistBuddy', '-c', 'Print :CFBundleIdentifier', str(app / 'Contents/Info.plist')], text=True).strip()
    if identifier != 'io.github.rsyumi.risunest.macos.bench':
        raise RuntimeError('Refusing a non-harness app')
    profile = Path.home() / 'Library/Application Support' / identifier
    if profile.exists():
        raise RuntimeError('Harness requires a fresh CI user profile; refusing existing data')
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    if args.appearance or args.capture_codec_experiment:
        if any(artifacts.iterdir()):
            raise RuntimeError('Appearance capture requires an empty artifact directory')
        if not args.system_theme:
            raise RuntimeError('Record the selected system theme explicitly')
        dark = subprocess.check_output(['osascript', '-e', 'tell application "System Events" to tell appearance preferences to get dark mode'], text=True).strip() == 'true'
        if dark != (args.system_theme == 'dark'):
            raise RuntimeError('Observed macOS appearance does not match requested system theme')
        if args.capture_codec_experiment:
            observations = []
            for codec in ['ffv1', 'copy']:
                directory = artifacts / codec
                directory.mkdir()
                label = 'appearance-app-light'
                outcome = {'codec': codec, 'captureSeconds': 6, 'runtimePassed': False, 'capturePassed': False}
                capture = None
                try:
                    run_phase(app, 'appearance-seed-light', directory, [])
                    capture = start_appearance_capture(directory, label, 'macos', args.capture_input,
                                                       experiment_codec=codec)
                except Exception as error:
                    outcome['preparationError'] = {'type': type(error).__name__, 'message': str(error)}
                if capture is not None:
                    try:
                        outcome['runtime'] = run_phase(app, label, directory, [])
                        outcome['runtimePassed'] = True
                    except Exception as error:
                        outcome['runtimeError'] = {'type': type(error).__name__, 'message': str(error)}
                    try:
                        outcome['capture'] = finish_appearance_capture(capture)
                        progress = (directory / f'{label}-capture-progress.txt').read_text()
                        duplicates = re.findall(r'^dup_frames=(\d+)$', progress, re.M)
                        drops = re.findall(r'^drop_frames=(\d+)$', progress, re.M)
                        if not duplicates or not drops or int(duplicates[-1]) != 0 or int(drops[-1]) != 0:
                            raise RuntimeError('Capture output duplicated or dropped frames')
                        outcome['capturePassed'] = True
                    except Exception as error:
                        outcome['captureError'] = {'type': type(error).__name__, 'message': str(error)}
                        metadata = capture['video'].with_suffix('.json')
                        if metadata.exists():
                            outcome['capture'] = json.loads(metadata.read_text())
                log = directory / f'{label}-capture.log'
                if log.exists():
                    statistics = re.search(r'bench: utime=([\d.]+)s stime=([\d.]+)s rtime=([\d.]+)s', log.read_text())
                    if statistics:
                        outcome['benchmarkSeconds'] = dict(zip(['user', 'system', 'elapsed'], map(float, statistics.groups())))
                observations.append(outcome)
            (artifacts / 'capture-codec-experiment.json').write_text(json.dumps({
                'diagnosticOnly': True, 'systemTheme': 'light', 'appTheme': 'light',
                'observations': observations}, indent=2))
            print(json.dumps({'captureCodecExperiment': observations}), flush=True)
            if not all(item['runtimePassed'] and item['capturePassed'] for item in observations):
                raise RuntimeError('Paired capture experiment failed one or more controls')
            return
        observations = []
        for theme in ['light', 'dark']:
            run_phase(app, f'appearance-seed-{theme}', artifacts, [])
            label = f'appearance-app-{theme}'
            capture = start_appearance_capture(artifacts, label, 'macos', args.capture_input)
            runtime_error = None
            try:
                observation = run_phase(app, label, artifacts, [])
            except BaseException as error:
                runtime_error = error
                raise
            finally:
                try:
                    capture_result = finish_appearance_capture(capture)
                except BaseException as capture_error:
                    secondary = {'type': type(capture_error).__name__, 'message': str(capture_error)}
                    try:
                        (artifacts / f'{label}-capture-failure.json').write_text(json.dumps(secondary, indent=2))
                    except OSError as report_error:
                        print(f'{label}: capture failure report could not be saved ({type(report_error).__name__})', file=sys.stderr, flush=True)
                    if runtime_error is None:
                        raise
                    print(f'{label}: secondary capture failure: {json.dumps(secondary)}', file=sys.stderr, flush=True)
            observations.append({'appTheme': theme, 'systemTheme': args.system_theme,
                                 'runtime': observation, 'capture': capture_result})
        (artifacts / 'appearance-result.json').write_text(json.dumps({'visualReview': 'required', 'observations': observations}, indent=2))
        print('Appearance captures complete; content background and title bar require separate visual review', flush=True)
        return
    fixture_root = Path(tempfile.mkdtemp(prefix='risunest-macos-', dir='/private/tmp'))
    fixtures = [fixture_root / 'synthetic 한글 # %.risup', fixture_root / 'synthetic-two.risum']
    for fixture in fixtures:
        fixture.write_text('synthetic file association fixture')
    configured_phases = os.environ.get('RISUNEST_MACOS_PHASES')
    phases = configured_phases.split(',') if configured_phases else ['contracts', 'restart', 'app', 'app-restart', 'streaming']
    allowed_phases = {'termination-probe', 'contracts', 'restart', 'app', 'app-restart', 'streaming'}
    if not phases or any(phase not in allowed_phases for phase in phases):
        raise RuntimeError('invalid RISUNEST_MACOS_PHASES')
    results = {phase: run_phase(app, phase, artifacts, fixtures) for phase in phases}
    (artifacts / 'result.json').write_text(json.dumps({'passed': True, 'phases': results}, indent=2))
    print('Mac WKWebView contracts, restart and product app passed', flush=True)


if __name__ == '__main__':
    main()
