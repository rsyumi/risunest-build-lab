import json, os, pathlib, plistlib, subprocess, sys, time

root = pathlib.Path.cwd()
artifacts = root / 'artifacts'
bench = root / 'benchmarks/ios/native'

def run(args, cwd=root, env=None):
    print('+', ' '.join(map(str,args)), flush=True)
    try:
        return subprocess.check_output(args, cwd=cwd, env=env, text=True, stderr=subprocess.STDOUT)
    except subprocess.CalledProcessError as error:
        print(error.output, flush=True)
        raise

env = os.environ.copy()
env['TAURI_ENV_PLATFORM'] = 'ios'
print(run(['pnpm','exec','vite','build','--mode','agent','--config','benchmarks/ios/vite.config.ts'],env=env))
print(run(['node',str(root/'node_modules/@tauri-apps/cli/tauri.js'),'ios','init','--ci','--skip-targets-install'],cwd=bench))
apple = bench / 'gen/apple'
for path in apple.glob('Sources/**/*'):
    if path.is_file() and path.suffix in ['.mm','.h']:
        path.write_text(path.read_text().replace('start_app', 'ios_bench_start'))
print(run(['node',str(root/'node_modules/@tauri-apps/cli/tauri.js'),'ios','build','--ci','--target','aarch64-sim','--no-sign'],cwd=bench))
identifier = 'io.github.rsyumi.risunest.ios.bench'
apps = []
for app in apple.rglob('*.app'):
    info = app / 'Info.plist'
    if info.exists():
        data=plistlib.loads(info.read_bytes())
        if data.get('CFBundleIdentifier') == identifier and data.get('CFBundleSupportedPlatforms') == ['iPhoneSimulator']:
            apps.append(app)
assert len(apps)==1, apps
app=apps[0]
print(run(['codesign','--force','--deep','--sign','-',str(app)]))
runtimes=json.loads(run(['xcrun','simctl','list','runtimes','--json']))['runtimes']
runtime=sorted([x for x in runtimes if x['isAvailable'] and '.iOS-' in x['identifier']],key=lambda x:tuple(map(int,x['version'].split('.'))))[-1]
types=json.loads(run(['xcrun','simctl','list','devicetypes','--json']))['devicetypes']
phone=[x for x in types if x['name'].startswith('iPhone')][-1]
device=run(['xcrun','simctl','create','RisuNest synthetic iOS',phone['identifier'],runtime['identifier']]).strip()
(artifacts/'ios-device.json').write_text(json.dumps({'udid':device,'runtime':runtime['version'],'device':phone['name']}))

def collect(phase,timeout=600):
    container=pathlib.Path(run(['xcrun','simctl','get_app_container',device,identifier,'data']).strip())
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        matches=list(container.rglob('verification-'+phase+'.jsonl'))
        if matches:
            text=matches[0].read_text()
            records=[json.loads(line) for line in text.splitlines(keepends=True) if line.strip() and line.endswith('\n')]
            (artifacts/('ios-'+phase+'.jsonl')).write_text(text)
            if any(x['stage']=='failure' for x in records): raise RuntimeError(records[-1])
            if any(x['stage']=='complete' for x in records): return records
        time.sleep(2)
    raise RuntimeError('Timed out: '+phase)

def launch(phase):
    child=os.environ.copy();child['SIMCTL_CHILD_RISUNEST_IOS_PHASE']=phase
    print(run(['xcrun','simctl','launch','--terminate-running-process',device,identifier],env=child))

try:
    print(run(['xcrun','simctl','boot',device]))
    print(run(['xcrun','simctl','bootstatus',device,'-b']))
    print(run(['xcrun','simctl','install',device,str(app)]))
    launch('contracts');first=collect('contracts')
    launch('reload');second=collect('reload',120)
    before=next(x['result'] for x in first if x['stage']=='persistence')
    after=next(x['result'] for x in second if x['stage']=='reload')
    assert before['finalHash']==after['finalHash'] and before['revision']==after['revision'], 'restart changed stored data'
    launch('background');time.sleep(3)
    print(run(['xcrun','simctl','launch',device,'com.apple.Preferences']))
    time.sleep(10)
    print(run(['xcrun','simctl','launch',device,identifier]))
    collect('background',120)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-harness.png')]))
    product=pathlib.Path((artifacts/'app-path.txt').read_text())
    print(run(['codesign','--force','--deep','--sign','-',str(product)]))
    print(run(['xcrun','simctl','install',device,str(product)]))
    print(run(['xcrun','simctl','launch',device,'io.github.rsyumi.risunest']))
    time.sleep(15)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-product-first-launch.png')]))
    (artifacts/'ios-runtime-result.json').write_text(json.dumps({'passed':True,'restartExact':True}))
finally:
    subprocess.run(['xcrun','simctl','shutdown',device],check=False)
