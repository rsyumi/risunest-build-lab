import { chromium } from '@playwright/test';
import fs from 'node:fs/promises';
import { nativeClient } from './native-client.mjs';
import { REALM_BLOCKED_URL_PATTERNS } from '../../scripts/realmBlocklist.mjs';
const [url,binary,out,mode='verify']=process.argv.slice(2);
if(!['127.0.0.1','localhost'].includes(new URL(url).hostname))throw Error('Loopback required');
const units=s=>Array.from({length:s.length},(_,i)=>s.charCodeAt(i));
const text=v=>typeof v==='string'?v:Array.from({length:Math.ceil(v.length/8192)},(_,i)=>String.fromCharCode(...v.slice(i*8192,(i+1)*8192))).join('');
const context={variables:[['v','A'],['score','7']].map(([k,v])=>[units(k),units(v)]),globals:[],index:7,user:units('Synthetic'),seed:12345};
const lanes=widths=>widths.reduce((all,n)=>all.flatMap(t=>Array.from({length:n},(_,i)=>[...t,(i+0.5)/n])),[[]]);
const histogram=values=>Object.fromEntries([...values.reduce((m,v)=>m.set(v,(m.get(v)??0)+1),new Map())].sort(([a],[b])=>a.localeCompare(b)));
const assert=(name,ok)=>{if(!ok)throw Error(name)};
const browser=await chromium.launch({headless:true});
let native;
try {
 native=nativeClient(binary);
 const page=await browser.newPage();
 await page.route('**/*',r=>['127.0.0.1','localhost'].includes(new URL(r.request().url()).hostname)?r.continue():r.abort());
 const cdp=await page.context().newCDPSession(page);await cdp.send('Network.enable');await cdp.send('Network.setBlockedURLs',{urls:REALM_BLOCKED_URL_PATTERNS});
 await page.goto(url);await page.waitForFunction(()=>window.__streamingSmoke?.marker==='synthetic-v1');
 await page.evaluate(async()=>{
  const {risuChatParser}=await import('/src/ts/parser/parser.svelte.ts');
  window.__rngParse=(source,tape=null,patch={})=>{
   const old=Math.random,values={v:'A',score:'7',...patch},writes=[];let draws=0;
   if(tape)Math.random=()=>{if(draws>=tape.length)throw Error('Oracle tape exhausted');return tape[draws++]};
   const start=performance.now();
   try {const output=risuChatParser(source,{chatID:7,userName:'Synthetic',runVar:true,getChatVar:k=>values[k]??'null',setChatVar:(k,v)=>{values[k]=v;writes.push([k,v])},globalChatVariables:{},functions:new Map([['f',{data:'{{setvar::x::1}}{{random::A::B}}',arg:[]}]])});return{output,draws,writes,ms:performance.now()-start}}finally{Math.random=old}
  };
 });
 const oracle=(source,tape)=>page.evaluate(({source,tape})=>window.__rngParse(source,tape),{source,tape});
 const register=(id,source)=>native.send({op:'relax-register',id,text:source.isWellFormed()?source:units(source),context});
 if(mode==='verify') {
  const cases=[
   {id:'choice',source:'{{random::A::B}}',widths:[2]},
   {id:'duplicates',source:'{{random::A::A::B}}',widths:[3]},
   {id:'json',source:'{{random::["A",1.0,{"2":1,"1":2}]}}',widths:[3]},
   {id:'escaped-comma',source:'{{random::a\\,b,c}}',widths:[2]},
   {id:'empty-array',source:'{{random::[]}}',widths:[1]},
   {id:'randint',source:'{{randint::-2::1}}',widths:[4]},
   {id:'dice-triangle',source:'{{dice::2d6}}',widths:[6,6]},
   {id:'dice-zero-count',source:'{{dice::0d6}}',widths:[]},
   {id:'roll-triangle',source:'{{roll::2d6}}',widths:[6,6]},
   {id:'roll-default-count',source:'{{roll::d2}}',widths:[2]},
   {id:'roll-single-side-argument',source:'{{roll::2}}',widths:[2]},
   {id:'roll-no-arguments',source:'{{roll}}',widths:[]},
   {id:'shared-temp',source:'{{settempvar::x::{{random::A::B}}}}{{tempvar::x}}|{{tempvar::x}}',widths:[2]},
   {id:'independent-joint',source:'{{random::A::B}}|{{random::A::B}}',widths:[2,2]},
   {id:'conditional',source:'{{settempvar::x::{{random::0::1}}}}{{#if {{tempvar::x}}}}{{random::A::B}}{{/if}}',widths:[2,2]},
   {id:'eager-when-temp',source:'{{#when 0}}{{settempvar::x::{{random::A::B}}}}ignored{{/when}}{{tempvar::x}}',widths:[2]},
   {id:'false-if-pure',source:'{{#if 0}}{{random::A::B}}{{/if}}X',widths:[]},
   {id:'dynamic-safe-operation',source:'{{{{random::getvar::tempvar}}::v}}',widths:[2]},
   {id:'unknown-correlated',source:'{{{{random::getvar::upper}}::v}}',reject:true},
   {id:'write-correlated',source:'{{{{random::tempvar::setvar}}::x::1}}',reject:true},
   {id:'host-write',source:'{{setvar::x::{{random::A::B}}}}{{getvar::x}}',reject:true},
   {id:'false-if-function',source:'{{#if 0}}{{call::f}}{{/if}}',reject:true},
   ...['{{random}}','{{pick::A::B}}','{{rollp::2d6}}','{{rollpick::2d6}}','{{hash::x}}','{{randint::0.5::2}}','{{randint::3::1}}','{{randint::0::Infinity}}','{{dice::1.5d2}}','{{dice::2d0}}','{{dice::Infinityd2}}','{{roll::1.5d2}}','{{roll::1d1.5}}'].map((source,i)=>({id:'excluded-'+i,source,reject:true})),
   ...Array.from({length:9},(_,i)=>({id:'independent-'+(i+1),source:Array.from({length:i+1},()=>'{{random::A::B}}').join('|'),widths:Array(i+1).fill(2),reject:i>=8})),
  ];
  const rows=[];let laneCount=0;
  for(const item of cases) {
   const p=await register('probe',item.source);
   if(item.reject){assert('Expected preflight rejection '+item.id,!!p.fallback);rows.push({id:item.id,fallback:p.fallback});continue}
   assert('Unexpected preflight rejection '+item.id,!p.fallback);assert('Preflight entropy '+item.id,p.entropyWords===0);
   const tapes=lanes(item.widths);const expected=await page.evaluate(({source,tapes})=>tapes.map(t=>window.__rngParse(source,t)),{source:item.source,tapes});
   const outputs=[];
   for(let i=0;i<tapes.length;i++){
    const r=await native.send({op:'relax-eval',id:'probe',tape:tapes[i]});assert('Lane mismatch '+item.id,text(r.output)===expected[i].output);assert('Unexpected host writes '+item.id,expected[i].writes.length===0);outputs.push(text(r.output));laneCount++;
   }
   assert('Probability mass '+item.id,JSON.stringify(histogram(outputs))===JSON.stringify(histogram(expected.map(e=>e.output))));
   rows.push({id:item.id,lanes:tapes.length,nodes:p.nodes,leaves:p.leaves,bytes:p.retainedBytes,histogram:histogram(outputs)});
  }
  assert('Duplicate weight',JSON.stringify(rows.find(r=>r.id==='duplicates').histogram)===JSON.stringify({A:2,B:1}));
  const triangle=rows.find(r=>r.id==='dice-triangle').histogram;
  for(let n=2;n<=12;n++)assert('Triangular probability '+n,triangle[String(n)]===6-Math.abs(7-n));
  assert('Shared choice has two outcomes',Object.keys(rows.find(r=>r.id==='shared-temp').histogram).length===2);
  assert('Independent joint has four outcomes',Object.keys(rows.find(r=>r.id==='independent-joint').histogram).length===4);
  const correlated='{{{{random::getvar::upper}}::v}}';const direct=[],naive=[];
  for(const [first,retry]of lanes([2,2])){direct.push((await oracle(correlated,[first])).output);naive.push((await oracle(correlated,[first<0.5?first:retry])).output)}
  assert('Correlated fallback changes probability',JSON.stringify(histogram(direct))!==JSON.stringify(histogram(naive)));
  const writes=await oracle('{{setvar::x::{{random::A::B}}}}{{getvar::x}}',[0.75]);assert('Original fallback writes once',writes.output==='B'&&JSON.stringify(writes.writes)===JSON.stringify([['x','B']]));
  await register('fresh','{{settempvar::x::{{random::A::B}}}}{{tempvar::x}}|{{tempvar::x}}');
  const fresh=[];for(const tape of [[0.25],[0.75],[0.25]])fresh.push(text((await native.send({op:'relax-eval',id:'fresh',tape})).output));assert('Fresh invocations',JSON.stringify(fresh)===JSON.stringify(['A|A','B|B','A|A']));
  const abort=await native.send({op:'relax-eval',id:'fresh',tape:[]});assert('Sampling failure aborts',abort.abort==='rng-tape-exhausted'&&!abort.fallback);
  const postFailure=[];for(const first of [0.25,0.75]){const r=await native.send({op:'relax-eval',id:'fresh',tape:[first]});const chosen=text(r.output);postFailure.push(chosen==='B|B'?'abort-after-B':chosen);const replay=await native.send({op:'relax-eval',id:'fresh',tape:[first]});assert('Explicit recovery replays choice',text(replay.output)===chosen)}assert('Post-choice failure retained',postFailure.includes('abort-after-B'));
  const generated='{{random::["\\u007b\\u007bupper\\u003a\\u003av\\u007d\\u007d","\\u007b\\u007blength\\u003a\\u003aabc\\u007d\\u007d"]}}';
  const checkpoint=[];
  for(const first of [0.25,0.75]){
   await register('checkpoint',generated);const intermediate=text((await native.send({op:'relax-eval',id:'checkpoint',tape:[first]})).output);
   const p=await register('checkpoint-next',intermediate);const actual=p.fallback?(await oracle(intermediate,[])).output:text((await native.send({op:'relax-eval',id:'checkpoint-next',tape:[]})).output);
   const expectedIntermediate=(await oracle(generated,[first])).output;const expected=(await oracle(expectedIntermediate,[])).output;assert('Retain completed checkpoint',actual===expected);checkpoint.push({first,fallback:!!p.fallback,output:actual});
  }
  assert('Generated checkpoint actually falls back',checkpoint[0].fallback===true&&checkpoint[0].output==='V'&&checkpoint[1].fallback===false&&checkpoint[1].output==='3');
  await register('entropy','{{random::A::B}}');const unseeded=[];
  for(let i=0;i<64;i++){const r=await native.send({op:'relax-eval',id:'entropy'});assert('Unseeded domain',['A','B'].includes(text(r.output))&&r.choices.length===1&&r.entropyWords>=1);unseeded.push(text(r.output))}
  await native.send({op:'context',context:{...context,variables:[[units('v'),units('B')]]}});const stale=await native.send({op:'relax-eval',id:'entropy',tape:[0.25]});assert('Context invalidates tree',stale.abort==='missing-program');
  const report={cases:cases.length,admitted:rows.filter(r=>!r.fallback).length,rejected:rows.filter(r=>r.fallback).length,laneCount,mismatches:0,rows,correlated:{original:histogram(direct),naiveResampling:histogram(naive),safePolicy:'preflight-reject-before-entropy'},singleWriteFallback:writes.writes,fresh,postFailure,checkpoint,unseededSamples:unseeded.length,unseededHistogram:histogram(unseeded),notes:'Finite lane histograms are exact enumeration; unseeded samples only check domain and entropy use, not statistical proof.'};
  await fs.writeFile(out,JSON.stringify(report,null,2));console.log(JSON.stringify({...report,rows:rows.map(({histogram,...r})=>r)},null,2));
 } else if(mode==='cache-measure') {
  const source='{{getvar::v}}/{{settempvar::x::{{random::A::B}}}}'+Array(1000).fill('{{tempvar::x}}').join('');
  const samples=[];const nativeRuns=100,originalRuns=100,cacheRuns=1000000;
  for(let round=0;round<14;round++){
   const js=await page.evaluate(({source,originalRuns,cacheRuns})=>{
    const build=patch=>[0.25,0.75].map(u=>window.__rngParse(source,[u],patch).output);
    let start=performance.now();let leaves=build({v:'A'});const preparationMs=performance.now()-start;
    if(leaves[0]!=='A/'+ 'A'.repeat(1000)||leaves[1]!=='A/'+'B'.repeat(1000))throw Error('JS cache lanes');
    const old=Math.random;let draws=0,lastUnit=0;Math.random=()=>{draws++;lastUnit=old();return lastUnit};
    let originalChecksum=0,cacheChecksum=0,originalExpectedChecksum=0,cacheExpectedChecksum=0;
    try{
     start=performance.now();for(let i=0;i<originalRuns;i++){const value=window.__rngParse(source).output;const chosen=Math.floor(lastUnit*2),last=value.charCodeAt(value.length-1);if(last!==65+chosen)throw Error('Original selected unit');originalChecksum+=value.length+last;originalExpectedChecksum+=value.length+65+chosen}
     const originalMs=performance.now()-start,originalDraws=draws;draws=0;
     start=performance.now();for(let i=0;i<cacheRuns;i++){const chosen=Math.floor(Math.random()*2),value=leaves[chosen],last=value.charCodeAt(value.length-1);if(last!==65+chosen)throw Error('Cached selected unit');cacheChecksum+=value.length+last;cacheExpectedChecksum+=value.length+65+chosen}
     const cacheMs=performance.now()-start,cacheDraws=draws;
     start=performance.now();leaves=build({v:'C'});const invalidateAndRebuildMs=performance.now()-start;
     if(!leaves.every(value=>value.startsWith('C/')))throw Error('JS context invalidation');
     return{preparationMs,originalMs,cacheMs,originalDraws,cacheDraws,originalChecksum,cacheChecksum,originalExpectedChecksum,cacheExpectedChecksum,invalidateAndRebuildMs,leafStringBytes:leaves.reduce((n,s)=>n+s.length*2,0)};
    }finally{Math.random=old}
   },{source,originalRuns,cacheRuns});
   assert('Original fresh draws',js.originalDraws===originalRuns);assert('Cached fresh draws',js.cacheDraws===cacheRuns);
   assert('Original expected checksum',js.originalChecksum===js.originalExpectedChecksum);assert('Cached expected checksum',js.cacheChecksum===js.cacheExpectedChecksum);
   assert('Original checksum domain',js.originalChecksum>=originalRuns*1067&&js.originalChecksum<=originalRuns*1068);
   assert('Cached checksum domain',js.cacheChecksum>=cacheRuns*1067&&js.cacheChecksum<=cacheRuns*1068);
   await native.send({op:'context',context});let start=performance.now();const p=await native.send({op:'relax-register',id:'cache',text:source});const preparationPipeMs=performance.now()-start;assert('Native cache prepare',!p.fallback);
   let nativeCoreMs=0,nativeChecksum=0,nativeExpectedChecksum=0,nativeChoices=0,nativeEntropyWords=0;start=performance.now();
   for(let i=0;i<nativeRuns;i++){
    const r=await native.send({op:'relax-eval',id:'cache'});assert('Native cache abort',!r.abort);const value=text(r.output);assert('Native context output',value.startsWith('A/'));nativeCoreMs+=r.coreMs;assert('Native selected unit',value.charCodeAt(value.length-1)===65+r.choices[0].index);nativeChecksum+=value.length+value.charCodeAt(value.length-1);nativeExpectedChecksum+=value.length+65+r.choices[0].index;nativeChoices+=r.choices.length;nativeEntropyWords+=r.entropyWords;
   }
   const nativePipeMs=performance.now()-start;assert('Native expected checksum',nativeChecksum===nativeExpectedChecksum);assert('Native fresh choice opportunities',nativeChoices===nativeRuns&&nativeEntropyWords>=nativeRuns);assert('Native checksum domain',nativeChecksum>=nativeRuns*1067&&nativeChecksum<=nativeRuns*1068);
   start=performance.now();const update=await native.send({op:'context',context:{...context,variables:[[units('v'),units('C')]]}});const rebuilt=await native.send({op:'relax-register',id:'cache',text:source});const invalidationPipeMs=performance.now()-start;
   const updated=await native.send({op:'relax-eval',id:'cache',tape:[0.25]});assert('Native context invalidation',text(updated.output)==='C/'+'A'.repeat(1000));
   if(round>=3)samples.push({js,native:{preparationCoreMs:p.coreMs,preparationPipeMs,coreMs:nativeCoreMs,pipeMs:nativePipeMs,checksum:nativeChecksum,expectedChecksum:nativeExpectedChecksum,choices:nativeChoices,entropyWords:nativeEntropyWords,invalidationCoreMs:update.coreMs+rebuilt.coreMs,invalidationPipeMs,retainedBytes:p.retainedBytes,nodes:p.nodes,leaves:p.leaves}});
  }
  const med=a=>a.sort((a,b)=>a-b)[a.length>>1];
  const summary={inputUnits:source.length,originalRuns,cacheRuns,nativeRuns,js:{preparationMs:med(samples.map(s=>s.js.preparationMs)),originalPerRunMs:med(samples.map(s=>s.js.originalMs/originalRuns)),cachedPerRunMs:med(samples.map(s=>s.js.cacheMs/cacheRuns)),invalidateAndRebuildMs:med(samples.map(s=>s.js.invalidateAndRebuildMs)),leafStringBytes:samples[0].js.leafStringBytes},native:{preparationCoreMs:med(samples.map(s=>s.native.preparationCoreMs)),preparationPipeMs:med(samples.map(s=>s.native.preparationPipeMs)),warmCorePerRunMs:med(samples.map(s=>s.native.coreMs/nativeRuns)),warmPipePerRunMs:med(samples.map(s=>s.native.pipeMs/nativeRuns)),invalidationCoreMs:med(samples.map(s=>s.native.invalidationCoreMs)),invalidationPipeMs:med(samples.map(s=>s.native.invalidationPipeMs)),retainedBytes:samples[0].native.retainedBytes,nodes:samples[0].native.nodes,leaves:samples[0].native.leaves}};
  await fs.writeFile(out,JSON.stringify({summary,samples},null,2));console.log(JSON.stringify(summary,null,2));
 } else {
  const cases=[...([10,100,1000].map(n=>({id:'shared-'+n,source:'{{settempvar::x::{{random::A::B}}}}'+Array(n).fill('{{tempvar::x}}').join('')}))),...([1,4,8,9].map(n=>({id:'independent-'+n,source:Array(n).fill('{{random::A::B}}').join('|')})))];
  const results=[];const med=a=>a.sort((a,b)=>a-b)[a.length>>1];
  for(const item of cases){const samples=[];let preparation;
   for(let i=0;i<14;i++){
    const js=await oracle(item.source,null);const start=performance.now();const p=await register('timing',item.source);const preparationPipeMs=performance.now()-start;preparation=p;
    let warmCoreMs,warmPipeMs;if(!p.fallback){const begin=performance.now();const r=await native.send({op:'relax-eval',id:'timing'});warmPipeMs=performance.now()-begin;warmCoreMs=r.coreMs;assert('Timing sample abort',!r.abort);const value=text(r.output);assert('Timing output domain',item.id.startsWith('shared')?/^(A+|B+)$/.test(value):/^[AB](\|[AB])*$/.test(value))}
    if(i>=3)samples.push({jsCoreMs:js.ms,preparationCoreMs:p.coreMs,preparationPipeMs,warmCoreMs,warmPipeMs});
   }
   results.push({id:item.id,inputUnits:item.source.length,preparation,samples,medians:Object.fromEntries(['jsCoreMs','preparationCoreMs','preparationPipeMs','warmCoreMs','warmPipeMs'].map(k=>[k,samples[0][k]===undefined?null:med(samples.map(r=>r[k]))]))});
  }
  await fs.writeFile(out,JSON.stringify({results},null,2));console.log(JSON.stringify(results.map(({samples,...r})=>r),null,2));
 }
}finally{try{native?.close()}finally{await browser.close()}}
