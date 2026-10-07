import json, os, pathlib, plistlib, secrets, shutil, socket, ssl, subprocess, sys, tempfile, time, threading, urllib.error, urllib.request
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
    info['NSLocalNetworkUsageDescription']='Connect to the synthetic local verification server.'
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
entitlements=artifacts/'ios-bench-simulator.entitlements'
entitlements.write_bytes(plistlib.dumps({'application-identifier':'RISUNESTSM.'+identifier,'keychain-access-groups':['RISUNESTSM.'+identifier]}))
print(run(['codesign','--force','--sign','-','--entitlements',str(entitlements),str(app)]))
runtimes=json.loads(run(['xcrun','simctl','list','runtimes','--json']))['runtimes']
runtime=sorted([x for x in runtimes if x['isAvailable'] and '.iOS-' in x['identifier']],key=lambda x:tuple(map(int,x['version'].split('.'))))[-1]
types=json.loads(run(['xcrun','simctl','list','devicetypes','--json']))['devicetypes']
available=json.loads(run(['xcrun','simctl','list','devices','available','--json']))['devices'][runtime['identifier']]
def compatible_type(prefix):
    names={item['name'] for item in available if item['isAvailable'] and item['name'].startswith(prefix)}
    candidates=[item for item in types if item['name'] in names]
    assert candidates, 'No compatible '+prefix+' device for '+runtime['identifier']
    return candidates[0]
phone=compatible_type('iPhone')
device=run(['xcrun','simctl','create','RisuNest synthetic iOS',phone['identifier'],runtime['identifier']]).strip()
(artifacts/'ios-device.json').write_text(json.dumps({'udid':device,'runtime':runtime['version'],'device':phone['name']}))

current_pid=None
device_prefix=''
memory={}
def collect(phase,timeout=600):
    container=pathlib.Path(run(['xcrun','simctl','get_app_container',device,identifier,'data']).strip())
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        if current_pid is not None:
            rss=subprocess.run(['ps','-p',str(current_pid),'-o','rss='],capture_output=True,text=True)
            if rss.returncode != 0:
                raise RuntimeError('Owned simulator process exited during '+phase)
            if rss.returncode == 0 and rss.stdout.strip().isdigit():
                memory.setdefault(device_prefix+phase,[]).append(int(rss.stdout.strip()))
                (artifacts/'ios-native-rss-kib.json').write_text(json.dumps(memory))
        matches=list(container.rglob('verification-'+phase+'.jsonl'))
        if matches:
            text=matches[0].read_text()
            complete_text=text[:text.rfind('\n')+1]
            records=[json.loads(line) for line in complete_text.splitlines() if line]
            (artifacts/('ios-'+device_prefix+phase+'.jsonl')).write_text(text)
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

def start_webdav():
    rclone=shutil.which('rclone')
    assert rclone, 'rclone unavailable'
    base=pathlib.Path(tempfile.mkdtemp(prefix='risunest-webdav-',dir=os.environ.get('RUNNER_TEMP'))).resolve()
    served=base/'served'
    served.mkdir()
    tag=secrets.token_hex(4)
    (base/'ca.cnf').write_text('[req]\ndistinguished_name=dn\nx509_extensions=v3_ca\nprompt=no\n[dn]\nCN=RisuNest synthetic loopback CA '+tag+'\n'
                               '[v3_ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n')
    (base/'server.ext').write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\n'
                                   'extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n')
    run(['openssl','req','-x509','-new','-newkey','rsa:2048','-nodes','-sha256','-days','30','-keyout','ca.key','-out','ca.pem','-config','ca.cnf'],cwd=base)
    run(['openssl','req','-new','-newkey','rsa:2048','-nodes','-sha256','-keyout','server.key','-out','server.csr','-subj','/CN=127.0.0.1 '+tag],cwd=base)
    run(['openssl','x509','-req','-in','server.csr','-CA','ca.pem','-CAkey','ca.key','-set_serial',str(secrets.randbits(63)),'-days','30','-sha256','-extfile','server.ext','-out','server.pem'],cwd=base)
    (base/'ca.key').unlink()
    print(run(['xcrun','simctl','keychain',device,'add-root-cert',str(base/'ca.pem')]))
    probe_context=ssl.create_default_context(cafile=str(base/'ca.pem'))
    with socket.socket() as probe:
        probe.bind(('127.0.0.1',0))
        port=probe.getsockname()[1]
    user='synthetic-'+secrets.token_hex(4)
    password=secrets.token_urlsafe(24)
    if os.environ.get('GITHUB_ACTIONS')=='true':
        print('::add-mask::'+password,flush=True)
    log=(artifacts/'webdav-server.log').open('w')
    process=subprocess.Popen([rclone,'serve','webdav',str(served),'--addr','127.0.0.1:'+str(port),'--config',str(base/'rclone.conf'),
                              '--cert',str(base/'server.pem'),'--key',str(base/'server.key')],
                             env={**os.environ,'RCLONE_USER':user,'RCLONE_PASS':password},stdout=log,stderr=subprocess.STDOUT)
    endpoint='https://127.0.0.1:'+str(port)
    deadline=time.monotonic()+60
    while True:
        assert process.poll() is None, 'WebDAV server exited'
        assert time.monotonic()<deadline, 'WebDAV server did not listen'
        try:
            urllib.request.urlopen(endpoint+'/',timeout=5,context=probe_context)
            raise RuntimeError('WebDAV server accepted an unauthenticated request')
        except urllib.error.HTTPError as error:
            assert error.code==401, error.code
            break
        except (urllib.error.URLError,ConnectionError,TimeoutError):
            time.sleep(.3)
    inputs={'URL':endpoint,'USER':user,'PASSWORD':password,'ROOT':'RisuNest'}
    runner={'TEST_RUNNER_RISUNEST_IOS_WEBDAV_'+key:value for key,value in inputs.items()}
    runner['TEST_RUNNER_RISUNEST_IOS_EXTERNAL_BEFORE']='webdav-before-'+secrets.token_hex(6)
    runner['TEST_RUNNER_RISUNEST_IOS_EXTERNAL_AFTER']='webdav-after-'+secrets.token_hex(6)
    def stop():
        files=[path for path in served.rglob('*') if path.is_file()]
        shape={'folders':sorted({path.relative_to(served).parts[0] for path in files}),'files':len(files),'bytes':sum(path.stat().st_size for path in files)}
        (artifacts/'webdav-store.json').write_text(json.dumps(shape,indent=2))
        print('webdav-store:',json.dumps(shape),flush=True)
        process.terminate()
        try: process.wait(timeout=10)
        except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=10)
        log.close()
        shutil.rmtree(base,ignore_errors=True)
    return runner,stop

