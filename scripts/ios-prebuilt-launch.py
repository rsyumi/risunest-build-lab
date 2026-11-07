import hashlib, json, os, pathlib, plistlib, subprocess, sys, time

downloads, artifacts = (pathlib.Path(arg).resolve() for arg in sys.argv[1:3])
artifacts.mkdir(parents=True, exist_ok=True)
snapshot = os.environ['SNAPSHOT_SHA']
identifier = 'io.github.rsyumi.risunest'

def run(args):
    print('+', ' '.join(map(str, args)), flush=True)
    try:
        return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT)
    except subprocess.CalledProcessError as error:
        print(error.output, flush=True)
        raise

def attempt(path):
    return int(path.name.rsplit('-', 1)[1])

candidates = sorted((path for path in downloads.glob('ios-simulator-' + snapshot + '-*')
                     if (path / 'RisuNest-ios-simulator-agent.app.zip').is_file()), key=attempt)
assert candidates, 'No prebuilt simulator app for ' + snapshot
source = candidates[-1]
environment = (source / 'environment.txt').read_text()
assert 'snapshot_sha=' + snapshot + '\n' in environment, 'Prebuilt app comes from another snapshot'
metadata = json.loads((source / 'source.json').read_text())
assert metadata['lane'] == 'ios', 'Expected ios snapshot lane'
archive = source / 'RisuNest-ios-simulator-agent.app.zip'
assert hashlib.sha256(archive.read_bytes()).hexdigest() == (source / 'app.sha256').read_text().split()[0], 'Prebuilt app hash mismatch'
for name in ['source.json', 'environment.txt', 'app.sha256']:
    (artifacts / ('prebuilt-' + name)).write_bytes((source / name).read_bytes())

unpacked = pathlib.Path(os.environ['RUNNER_TEMP']) / 'prebuilt-app'
run(['ditto', '-x', '-k', str(archive), str(unpacked)])
apps = list(unpacked.glob('*.app'))
assert len(apps) == 1, apps
app = apps[0]
info = plistlib.loads((app / 'Info.plist').read_bytes())
assert info['CFBundleIdentifier'] == identifier and 'iPhoneSimulator' in info['CFBundleSupportedPlatforms'], 'Unexpected prebuilt app'
executable = info['CFBundleExecutable']

wanted_runtime = os.environ.get('IOS_CHECK_RUNTIME', '')
runtimes = [x for x in json.loads(run(['xcrun', 'simctl', 'list', 'runtimes', '--json']))['runtimes']
            if x['isAvailable'] and '.iOS-' in x['identifier'] and (not wanted_runtime or x['version'] == wanted_runtime or x['version'].startswith(wanted_runtime + '.'))]
assert runtimes, 'No available iOS runtime ' + wanted_runtime
runtime = sorted(runtimes, key=lambda x: (tuple(map(int, x['version'].split('.'))), len(x.get('supportedDeviceTypes', []))))[-1]
types = json.loads(run(['xcrun', 'simctl', 'list', 'devicetypes', '--json']))['devicetypes']
available = json.loads(run(['xcrun', 'simctl', 'list', 'devices', 'available', '--json']))['devices'].get(runtime['identifier'], [])
names = {item['name'] for item in available if item['isAvailable'] and item['name'].startswith('iPhone')}
if not names:
    names = {item['name'] for item in runtime.get('supportedDeviceTypes', []) if item.get('productFamily') == 'iPhone'}
phones = [item for item in types if item['name'] in names]
assert phones, 'No compatible iPhone device for ' + runtime['identifier']
phone = phones[0]
device = run(['xcrun', 'simctl', 'create', 'RisuNest prebuilt iOS', phone['identifier'], runtime['identifier']]).strip()

result = {
    'snapshot': snapshot,
    'sourceCommit': metadata['source_commit'],
    'prebuiltRun': os.environ['PREBUILT_RUN_ID'],
    'prebuiltArtifact': source.name,
    'buildXcode': [line for line in environment.splitlines() if line.startswith('Xcode ') or line.startswith('Build version')],
    'runXcode': run(['xcodebuild', '-version']).split(),
    'host': run(['sw_vers']).split(),
    'runtime': runtime['version'],
    'runtimeBuild': runtime.get('buildversion'),
    'runtimeIdentifier': runtime['identifier'],
    'device': phone['name'],
    'minimumOS': info.get('MinimumOSVersion'),
    'launches': [],
}

def launch(name, checkpoints):
    output = run(['xcrun', 'simctl', 'launch',
                  '--stdout=' + str(artifacts / (name + '.stdout.log')),
                  '--stderr=' + str(artifacts / (name + '.stderr.log')), device, identifier])
    print(output, flush=True)
    pid = int(output.strip().rsplit(':', 1)[1])
    record = {'name': name, 'pid': pid, 'aliveAfterSeconds': {}}
    result['launches'].append(record)
    elapsed = 0
    for seconds in checkpoints:
        time.sleep(seconds - elapsed)
        elapsed = seconds
        record['aliveAfterSeconds'][str(seconds)] = subprocess.run(['ps', '-p', str(pid)], capture_output=True).returncode == 0
        subprocess.run(['xcrun', 'simctl', 'io', device, 'screenshot', str(artifacts / (name + '-' + str(seconds) + 's.png'))], check=False)
    with (artifacts / (name + '.log')).open('w') as log:
        subprocess.run(['xcrun', 'simctl', 'spawn', device, 'log', 'show', '--last', str(elapsed + 30) + 's', '--style', 'compact',
                        '--predicate', 'processIdentifier == ' + str(pid) + ' OR process BEGINSWITH "com.apple.WebKit"'],
                       stdout=log, stderr=subprocess.STDOUT, check=False)

try:
    run(['xcrun', 'simctl', 'boot', device])
    run(['xcrun', 'simctl', 'bootstatus', device, '-b'])
    run(['codesign', '--force', '--deep', '--sign', '-', str(app)])
    run(['xcrun', 'simctl', 'install', device, str(app)])
    launch('product-first-launch', [15, 60, 120])
    subprocess.run(['xcrun', 'simctl', 'terminate', device, identifier], check=False)
    time.sleep(3)
    launch('product-second-launch', [15, 60])
finally:
    reports = pathlib.Path.home() / 'Library/Logs/DiagnosticReports'
    crashes = artifacts / 'crash-reports'
    for report in reports.glob(executable + '*') if reports.is_dir() else []:
        crashes.mkdir(exist_ok=True)
        (crashes / report.name).write_bytes(report.read_bytes())
    (artifacts / 'prebuilt-launch.json').write_text(json.dumps(result, indent=1))
    subprocess.run(['xcrun', 'simctl', 'shutdown', device], check=False)
