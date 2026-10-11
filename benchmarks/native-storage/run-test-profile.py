import argparse
import ctypes
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path


def cpu_seconds(process):
    if os.name == 'nt':
        from ctypes import wintypes
        get_times = ctypes.WinDLL('kernel32', use_last_error=True).GetProcessTimes
        get_times.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
        get_times.restype = wintypes.BOOL
        values = [wintypes.FILETIME() for _ in range(4)]
        if not get_times(int(process._handle), *(ctypes.byref(value) for value in values)):
            raise ctypes.WinError(ctypes.get_last_error())
        return sum((value.dwHighDateTime << 32) | value.dwLowDateTime for value in values[2:]) / 10_000_000
    import resource
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime + usage.ru_stime


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--test', required=True, action='append')
    parser.add_argument('--threads', type=int, action='append')
    parser.add_argument('--skip-system-temp', action='store_true')
    parser.add_argument('--root', action='append', default=[], metavar='LABEL=PATH')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    threads = args.threads or [1]
    if any(value < 1 for value in threads) or len(set(args.test)) != len(args.test):
        parser.error('threads must be positive and tests must be distinct')
    listed = subprocess.run([str(binary), *args.test, '--exact', '--list'],
                            text=True, capture_output=True, check=True)
    for name in args.test:
        if f'{name}: test' not in listed.stdout.splitlines():
            parser.error(f'binary does not list the exact requested test: {name}')
    roots = [] if args.skip_system_temp else [('system-temp', None)]
    for spec in args.root:
        label, separator, value = spec.partition('=')
        if not separator or not label or not Path(value).is_dir():
            parser.error('root must be LABEL=an-existing-directory')
        roots.append((label, Path(value).resolve()))
    if not roots:
        parser.error('at least one temporary root is required')
    for label, parent, concurrency in [(label, parent, n) for label, parent in roots for n in threads]:
        with tempfile.TemporaryDirectory(prefix='native-test-profile-', dir=parent) as directory:
            env = dict(os.environ, TEMP=directory, TMP=directory, TMPDIR=directory)
            print(json.dumps({'event': 'start', 'root': label, 'tests': args.test,
                              'threads': concurrency, 'volume': Path(directory).anchor}), flush=True)
            before_cpu = 0.0
            if os.name != 'nt':
                import resource
                usage = resource.getrusage(resource.RUSAGE_CHILDREN)
                before_cpu = usage.ru_utime + usage.ru_stime
            started = time.perf_counter()
            process = subprocess.Popen([str(binary), *args.test, '--exact', '--nocapture',
                                        f'--test-threads={concurrency}'], env=env)
            code = process.wait()
            print(json.dumps({'event': 'complete', 'root': label, 'tests': args.test,
                              'threads': concurrency,
                              'exit_code': code, 'wall_seconds': round(time.perf_counter() - started, 6),
                              'cpu_seconds': round(cpu_seconds(process) - before_cpu, 6)}), flush=True)
            if code:
                raise SystemExit(code)


if __name__ == '__main__':
    main()