def launch(phase):
    global current_pid
    child=os.environ.copy();child['SIMCTL_CHILD_RISUNEST_IOS_PHASE']=phase
    child['SIMCTL_CHILD_RISUNEST_IOS_STREAM_URL']='http://127.0.0.1:'+str(server.server_port)
    output=run(['xcrun','simctl','launch','--terminate-running-process',
                '--stdout='+str(artifacts/('ios-'+device_prefix+phase+'.stdout.log')),
                '--stderr='+str(artifacts/('ios-'+device_prefix+phase+'.stderr.log')),device,identifier],env=child)
    print(output)
    current_pid=int(output.strip().rsplit(':',1)[1])

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
    launch('app');collect('app',180)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-product-chat.png')]))
    launch('app-restart');collect('app-restart',120)
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
    webdav,stop_webdav=start_webdav()
    try:
        output=run(['xcodebuild','test','-project','RisuNestUITests.xcodeproj','-scheme','RisuNestUITests',
               '-destination','platform=iOS Simulator,id='+device,'-derivedDataPath',str(uitests/'DerivedData'),
               '-resultBundlePath',str(artifacts/'ios-ui.xcresult'),
               '-skip-testing:RisuNestUITests/NativeUITests/testLegacyRestore100Raw',
               '-skip-testing:RisuNestUITests/NativeUITests/testLegacyRestore100Gzip',
               '-skip-testing:RisuNestUITests/NativeUITests/testLegacyRestore300Raw',
               '-skip-testing:RisuNestUITests/NativeUITests/testLegacyRestore300Gzip',
               '-skip-testing:RisuNestUITests/NativeUITests/testLegacyRestore600Raw',
               '-skip-testing:RisuNestUITests/NativeUITests/testLegacyRestore600Gzip',
               'CODE_SIGNING_ALLOWED=NO'],cwd=uitests,env={**os.environ,**webdav})
    finally:
        stop_webdav()
    print(output)
    product=pathlib.Path((artifacts/'app-path.txt').read_text())
    print(run(['codesign','--force','--deep','--sign','-',str(product)]))
    print(run(['xcrun','simctl','install',device,str(product)]))
    print(run(['xcrun','simctl','launch',device,'io.github.rsyumi.risunest']))
    time.sleep(15)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-product-first-launch.png')]))
    print(run(['xcrun','simctl','shutdown',device]))
    tablet=compatible_type('iPad')
    device_prefix='ipad-'
    device=run(['xcrun','simctl','create','RisuNest synthetic iPad',tablet['identifier'],runtime['identifier']]).strip()
    print(run(['xcrun','simctl','boot',device]))
    print(run(['xcrun','simctl','bootstatus',device,'-b']))
    print(run(['xcrun','simctl','install',device,str(app)]))
    launch('ipad');collect('ipad',120)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-ipad.png')]))
    launch('app');collect('app',180)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-product-chat-ipad.png')]))
    launch('app-restart');collect('app-restart',120)
    print(run(['xcrun','simctl','install',device,str(product)]))
    print(run(['xcrun','simctl','launch',device,'io.github.rsyumi.risunest']))
    time.sleep(15)
    print(run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-product-ipad.png')]))
    (artifacts/'ios-runtime-result.json').write_text(json.dumps({'passed':True,'restartExact':True,'productUIPhoneAndPad':True}))
except Exception:
    subprocess.run(['xcrun','simctl','io',device,'screenshot',str(artifacts/'ios-failure.png')],check=False)
    if current_pid:
        with (artifacts/'ios-owned-process.log').open('w') as output:
            subprocess.run(['xcrun','simctl','spawn',device,'log','show','--last','3m','--style','compact',
                            '--predicate','processIdentifier == '+str(current_pid)],stdout=output,stderr=subprocess.STDOUT,check=False)
    raise
finally:
    if (artifacts/'ios-ui.xcresult').exists():
        subprocess.run(['xcrun','xcresulttool','export','attachments','--path',str(artifacts/'ios-ui.xcresult'),
                        '--output-path',str(artifacts/'ios-ui-attachments')],check=False)
    subprocess.run(['xcrun','simctl','shutdown',device],check=False)
