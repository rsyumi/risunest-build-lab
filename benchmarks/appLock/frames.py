"""Synthetic native return-frame verification, using product gates and native lifecycle."""
import argparse
import json
import importlib.util
import os
from pathlib import Path
import subprocess
import time

PHASES = ('app-lock-immediate', 'app-lock-timed', 'app-lock-immediate-mutant')


def output(command, **kwargs):
    return subprocess.check_output(list(map(str, command)), text=True, **kwargs).strip()


def metadata(path):
    return json.loads(output(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_streams', '-of', 'json', path]))['streams'][0]


def rgb_image(path):
    info = metadata(path)
    data = subprocess.check_output(['ffmpeg', '-v', 'error', '-i', str(path), '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'])
    return int(info['width']), int(info['height']), data


def color(pixel):
    r, g, b = pixel
    if r > 180 and g < 80 and b > 180:
        return 1
    if r < 80 and g > 180 and b > 180:
        return 2
    return 0


def stripe(row):
    runs = []
    previous = 0
    start = 0
    for x in range(len(row) // 3):
        value = color(row[x * 3:x * 3 + 3])
        if value != previous:
            if previous:
                runs.append((previous, start, x))
                runs = runs[-4:]
                widths = [end - begin for _, begin, end in runs]
                if (len(runs) == 4 and [run[0] for run in runs] == [1, 2, 1, 2]
                        and min(widths) >= 8 and max(widths) <= min(widths) * 1.25
                        and all(0 <= right[1] - left[2] <= max(2, min(widths) * .1)
                                for left, right in zip(runs, runs[1:]))):
                    return runs[0][1], runs[-1][2]
            previous, start = value, x
    return None


def find_marker(width, height, data):
    for y in range(0, height, 2):
        found = stripe(data[y * width * 3:(y + 1) * width * 3] + bytes(3))
        if found:
            left, right = found
            return [left, y, right - left, max(8, int((right - left) * .75) - 2)]
    raise RuntimeError('Visible synthetic-content baseline has no checkerboard')


def frames(path, crop, tail=None, scale=None):
    x, y, width, height = crop
    command = ['ffmpeg', '-v', 'error']
    if tail:
        command += ['-sseof', str(-tail)]
    filters = f'format=rgb24,crop={width}:{height}:{x}:{y}'
    if scale:
        filters += f',scale={scale[0]}:{scale[1]}:flags=neighbor'
        width, height = scale
    command += ['-i', str(path), '-vf', filters, '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-']
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    size = width * height * 3
    try:
        while True:
            data = process.stdout.read(size)
            if not data:
                break
            if len(data) != size:
                raise RuntimeError('Truncated decoded video frame')
            yield data
        error = process.stderr.read().decode(errors='replace')
        if process.wait(timeout=15):
            raise RuntimeError('Frame decode failed: ' + error)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)


def cadence(path):
    probe = json.loads(output(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_frames',
        '-show_entries', 'frame=best_effort_timestamp_time', '-of', 'json', path]))
    times = [float(frame['best_effort_timestamp_time']) for frame in probe['frames']]
    gaps = [end - start for start, end in zip(times, times[1:])]
    if len(times) < 30 or not gaps or min(gaps) <= 0 or max(gaps) > .04:
        raise RuntimeError('Capture cadence cannot resolve a display-frame content flash')
    return {'frames': len(times), 'maximumFrameGapSeconds': max(gaps)}


def assess(video, baseline, ready, locked, expect_exposure=False):
    timing = cadence(video)
    width, height, data = rgb_image(baseline)
    marker = find_marker(width, height, data)
    info = metadata(video)
    sx, sy = int(info['width']) / width, int(info['height']) / height
    if abs(sx / sy - 1) > .02:
        raise RuntimeError('Capture and baseline have different viewport proportions')
    detection_width = min(640, int(info['width']))
    detection_height = round(int(info['height']) * detection_width / int(info['width']))
    exposures = []
    count = 0
    # Inspect the whole frame so activation animations cannot move exposed content outside a fixed crop.
    for count, data in enumerate(frames(video, [0, 0, int(info['width']), int(info['height'])],
                                        scale=(detection_width, detection_height)), 1):
        hits = 0
        for y in range(0, detection_height, max(1, detection_height // 120)):
            if stripe(data[y * detection_width * 3:(y + 1) * detection_width * 3] + bytes(3)) is not None:
                hits += 1
                if hits >= 3:
                    exposures.append(count)
                    break
    scale = marker[2] / ready['marker']['width']
    origin_x = marker[0] - ready['marker']['x'] * scale
    origin_y = marker[1] - ready['marker']['y'] * scale
    visible_keys = 0
    for key in locked['keys']:
        left = int((origin_x + (key['x'] + key['width'] * .25) * scale) * sx)
        top = int((origin_y + (key['y'] + key['height'] * .25) * scale) * sy)
        key_crop = [left, top, int(key['width'] * scale * sx * .5), int(key['height'] * scale * sy * .5)]
        if left < 0 or top < 0 or left + key_crop[2] > int(info['width']) or top + key_crop[3] > int(info['height']):
            raise RuntimeError('Shared keypad falls outside the captured viewport')
        contrasting = 0
        for pixels in frames(video, key_crop, tail=.35):
            luminance = [(pixels[i] + pixels[i + 1] + pixels[i + 2]) // 3 for i in range(0, len(pixels), 3)]
            if max(luminance) - min(luminance) > 60:
                contrasting += 1
        if contrasting >= 3:
            visible_keys += 1
    result = {**timing, 'decodedFrames': count, 'exposureFrames': exposures, 'visibleKeypadKeys': visible_keys,
              'baselineMarker': marker, 'mutation': expect_exposure, 'retained': locked['retained'],
              'blockedAfterReturnMs': locked['blockedAfterReturnMs']}
    video.with_suffix('.frames.json').write_text(json.dumps(result, indent=2))
    if visible_keys < 7:
        raise RuntimeError('Final native frames do not show the shared PIN keypad')
    if bool(exposures) != expect_exposure:
        raise RuntimeError('Mutation did not expose retained content' if expect_exposure else 'Retained content appeared in native return frames')
    return result


def read_records(path):
    if not path.exists():
        return []
    text = path.read_text()
    return [json.loads(line) for line in text[:text.rfind('\n') + 1].splitlines() if line]


def wait_stage(path, stage, timeout, process=None):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        records = read_records(path)
        failure = next((item for item in records if item['stage'] == 'failure'), None)
        if failure:
            raise RuntimeError(str(failure))
        found = [item['result'] for item in records if item['stage'] == stage]
        if found:
            if len(found) != 1:
                raise RuntimeError('Duplicate native trace stage: ' + stage)
            return found[0]
        if process is not None and process.poll() is not None:
            raise RuntimeError('Synthetic native process exited before ' + stage)
        time.sleep(.05)
    raise RuntimeError('Timed out waiting for ' + stage)


def run_macos(app, artifacts, capture_input, start_capture, finish_capture):
    outcomes = []
    for phase in PHASES:
        report = artifacts / (phase + '.jsonl')
        if report.exists():
            raise RuntimeError('Refusing to overwrite frame evidence')
        environment = {**os.environ, 'RISUNEST_MACOS_PHASE': phase, 'RISUNEST_MACOS_REPORT': str(report)}
        capture = None
        with (artifacts / (phase + '.log')).open('w') as log:
            process = subprocess.Popen([str(app / 'Contents/MacOS/risunest-macos-bench')], env=environment, stdout=log, stderr=subprocess.STDOUT)
            try:
                ready = wait_stage(report, 'app-lock-ready', 120, process)
                baseline = artifacts / (phase + '-baseline.png')
                subprocess.run(['screencapture', '-x', str(baseline)], check=True)
                subprocess.run(['open', '-a', 'Finder'], check=True)
                time.sleep(.4)
                if phase == 'app-lock-timed':
                    time.sleep(65)
                capture = start_capture(artifacts, phase, 'macos', capture_input)
                subprocess.run(['open', '-a', str(app)], check=True)
                locked = wait_stage(report, 'app-lock-locked', 45, process)
                time.sleep(.6)
                completed_capture, capture = capture, None
                finish_capture(completed_capture)
                video = completed_capture['video']
                outcomes.append(assess(video, baseline, ready, locked, phase.endswith('mutant')))
            finally:
                try:
                    if capture is not None:
                        finish_capture(capture)
                finally:
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=5)
    (artifacts / 'app-lock-result.json').write_text(json.dumps({'passed': True, 'observations': outcomes}, indent=2))


def run_ios(device, artifacts, capture_input):
    if os.environ.get('GITHUB_ACTIONS') != 'true':
        raise RuntimeError('Simulator capture requires the fresh hosted synthetic CI profile')
    identifier = 'io.github.rsyumi.risunest.ios.bench'
    container = Path(output(['xcrun', 'simctl', 'get_app_container', device, identifier, 'data']))
    devices = json.loads(output(['xcrun', 'simctl', 'list', 'devices', 'booted', '--json']))['devices']
    booted = [item for runtime in devices.values() for item in runtime if item['state'] == 'Booted']
    if len(booted) != 1 or booted[0]['udid'] != device or not booted[0]['name'].startswith('RisuNest synthetic'):
        raise RuntimeError('Hosted capture requires only the owned synthetic Simulator to be booted')
    (artifacts / 'simulator-selection.json').write_text(json.dumps({
        'udid': device, 'name': booted[0]['name'], 'onlyBootedDevice': True, 'captureInput': capture_input}, indent=2))
    specification = importlib.util.spec_from_file_location('appearance_capture', Path(__file__).resolve().parents[1] / 'macos/run.py')
    capture_tools = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(capture_tools)
    subprocess.run(['open', '-a', 'Simulator'], check=True)
    outcomes = []
    for phase in PHASES:
        if list(container.rglob('verification-' + phase + '.jsonl')):
            raise RuntimeError('Refusing an existing app-lock verification phase')
        environment = {**os.environ, 'SIMCTL_CHILD_RISUNEST_IOS_PHASE': phase}
        subprocess.run(['xcrun', 'simctl', 'launch', '--terminate-running-process', device, identifier], env=environment, check=True)
        deadline = time.monotonic() + 120
        while True:
            matches = list(container.rglob('verification-' + phase + '.jsonl'))
            if len(matches) == 1:
                report = matches[0]
                break
            if time.monotonic() > deadline:
                raise RuntimeError('Synthetic frame report was not created')
            time.sleep(.1)
        ready = wait_stage(report, 'app-lock-ready', 120)
        baseline = artifacts / (phase + '-baseline.png')
        subprocess.run(['open', '-a', 'Simulator'], check=True)
        deadline = time.monotonic() + 15
        while True:
            subprocess.run(['screencapture', '-x', str(baseline)], check=True)
            try:
                find_marker(*rgb_image(baseline))
                break
            except RuntimeError:
                if time.monotonic() >= deadline:
                    raise RuntimeError('Owned Simulator synthetic content is not visible on the captured host display')
                time.sleep(.1)
        subprocess.run(['xcrun', 'simctl', 'launch', device, 'com.apple.Preferences'], check=True)
        time.sleep(.4)
        if phase == 'app-lock-timed':
            time.sleep(65)
        capture = capture_tools.start_appearance_capture(artifacts, phase, 'macos', capture_input)
        try:
            subprocess.run(['xcrun', 'simctl', 'launch', device, identifier], check=True)
            locked = wait_stage(report, 'app-lock-locked', 45)
            time.sleep(.6)
        finally:
            capture_tools.finish_appearance_capture(capture)
        video = capture['video']
        (artifacts / (phase + '.jsonl')).write_text(report.read_text())
        outcomes.append(assess(video, baseline, ready, locked, phase.endswith('mutant')))
        subprocess.run(['xcrun', 'simctl', 'terminate', device, identifier], check=True)
    (artifacts / 'app-lock-result.json').write_text(json.dumps({'passed': True, 'observations': outcomes}, indent=2))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--device', required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--capture-input', required=True)
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    if any(args.artifacts.iterdir()):
        parser.error('Use an empty frame artifact directory')
    run_ios(args.device, args.artifacts.resolve(), args.capture_input)


if __name__ == '__main__':
    main()
