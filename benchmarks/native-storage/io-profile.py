import argparse
import json
import os
import platform
import sqlite3
import tempfile
import time
from pathlib import Path


def emit(**value):
    print(json.dumps(value), flush=True)


def measure(label, operation, count, action):
    wall = time.perf_counter()
    cpu = time.process_time()
    action()
    elapsed = time.perf_counter() - wall
    emit(root=label, operation=operation, count=count,
         wall_seconds=round(elapsed, 6),
         cpu_seconds=round(time.process_time() - cpu, 6),
         milliseconds_per_operation=round(elapsed * 1000 / count, 4))


def profile(label, parent, count):
    with tempfile.TemporaryDirectory(prefix='native-storage-profile-', dir=parent) as name:
        root = Path(name)
        emit(root=label, volume=root.anchor, device=root.stat().st_dev)
        paths = [root / f'object-{index:06}' for index in range(count)]
        payload = b'synthetic-storage-profile' * 10
        measure(label, 'create_write_close', count,
                lambda: [path.write_bytes(payload) for path in paths])
        measure(label, 'read_open_close', count,
                lambda: [path.read_bytes() for path in paths])
        measure(label, 'stat', count * 10,
                lambda: [path.stat() for _ in range(10) for path in paths])

        def sync_files():
            for path in paths:
                with path.open('r+b') as handle:
                    handle.write(payload)
                    handle.flush()
                    os.fsync(handle.fileno())
        measure(label, 'write_flush_fsync_close', count, sync_files)

        def rename_files():
            for path in paths:
                other = path.with_suffix('.moved')
                path.rename(other)
                other.rename(path)
        measure(label, 'rename', count * 2, rename_files)
        measure(label, 'unlink', count, lambda: [path.unlink() for path in paths])

        for journal, synchronous in [('DELETE', 'FULL'), ('WAL', 'FULL'), ('WAL', 'NORMAL')]:
            for batch in [1, count]:
                db_path = root / f'{journal}-{synchronous}-{batch}.sqlite'
                with sqlite3.connect(db_path) as db:
                    db.execute(f'PRAGMA journal_mode={journal}')
                    db.execute(f'PRAGMA synchronous={synchronous}')
                    db.execute('CREATE TABLE records(id INTEGER PRIMARY KEY, value BLOB)')
                    db.commit()

                    def write_rows():
                        for index in range(count):
                            db.execute('INSERT INTO records VALUES (?, ?)', (index, payload))
                            if (index + 1) % batch == 0:
                                db.commit()
                        db.commit()
                    measure(label, f'sqlite_{journal}_{synchronous}_batch_{batch}', count, write_rows)
                    assert db.execute('SELECT count(*) FROM records').fetchone()[0] == count
                db.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', action='append', default=[], metavar='LABEL=PATH')
    parser.add_argument('--count', type=int, default=200)
    args = parser.parse_args()
    if not 1 <= args.count <= 10000:
        parser.error('count must be from 1 to 10000')
    roots = [('system-temp', None)]
    for spec in args.root:
        label, separator, value = spec.partition('=')
        if not separator or not label or not Path(value).is_dir():
            parser.error('root must be LABEL=an-existing-directory')
        roots.append((label, Path(value).resolve()))
    emit(platform=platform.system(), architecture=platform.machine(),
         python=platform.python_version(), sqlite=sqlite3.sqlite_version,
         cpu_count=os.cpu_count(), count=args.count)
    for label, parent in roots:
        profile(label, parent, args.count)


if __name__ == '__main__':
    main()
