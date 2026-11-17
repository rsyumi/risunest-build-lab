import {chromium} from '@playwright/test';
import fs from 'node:fs/promises';
import {nativeClient} from './native-client.mjs';
import {REALM_BLOCKED_URL_PATTERNS} from '../../scripts/realmBlocklist.mjs';
export const toUnits=s=>Array.from({length:s.length},(_,i)=>s.charCodeAt(i));
export const fromUnits=a=>a===undefined?undefined:typeof a==="string"?a:Array.from({length:Math.ceil(a.length/8192)},(_,i)=>String.fromCharCode(...a.slice(i*8192,(i+1)*8192))).join('');
const [url,binary,out,mode='verify']=process.argv.slice(2);
if(!['127.0.0.1','localhost'].includes(new URL(url).hostname))throw Error('Loopback required');
const variables={enabled:'1',score:'7',next:'{{getvar::score}}',name:'equal',sep:'::',a:'a::a',list:'["x",1.0,null]',off:'0',surrogate:'\ud800',expr:'#if 1',close:'}}',open:'{{'};
const globals={toggle_demo:'1'};
const wire={variables:Object.entries(variables).map(([k,v])=>[toUnits(k),toUnits(v)]),globals:Object.entries(globals).map(([k,v])=>[toUnits(k),toUnits(v)]),index:7,user:toUnits('Synthetic'),seed:12345};
const browser=await chromium.launch({headless:true});
let native;
try{
native=nativeClient(binary);
const page=await browser.newPage();
await page.route('**/*',r=>['127.0.0.1','localhost'].includes(new URL(r.request().url()).hostname)?r.continue():r.abort());
const cdp=await page.context().newCDPSession(page);await cdp.send('Network.enable');await cdp.send('Network.setBlockedURLs',{urls:REALM_BLOCKED_URL_PATTERNS});
await page.goto(url);await page.waitForFunction(()=>window.__streamingSmoke?.marker==='synthetic-v1');
await page.evaluate(async({variables,globals})=>{
 const {risuChatParser}=await import('/src/ts/parser/parser.svelte.ts');
 window.__rustCbsParse=(text,repeat=1,patch={},initialSeed=12345)=>{
  const old=Math.random;let output,draws,finalState;const start=performance.now();
  try{for(let r=0;r<repeat;r++){let seed=initialSeed;const values={...variables,...patch};draws=0;Math.random=()=>{draws++;return((seed=(Math.imul(seed,1664525)+1013904223)>>>0)/4294967296)};
   output=risuChatParser(text,{chatID:7,userName:'Synthetic',runVar:true,getChatVar:k=>values[k]??'null',setChatVar:(k,v)=>{values[k]=v},globalChatVariables:globals,functions:new Map([['syntheticCall',{data:'{{random::a::b}}',arg:[]}]])}); finalState=seed;
  }}finally{Math.random=old}
  return{output,draws,rngState:finalState,ms:performance.now()-start};
 };
},{variables,globals});
if(mode==='verify'){
 const examples=[
  '{{arrayelement::[1.0]::0}}','{{join::[1.0,2.0]::,}}','{{? 0.0000001}}','{{? NuLl+2}}','{{split::😀::}}','{{join::[["a","b"],{"x":1}]::;}}',
  '{{length}}','{{less::inf::0}}','{{#when::keep::1}}\n x \n{{/when}}','{{arrayelement::[{"2":1,"1":2,"a":3,"a":4}]::0}}',
  '{{arrayelement::[1e400]::0}}','{{arrayelement::[1e-7]::0}}','{{arrayelement::[1000000000000000100]::0}}','{{arrayelement::[1,2,3]::NaN}}',
  '{{arrayelement::[1,2,3]::Infinity}}','{{arrayelement::[1,2,3]::-Infinity}}','{{arrayelement::[1,2,3]::-1.9}}',
  '{{getvar::score}}','{{getglobalvar::toggle_demo}}','{{CHAT_INDEX}}','{{length::😀한글}}','{{equal::7::7}}',
  '{{greater_equal:: 0x10 ::2}}','{{less::abc::7}}','{{not::true}}','{{and::1::true}}','{{contains::alphabet::pha}}',
  '{{startswith::alphabet::alpha}}','{{endswith::alphabet::bet}}','{{split::a,b,c::,}}','{{arrayelement::{{split::a,b,c::,}}::-1}}',
  '{{join::["a",null,7]::;}}','{{makearray::a::b::c}}','{{arraylength::a§b§c}}','{{trim::　 hello \ufeff}}',
  '{{random::a::b::c}}','{{random::a,b,c}}','{{random::["a",4,null]}}','{{random}}',
  '{{#if 0}}{{random::a::b}}{{/if}}','{{#if 1}}\n a\n b\n{{/if}}','{{#if_pure 1}}  a\n b  {{/if}}',
  '{{#when 0}}{{settempvar::x::1}}a{{:else}}b{{/when}}{{gettempvar::x}}','{{#when::var::enabled}}\n a \n{{:else}}\n b \n{{/when}}',
  '{{#when::7::>=::2}}yes{{/when}}','{{settempvar::x::{{getvar::score}}}}{{gettempvar::x}}','{{getvar::next}}',
  '{{{{getvar::name}}::a::a}}','{{equal{{getvar::sep}}a{{getvar::sep}}a}}','{{equal::{{getvar::a}}}}',
  '{{{{getvar::expr}}}}x{{/if}}','{{makearray::{{getvar::a}}}}','{{split::\ud800\udc00\ud800x\udc00::}}',
  '{{arrayelement::["\\ud800"]::0}}','{{join::["\\ud800","\\udc00"]::}}','{{length::{{getvar::surrogate}}}}',
  '{{settempvar::x::1}}{{settempvar::x}}{{tempvar::x}}','{{tempvar}}','{{settempvar}}{{tempvar::undefined}}',
  '{{equal}}','{{equal::x}}','{{split}}','{{split::}}','{{split::abc}}','{{join::["a","b"]}}',
  '{{arrayelement}}','{{arrayelement::a§b}}','{{contains::undefined}}','{{not}}','{{calc}}','{{getvar}}',
  '{{settempvar::__force_return__::1}}x','{{tempvar::__proto__}}','{{unknown}}','{{#each a§b as x}}{{slot::x}}{{/each}}',
  '{{#when::legacy::1}} x {{/when}}','{{setvar::x::1}}','{{call::custom}}','{{? $external+1}}',
  '{{equal:a:a}}','{{split:a,b:,}}','{{equal::a::a:}}','{{#if 1}}x{{/unrelated}}',
  '{{#if 0}}{{#when 1}}{{random::a::b}}{{/when}}{{/if}}{{random::a::b}}',
 ];
 for(const op of ['length','trim','contains','join','arraylength','arrayelement','random','makearray','settempvar','gettempvar'])for(const tail of ['','::','::x','::x::','::x::::y'])examples.push('{{'+op+tail+'}}');
 for(const expr of ['1+2*3','(2+3)*4','2^3^2','7>=2','0.0+2','-2*3','2*-3','!0','1&&7','0||7','4!=3','1/0','0/0','7%3','-0','(7-2)*(3+1)','null+2','NuLl+2','0.0000001','1000000000000000100'])examples.push('{{? '+expr+'}}','{{calc::'+expr+'}}');
 for(const n of [1,2,3,127,128,129,255,511,512,513,1024])examples.push('{{length::'.repeat(n)+'x'+'}}'.repeat(n));
 for(const s of ['{{','}}','{{length::x','{{#if 1}}x','{#if 1\nx#}','<user>','{{unknown::{{length::x}}}}','{{#when 0}}a{{:else}}{{settempvar::x::1}}b{{/when}}{{gettempvar::x}}'])examples.push(s);
 for(const value of ['0','-0','1.0','0.0000001','1e-7','1e21','1e400','1e-400','9007199254740993','1000000000000000100','null','true','false','"\\ud800"','"\\udc00"','["a",null,["b","c"]]','{"3":1,"01":2,"2":3,"b":4}']) {
  examples.push('{{arrayelement::['+value+']::0}}','{{join::['+value+']::;}}');
 }
 for(const value of ['',' ','inf','Inf','infinity','Infinity','-Infinity','+Infinity','nan','NaN','0x10','0x','+0x10','0b10','0o10','01','.0','1e+2','1e-2','0x20000000000000','0xFFFFFFFFFFFFFFFFFFFFFFFF']) {
  for(const op of ['less','greater','equal','arrayelement'])examples.push('{{'+op+'::'+(op==='arrayelement'?'["a","b"]':value)+'::'+(op==='arrayelement'?value:'0')+'}}');
 }
 for(const value of ['😀','\ud800','\udc00','😀\ud800','a::b','a:b','\u0085x\ufeff','{{','}}','#}']) {
  examples.push('{{split::'+value+'::}}','{{length::'+value+'}}','{{makearray::'+value+'}}','{{trim::'+value+'}}');
 }
 for(const key of ['__proto__','constructor','toString','valueOf','hasOwnProperty','__defineGetter__'])examples.push('{{tempvar::'+key+'}}','{{getvar::'+key+'}}','{{getglobalvar::'+key+'}}','{{#when::var::'+key+'}}x{{/when}}');
 for(const n of [63,64,65,128,512]){const nested="[".repeat(n)+"1"+"]".repeat(n);for(const op of ["arrayelement","join","arraylength"])examples.push("{{"+op+"::"+nested+"::0}}")}
 examples.push("{{#if 1}}x{{//}}","{{#when 1}}x{{//}}","{{#if 1}}x{{/{{random::if::when}}}}","{{#if 1}}x{{/}}");
 for(const value of ['7한','　7한','7😀','7\ud800','-7한','+Infinity한','1e2한','1e한'])for(const op of ['>','<'])examples.push('{{#when::'+value+'::'+op+'::2}}yes{{:else}}no{{/when}}');
 examples.push('{{#if 0}}{{call::syntheticCall}}{{/if}}{{random::a::b}}','{{#if 0}}{{{{getvar::name}}::syntheticCall}}{{/if}}','{{#if_pure 0}}{{upper::x}}{{/if}}','{{#func f}}{{setvar::x::1}}{{random}}{{/}}{{#if 0}}{{call::f}}{{/}}{{getvar::x}}','{{#if 0}}{{ca{{getvar::name}}ll::syntheticCall}}{{/if}}');
 let seed=739;const rand=()=>((seed=(Math.imul(seed,1664525)+1013904223)>>>0)/4294967296);
 const generated=Array.from({length:2500},()=>Array.from({length:1+Math.floor(rand()*8)},()=>examples[Math.floor(rand()*Math.min(examples.length,180))]).join(' | '));
 const texts=[...examples,...generated];const expected=await page.evaluate(texts=>texts.map(text=>{try{return window.__rustCbsParse(text)}catch(e){return{error:String(e)}}}),texts);
 const rows=[];let equal=0,fallback=0,mismatch=0,foldedEqual=0,foldedFallback=0,foldedMismatch=0;const fallbackReasons={};
 for(let i=0;i<texts.length;i++){
   const r=await native.send({op:'evaluate',text:texts[i].isWellFormed()?texts[i]:toUnits(texts[i]),context:wire});
   const folded=await native.send({op:"evaluate",text:texts[i].isWellFormed()?texts[i]:toUnits(texts[i]),context:wire,fold:true});const actual=fromUnits(r.output);const e=expected[i];if(folded.fallback)foldedFallback++;else if(fromUnits(folded.output)===e.output&&folded.draws===e.draws&&!e.error)foldedEqual++;else{foldedMismatch++;rows.push({i,text:texts[i],expected:e,folded})}const ok=actual===e.output&&r.draws===e.draws&&!e.error;
   if(r.fallback){fallback++;fallbackReasons[r.fallback]=(fallbackReasons[r.fallback]??0)+1}else if(ok)equal++;else mismatch++;
   if(i<examples.length||!ok&&!r.fallback)rows.push({i,text:texts[i],expected:e,actual,draws:r.draws,fallback:r.fallback,ok});
 }
 if(mismatch||foldedMismatch){await fs.writeFile(out,JSON.stringify({total:texts.length,equal,fallback,mismatch,foldedEqual,foldedFallback,foldedMismatch,rows:rows.filter(r=>!r.ok&&!r.fallback)},null,2));throw Error("Differential mismatch")}
 const runtimeRows=[];
 for(const value of ['[1.0]','[1e-7]','["\\ud800"]','[["a","b"]]','[null]','a::b','["a::b"]','[1e400]','['.repeat(66)+'1'+']'.repeat(66)]){
  const text='{{arrayelement::{{getvar::payload}}::0}}';const expected=await page.evaluate(({text,value})=>window.__rustCbsParse(text,1,{payload:value}),{text,value});
  const response=await native.send({op:'evaluate',text,context:{...wire,variables:[...wire.variables,[toUnits('payload'),toUnits(value)]]}});
  const ok=response.fallback||fromUnits(response.output)===expected.output&&response.draws===expected.draws;runtimeRows.push({kind:'payload',value,ok,fallback:response.fallback});if(!ok)throw Error('Runtime payload mismatch');
 }
 for(const value of ['length','trim','split','random','equal','unknown','#if 1','getvar','settempvar']){
  const text='{{{{getvar::op}}::x}}';const expected=await page.evaluate(({text,value})=>window.__rustCbsParse(text,1,{op:value}),{text,value});
  const response=await native.send({op:'evaluate',text,context:{...wire,variables:[...wire.variables,[toUnits('op'),toUnits(value)]]}});
  const ok=response.fallback||fromUnits(response.output)===expected.output&&response.draws===expected.draws;runtimeRows.push({kind:'operation',value,ok,fallback:response.fallback});if(!ok)throw Error('Runtime operation mismatch');
 }
 const checkpoints=[
  {id:'cross-boundary',input:'{{getvar::to',rules:[['to','score}}']]},
  {id:'generated-next-rule',input:'x',rules:[['x','{{getvar::next}}'],['missing','']]},
  {id:'capture-introduced',input:'[{{getvar::score}}]',rules:[['\\[(.*?)\\]','$1']]},
  {id:'temporary-across-matches',input:'x x',rules:[['x','{{gettempvar::x}}{{settempvar::x::1}}']]},
  {id:'temporary-reset-between-rules',input:'x y',rules:[['x','{{settempvar::x::1}}'],['y','{{gettempvar::x}}']]},
  {id:'eager-when-random',input:'x y',rules:[['x','{{#when 0}}{{random::a::b}}{{:else}}b{{/when}}{{random::a::b}}'],['y','{{random::a::b}}']]},
  {id:'missing-rule-random-reparse',input:'{{getvar::next}}x',rules:[['missing',''],['x','{{random::a::b}}']]},
  {id:'pure-generated-next-rule',input:'x',rules:[['x','{{arrayelement::["\\u007b\\u007blength\\u003a\\u003aabc\\u007d\\u007d"]::0}}'],['missing','']]},
  {id:'dynamic-delimiter',input:'x',rules:[['x','{{equal{{getvar::sep}}a{{getvar::sep}}a}}']]},
 ];
 for(const item of checkpoints) {
  let actual=item.input,actualFolded=item.input,expected=item.input,nativeSeed=12345,foldedSeed=12345,jsSeed=12345,nativeDraws=0,foldedDraws=0,jsDraws=0;
  for(const[pattern,replacement]of item.rules) {
   actual=actual.replace(new RegExp(pattern,'g'),replacement);actualFolded=actualFolded.replace(new RegExp(pattern,'g'),replacement);expected=expected.replace(new RegExp(pattern,'g'),replacement);
   const e=await page.evaluate(({text,seed})=>window.__rustCbsParse(text,1,{},seed),{text:expected,seed:jsSeed});
   const n=await native.send({op:'evaluate',text:actual.isWellFormed()?actual:toUnits(actual),context:{...wire,seed:nativeSeed}});
   if(n.fallback)throw Error('Checkpoint unexpectedly fell back '+item.id+'/'+n.fallback);
   const f=await native.send({op:'evaluate',text:actualFolded.isWellFormed()?actualFolded:toUnits(actualFolded),context:{...wire,seed:foldedSeed},fold:true});if(f.fallback)throw Error('Folded checkpoint fallback '+item.id);actualFolded=fromUnits(f.output);foldedSeed=f.rngState;foldedDraws+=f.draws;
   expected=e.output;jsSeed=e.rngState;jsDraws+=e.draws;actual=fromUnits(n.output);nativeSeed=n.rngState;nativeDraws+=n.draws;
  }
  item.ok=expected===actual&&jsDraws===nativeDraws&&jsSeed===nativeSeed&&expected===actualFolded&&jsDraws===foldedDraws&&jsSeed===foldedSeed;item.draws=jsDraws;
  if(!item.ok)throw Error('Checkpoint mismatch '+item.id);
 }
 const firstNine=rows.slice(0,9);if(firstNine.some(r=>!r.ok))throw Error('Known regression not admitted and equal');
 const lifecycle=[];
 const assert=(name,ok)=>{lifecycle.push({name,ok});if(!ok)throw Error('Lifecycle '+name)};
 let r=await native.send({op:'register',id:'state',text:toUnits('{{getvar::score}}/{{chatindex}}/{{tempvar::x}}{{settempvar::x::1}}')});assert('register',r.programs===1);
 for(let i=0;i<20;i++){r=await native.send({op:'context',context:{...wire,index:i,variables:[[toUnits('score'),toUnits(String(i))]]}});const v=await native.send({op:'eval',id:'state',revision:r.revision});assert('context-'+i,fromUnits(v.output)===i+'/'+i+'/')}
 r=await native.send({op:'eval',id:'state',revision:0});assert('stale-context',r.fallback==='stale-context');
 r=await native.send({op:'eval',id:'state',cancelled:true});assert('cancel-before',r.fallback==='cancelled');
 r=await native.send({op:'eval',id:'state',budget:1});assert('budget',r.fallback==='budget');
 r=await native.send({op:'register',id:'failed-state',text:toUnits('{{settempvar::x::1}}{{random::a::b}}{{upper::x}}')});
 r=await native.send({op:'eval',id:'failed-state',context:wire});assert('failed-eval-fallback',r.fallback==='unsupported-operator');
 r=await native.send({op:'evaluate',text:'{{tempvar::x}}{{random::a::b}}',context:wire});
 const isolated=await page.evaluate(()=>window.__rustCbsParse('{{tempvar::x}}{{random::a::b}}'));assert('failed-state-discard',fromUnits(r.output)===isolated.output&&r.draws===isolated.draws);
 for(let i=0;i<62;i++){r=await native.send({op:'register',id:'bounded-'+i,text:'x'});assert('bounded-register-'+i,!r.fallback)}
 r=await native.send({op:'register',id:'overflow',text:'x'});assert('entry-limit',r.fallback==='resident-limit');
 r=await native.send({op:'release'});assert('release',r.programs===0&&r.retainedBytes===0&&r.variables===0);
 r=await native.send({op:'eval',id:'state'});assert('released-program',r.fallback==='missing-program');
 const report={total:texts.length,equal,fallback,mismatch,foldedEqual,foldedFallback,foldedMismatch,fallbackReasons,runtimeRows,checkpoints,lifecycle,rows};await fs.writeFile(out,JSON.stringify(report,null,2));console.log(JSON.stringify({...report,lifecycle:lifecycle.length,checkpoints:checkpoints.map(({id,ok,draws})=>({id,ok,draws})),rows:rows.filter(r=>!r.ok&&!r.fallback).slice(0,20)},null,2));if(mismatch||foldedMismatch)throw Error("Differential mismatch");
}else{
 const cases=[10,50,200,1000].flatMap(count=>[
  {id:'dynamic-'+count,text:Array.from({length:count},()=>'<panel>'+'{{getvar::score}}:{{equal::{{getvar::score}}::7}}/{{arrayelement::{{split::a,b,c::,}}::1}}/{{#when::var::enabled}}{{random::a::b}}{{:else}}x{{/when}}'+'</panel>').join('')},
  {id:'utf16-'+count,text:Array.from({length:count},()=> '한글😀 {{length::{{getvar::surrogate}}}} {{arrayelement::[1.0,1e-7]::1}} {{split::😀::}}').join('\n')}
 ]);
 const results=[];
 for(const item of cases){
  const text=item.text.replaceAll('<panel>','[panel]').replaceAll('</panel>','[/panel]');const expected=await page.evaluate(text=>window.__rustCbsParse(text),text);
  await native.send({op:'release'});const prepStart=performance.now();let r=await native.send({op:'register',id:item.id,text:text.isWellFormed()?text:toUnits(text)});const preparationMs=performance.now()-prepStart;if(r.fallback)throw Error(r.fallback);const rawBytes=r.retainedBytes;const foldStart=performance.now();const f=await native.send({op:'register',id:item.id+'-folded',text:text.isWellFormed()?text:toUnits(text),fold:true});const foldPreparationMs=performance.now()-foldStart;if(f.fallback)throw Error(f.fallback);
  await native.send({op:'context',context:wire});const rows=[];
  for(let n=0;n<14;n++)for(const variant of ['product','native-cold','native-resident','native-folded']){
    const start=performance.now();const r=variant==='product'?await page.evaluate(text=>window.__rustCbsParse(text),text):await native.send({op:'eval',...(variant==='native-cold'?{text:text.isWellFormed()?text:toUnits(text)}:{id:item.id+(variant==='native-folded'?'-folded':'')})});const ms=performance.now()-start;
    const actual=variant==='product'?r.output:fromUnits(r.output);if(actual!==expected.output||r.draws!==expected.draws)throw Error('Mismatch '+item.id+'/'+variant);
    if(n>=3)rows.push({variant,ms,coreMs:variant==='product'?r.ms:r.coreMs});
  }results.push({id:item.id,inputUnits:text.length,outputUnits:expected.output.length,preparationMs,foldPreparationMs,folded:f.folded,rawBytes,foldedBytes:f.retainedBytes-rawBytes,rows});
 }
 const fallbackText=Array.from({length:200},()=>'{{getvar::score}}{{equal::{{getvar::score}}::7}}{{random::a::b}}').join('')+'{{upper::x}}';
 await native.send({op:'release'});await native.send({op:'register',id:'fallback',text:fallbackText});await native.send({op:'context',context:wire});
 const fallbackRows=[];
 for(let i=0;i<14;i++){
  const baseline=await page.evaluate(t=>window.__rustCbsParse(t),fallbackText);
  const start=performance.now();const attempt=await native.send({op:'eval',id:'fallback'});const pipeMs=performance.now()-start;
  if(attempt.fallback!=='unsupported-operator')throw Error('Expected runtime admission failure');
  const retried=await page.evaluate(t=>window.__rustCbsParse(t),fallbackText);
  if(retried.output!==baseline.output||retried.draws!==baseline.draws)throw Error('Fallback changed output or order');
  if(i>=3)fallbackRows.push({productMs:baseline.ms,nativeAttemptCoreMs:attempt.coreMs,nativeAttemptPipeMs:pipeMs,fallbackProductMs:retried.ms,combinedCoreMs:attempt.coreMs+retried.ms});
 }
 await fs.writeFile(out,JSON.stringify({results,fallbackRows},null,2));const med=a=>a.sort((a,b)=>a-b)[a.length>>1];console.log(JSON.stringify(results.map(r=>({...r,rows:undefined,timing:Object.fromEntries(['product','native-cold','native-resident','native-folded'].map(v=>[v,{coreMs:med(r.rows.filter(x=>x.variant===v).map(x=>x.coreMs)),roundTripMs:med(r.rows.filter(x=>x.variant===v).map(x=>x.ms))}]))})),null,2));
}
}finally{try{native?.close()}finally{await browser.close()}}
