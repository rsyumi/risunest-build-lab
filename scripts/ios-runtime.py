import json, os, pathlib, plistlib, subprocess, sys, time, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

root = pathlib.Path.cwd()
artifacts = root / 'artifacts'
bench = root / 'benchmarks/ios/native'
product_binary=pathlib.Path((artifacts/'executable-path.txt').read_text()).read_bytes()
assert all(marker not in product_binary for marker in [b'ios_bench_phase',b'ios_bench_report',b'RISUNEST_IOS_PHASE']), 'Verification commands in product binary'
del product_binary

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
project = apple / 'project.yml'
text = project.read_text()
assert 'node tauri ios xcode-script' in text, 'Unexpected generated Xcode command'
project.write_text(text.replace('node tauri ios xcode-script', 'node ' + str(root/'node_modules/@tauri-apps/cli/tauri.js') + ' ios xcode-script'))
print(run(['xcodegen','generate','--spec','project.yml'],cwd=apple))
for path in apple.glob('*_iOS/Info.plist'):
    info=plistlib.loads(path.read_bytes())
    info['BGTaskSchedulerPermittedIdentifiers']=['io.github.rsyumi.risunest.ios.bench.generation']
    info['UIBackgroundModes']=['processing']
    info['UIFileSharingEnabled']=True
    info['LSSupportsOpeningDocumentsInPlace']=True
    path.write_bytes(plistlib.dumps(info))

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
            complete_text=text[:text.rfind('\n')+1]
            records=[json.loads(line) for line in complete_text.splitlines() if line]
            (artifacts/('ios-'+phase+'.jsonl')).write_text(text)
            if any(x['stage']=='failure' for x in records): raise RuntimeError(records[-1])
            if any(x['stage']=='complete' for x in records): return records
        time.sleep(2)
    raise RuntimeError('Timed out: '+phase)

class StreamHandler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        if self.path not in ['/stream','/slow']:
            self.send_error(404); return
        payload=('data: {"text":"합성🐿️"}\n\n' * (8 if self.path=='/stream' else 200)).encode()
        self.send_response(200)
        self.send_header('Content-Type','text/event-stream')
        self.send_header('Content-Length',str(len(payload)))
        self.end_headers()
        try:
            for offset in range(0,len(payload),7):
                self.wfile.write(payload[offset:offset+7]); self.wfile.flush(); time.sleep(.01)
        except (BrokenPipeError, ConnectionResetError): pass
server=ThreadingHTTPServer(('127.0.0.1',0),StreamHandler)
threading.Thread(target=server.serve_forever,daemon=True).start()

def launch(phase):
    child=os.environ.copy();child['SIMCTL_CHILD_RISUNEST_IOS_PHASE']=phase
    child['SIMCTL_CHILD_RISUNEST_IOS_STREAM_URL']='http://127.0.0.1:'+str(server.server_port)
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
    launch('restore');collect('restore',120)
    launch('background');time.sleep(3)
    print(run(['xcrun','simctl','launch',device,'com.apple.Preferences']))
    time.sleep(10)
    print(run(['xcrun','simctl','launch',device,identifier]))
    collect('background',120)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-harness.png')]))
    uitests=root/'.ios-ui-tests'
    uitests.mkdir()
    spec={
        'name':'RisuNestUITests',
        'targets':{'RisuNestUITests':{
            'type':'bundle.ui-testing','platform':'iOS','deploymentTarget':'16.4',
            'sources':[str(root/'benchmarks/ios/NativeUITests.swift')],
            'settings':{'base':{'GENERATE_INFOPLIST_FILE':'YES','PRODUCT_BUNDLE_IDENTIFIER':identifier+'.uitests'}}}},
        'schemes':{'RisuNestUITests':{'build':{'targets':{'RisuNestUITests':['test']}},'test':{'targets':['RisuNestUITests']}}}
    }
    (uitests/'project.json').write_text(json.dumps(spec))
    print(run(['xcodegen','generate','--spec','project.json'],cwd=uitests))
    print(run(['xcodebuild','test','-project','RisuNestUITests.xcodeproj','-scheme','RisuNestUITests',
               '-destination','platform=iOS Simulator,id='+device,'-derivedDataPath',str(uitests/'DerivedData'),
               '-resultBundlePath',str(artifacts/'ios-ui.xcresult'),'CODE_SIGNING_ALLOWED=NO'],cwd=uitests))
    product=pathlib.Path((artifacts/'app-path.txt').read_text())
    print(run(['codesign','--force','--deep','--sign','-',str(product)]))
    print(run(['xcrun','simctl','install',device,str(product)]))
    print(run(['xcrun','simctl','launch',device,'io.github.rsyumi.risunest']))
    time.sleep(15)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-product-first-launch.png')]))
    print(run(['xcrun','simctl','shutdown',device]))
    tablet=[x for x in types if x['name'].startswith('iPad')][-1]
    device=run(['xcrun','simctl','create','RisuNest synthetic iPad',tablet['identifier'],runtime['identifier']]).strip()
    print(run(['xcrun','simctl','boot',device]))
    print(run(['xcrun','simctl','bootstatus',device,'-b']))
    print(run(['xcrun','simctl','install',device,str(app)]))
    launch('ipad');collect('ipad',120)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-ipad.png')]))
    print(run(['xcrun','simctl','install',device,str(product)]))
    print(run(['xcrun','simctl','launch',device,'io.github.rsyumi.risunest']))
    time.sleep(15)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-product-ipad.png')]))
    (artifacts/'ios-runtime-result.json').write_text(json.dumps({'passed':True,'restartExact':True}))
finally:
    subprocess.run(['xcrun','simctl','shutdown',device],check=False)
