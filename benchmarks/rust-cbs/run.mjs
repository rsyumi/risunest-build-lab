import {chromium} from '@playwright/test';
import fs from 'node:fs/promises';
import strictAssert from 'node:assert/strict';
import {nativeClient} from './native-client.mjs';
import {REALM_BLOCKED_URL_PATTERNS} from '../../scripts/realmBlocklist.mjs';
import crypto from 'node:crypto';
import {fixtureSets,caseText,sourceDigests} from './fixtures/cases.mjs';
import {hostSnapshots,hostCases,hostClock,hostViewport,hostProfiles,hostZoneRange,captureLocalTime,hostOracleSources} from './fixtures/host-cases.mjs';
export const toUnits=s=>Array.from({length:s.length},(_,i)=>s.charCodeAt(i));
export const fromUnits=a=>a===undefined?undefined:typeof a==="string"?a:Array.from({length:Math.ceil(a.length/8192)},(_,i)=>String.fromCharCode(...a.slice(i*8192,(i+1)*8192))).join('');
const [url,binary,out,mode='verify']=process.argv.slice(2);
if(!['127.0.0.1','localhost'].includes(new URL(url).hostname))throw Error('Loopback required');
const variables={enabled:'1',score:'7',next:'{{getvar::score}}',name:'equal',sep:'::',a:'a::a',list:'["x",1.0,null]',off:'0',surrogate:'\ud800',expr:'#if 1',close:'}}',open:'{{',replacementNext:'{{user}}',replacementName:'replace',replacementTail:'::x::x::y',arrayName:'arraysplice',arrayTail:'::[1,2]::1::1::x',arrayInput:'[1,true,null,[2],{"x":3}]',arrayNext:'{{user}}',hashName:'hash',hashTail:'::Synthetic',hashInput:'Synthetic',hashNext:'{{hash::Synthetic}}',scalarNext:'{{fixnum::1.005::2}}',aggregateNext:'{{sum::[true,2]}}'};
const globals={toggle_demo:'1'};
const wire={variables:Object.entries(variables).map(([k,v])=>[toUnits(k),toUnits(v)]),globals:Object.entries(globals).map(([k,v])=>[toUnits(k),toUnits(v)]),index:7,user:toUnits('Synthetic'),seed:12345,functions:[[toUnits('syntheticCall'),toUnits('{{random::a::b}}')]]};
const utf16Sha256=units=>{const b=Buffer.alloc(units.length*2);units.forEach((u,i)=>b.writeUInt16LE(u,i*2));return crypto.createHash('sha256').update(b).digest('hex')};
const fixtureUrl=set=>new URL('./fixtures/'+set+'.json',import.meta.url);
const hostFixtureUrl=new URL('./fixtures/host.json',import.meta.url);
// A fixture expectation: the text, or a hash of it for table cases, with the RNG draws and state.
const expectation=(item,r)=>{
 if(r.error)return{error:r.error};
 const one=x=>item.hash?{outputSha256:utf16Sha256(x.units),outputUnits:x.units.length,draws:x.draws,rngState:x.rngState}:{output:fromUnits(x.units),draws:x.draws,rngState:x.rngState};
 return r.steps?{steps:r.steps.map(one)}:one(r);
};
const entries=object=>Object.entries(object).map(([k,v])=>[toUnits(k),toUnits(v)]);
const browser=await chromium.launch({headless:true});
let native;
try{
if(mode!=='generate')native=nativeClient(binary);
// A page of the app with the oracle helpers installed; the host pages differ in time zone and default locale.
const openPage=async options=>{
const page=await browser.newPage({viewport:hostViewport,...options});
await page.route('**/*',r=>['127.0.0.1','localhost'].includes(new URL(r.request().url()).hostname)?r.continue():r.abort());
const cdp=await page.context().newCDPSession(page);await cdp.send('Network.enable');await cdp.send('Network.setBlockedURLs',{urls:REALM_BLOCKED_URL_PATTERNS});
await page.goto(url);await page.waitForFunction(()=>window.__streamingSmoke?.marker==='synthetic-v1');
await page.evaluate(async({variables,globals})=>{
 const {risuChatParser}=await import('/src/ts/parser/parser.svelte.ts');
 const {pickHashRand,sfc32}=await import('/src/ts/util.ts');
 const {CbsHistoryAccessError,createCbsHistoryAccess,CbsHistoryDependencies,cbsHistorySearchKey}=await import('/src/ts/cbsHistoryAccess.ts');
 const {recordChatVariableReads}=await import('/src/ts/process/scriptVariableReads.ts');
 const {DBState,selectedCharID,CurrentTriggerIdStore}=await import('/src/ts/stores.svelte.ts');
 const {normalizeDatabaseDefaults}=await import('/src/ts/storage/database.svelte.ts');
 const chatVar=await import('/src/ts/parser/chatVar.svelte.ts');
 const {attachHistoryWindow,getHistoryWindowStart}=await import('/src/ts/process/historyWindowIndex.ts');
 const {getModules,getModuleLorebooks}=await import('/src/ts/process/modules.ts');
 const {getModelInfo}=await import('/src/ts/model/modellist.ts');
 const {appVer}=await import('/src/ts/appVersion.ts');
 const {isTauri}=await import('/src/ts/platform.ts');
 // Date with now() and the argument-free constructor fixed at `ms`: the clock boundary of the oracle.
 const RealDate=Date;
 const fixedDate=ms=>class extends RealDate{constructor(...a){if(a.length)super(...a);else super(ms)}static now(){return ms}};
 const current=store=>{let value;store.subscribe(v=>{value=v})();return value};
 const encode=s=>Array.from({length:s.length},(_,i)=>s.charCodeAt(i));
 const decode=u=>typeof u==='string'?u:Array.from({length:Math.ceil(u.length/8192)},(_,i)=>String.fromCharCode(...u.slice(i*8192,(i+1)*8192))).join('');
 const tagged=v=>v===undefined?{kind:'undefined'}:{kind:'text',units:encode(v)};
 window.__rustCbsEffects=({text,input})=>{
  const values=Object.create(null),faults=new Map(),written=new Set(),overlay=new Map();
  for(const [key,cell]of input.variables??[]){const k=decode(key);if(cell.kind==='text')values[k]=decode(cell.units);else if(cell.kind==='undefined')values[k]=undefined;else faults.set(k,cell)}
  const effects=[],reads=[],keys=[],seen=new Set();let sequence=0,untracked=false;
  const read=key=>{reads.push({sequence:sequence++,key:tagged(key)});if(!seen.has(key)){seen.add(key);keys.push(tagged(key))}};
  const context=input.context??{};let seed=context.seed??0,draws=0,result;const old=Math.random;
  const args={runVar:input.runVar??false,rmVar:input.rmVar??false,chatID:context.index??0,userName:decode(context.user??[]),globalChatVariables:Object.fromEntries((context.globals??[]).map(([k,v])=>[decode(k),decode(v)]))};
  let history,access;
  if(input.history){
   const capture=input.history,indices=new Set();
   history={provenance:structuredClone(capture.provenance),count:false,rows:[]};
   access=createCbsHistoryAccess({length:capture.length,rows:new Map(capture.rows.map(row=>[row.index,{data:decode(row.data),role:decode(row.role)}]))});
   args.historyAccess={...access,get length(){history.count=true;return access.length},row(index){
    const absolute=index===0?0:index;
    history.count=true;if(Number.isInteger(index)&&index>=0&&index<access.length&&!indices.has(absolute)){indices.add(absolute);history.rows.push(absolute)}
    return access.row(index);
   }};
   args.db={characters:capture.selectedCharacter?[{chaId:'synthetic-history',chatPage:0,chats:[{message:[]}]}]:[]};
   args.selectedCharID=0;
   args.cbsConditions={firstmsg:capture.firstmsg??false,chatRole:capture.conditionChatRole==null?undefined:decode(capture.conditionChatRole)};
   args.role=capture.role==null?undefined:decode(capture.role);
  }
  if(input.mode==='snapshotOnly')args.chatVariables=values;
  else{
   args.getChatVar=key=>{read(key);const k=String(key),fault=!written.has(k)&&faults.get(k);if(fault){if(fault.kind==='historyError')throw new CbsHistoryAccessError(decode(fault.access));throw new Error(decode(fault.message))}return values[key]??'null'};
   args.setChatVar=(key,value)=>{effects.push({sequence:sequence++,key:tagged(key),value:tagged(value)});values[key]=value;written.add(String(key));overlay.set(String(key),value)};
  }
  const recorder={note:key=>{if(input.mode==='snapshotOnly')read(key)},markUntracked:()=>{untracked=true}};
  try{
   Math.random=()=>{draws++;return((seed=(Math.imul(seed,1664525)+1013904223)>>>0)/4294967296)};
   result={output:encode(recordChatVariableReads(recorder,()=>risuChatParser(decode(text),args)))};
  }catch(error){result={error:{name:error.name,message:encode(error.message),...(error instanceof CbsHistoryAccessError?{access:encode(error.access)}:{})}}}
  finally{Math.random=old}
  if(access){
   window.__rustCbsHistoryCompletion=null;
   try{access.assertComplete()}catch(error){window.__rustCbsHistoryCompletion={name:error.name,access:encode(error.access)}}
  }
  return{status:'evaluated',result,effects,reads,dependencies:{keys,untracked},overlay:Array.from(overlay,([k,v])=>[encode(k),tagged(v)]),draws,rngState:seed,
   ...(history?{history}:{}),
   finalVariables:Object.keys(values).sort().map(k=>[encode(k),tagged(values[k])])};
 };
 window.__rustCbsHistoryInvalidation=({observation,changes})=>{
  const dependencies=new CbsHistoryDependencies();dependencies.count=observation.count;
  for(const index of observation.rows)dependencies.range(index,index+1);
  return changes.map(change=>dependencies.affectedBy([change]));
 };
 window.__rustCbsHashCore=(word,cid,seeds,count)=>{
  const random=sfc32(...seeds);
  return {value:pickHashRand(cid,word),sequence:Array.from({length:count},()=>random())};
 };
 window.__rustCbsFixture=(item,context)=>{
  const old=Math.random;
  const run=(text,seed)=>{
   const values={...context.variables,...(item.patch??{})};let draws=0;
   Math.random=()=>{draws++;return((seed=(Math.imul(seed,1664525)+1013904223)>>>0)/4294967296)};globalThis.Date=fixedDate(context.clock);
   const output=risuChatParser(text,{chatID:context.index,userName:context.user,runVar:true,getChatVar:k=>values[k]??'null',setChatVar:(k,v)=>{values[k]=v},globalChatVariables:{...context.globals},
    functions:new Map(Object.entries(context.functions).map(([k,v])=>[k,{data:v,arg:[]}])),visualize:item.visualize??false});
   return{units:encode(output),draws,rngState:seed};
  };
  try{
   const seed=item.seed??context.seed;
   if(!item.steps)return run(decode(item.units),seed);
   let text=decode(item.units),state=seed;const steps=[];
   for(const [search,replacement]of item.steps){text=text.split(search).join(replacement);const r=run(text,state);steps.push(r);text=decode(r.units);state=r.rngState}
   return{steps};
  }catch(error){return{error:String(error)}}finally{Math.random=old;globalThis.Date=RealDate}
 };
 window.__rustCbsParse=(text,repeat=1,patch={},initialSeed=12345)=>{
  const old=Math.random;let output,draws,finalState;const start=performance.now();
  try{for(let r=0;r<repeat;r++){let seed=initialSeed;const values={...variables,...patch};draws=0;Math.random=()=>{draws++;return((seed=(Math.imul(seed,1664525)+1013904223)>>>0)/4294967296)};
   output=risuChatParser(text,{chatID:7,userName:'Synthetic',runVar:true,getChatVar:k=>values[k]??'null',setChatVar:(k,v)=>{values[k]=v},globalChatVariables:globals,functions:new Map([['syntheticCall',{data:'{{random::a::b}}',arg:[]}]])}); finalState=seed;
  }}finally{Math.random=old}
  return{output,draws,rngState:finalState,ms:performance.now()-start};
 };
 // The database the snapshot describes as the selected state, with its history window on the chat the parser reads.
 const install=snapshot=>{
  DBState.db=normalizeDatabaseDefaults(structuredClone(snapshot.database));selectedCharID.set(snapshot.selected);CurrentTriggerIdStore.set(snapshot.triggerId);
  if(snapshot.windowStart!==undefined){
   const chat=()=>{const character=DBState.db.characters[snapshot.selected];return character.chats[character.chatPage]};
   attachHistoryWindow(chat(),snapshot.windowStart);
   if(getHistoryWindowStart(chat())!==snapshot.windowStart)throw Error('The history window is not on the chat the parser reads');
  }
 };
 const historyAccess=h=>createCbsHistoryAccess({length:h.length,rows:new Map(h.rows??[]),...(h.complete?{complete:h.complete}:{}),
  ...(h.searches?{searches:new Map(h.searches.map(([role,before,count,rows])=>[cbsHistorySearchKey(role,before,count),rows]))}:{})});
 // One host case: the snapshot is installed, each step is one risuChatParser call whose setChatVar calls are
 // recorded and applied, and Math.random replays the tape.
 window.__rustCbsHost=({snapshot,steps,tape,clock,consistantChar,tokenizeAccurate})=>{
  const old={db:DBState.db,selected:current(selectedCharID),trigger:current(CurrentTriggerIdStore),random:Math.random};
  let consumed=0;const results=[];
  try{
   install(snapshot);
   Math.random=()=>{if(consumed>=tape.length)throw Error('tape exhausted');return tape[consumed++]};
   globalThis.Date=fixedDate(clock);
   for(const step of steps){
    for(const [key,value]of step.apply??[])chatVar.setChatVar(key,value);
    const character=DBState.db.characters[current(selectedCharID)];
    const chat=character?.chats[character.chatPage];
    for(const edit of step.edits??[]){
     if(edit.kind==='setData'){const row=chat.message.at(edit.index);if(row)row.data=edit.data}
     else if(edit.kind==='cut')chat.message=chat.message.slice(edit.start,edit.end);
     else if(edit.kind==='append')chat.message.push({role:edit.role,data:edit.data,chatId:edit.chatId});
     else if(edit.kind==='note')chat.note=edit.data;
     else throw Error('Unknown synthetic view edit');
    }
    const effects=[];
    const scoped=step.scope?new Map(Object.entries(step.scope.variables??{})):null;
    const previousTrigger=current(CurrentTriggerIdStore);
    if(scoped)CurrentTriggerIdStore.set(step.scope.triggerId??null);
    try{
     const output=risuChatParser(step.text,{chatID:step.chatIndex??-1,runVar:step.runVar??false,rmVar:step.rmVar??false,visualize:step.visualize??false,role:step.role,
      cbsConditions:{firstmsg:step.firstmsg,chatRole:step.chatRole},consistantChar,tokenizeAccurate,
      projectedChatID:step.projectedChatID,historyOffset:step.historyOffset,currentTime:step.currentTime,triggerId:scoped?step.scope.triggerId:step.triggerId,
      historyAccess:step.history?historyAccess(structuredClone(step.history)):undefined,
      getChatVar:scoped?(key=>scoped.get(String(key))??'null'):undefined,
      setChatVar:(key,value)=>{if(scoped)scoped.set(String(key),value);else chatVar.setChatVar(key,value);effects.push([String(key),value===undefined?null:value])}});
     results.push({output,effects});
    }catch(error){results.push({error:error.name});break}
    finally{CurrentTriggerIdStore.set(previousTrigger)}
   }
  }finally{DBState.db=old.db;selectedCharID.set(old.selected);CurrentTriggerIdStore.set(old.trigger);Math.random=old.random;globalThis.Date=RealDate}
  return{steps:results,consumed};
 };
 // What the snapshot's database fields read through product helpers, as JSON: the owner's reference for filling them.
 window.__rustCbsCapture=snapshot=>{
  const old={db:DBState.db,selected:current(selectedCharID),trigger:current(CurrentTriggerIdStore)};
  try{
   install(snapshot);
   const db=DBState.db,character=db.characters[snapshot.selected];
   return JSON.parse(JSON.stringify({
    modules:getModules().map(module=>({namespace:module.namespace,assets:module.assets})),moduleLorebooks:getModuleLorebooks(),modelInfo:getModelInfo(db.aiModel),language:db.language,
    character:character?{globalLore:character.globalLore,localLore:character.chats[character.chatPage].localLore,emotionImages:character.emotionImages,
     additionalAssets:character.additionalAssets,prebuiltAssetCommand:character.prebuiltAssetCommand,prebuiltAssetExclude:character.prebuiltAssetExclude}:null,
   }));
  }finally{DBState.db=old.db;selectedCharID.set(old.selected);CurrentTriggerIdStore.set(old.trigger)}
 };
 window.__rustCbsPlatform=()=>({tauri:isTauri,appVersion:appVer,language:navigator.language});
},{variables,globals});
return page;
};
const page=await openPage({timezoneId:'UTC'});
const hostPages={};
for(const [name,{timeZone,locale}]of Object.entries(hostProfiles))hostPages[name]=await openPage({timezoneId:timeZone,locale});
const hostSnapshotsJson=JSON.parse(JSON.stringify(hostSnapshots));
// Each case runs on the page of its profile with Date fixed at its clock.
const hostOracle=async cases=>{
 const results=[];
 for(const c of cases)if(!hostPages[c.profile??'utc'])throw Error(`Unknown host profile ${c.profile}`);
 for(const [name,hostPage]of Object.entries(hostPages)){
  const selected=cases.map((c,index)=>({c,index})).filter(({c})=>(c.profile??'utc')===name);
  for(let i=0;i<selected.length;i+=32){
   const batch=selected.slice(i,i+32);
   const items=batch.map(({c})=>({snapshot:structuredClone(hostSnapshotsJson[c.snapshot]),steps:c.steps,tape:c.tape,clock:c.clock??hostClock,consistantChar:c.consistantChar,tokenizeAccurate:c.tokenizeAccurate}));
   const output=await hostPage.evaluate(items=>items.map(item=>window.__rustCbsHost(item)),items);
   batch.forEach(({index},k)=>{results[index]=output[k]});
  }
 }
 return results;
};
// Per profile, the platform values and the local time capture; per snapshot, the helper reads.
const hostEnvironments=async()=>{
 const environments={};
 for(const [name,hostPage]of Object.entries(hostPages))environments[name]={...await hostPage.evaluate(()=>window.__rustCbsPlatform()),localTime:await hostPage.evaluate(captureLocalTime,hostZoneRange)};
 return environments;
};
const hostCaptures=async()=>{
 const captures={};
 for(const name of Object.keys(hostSnapshotsJson))captures[name]=await hostPages.utc.evaluate(snapshot=>window.__rustCbsCapture(snapshot),structuredClone(hostSnapshotsJson[name]));
 return captures;
};
const oracleFixture=async(context,cases)=>{
 const results=[];let batch=[],size=0;
 const flush=async()=>{if(!batch.length)return;results.push(...await page.evaluate(({items,context})=>items.map(item=>window.__rustCbsFixture(item,context)),{items:batch,context}));batch=[];size=0};
 for(const item of cases){const text=caseText(item);batch.push({patch:item.patch,seed:item.seed,visualize:item.visualize,steps:item.steps,units:toUnits(text)});size+=text.length;if(size>262144||batch.length>=256)await flush()}
 await flush();
 return results.map((r,i)=>expectation(cases[i],r));
};
const nativeFixture=async(context,item,fold)=>{
 const send=async(text,seed)=>{
  const r=await native.send({op:'evaluate',text:toUnits(text),fold,visualize:item.visualize??false,
   context:{variables:entries({...context.variables,...(item.patch??{})}),globals:entries(context.globals),index:context.index,user:toUnits(context.user),seed,functions:entries(context.functions)}});
  return r.fallback?r:{units:typeof r.output==='string'?toUnits(r.output):r.output,draws:r.draws,rngState:r.rngState};
 };
 const seed=item.seed??context.seed;
 if(!item.steps){const r=await send(caseText(item),seed);return r.fallback?{fallback:r.fallback}:expectation(item,r)}
 let text=item.text,state=seed;const steps=[];
 for(const [search,replacement]of item.steps){text=text.split(search).join(replacement);const r=await send(text,state);if(r.fallback)return{fallback:r.fallback};steps.push(r);text=fromUnits(r.units);state=r.rngState}
 return expectation(item,{steps});
};
// Replays the committed fixtures against the live parser (drift) and the native engine, raw and folded.
const replayFixtures=async()=>{
 const fixtureReport={},fixtureFailures=[];
 for(const set of Object.keys(fixtureSets)){
  const fixture=JSON.parse(await fs.readFile(fixtureUrl(set),'utf8'));
  strictAssert.deepEqual(fixture.context,fixtureSets[set].context,set+' context');
  strictAssert.deepEqual(fixture.cases.map(({expected,...item})=>item),JSON.parse(JSON.stringify(fixtureSets[set].cases)),set+' cases changed; run the generate mode');
  const live=await oracleFixture(fixture.context,fixture.cases);
  const counts={cases:fixture.cases.length,drift:0,equal:0,refused:0,mismatch:0,foldedEqual:0,foldedRefused:0,foldedMismatch:0};
  for(const [i,item]of fixture.cases.entries()){
   if(!item.refusal){try{strictAssert.deepEqual(live[i],item.expected)}catch{counts.drift++;fixtureFailures.push({set,id:item.id,kind:'drift',expected:item.expected,live:live[i]})}}
   for(const fold of [false,true]){
    const actual=await nativeFixture(fixture.context,item,fold);const key=name=>fold?'folded'+name[0].toUpperCase()+name.slice(1):name;
    // A product exception has no native equivalent, so the native engine must hand the text back.
    if(item.expected.error&&!item.refusal){if(actual.fallback)counts[key('refused')]++;else{counts[key('mismatch')]++;fixtureFailures.push({set,id:item.id,fold,kind:'exception',expected:item.expected,actual})}}
    else if(item.refusal){if(actual.fallback===item.refusal)counts[key('refused')]++;else{counts[key('mismatch')]++;fixtureFailures.push({set,id:item.id,fold,kind:'refusal',refusal:item.refusal,actual})}}
    else{let equal=!actual.fallback;try{strictAssert.deepEqual(actual,item.expected)}catch{equal=false}
     if(equal)counts[key('equal')]++;else{counts[key('mismatch')]++;fixtureFailures.push({set,id:item.id,fold,kind:'mismatch',expected:item.expected,actual})}}
   }
  }
  fixtureReport[set]=counts;
 }
 // The host set has no native-binary lane; the crate test replays it through the TextProcessor adapter.
 const host=JSON.parse(await fs.readFile(hostFixtureUrl,'utf8'));
 strictAssert.deepEqual(host.snapshots,hostSnapshotsJson,'host snapshots changed; run the generate mode');
 strictAssert.deepEqual([host.profiles,host.zoneRange],JSON.parse(JSON.stringify([hostProfiles,hostZoneRange])),'host profiles changed; run the generate mode');
 strictAssert.deepEqual(host.cases.map(({expected,...item})=>item),JSON.parse(JSON.stringify(hostCases)),'host cases changed; run the generate mode');
 const hostLive=await hostOracle(host.cases);
 fixtureReport.host={cases:host.cases.length,drift:0};
 for(const [i,item]of host.cases.entries()){try{strictAssert.deepEqual(hostLive[i],item.expected)}catch{fixtureReport.host.drift++;fixtureFailures.push({set:'host',id:item.id,kind:'drift',expected:item.expected,live:hostLive[i]})}}
 for(const [id,recorded,live]of [['environments',host.environments,await hostEnvironments()],['captures',host.captures,await hostCaptures()]]){
  try{strictAssert.deepEqual(JSON.parse(JSON.stringify(live)),recorded)}catch{fixtureReport.host.drift++;fixtureFailures.push({set:'host',id,kind:'drift'})}
 }
 if(fixtureFailures.length){await fs.writeFile(out,JSON.stringify({fixtureReport,fixtureFailures},null,2));console.log(JSON.stringify({fixtureReport,fixtureFailures:fixtureFailures.slice(0,40)},null,2));throw Error('Fixture mismatch')}
 return fixtureReport;
};
if(mode==='generate'){
 const environment=await page.evaluate(()=>({locale:Intl.DateTimeFormat().resolvedOptions().locale,rootCase:'I'.toLocaleLowerCase()==='i'&&'i'.toLocaleUpperCase()==='I'&&'\u0130'.toLocaleLowerCase()==='i\u0307',
  timeZone:Intl.DateTimeFormat().resolvedOptions().timeZone,userAgent:navigator.userAgent,prototypes:Object.fromEntries([['object',Object.prototype],['array',Array.prototype],['string',String.prototype],['number',Number.prototype],['boolean',Boolean.prototype]].map(([k,v])=>[k,Object.getOwnPropertyNames(v).sort()]))}));
 if(!environment.rootCase)throw Error('The oracle needs a default locale with root case mapping');
 const summary={};
 for(const [set,{context,cases}]of Object.entries(fixtureSets)){
  const expected=await oracleFixture(context,cases);
  const header=JSON.stringify({schema:1,set,generator:'benchmarks/rust-cbs/run.mjs generate',sourceSha256:sourceDigests(),environment,context});
  await fs.writeFile(fixtureUrl(set),header.slice(0,-1)+',"cases":[\n'+cases.map((item,i)=>JSON.stringify({...item,expected:expected[i]})).join(',\n')+'\n]}\n');
  summary[set]={cases:cases.length,refusals:cases.filter(c=>c.refusal).length,hashed:cases.filter(c=>c.hash).length,errors:expected.filter(e=>e.error).length};
 }
 const hostExpected=await hostOracle(hostCases);
 const environments=await hostEnvironments(),captures=await hostCaptures();
 const hostHeader=JSON.stringify({schema:1,set:'host',generator:'benchmarks/rust-cbs/run.mjs generate',sourceSha256:sourceDigests(hostOracleSources),environment,clock:hostClock,viewport:hostViewport,
  profiles:hostProfiles,zoneRange:hostZoneRange,environments,captures,snapshots:hostSnapshotsJson});
 await fs.writeFile(hostFixtureUrl,hostHeader.slice(0,-1)+',"cases":[\n'+hostCases.map((item,i)=>JSON.stringify({...item,expected:hostExpected[i]})).join(',\n')+'\n]}\n');
 const hostSteps=hostExpected.flatMap(e=>e.steps);
 summary.host={cases:hostCases.length,steps:hostSteps.length,errors:hostSteps.filter(e=>e.error).length,refusals:hostCases.flatMap(c=>c.steps).filter(s=>s.refusal).length,
  effects:hostSteps.reduce((n,e)=>n+(e.effects?.length??0),0),consumed:hostExpected.reduce((n,e)=>n+e.consumed,0),
  timeFormats:Object.fromEntries(Object.entries(environments).map(([name,e])=>[name,[e.localTime.timeFormat!==null,e.localTime.dateFormat!==null,e.localTime.zone.transitions.length]]))};
 await fs.writeFile(out,JSON.stringify({environment,summary},null,2));console.log(JSON.stringify({locale:environment.locale,summary},null,2));
}else if(mode==='fixtures'){
 const fixtureReport=await replayFixtures();
 await fs.writeFile(out,JSON.stringify({fixtureReport},null,2));console.log(JSON.stringify({fixtureReport},null,2));
}else if(mode==='verify'){
 const effectTag=value=>value===undefined?{kind:'undefined'}:{kind:'text',units:toUnits(value)};
 const effectInput=(values={},options={})=>({mode:'mutableMap',runVar:true,variables:Object.entries(values).map(([key,value])=>[toUnits(key),effectTag(value)]),context:{globals:[[toUnits('toggle_demo'),toUnits('1')]],index:7,user:toUnits('Synthetic'),seed:12345},...options});
 const effectCases=[];
 const addEffects=(id,text,values={},options={})=>effectCases.push({id,text:toUnits(text),input:effectInput(values,options)});
 for(const op of ['setvar','addvar','setdefaultvar'])for(const runVar of [false,true])for(const rmVar of [false,true]){
  addEffects(`${op}-flags-${runVar}-${rmVar}`,'{{'+op+'::score::{{getvar::value}}}}',{value:'2',score:'3'},{runVar,rmVar});
  addEffects(`${op}-nested-flags-${runVar}-${rmVar}`,'{{'+op+'::score::{{setvar::nested::8}}{{getvar::value}}}}',{value:'2'},{runVar,rmVar});
 }
 for(const [id,text,values]of [
  ['ordered','{{setvar::x::4}}{{addvar::x::3}}{{setdefaultvar::x::9}}{{getvar::x}}',{x:'2'}],
  ['repeated','{{setvar::x::1}}{{setvar::x::1}}{{getvar::x}}',{}],
  ['nested-key-value','{{setvar::{{getvar::key}}::{{setvar::x::4}}{{addvar::x::2}}{{getvar::x}}}}{{getvar::answer}}',{key:'answer'}],
  ['dynamic-name','{{{{getvar::name}}::x::7}}{{getvar::x}}',{name:'setvar'}],
  ['dynamic-delimiter','{{setvar{{getvar::tail}}}}{{getvar::x}}',{tail:'::x::8'}],
  ['normalized-name','{{ SeT_Var :x:3}}{{getvar::x}}',{}],
  ['missing-key','{{setvar}}{{getvar}}{{getvar::undefined}}',{}],
  ['empty-key','{{setvar::::}}{{getvar::}}',{}],
  ['undefined-empty-string','{{setvar::x}}{{getvar::x}}/{{setvar::x::}}{{getvar::x}}/{{setvar::x::undefined}}{{getvar::x}}',{}],
  ['default-missing','{{setdefaultvar::x::default}}{{getvar::x}}',{}],
  ['default-undefined','{{setdefaultvar::x::default}}{{getvar::x}}',{x:undefined}],
  ['missing-amount','{{addvar::x}}{{getvar::x}}',{x:'2'}],
  ['utf16','{{setvar::{{getvar::key}}::{{getvar::value}}}}{{getvar::{{getvar::key}}}}',{key:'😀\ud800',value:'\udc00값'}],
  ['if-false','{{#if 0}}{{setvar::x::9}}{{getvar::missing}}{{/if}}{{getvar::x}}',{x:'2'}],
  ['if-pure-false','{{#if_pure 0}}{{addvar::x::9}}{{/if}}{{getvar::x}}',{x:'2'}],
  ['if-true','{{#if 1}}  {{setvar::x::3}}yes\n  {{getvar::x}}{{/if}}',{}],
  ['if-pure-true','{{#if_pure 1}} {{setvar::x::3}}{{getvar::x}} {{/if}}',{}],
  ['when-false','{{#when 0}}{{setvar::x::4}}yes{{:else}}{{addvar::x::2}}no{{/when}}{{getvar::x}}',{x:'1'}],
  ['when-true','{{#when 1}}{{setvar::x::4}}yes{{:else}}{{addvar::x::2}}no{{/when}}{{getvar::x}}',{x:'1'}],
  ['when-keep','{{#when::keep::var::enabled}}\n {{setvar::x::4}}yes\n{{:else}}\n{{addvar::x::2}}no\n{{/when}}{{getvar::x}}',{enabled:'1'}],
  ['condition-order','{{setvar::enabled::1}}{{#when::var::enabled}}{{addvar::x::2}}{{/when}}{{getvar::x}}',{x:'3'}],
  ['global-dependency','{{getglobalvar::toggle_demo}}{{#when::toggle::demo}}{{setvar::x::yes}}{{/when}}',{}],
  ['seeded-verification','{{setvar::x::{{random::a::b}}}}{{getvar::x}}',{}],
 ])addEffects(id,text,values);
 for(const value of ['', 'null', '0', 'false', 'undefined'])addEffects('default-'+value,'{{setdefaultvar::x::default}}{{getvar::x}}',{x:value});
 for(const [value,amount]of [['2',''],[' \u00a0','2'],['0x10','0b10'],['0o10','2'],['bad','2'],['Infinity','-Infinity'],['-0','-0'],['9007199254740992','1'],['1.5','-0.25']])
  addEffects('add-'+value+'/'+amount,'{{addvar::x::'+amount+'}}{{getvar::x}}',{x:value});
 for(const op of ['setvar','addvar','setdefaultvar'])addEffects('snapshot-'+op,'{{'+op+'::x::9}}{{'+op+'::missing::4}}{{getvar::x}}',{x:'2'},{mode:'snapshotOnly'});
 for(const kind of ['error','historyError']){
  const fault=kind==='error'?{kind,message:toUnits('synthetic fault')}:{kind,access:toUnits('row:2')};
  for(const [id,text]of [
   ['callback','{{setvar::a::1}}{{getvar::fault}}{{setvar::b::2}}'],
   ['add-callback','{{setvar::a::1}}{{addvar::fault::2}}{{setvar::b::2}}'],
   ['default-callback','{{setvar::a::1}}{{setdefaultvar::fault::2}}{{setvar::b::2}}'],
   ['condition','{{setvar::a::1}}{{#when::var::fault}}{{setvar::body::3}}{{/when}}{{setvar::b::2}}'],
   ['nested-condition','{{setvar::a::1}}{{#when {{getvar::fault}}}}{{setvar::body::3}}{{/when}}{{setvar::b::2}}'],
   ['nested-writer','{{setvar::a::1}}{{setvar::b::{{getvar::fault}}}}{{setvar::c::3}}'],
   ['overwritten-fault','{{setvar::fault::1}}{{getvar::fault}}{{#when::var::fault}}yes{{/when}}'],
  ]){addEffects(kind+'-'+id,text);effectCases.at(-1).input.variables.push([toUnits('fault'),fault])}
 }
 const finalEffectVariables=(input,effects)=>{
  const values=new Map(input.variables.filter(([,v])=>v.kind==='text'||v.kind==='undefined').map(([k,v])=>[fromUnits(k),v]));
  for(const effect of effects)values.set(effect.key.kind==='undefined'?'undefined':fromUnits(effect.key.units),effect.value);
  return Array.from(values.keys()).sort().map(k=>[toUnits(k),values.get(k)]);
 };
 const effectRows=[];
 for(const item of effectCases){
  const expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
  for(const fold of [false,true]){
   const actual=await native.send({op:'evaluateEffects',text:item.text,input:item.input,fold});
   const {coreMs,...outcome}=actual;
   strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(item.input,actual.effects??[])},expected,item.id+'/'+fold);
   effectRows.push({id:item.id,fold,ok:true,effects:actual.effects.length,reads:actual.reads.length,error:actual.result.error?.name});
  }
 }
 const effectRefusals=[];
 for(const item of [
  {id:'unknown-after-write',text:'{{setvar::x::1}}{{unknown}}',reason:'unsupported-operator'},
  {id:'dynamic-unknown',text:'{{setvar::x::1}}{{{{getvar::name}}}}',values:{name:'unknown'},reason:'unsupported-operator'},
  {id:'history-unimplemented',text:'{{setvar::x::1}}{{lastmessage}}',reason:'unsupported-operator'},
  {id:'false-call',text:'{{setvar::x::1}}{{#if 0}}{{call::f}}{{/if}}',reason:'lazy-body-domain'},
  {id:'false-dynamic',text:'{{#if 0}}{{{{getvar::name}}::x::1}}{{/if}}',values:{name:'setvar'},reason:'lazy-body-domain'},
  {id:'prototype-write',text:'{{setvar::__proto__::x}}',reason:'variable-prototype-key'},
  {id:'mixed-binding',text:'{{setvar::x::1}}',options:{mode:'mixed'},reason:'binding-domain'},
  {id:'custom-callback',text:'{{setvar::x::1}}',options:{getChatVar:'custom'},reason:'binding-domain'},
  {id:'nested-snapshot',text:'{{setvar::x::1}}',options:{context:{chatVariables:{x:'9'}}},reason:'binding-domain'},
  {id:'nested-getter',text:'{{setvar::x::1}}',options:{context:{getChatVar:'custom'}},reason:'binding-domain'},
  {id:'nested-setter',text:'{{setvar::x::1}}',options:{context:{setChatVar:'custom'}},reason:'binding-domain'},
  {id:'nested-mixed-binding',text:'{{setvar::x::1}}',options:{context:{chatVariables:{x:'9'},getChatVar:'custom'}},reason:'binding-domain'},
  {id:'snapshot-fault',text:'{{getvar::fault}}',options:{mode:'snapshotOnly',variables:[[toUnits('fault'),{kind:'error',message:toUnits('fault')}]]},reason:'binding-domain'},
 ])for(const fold of [false,true])for(let attempt=0;attempt<2;attempt++){
  const response=await native.send({op:'evaluateEffects',text:item.text,input:effectInput(item.values,item.options),fold});
  const {coreMs,...outcome}=response;strictAssert.deepEqual(outcome,{status:'rejected',reason:item.reason},item.id);
  effectRefusals.push({id:item.id,fold,attempt,ok:true});
 }
 const effectInterruptions=[];
 for(const [reason,limits,controls]of [
  ['effect-limit',{effects:1},{}],['read-limit',{reads:1},{}],['retained-limit',{retainedBytes:1},{}],
  ['budget',{}, {budget:4}],['cancelled',{}, {cancelled:true}],
 ]){
  const response=await native.send({op:'evaluateEffects',text:'{{setvar::x::1}}{{getvar::x}}{{setvar::y::2}}{{getvar::y}}',input:effectInput({},{limits}),...controls});
  const {coreMs,...outcome}=response;strictAssert.deepEqual(outcome,{status:'interrupted',reason});effectInterruptions.push({reason,ok:true});
 }
 for(const fold of [false,true]){
  const item={text:toUnits('{{setvar::x::1}}{{getvar::large}}'),input:effectInput({large:'A'.repeat(128)})};
  const expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
  for(const maxOutput of [64,256]){
   const actual=await native.send({op:'evaluateEffects',...item,fold,maxOutput});const {coreMs,...outcome}=actual;
   if(maxOutput===64)strictAssert.deepEqual(outcome,{status:'interrupted',reason:'output-limit'});
   else{strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(item.input,actual.effects)},expected);strictAssert.equal(actual.effects.length,1)}
   effectInterruptions.push({reason:'output-after-effect',fold,maxOutput,ok:true});
  }
 }
 const effectCheckpoints=[];
 for(const fold of [false,true]){
  let input=effectInput({next:'{{setvar::x::5}}{{gettempvar::t}}'}),text='TOKEN',writes=0;
  for(const [step,pattern,replacement]of [[0,'TOKEN','{{settempvar::t::old}}{{getvar::next}}'],[1,'absent',''],[2,'^','{{addvar::x::1}}{{getvar::x}}'],[3,'$','{{getvar::fault}}']]){
   text=text.replace(new RegExp(pattern,'g'),replacement);
   if(step===3)input={...input,variables:[...input.variables,[toUnits('fault'),{kind:'historyError',access:toUnits('checkpoint')}]]};
   const item={text:toUnits(text),input},expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
   const actual=await native.send({op:'evaluateEffects',...item,fold});const {coreMs,...outcome}=actual;
   const finalVariables=finalEffectVariables(input,actual.effects??[]);
   strictAssert.deepEqual({...outcome,finalVariables},expected,'effect-checkpoint-'+step+'/'+fold);
   writes+=actual.effects.length;strictAssert.equal(actual.effects.length,[0,1,1,0][step]);
   if(step<3){text=fromUnits(actual.result.output);input={...input,variables:finalVariables}}
   else{strictAssert.equal(actual.result.error.name,'CbsHistoryAccessError');strictAssert.equal(writes,2);strictAssert.deepEqual(finalVariables.find(([k])=>fromUnits(k)==='x')?.[1],effectTag('6'))}
   effectCheckpoints.push({step,fold,ok:true,writes});
  }
 }
 const historyCapture=(patch={})=>({mode:'preparedAbsolute',length:10,rows:[{index:3,data:toUnits('absolute-three'),role:toUnits('user')},{index:9,data:toUnits('tail'),role:toUnits('char')}],
  selectedCharacter:true,provenance:{captureId:toUnits('capture\ud800'),characterId:toUnits('character'),conversationId:toUnits('conversation'),revision:'18446744073709551615'},
  limits:{rows:64,inputUnits:16384,retainedBytes:131072,dependencies:64},...patch});
 const historyInput=(history={},values={},context={})=>{const input=effectInput(values);return{...input,context:{...input.context,index:-1,...context},history:historyCapture(history)}};
 const historyCases=[];
 const addHistory=(id,text,history={},values={},context={})=>historyCases.push({id,text:toUnits(text),input:historyInput(history,values,context)});
 addHistory('sparse-total-aliases','{{lastmessageid}}/{{lastmessageindex}}/{{LAST_MESSAGE_INDEX}}/{{lastmessage}}/{{previous_chat_log::3}}');
 addHistory('first-read-order','{{previouschatlog::3}}{{lastmessage}}{{previouschatlog::3}}');
 addHistory('empty-history','{{lastmessage}}/{{lastmessageid}}/{{previouschatlog::0}}',{length:0,rows:[]});
 addHistory('empty-data-and-surrogates','{{previouschatlog::0}}/{{lastmessage}}',{length:2,rows:[{index:0,data:[],role:toUnits('user')},{index:1,data:toUnits('\ud800😀\udc00'),role:toUnits('char')}]});
 for(const selectedCharacter of [false,true])for(const [id,text,index]of [
  ['tail','{{lastmessage}}',-1],['count','{{lastmessageid}}',-1],['absolute','{{previouschatlog::3}}',-1],['current-role','{{role}}',3],['outside-role','{{role}}',10],
 ])addHistory('selection-'+selectedCharacter+'-'+id,text,{selectedCharacter},{},{index});
 for(const argument of [undefined,'',' ','-0','0x3','0b11','0o3','3','3.0','3.5','-1','NaN','Infinity','-Infinity','undefined','10','9007199254740992'])
  addHistory('number-'+String(argument),'{{previouschatlog'+(argument===undefined?'':'::'+argument)+'}}');
 for(const [id,history,index]of [
  ['override',{conditionChatRole:toUnits('system'),firstmsg:true},3],['first',{conditionChatRole:[],firstmsg:true},3],
  ['row',{conditionChatRole:[],firstmsg:false},3],['fallback',{},-1],['empty-fallback',{role:[]},-1],['text-fallback',{role:toUnits('system')},-1],
  ['negative-row',{},-2],['missing-row',{},4],
 ])addHistory('role-'+id,'{{setvar::before::1}}{{role}}{{setvar::after::2}}',history,{},{index});
 addHistory('real-history-prefix','{{setvar::score::changed}}{{lastmessage}}{{setvar::later::no}}',{rows:[]});
 addHistory('nested-undefined-prefix','{{previouschatlog::{{setvar::undefinedValue}}4}}{{setvar::later::no}}');
 addHistory('nested-key-read','{{previouschatlog::{{setvar::index::3}}{{getvar::index}}}}');
 addHistory('dynamic-name','{{{{getvar::name}}::3}}',{}, {name:'previous_chat_log'});
 addHistory('dynamic-delimiter','{{previouschatlog{{getvar::tail}}}}',{}, {tail:'::3'});
 addHistory('skipped-if','{{#if 0}}{{lastmessage}}{{setvar::x::1}}{{/if}}',{rows:[]});
 addHistory('skipped-pure-if','{{#if_pure 0}}{{lastmessage}}{{setvar::x::1}}{{/if}}',{rows:[]});
 addHistory('taken-if','{{#if 1}}{{lastmessage}}{{/if}}');
 addHistory('eager-discarded-branch','{{#when 0}}{{setvar::a::1}}{{previouschatlog::3}}{{:else}}{{setvar::b::2}}{{lastmessage}}{{/when}}');
 addHistory('eager-history-error','{{#when 0}}{{setvar::a::1}}{{lastmessage}}{{:else}}no{{/when}}',{rows:[]});
 addHistory('generated-writer-inert','{{settempvar::t::old}}{{lastmessage}}',{length:1,rows:[{index:0,data:toUnits('{{setvar::x::5}}{{gettempvar::t}}'),role:toUnits('user')}]});
 const historyRows=[],historyInvalidation=[];
 for(const item of historyCases)for(const binding of ['mutableMap','snapshotOnly']){
  const input={...item.input,mode:binding};
  const expected=await page.evaluate(item=>window.__rustCbsEffects(item),{...item,input});
  if(item.id==='real-history-prefix'){
   strictAssert.deepEqual(await page.evaluate(()=>window.__rustCbsHistoryCompletion),{name:'CbsHistoryAccessError',access:toUnits('row 9')});
   strictAssert.equal(expected.effects.length,binding==='mutableMap'?1:0);strictAssert.equal(expected.result.output,undefined);
  }
  for(const fold of [false,true]){
   const actual=await native.send({op:'evaluateEffects',text:item.text,input,fold});const {coreMs,...outcome}=actual;
   strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(input,actual.effects??[])},expected,item.id+'/'+binding+'/'+fold);
   historyRows.push({id:item.id,binding,fold,ok:true,error:actual.result.error?.name});
  }
  if(item.id==='first-read-order'){
   const changes=[{start:0,end:0,countChanged:true},{start:3,end:4,countChanged:false},{start:4,end:9,countChanged:false}];
   const affected=await page.evaluate(input=>window.__rustCbsHistoryInvalidation(input),{observation:expected.history,changes});
   strictAssert.deepEqual(affected,[true,true,false]);historyInvalidation.push({binding,affected});
  }
 }
 const historyRefusals=[];
 const malformedHistory=[
  ['mode',{mode:'live'},'history-domain'],['projected-mode',{mode:'projected'},'history-domain'],
  ['offset',{historyOffset:3},'binding-domain'],['callback',{row:'custom'},'binding-domain'],
  ['unsafe-length',{length:9007199254740992},'history-domain'],['negative-length',{length:-1},'history-domain'],
  ['duplicate',{rows:[{index:3,data:[],role:[]},{index:3,data:[],role:[]}]},'history-domain'],
  ['outside',{rows:[{index:10,data:[],role:[]}]},'history-domain'],['fraction',{rows:[{index:0.5,data:[],role:[]}]},'binding-domain'],
  ['row-field',{rows:[{index:3,data:[],role:[],custom:true}]},'binding-domain'],
  ['missing-data',{rows:[{index:3,role:[]}]},'binding-domain'],
  ['missing-limits',{limits:undefined},'binding-domain'],['zero-limit',{limits:{rows:0,inputUnits:100,retainedBytes:10000,dependencies:1}},'invalid-history-limits'],
  ['small-rows',{limits:{rows:1,inputUnits:10000,retainedBytes:10000,dependencies:3}},'history-input-limit'],
  ['small-units',{limits:{rows:10,inputUnits:1,retainedBytes:10000,dependencies:3}},'history-input-limit'],
  ['small-bytes',{limits:{rows:10,inputUnits:10000,retainedBytes:1,dependencies:3}},'history-input-limit'],
  ['long-label',{provenance:{captureId:Array(257).fill(65),characterId:[],conversationId:[]}},'history-domain'],
  ['numeric-revision',{provenance:{captureId:[],characterId:[],conversationId:[],revision:9007199254740992}},'binding-domain'],
  ['noncanonical-revision',{provenance:{captureId:[],characterId:[],conversationId:[],revision:'01'}},'history-domain'],
  ['label-authority',{provenance:{captureId:[],characterId:[],conversationId:[],ownerPermit:'custom'}},'binding-domain'],
 ];
 for(const [id,patch,reason]of malformedHistory)for(const fold of [false,true]){
  const actual=await native.send({op:'evaluateEffects',text:'{{setvar::x::1}}{{lastmessage}}',input:historyInput(patch),fold});
  const {coreMs,...outcome}=actual;strictAssert.deepEqual(outcome,{status:'rejected',reason},id);historyRefusals.push({id,fold,ok:true});
 }
 for(const [id,text,reason]of [['lazy-dynamic','{{#if 0}}{{{{getvar::name}}}}{{/if}}','lazy-body-domain'],
  ['role-search','{{setvar::x::1}}{{previoususerchat}}','unsupported-operator'],['unknown-after-history','{{setvar::x::1}}{{lastmessage}}{{unknown}}','unsupported-operator']]){
  const actual=await native.send({op:'evaluateEffects',text,input:historyInput({}, {name:'lastmessage'})});const {coreMs,...outcome}=actual;
  strictAssert.deepEqual(outcome,{status:'rejected',reason},id);historyRefusals.push({id,ok:true});
 }
 const historyInterruptions=[];
 for(const fold of [false,true])for(const small of [false,true]){
  for(const kind of ['dependencies','output']){
   const capture=kind==='dependencies'?historyCapture({limits:{rows:64,inputUnits:16384,retainedBytes:131072,dependencies:small?1:2}}):historyCapture({length:1,rows:[{index:0,data:toUnits('A'.repeat(128)),role:toUnits('user')}]});
   const item={text:toUnits('{{setvar::x::1}}'+(kind==='dependencies'?'{{previouschatlog::3}}{{lastmessage}}':'{{lastmessage}}')),input:historyInput(capture)};
   const actual=await native.send({op:'evaluateEffects',...item,fold,...(kind==='output'?{maxOutput:small?64:256}:{})});const {coreMs,...outcome}=actual;
   if(small)strictAssert.deepEqual(outcome,{status:'interrupted',reason:kind==='dependencies'?'history-dependency-limit':'output-limit'});
   else{const expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(item.input,actual.effects)},expected);strictAssert.equal(actual.effects.length,1)}
   historyInterruptions.push({kind,fold,small,ok:true});
  }
 }
 for(const fold of [false,true])for(const stage of ['admitted-writer','copy-interrupted','copy-success']){
  const input=historyInput({length:1,rows:[{index:0,data:toUnits('R'.repeat(16384)),role:toUnits('user')}],
   limits:{rows:64,inputUnits:32768,retainedBytes:stage==='copy-success'?131072:49152,dependencies:64}});
  const item={text:toUnits('{{setvar::x::1}}'+(stage==='admitted-writer'?'':'{{lastmessage}}')),input};
  const actual=await native.send({op:'evaluateEffects',...item,fold,maxOutput:65536});const {coreMs,...outcome}=actual;
  if(stage==='copy-interrupted')strictAssert.deepEqual(outcome,{status:'interrupted',reason:'history-retained-limit'});
  else{
   const expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
   strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(input,actual.effects)},expected);
   strictAssert.deepEqual(actual.effects,[{sequence:0,key:effectTag('x'),value:effectTag('1')}]);
   strictAssert.equal(actual.history.count,stage==='copy-success');
  }
  historyInterruptions.push({kind:'retained-bytes',fold,stage,ok:true});
 }
 for(const controls of [{budget:4},{cancelled:true}]){
  const actual=await native.send({op:'evaluateEffects',text:'{{setvar::x::1}}{{lastmessage}}',input:historyInput(),...controls});const {coreMs,...outcome}=actual;
  strictAssert.deepEqual(outcome,{status:'interrupted',reason:controls.cancelled?'cancelled':'budget'});historyInterruptions.push({controls,ok:true});
 }
 const historyCheckpoints=[];
 for(const fold of [false,true]){
  let input=historyInput({length:1,rows:[{index:0,data:toUnits('{{setvar::x::5}}{{gettempvar::t}}'),role:toUnits('user')}]}),text='TOKEN';
  for(const [step,pattern,replacement]of [[0,'TOKEN','{{settempvar::t::old}}{{lastmessage}}'],[1,'absent',''],[2,'^','{{addvar::x::1}}{{getvar::x}}/{{lastmessage}}']]){
   if(step===2)input={...input,history:historyCapture({length:1,rows:[{index:0,data:toUnits('new-capture'),role:toUnits('char')}],provenance:{...input.history.provenance,captureId:toUnits('next-capture')}})};
   text=text.replace(new RegExp(pattern,'g'),replacement);
   const item={text:toUnits(text),input},expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
   const actual=await native.send({op:'evaluateEffects',...item,fold});const {coreMs,...outcome}=actual;
   const finalVariables=finalEffectVariables(input,actual.effects??[]);
   strictAssert.deepEqual({...outcome,finalVariables},expected,'history-checkpoint-'+step+'/'+fold);
   strictAssert.equal(actual.effects.length,[0,1,1][step]);
   text=fromUnits(actual.result.output);input={...input,variables:finalVariables};
   if(step===2)strictAssert.equal(text,'6/new-capture');
   historyCheckpoints.push({step,fold,ok:true,effects:actual.effects.length});
  }
 }
 const programProfile=(limits={})=>({mode:'localFunctions',limits:{functions:256,frames:32,sourceUnits:2097152,expandedUnits:8388608,retainedBytes:33554432,...limits}});
 const programInput=(values={},options={})=>effectInput(values,{program:programProfile(),...options});
 const programCases=[];
 const addProgram=(id,text,values={},options={})=>programCases.push({id,text:toUnits(text),input:programInput(values,options)});
 for(const [id,text,values]of [
  ['literal','{{#func f}}  F  {{/func}}A{{call::f}}B'],
  ['arg-zero','{{#func f}}{{arg::0}}/{{arg::1}}{{/func}}{{call::f::X}}'],
  ['missing-argument','{{#func f}}{{arg::1}}{{/func}}{{call::f}}'],
  ['empty-argument','{{#func f}}{{arg::1}}{{/func}}{{call::f::}}'],
  ['empty-name','{{#func  ignored}} E {{/func}}[{{call::}}]'],
  ['formal-names-inert','{{#func f named}}{{arg::1}}/{{arg::named}}{{/func}}{{call::f::X}}'],
  ['unknown','{{call::missing::{{setvar::x::1}}}}'],
  ['case-sensitive-call','{{#func f}}F{{/func}}{{CALL::f}}/{{c_a-ll:f}}'],
  ['utf16-name','{{#func \ud800}}F\udc00{{/func}}{{call::\ud800}}'],
  ['dynamic-name','{{#func f}}{{arg::1}}{{/func}}{{call::{{getvar::name}}::{{getvar::value}}}}',{name:'f',value:'X'}],
  ['dynamic-call','{{#func f}}F{{/func}}{{{{getvar::name}}::f}}',{name:'call'}],
  ['dynamic-header-name','{{#func {{getvar::name}}}}F{{/func}}{{call::f}}',{name:'f'}],
  ['false-if-call','{{#func bump}}{{addvar::x::1}}{{/func}}{{#if 0}}{{call::bump}}{{/if}}{{getvar::x}}',{x:'0'}],
  ['false-if-pure-call','{{#func bump}}{{addvar::x::1}}{{/func}}{{#if_pure 0}}{{call::bump}}{{/if}}{{getvar::x}}',{x:'0'}],
  ['capture-call','{{#func emit}}{{addvar::x::1}}B{{/func}}{{#func f}}A{{call::emit}}C{{/func}}{{getvar::x}}/{{call::f}}/{{getvar::x}}',{x:'0'}],
  ['nested-preserved','{{#func bump}}{{addvar::x::1}}{{/func}}{{#if 0}}{{#when::var::x}}{{arg::{{setvar::x::100}}}}{{call::bump}}{{/when}}{{/if}}{{getvar::x}}',{x:'0'}],
  ['nested-definition','{{#func f}}{{#func g}}G{{/func}}{{call::g}}{{/func}}{{call::f}}/{{call::g}}'],
  ['redefinition','{{#func g}}old{{/func}}{{#func f}}{{#func g}}new{{/func}}{{/func}}{{call::f}}{{call::g}}'],
  ['temps','{{#func f}}[{{tempvar::t}}]{{settempvar::t::child}}{{tempvar::t}}{{/func}}{{settempvar::t::parent}}{{call::f}}/{{tempvar::t}}'],
  ['sibling-19','{{#func f}}R{{/func}}'+'{{call::f}}'.repeat(19)+'{{call::missing}}'],
  ['sibling-20','{{#func f}}R{{/func}}'+'{{call::f}}'.repeat(20)+'{{call::missing}}'],
  ['recursion','{{#func f}}{{call::f}}{{/func}}{{call::f}}'],
  ['root-return','before{{setvar::x::1}}{{return::R}}{{setvar::y::2}}after'],
  ['missing-return','before{{return}}after'],
  ['empty-return','before{{return::}}after'],
  ['text-undefined-return','before{{return::undefined}}after'],
  ['nested-return','{{equal::{{return::R}}::x}}{{setvar::suffix::1}}'],
  ['child-return','{{#func f}}before{{setvar::x::1}}{{return::R}}{{setvar::childSuffix::1}}{{/func}}A{{call::f}}{{setvar::parentSuffix::1}}B'],
  ['discarded-when-return','{{#when 0}}{{return::R}}{{:else}}B{{/when}}suffix'],
  ['returned-cbs','{{#func f}}{{return::{{getvar::raw}}}}{{/func}}A{{call::f}}B',{raw:'{{setvar}}'}],
  ['nested-residual-read','{{#func f}}{{arg::{{getvar::index}}}}{{/func}}{{call::f::X}}',{index:'1'}],
  ['nested-residual-writer','{{arg::{{setvar::x::1}}}}'],
  ['nested-residual-return','{{arg::{{return::R}}}}suffix'],
  ['nested-residual-preserved','{{#if 0}}{{arg::{{setvar::x::1}}}}{{/if}}'],
  ['when-lines','{{#func f}}{{#when 1}}\n  A\n{{:else}}\n B\n{{/when}}{{/func}}{{call::f}}'],
  ['when-keep-lines','{{#func f}}{{#when::keep::0}}\n  A\n{{:else}}\n B\n{{/when}}{{/func}}{{call::f}}'],
  ['if-lines','{{#func f}}{{#if 1}}\n  A\n B  \n{{/if}}{{/func}}{{call::f}}'],
  ['if-pure-lines','{{#func f}}{{#if_pure 1}}\n A\n{{/if}}{{/func}}{{call::f}}'],
  ['rng-order','{{#func f}}{{random::a::b}}{{/func}}{{#if 0}}{{call::f}}{{/if}}{{call::f}}{{random::a::b}}'],
  ['disabled-parent-writer','{{#func f}}{{setvar::x::{{call::g}}}}{{/func}}{{#func g}}G{{/func}}{{call::f}}{{getvar::x}}'],
 ])addProgram(id,text,values);
 for(const token of ['{{ARG::1}}','{{ arg::1}}','{{a_r-g:1}}','{{arg:1}}','{{arg}}','{{arg::}}','{{arg::01}}','{{arg::-0}}','{{arg::-1}}','{{arg::NaN}}','{{arg::x}}','{{arg::1::extra}}'])
  addProgram('residual-'+token,'{{#func f}}'+token+'{{/func}}{{call::f::X}}');
 for(const replacement of ['$$','$&','$\x60',"$'",'$1$99$<name>','\ud800'])
  addProgram('replacement-'+replacement,'{{#func f}}A{{arg::1}}B{{/func}}{{call::f::'+replacement+'}}');
 addProgram('introduced-later-placeholder',"{{#func f}}{{arg::1}}{{arg::2}}{{/func}}{{call::f::$'::Z}}");
 addProgram('introduced-missing-placeholder',"{{#func f}}{{arg::1}}{{arg::2}}{{/func}}{{call::f::$'}}");
 for(const [id,text]of [
  ['history-child-prefix','{{#func f}}{{setvar::child::1}}{{previouschatlog::3}}{{lastmessage}}{{setvar::late::1}}{{/func}}{{setvar::parent::1}}{{call::f}}{{setvar::suffix::1}}'],
  ['history-capture-prefix','{{#func f}}{{setvar::x::1}}{{lastmessage}}{{/func}}{{#func g}}{{call::f}}{{/func}}'],
  ['history-ordered','{{#func f}}{{previouschatlog::3}}/{{role::3}}{{/func}}{{lastmessageid}}/{{call::f}}/{{lastmessage}}'],
 ])addProgram(id,text,{}, {context:{...effectInput().context,index:-1},history:historyCapture({rows:id==='history-ordered'?historyCapture().rows:[historyCapture().rows[0]]})});
 const programRows=[];
 for(const item of programCases)for(const binding of ['mutableMap','snapshotOnly']){
  const test={...item,input:{...item.input,mode:binding}};
  const expected=await page.evaluate(item=>window.__rustCbsEffects(item),test);
  for(const fold of [false,true]){
   const actual=await native.send({op:'evaluateEffects',text:test.text,input:test.input,fold});const {coreMs,...outcome}=actual;
   strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(test.input,actual.effects??[])},expected,item.id+'/'+binding+'/'+fold);
   programRows.push({id:item.id,binding,fold,ok:true,effects:actual.effects.length,reads:actual.reads.length,error:actual.result.error?.name});
  }
 }
 for(const fault of [{kind:'error',message:toUnits('failure')},{kind:'historyError',access:toUnits('captured-fault')}])
  for(const read of ['{{getvar::fault}}','{{#when::var::fault}}body{{/when}}','{{#when {{getvar::fault}}}}body{{/when}}']){
   const item={text:toUnits('{{#func f}}{{setvar::child::1}}'+read+'{{setvar::childSuffix::1}}{{/func}}{{setvar::parent::1}}{{call::f}}{{setvar::parentSuffix::1}}'),
    input:programInput({}, {variables:[[toUnits('fault'),fault]]})};
   const expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
   for(const fold of [false,true]){
    const actual=await native.send({op:'evaluateEffects',...item,fold});const {coreMs,...outcome}=actual;
    strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(item.input,actual.effects??[])},expected);
    programRows.push({id:fault.kind+'/'+read,binding:'mutableMap',fold,ok:true,effects:actual.effects.length,error:actual.result.error?.name});
   }
  }
 for(const flags of [{runVar:false},{rmVar:true},{runVar:false,rmVar:true}]){
  const item={text:toUnits('{{#func f}}{{arg::{{setvar::x::1}}}}{{/func}}{{setvar::y::{{call::f}}}}{{getvar::x}}'),input:programInput({},flags)};
  const expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
  for(const fold of [false,true]){
   const actual=await native.send({op:'evaluateEffects',...item,fold});const {coreMs,...outcome}=actual;
   strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(item.input,actual.effects??[])},expected);
   programRows.push({id:'writer-flags',flags,fold,ok:true});
  }
 }
 const programRefusals=[];
 for(const item of [
  {text:'{{setvar::x::1}}{{unknown}}',reason:'unsupported-operator'},
  {text:'{{arg::{{unknown}}}}',reason:'unsupported-operator'},
  {text:'{{#func f}}{{unknown}}{{/func}}',reason:'unsupported-operator'},
  {text:'{{#each x as y}}x{{/each}}',reason:'block-form'},
  {text:'{{#func::f}}x{{/func}}',reason:'block-form'},
  {text:'{{{{getvar::kind}}}}x{{/func}}',values:{kind:'#func f'},reason:'dynamic-block'},
  {text:'{{#func f}}x{{/{{getvar::close}}}}',values:{close:'func'},reason:'dynamic-block-close'},
  {text:'{{setvar::x::1}}',program:{...programProfile(),functions:[]},reason:'binding-domain'},
  {text:'{{setvar::x::1}}',program:{...programProfile(),callStack:1},reason:'binding-domain'},
  {text:'{{setvar::x::1}}',program:{...programProfile(),mode:'external'},reason:'program-domain'},
  {text:'{{setvar::x::1}}',program:programProfile({unknown:1}),reason:'binding-domain'},
  {text:'plain',program:programProfile({frames:0}),reason:'invalid-program-limits'},
  {text:'plain',program:programProfile({sourceUnits:1}),reason:'program-input-limit'},
 ])for(const fold of [false,true]){
  const actual=await native.send({op:'evaluateEffects',text:item.text,input:programInput(item.values,{program:item.program??programProfile()}),fold});const {coreMs,...outcome}=actual;
  strictAssert.deepEqual(outcome,{status:'rejected',reason:item.reason},item.text);programRefusals.push({text:item.text,fold,reason:item.reason,ok:true});
 }
 const programInterruptions=[];
 for(const item of [
  {id:'functions',text:'{{#func f}}F{{/func}}{{setvar::x::1}}{{#func g}}G{{/func}}',limits:{functions:1},reason:'program-function-limit'},
  {id:'frames',text:'{{#func f}}F{{/func}}{{setvar::x::1}}{{call::f}}',limits:{frames:1},reason:'program-frame-limit'},
  {id:'expanded',text:'{{#func f}}hello{{/func}}{{setvar::x::1}}{{call::f}}',limits:{expandedUnits:1},reason:'program-expanded-limit'},
  {id:'source',text:'{{#func f}}'+'a'.repeat(64)+'{{arg::1}}'+'b'.repeat(64)+'{{/func}}{{setvar::x::1}}{{call::f::$\x60$\x60$\x60}}',limits:{sourceUnits:256},reason:'program-source-limit'},
  {id:'retained',text:'{{setvar::x::1}}{{getvar::large}}',values:{large:'A'.repeat(4096)},limits:{retainedBytes:2048},reason:'program-retained-limit'},
  {id:'output',text:'{{setvar::x::1}}{{getvar::large}}',values:{large:'A'.repeat(128)},controls:{maxOutput:64},reason:'output-limit'},
  {id:'work',text:'{{#func f}}'+'a'.repeat(4096)+'{{arg::1}}{{/func}}{{setvar::x::1}}{{call::f::X}}',controls:{budget:100},reason:'budget'},
  {id:'cancel',text:'{{setvar::x::1}}{{call::f}}',controls:{cancelled:true},reason:'cancelled'},
 ])for(const fold of [false,true]){
  const input=programInput(item.values,{program:programProfile(item.limits)});
  const actual=await native.send({op:'evaluateEffects',text:item.text,input,fold,...item.controls});const {coreMs,...outcome}=actual;
  strictAssert.deepEqual(outcome,{status:'interrupted',reason:item.reason},item.id);programInterruptions.push({id:item.id,fold,stage:'limited',ok:true});
  const control={text:toUnits(item.text),input:programInput(item.values)},expected=await page.evaluate(item=>window.__rustCbsEffects(item),control);
  const success=await native.send({op:'evaluateEffects',...control,fold});const {coreMs:ignored,...successful}=success;
  strictAssert.deepEqual({...successful,finalVariables:finalEffectVariables(control.input,success.effects??[])},expected,item.id+'/control');
  programInterruptions.push({id:item.id,fold,stage:'sufficient',ok:true});
 }
 const programTempParent='P'.repeat(4096),programTempChild='c'.repeat(256);
 const programTempBody=Array.from({length:100},(_,i)=>'{{settempvar::c'+String(i).padStart(3,'0')+'::{{getvar::child}}}}').join('');
 const programTempPrefix='{{#func f}}[{{gettempvar::p}}]'+programTempBody+'{{settempvar::p::child}}{{gettempvar::p}}{{/func}}{{setvar::effect::kept}}';
 for(const fold of [false,true])for(const [stage,parentLive,childLive,retainedBytes,expectedText]of [
  ['parent-only',true,false,86016,'/'+programTempParent],
  ['child-only',false,true,86016,'[]child/'],
  ['combined-low',true,true,86016,null],
  ['combined-high',true,true,262144,'[]child/'+programTempParent],
 ]){
  const item={text:toUnits(programTempPrefix+(parentLive?'{{settempvar::p::{{getvar::parent}}}}':'')+(childLive?'{{call::f}}':'')+'/{{gettempvar::p}}'),
   input:programInput({parent:programTempParent,child:programTempChild},{program:programProfile({retainedBytes})})};
  const actual=await native.send({op:'evaluateEffects',...item,fold,maxOutput:65536});const {coreMs,...outcome}=actual;
  if(expectedText===null)strictAssert.deepEqual(outcome,{status:'interrupted',reason:'program-retained-limit'},stage);
  else{
   const expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
   strictAssert.deepEqual({...outcome,finalVariables:finalEffectVariables(item.input,actual.effects??[])},expected,stage);
   strictAssert.equal(fromUnits(actual.result.output),expectedText,stage);
   strictAssert.deepEqual(actual.effects,[{sequence:0,key:effectTag('effect'),value:effectTag('kept')}],stage);
  }
  programInterruptions.push({id:'live-parent-child-temps',fold,stage,ok:true});
 }

 const programCheckpoints=[];
 for(const fold of [false,true]){
  let input=programInput({raw:'{{setvar}}'}),text='{{#func f}}{{return::{{getvar::raw}}}}{{/func}}{{settempvar::t::old}}A{{call::f}}B';
  for(let step=0;step<3;step++){
   if(step===2)text+='{{call::f}}/{{gettempvar::t}}';
   const item={text:toUnits(text),input},expected=await page.evaluate(item=>window.__rustCbsEffects(item),item);
   const actual=await native.send({op:'evaluateEffects',...item,fold});const {coreMs,...outcome}=actual;
   const finalVariables=finalEffectVariables(input,actual.effects??[]);
   strictAssert.deepEqual({...outcome,finalVariables},expected,'program-checkpoint-'+step+'/'+fold);
   strictAssert.equal(fromUnits(actual.result.output),['A{{setvar}}B','AB','AB{{call::f}}/'][step]);
   strictAssert.equal(actual.effects.length,[0,1,0][step]);
   text=fromUnits(actual.result.output);input={...input,variables:finalVariables};
   programCheckpoints.push({step,fold,ok:true,effects:actual.effects.length});
  }
 }

 const hashCoreRows=[];
 const hashWords=['','a','Synthetic','😀','\ud800','\udc00','😀\ud800x\udc00','\u0000\uffff','a'.repeat(4096),'\uffff'.repeat(4096),'ab'.repeat(32768)];
 const hashCids=[undefined,0,-0,0.5,1,1.5,-1.5,999,999.5,1000,1000.5,-1000,4294967297,Number.MAX_SAFE_INTEGER,1e30,'NaN','Infinity','-Infinity'];
 const hashSeeds=[[0,0,0,0],[1,2,3,4],[-1,-2,-3,-4],[4294967295,4294967296,4294967297,-4294967297],[1.9,-1.9,1e30,-1e30],['NaN','Infinity','-Infinity',0],[Number.MAX_SAFE_INTEGER,-Number.MAX_SAFE_INTEGER,2147483648,-2147483649]];
 for(const word of hashWords)for(const cid of hashCids) {
  const seeds=hashSeeds[hashCoreRows.length%hashSeeds.length],count=16;
  const expected=await page.evaluate(({word,cid,seeds,count})=>window.__rustCbsHashCore(word,cid===undefined?undefined:Number(cid),seeds.map(Number),count),{word,cid,seeds,count});
  const response=await native.send({op:'hash-core',word:toUnits(word),cid,seeds,count});
  const ok=!response.fallback&&response.value===expected.value&&JSON.stringify(response.sequence)===JSON.stringify(expected.sequence);
  hashCoreRows.push({wordUnits:word.length,cid:cid??'missing',seeds,ok,fallback:response.fallback});
  if(!ok)throw Error('Hash core mismatch '+JSON.stringify({wordUnits:word.length,cid,seeds,expected,response}));
 }
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
 const requiredReplaceStart=examples.length;
 const replacementCases=[
  '{{replace}}','{{replace::abc}}','{{replace::abc::b}}','{{replace::undefined}}','{{replace::undefined::undefined::}}',
  '{{replace::::::_}}','{{replace::abc::b::$$|$&|$`|$\'|$1|$99|$<name>}}',
  '{{replace::aaaaa::aa::X}}','{{replace::abababacababac::ababac::X}}',
  '{{replace::x::x::{{getvar::next}}}}','{{{{getvar::replacementName}}::x::x::y}}','{{replace{{getvar::replacementTail}}}}',
  '{{#if 0}}{{replace::x::x::y}}{{/if}}','{{#if 1}}{{replace::x::x::y}}{{/if}}',
  '{{#when 0}}{{replace::x::x::{{settempvar::x::yes}}}}{{/when}}{{gettempvar::x}}',
  '{{replace::x x::x::{{gettempvar::x}}{{settempvar::x::1}}}}{{gettempvar::x}}',
  '{{re_place::abc::b::x}}','{{REPLACE:abc:b:x}}',
 ];
 for(const input of ['', 'a', 'ababa', 'ababababac', '😀\ud800', '\udc00x'])
  for(const search of ['', 'a', 'aba', '\ud83d', '\ud800', 'missing'])
   for(const replacement of ['', '_', '$$', '$&', '$`', "$'", '$1$99$<name>', '$$$&$`', '\udc00'])
    replacementCases.push('{{replace::'+input+'::'+search+'::'+replacement+'}}');
 examples.push(...replacementCases);
 const requiredArrayStart=examples.length;
 const arrayCases=[
  String.raw`{{arraypush::["\u+041"]::x}}`,String.raw`{{arrayelement::["\u+041"]::0}}`,
  '{{{{getvar::arrayName}}::[1,2,3]::1::1::x}}','{{arraysplice{{getvar::arrayTail}}}}','{{ARRAY_POP::[1,2]}}',
  '{{#if 0}}{{arraypush::[]::{{settempvar::x::yes}}}}{{/if}}{{gettempvar::x}}',
  '{{#if 1}}{{arrayshift::[1,2]}}{{/if}}','{{#if_pure 1}}{{arraypop::[1,2]}}{{/if}}',
  '{{#when 0}}{{arraypush::[]::{{settempvar::x::yes}}}}{{/when}}{{gettempvar::x}}',
  String.raw`{{arrayshift::[0,"a\u003a\u003ab",["c\u003a\u003ad"],{"x":"e\u003a\u003af"}]}}`,
 ];
 for(const op of ['arrayshift','arraypop','arraypush','arraysplice']) {
  arrayCases.push('{{'+op+'}}','{{'+op+'::{{getvar::arrayInput}}}}');
  arrayCases.push('{{{{arrayelement::'+JSON.stringify([op])+'::0}}::[1,2]}}', '{{'+op+'{{getvar::sep}}[1,2]}}');
  for(const block of ['#if 1','#if 0','#when 1','#when 0'])
   arrayCases.push('{{'+block+'}}{{'+op+'::[1,2]::{{settempvar::x::yes}}}}{{/}}{{gettempvar::x}}');
  for(const input of ['', '[]', '[1,true,null]', '[1e400,-0,1e-7]', String.raw`["\ud800","\udc00","😀"]`, '[0,[1,2],{"2":1,"1":2,"x":3,"x":4}]', 'a§b§', 'null', '42', '{"x":1}', '[invalid', String.raw`["a\u003a\u003ab"]`])
   for(const tail of ['', '::', '::x', '::false', '::1::1::x', '::0::0'])
    arrayCases.push('{{'+op+'::'+input+tail+'}}');
 }
 for(const start of ['', 'NaN', 'Infinity', '-Infinity', '-0', '-1.9', '1.9', '-999', '999', '0x1', '0b10', 'abc'])
  for(const count of ['', 'NaN', 'Infinity', '-Infinity', '-1.9', '1.9', '999', '0o2'])
   arrayCases.push('{{arraysplice::[1,2,3]::'+start+'::'+count+'::x}}');
 examples.push(...arrayCases);
 const requiredHashStart=examples.length;
 const hashCases=[
  '{{hash}}','{{HASH}}','{{ha_sh}}','{{hash::}}','{{HASH:}}','{{ha-sh::Synthetic}}',
  '{{{{getvar::hashName}}::Synthetic}}','{{hash{{getvar::hashTail}}}}','{{hash::{{getvar::hashInput}}}}',
  '{{hash::Synthetic::ignored}}','{{hash::{{hash::Synthetic}}}}',
  '{{#if 0}}{{hash::{{settempvar::x::yes}}}}{{/}}{{gettempvar::x}}',
  '{{#if 1}}{{hash::{{getvar::hashInput}}}}{{/}}','{{#if_pure 1}} {{hash::}} {{/}}',
  '{{#when 0}}{{hash::{{settempvar::x::yes}}}}{{/}}{{gettempvar::x}}',
  '{{#when 1}}{{{{getvar::hashName}}::{{getvar::hashInput}}}}{{/}}',
 ];
 for(const value of ['', 'undefined', 'null', 'NaN', 'Infinity', '0', '-0', '0x10', 'hello', '한글😀', '\ud800', '\udc00', '\ud800x\udc00', '\u0000\uffff', '\uffff'.repeat(4096), 'ab'.repeat(8192)]) {
  hashCases.push('{{hash::'+value+'}}','{{hash:'+value+'}}');
  for(const block of ['#if 0','#if 1','#when 0','#when 1'])hashCases.push('{{'+block+'}}{{hash::'+value+'}}{{/}}');
 }
 examples.push(...hashCases);

 const requiredScalarStart=examples.length;
 const scalarOperations=['round','floor','ceil','abs','remaind','tonumber','fixnum','fixnumber','unicodeencode','unicodedecode','u','ue','unicodedecodefromhex','unicodeencodefromhex'];
 const numericInputs=['',' ','\u0085','\u180e','\ufeff\u00a0\u20281.5\u2029','undefined','null','true','NaN','inf','Infinity','+Infinity','-Infinity','-0','-5e-324','5e-324','1e400','-1e400','1e-400','0.49999999999999994','0.5','-0.5','-0.5000000000000001','-1.5','4503599627370496','9007199254740993','1000000000000000128','1e21','0x20000000000001','0x20000000000003','0x1000000000000080','0x1000000000000081','0x','0b','0o','+0x10','-0x10','0b101','0o777','0x10junk','1e','1e+','1_2','.','+','😀','\ud800'];
 const scalarCases=[];
 for(const op of scalarOperations) {
  scalarCases.push('{{'+op+'}}','{{'+op+'::}}','{{'+op.toUpperCase()+':1.5:2}}',
   '{{{{arrayelement::'+JSON.stringify([op])+'::0}}::1.5::2}}','{{'+op+'{{getvar::sep}}1.5{{getvar::sep}}2}}');
  for(const block of ['#if 0','#if 1','#if_pure 1','#when 0','#when 1'])
   scalarCases.push('{{'+block+'}}{{'+op+'::{{settempvar::x::yes}}1.5::2}}{{/}}{{gettempvar::x}}');
 }
 for(const op of ['round','floor','ceil','abs'])for(const value of numericInputs)scalarCases.push('{{'+op+'::'+value+'}}');
 for(const left of ['','NaN','Infinity','-Infinity','-0','-7','7','0.1','5e-324','0x20000000000003'])
  for(const right of ['','0','-0','3','-3','Infinity','-Infinity','NaN','0.01','5e-324'])
   scalarCases.push('{{remaind::'+left+'::'+right+'}}');
 for(const value of ['','NaN','Infinity','-Infinity','-0','-0.0001','-5e-324','5e-324','0.125','-0.125','1.005','2.55','2.35','1000000000000000128','1e21','-1e21','999999999999999900000'])
  for(const digits of ['','NaN','0','1','2','20','100','100.9','101','-0.9','-1','1.9','Infinity','-Infinity','0x2','0b10'])
   scalarCases.push('{{fixnum::'+value+'::'+digits+'}}');
 for(const value of ['','A','😀','\ud800','\udc00','😀\ud800x\udc00'])
  for(const index of ['','0','1','2','-0.9','-1','1.9','NaN','Infinity','-Infinity','0x1'])
   scalarCases.push('{{unicodeencode::'+value+'::'+index+'}}');
 for(const value of numericInputs.concat(['65535','65536','65537.9','-1.9','55296','56320']))
  scalarCases.push('{{unicodedecode::'+value+'}}');
 for(const value of ['','+','-','0x','+0x41tail','-0x41tail','\ufeff +41z','41.5','0b10','fffg','Infinity','NaN','d800','dc00','1000000000000080','1000000000000081','-1000000000000081','f'.repeat(256),'0'.repeat(1024)+'41','😀','\ud800'])
  for(const op of ['u','ue'])scalarCases.push('{{'+op+'::'+value+'}}');
 for(const value of ['abc-1.2e3 +4','\t\n\r\u00a0\u1680\u2000\u2028\u2029\u202f\u205f\u3000\ufeff','\u0085\u180e','４١😀\ud800x\udc00','....1..2',''])
  scalarCases.push('{{tonumber::'+value+'}}');
 for(const value of ['0x'+'f'.repeat(256),'0x'+'f'.repeat(13)+'8'+'0'.repeat(242),'0x'+'f'.repeat(13)+'c'+'0'.repeat(242),'0x'+'f'.repeat(256)+'z','0b1'+'0'.repeat(1024),'0o1'+'0'.repeat(342),'0x'+'0'.repeat(1024)+'20000000000003'])
  scalarCases.push('{{round::'+value+'}}','{{unicodedecode::'+value+'}}');
 scalarCases.push('{{unicodeencode::{{u::d800}}}}','{{fixnum::{{abs::-1.005}}::2}}','{{unicode_encode::😀::1}}','{{unicode_decode::65}}',
  '{{greater::0x20000000000003::9007199254740994}}','{{arraysplice::[1,2]::0xFFFFFFFFFFFFFFFFFFFFFFFF::0::x}}',
  '{{length::{{tonumber::a 1.2e3}}}}','{{round::{{fixnumber::2.55::1}}}}');
 examples.push(...scalarCases);
 const unsupportedScalarStart=examples.length;
 const unsupportedScalarCases=['{{pow}}','{{pow::2::3}}','{{pow::1::NaN}}','{{pow::-1::Infinity}}','{{pow::{{round::1.5}}::3}}'];
 examples.push(...unsupportedScalarCases);
 const requiredAggregateStart=examples.length;
 const aggregateOperations=['min','max','sum','average'];
 const aggregateInputs=['','[]','[1,2,3]','[null,true,false,-0,2]','["2","bad",{},[],[3],[1,2]]','null','true','false','"2"','42','1§2§bad','[invalid','[1e16,1,-1e16]','[1e16,-1e16,1]','[1e400,-1e400,4]','[-5e-324,0]','[1e21,1e-7]','[["2"],[null],[true],[],[[]],[[3]],{}]',
  '[{"toString":null}]','[{"toString":0}]','[{"toString":"7"}]','[{"valueOf":1,"toString":true}]','[[{"toString":null}]]','[1,{"toString":null}]','[{"valueOf":null}]','[{"valueOf":{"toString":null} }]','[{"__proto__":{"toString":null} }]','[{"x":{"toString":null} }]','{"toString":null} ','[{"to\\u0053tring":false}]','["\\ud800","\\udc00","😀"]','["0x20000000000003","0b10","0o10"]','["0x'+'f'.repeat(256)+'"]'];
 const aggregateCases=[];
 const aggregateDelimiterInputs=[['nested-valueof','[{"valueOf":{"toString":null}}]'],['nested-proto','[{"__proto__":{"toString":null}}]'],['nested-unused','[{"x":{"toString":null}}]'],['nonarray-object','{"toString":null}']];
 const aggregateDelimiterControls=[];
 for(const op of aggregateOperations) {
  aggregateCases.push('{{'+op+'}}','{{'+op.toUpperCase()+':1:2}}','{{'+op.split('').join('_')+'::1::2}}',
   '{{{{arrayelement::'+JSON.stringify([op])+'::0}}::[1,2]}}','{{'+op+'{{getvar::sep}}[1,2]}}');
  for(const input of aggregateInputs)aggregateCases.push('{{'+op+'::'+input+'}}','{{'+op+'::'+input+'::2}}');
  for(const [id,input] of aggregateDelimiterInputs)for(const multiple of [false,true]) {
   const source='{{'+op+'::'+input+(multiple?'::2':'')+'}}';
   aggregateDelimiterControls.push({id:op+'-'+id+(multiple?'-multiple':'-single'),source});
   aggregateCases.push(source);
  }
  for(const block of ['#if 0','#if 1','#if_pure 1','#when 0','#when 1']) {
   aggregateCases.push('{{'+block+'}}{{'+op+'::["{{settempvar::x::yes}}",2]}}{{/}}{{gettempvar::x}}');
   aggregateCases.push('{{'+block+'}}{{'+op+'::[{"toString":null},"{{settempvar::x::yes}}"]}}{{/}}{{gettempvar::x}}');
  }
  aggregateCases.push('{{'+op+'::Infinity::-Infinity::4}}','{{'+op+'::NaN::3}}','{{'+op+'::-0::0}}','{{'+op+'::0::-0}}');
  aggregateCases.push('{{'+op+'::'+ '['.repeat(64)+'1'+']'.repeat(64)+'}}');
  const boundary=Array(8192).fill('1');
  aggregateCases.push('{{'+op+'::['+boundary.join(',')+']}}','{{'+op+'::'+boundary.join('::')+'}}');
 }
 const aboveAggregateBoundary=Array(8193).fill('1');
 for(const op of ['sum','average'])aggregateCases.push('{{'+op+'::['+aboveAggregateBoundary.join(',')+']}}','{{'+op+'::'+aboveAggregateBoundary.join('::')+'}}');
 examples.push(...aggregateCases);


 let seed=739;const rand=()=>((seed=(Math.imul(seed,1664525)+1013904223)>>>0)/4294967296);
 const generated=Array.from({length:2500},()=>Array.from({length:1+Math.floor(rand()*8)},()=>examples[Math.floor(rand()*Math.min(examples.length,180))]).join(' | '));
 const texts=[...examples,...generated];const expected=await page.evaluate(texts=>texts.map(text=>{try{return window.__rustCbsParse(text)}catch(e){return{error:String(e)}}}),texts);
 const rows=[];let equal=0,fallback=0,mismatch=0,foldedEqual=0,foldedFallback=0,foldedMismatch=0;const fallbackReasons={};
 for(let i=0;i<texts.length;i++){
   const r=await native.send({op:'evaluate',text:texts[i].isWellFormed()?texts[i]:toUnits(texts[i]),context:wire});
   const folded=await native.send({op:"evaluate",text:texts[i].isWellFormed()?texts[i]:toUnits(texts[i]),context:wire,fold:true});const actual=fromUnits(r.output);const e=expected[i];if(i>=requiredReplaceStart&&i<requiredReplaceStart+replacementCases.length&&(r.fallback||folded.fallback))throw Error('Replacement admission failed '+i+': '+(r.fallback??folded.fallback));if(i>=requiredArrayStart&&i<requiredArrayStart+arrayCases.length&&(r.fallback||folded.fallback))throw Error('Array admission failed '+i+': '+(r.fallback??folded.fallback));if(i>=requiredHashStart&&i<requiredHashStart+hashCases.length&&(r.fallback||folded.fallback))throw Error('Hash admission failed '+i+': '+(r.fallback??folded.fallback));if(i>=requiredScalarStart&&i<requiredScalarStart+scalarCases.length&&(r.fallback||folded.fallback))throw Error('Scalar admission failed '+i+': '+(r.fallback??folded.fallback));if(i>=unsupportedScalarStart&&i<unsupportedScalarStart+unsupportedScalarCases.length&&(r.fallback!=='unsupported-operator'||folded.fallback!=='unsupported-operator'))throw Error('Pow must remain explicitly unsupported');if(i>=requiredAggregateStart&&i<requiredAggregateStart+aggregateCases.length&&(r.fallback||folded.fallback))throw Error('Aggregate admission failed '+i+': '+(r.fallback??folded.fallback));if(folded.fallback)foldedFallback++;else if(fromUnits(folded.output)===e.output&&folded.draws===e.draws&&!e.error)foldedEqual++;else{foldedMismatch++;rows.push({i,text:texts[i],expected:e,folded})}const ok=actual===e.output&&r.draws===e.draws&&!e.error;
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
 for(const op of ['arrayshift','arraypop','arraypush','arraysplice']) {
  for(const value of ['[]','a§b','[1,true,null,[2],{"x":3}]',String.raw`["\ud800","\udc00"]`,String.raw`["a\u003a\u003ab",["c\u003a\u003ad"]]`]) {
   const source='{{'+op+'::{{getvar::payload}}}}';
   const expected=await page.evaluate(({source,value})=>window.__rustCbsParse(source,1,{payload:value}),{source,value});
   for(const fold of [false,true]) {
    const response=await native.send({op:'evaluate',text:source,context:{...wire,variables:[...wire.variables,[toUnits('payload'),toUnits(value)]]},fold});
    const ok=!response.fallback&&fromUnits(response.output)===expected.output&&response.draws===expected.draws;
    runtimeRows.push({kind:'array-payload',op,fold,ok,fallback:response.fallback});
    if(!ok)throw Error('Runtime array admission/output mismatch '+op+'/'+fold);
   }
  }
 }
 for(const value of ['', 'undefined', '😀\ud800', '\udc00x', '\uffff'.repeat(4096), 'ab'.repeat(8192)]) {
  const source='{{hash::{{getvar::payload}}}}';
  const expected=await page.evaluate(({source,value})=>window.__rustCbsParse(source,1,{payload:value}),{source,value});
  for(const fold of [false,true]) {
   const response=await native.send({op:'evaluate',text:source,context:{...wire,variables:[...wire.variables,[toUnits('payload'),toUnits(value)]]},fold});
   const ok=!response.fallback&&fromUnits(response.output)===expected.output&&response.draws===expected.draws;
   runtimeRows.push({kind:'hash-payload',wordUnits:value.length,fold,ok,fallback:response.fallback});
   if(!ok)throw Error('Runtime hash admission/output mismatch '+fold);
  }
 }
 for(const op of scalarOperations) {
  for(const value of ['','-0','0x1000000000000081','\ufeff1.005','😀\ud800','\udc00']) {
   const source='{{{{getvar::scalarOp}}{{getvar::sep}}{{getvar::payload}}{{getvar::sep}}2}}';
   const patch={scalarOp:op,payload:value};
   const expected=await page.evaluate(({source,patch})=>window.__rustCbsParse(source,1,patch),{source,patch});
   for(const fold of [false,true]) {
    const response=await native.send({op:'evaluate',text:source,context:{...wire,variables:[...wire.variables,...Object.entries(patch).map(([key,value])=>[toUnits(key),toUnits(value)])]},fold});
    const ok=!response.fallback&&fromUnits(response.output)===expected.output&&response.draws===expected.draws;
    runtimeRows.push({kind:'scalar-payload',op,inputUnits:value.length,fold,ok,fallback:response.fallback});
    if(!ok)throw Error('Runtime scalar admission/output mismatch '+op+'/'+fold);
   }
  }
 }
 const aggregateGuardRows=[];
 for(const op of ['min','max'])for(const input of ['['+aboveAggregateBoundary.join(',')+']',aboveAggregateBoundary.join('::')])for(const fold of [false,true]) {
  const response=await native.send({op:'evaluate',text:'{{'+op+'::'+input+'}}',context:wire,fold});
  const ok=response.fallback==='aggregate-spread-limit';
  aggregateGuardRows.push({kind:'spread',op,count:8193,fold,ok,fallback:response.fallback});
  if(!ok)throw Error('Aggregate spread refusal missing '+op+'/'+fold);
 }
 for(const op of aggregateOperations)for(const fold of [false,true]) {
  const response=await native.send({op:'evaluate',text:'{{'+op+'::'+'['.repeat(65)+'1'+']'.repeat(65)+'}}',context:wire,fold});
  const ok=response.fallback==='json-depth';
  aggregateGuardRows.push({kind:'depth',op,fold,ok,fallback:response.fallback});
  if(!ok)throw Error('Aggregate depth refusal missing '+op+'/'+fold);
 }
 for(const op of ['sum','average']) {
  const response=await native.send({op:'evaluate',text:'{{'+op+'::["'+'1'.repeat(8192)+'"]}}',context:wire,budget:16});
  const ok=response.fallback==='budget';
  aggregateGuardRows.push({kind:'work',op,ok,fallback:response.fallback});
  if(!ok)throw Error('Aggregate work refusal missing '+op);
 }
 for(const op of aggregateOperations)for(const value of ['[]','[true,null,["2"]]','1§2§bad','[1e400,-1e400]','[{"toString":null}]','[[{"toString":0}]]','[{"__proto__":{"toString":null}}]','[{"x":{"toString":null}}]'])for(const multiple of [false,true]) {
  const source='{{{{getvar::aggregateOp}}{{getvar::sep}}{{getvar::payload}}'+(multiple?'{{getvar::sep}}2':'')+'}}';
  const patch={aggregateOp:op,payload:value};
  const expected=await page.evaluate(({source,patch})=>window.__rustCbsParse(source,1,patch),{source,patch});
  for(const fold of [false,true]) {
   const response=await native.send({op:'evaluate',text:source,context:{...wire,variables:[...wire.variables,...Object.entries(patch).map(([key,value])=>[toUnits(key),toUnits(value)])]},fold});
   const ok=!response.fallback&&fromUnits(response.output)===expected.output&&response.draws===expected.draws;
   runtimeRows.push({kind:'aggregate-payload',op,multiple,inputUnits:value.length,fold,ok,fallback:response.fallback});
   if(!ok)throw Error('Runtime aggregate admission/output mismatch '+op+'/'+fold);
  }
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
  {id:'replace-generated-next-rule',input:'x',rules:[['x','{{replace::x::x::{{getvar::replacementNext}}}}'],['missing','']]},
  {id:'replace-cross-boundary',input:'{{getvar::to',rules:[['to','{{replace::score::score::score}}}}']]},
  {id:'replace-temporary-reset',input:'x y',rules:[['x','{{replace::x::x::{{settempvar::x::1}}}}'],['y','{{replace::x::x::{{gettempvar::x}}}}']]},
  {id:'hash-generated-next-rule',input:'x',rules:[['x','{{getvar::hashNext}}'],['missing','']]},
  {id:'hash-cross-boundary',input:'{{hash::TOKEN',rules:[['TOKEN','Synthetic}}']]},
  {id:'hash-dynamic-delimiter',input:'x',rules:[['x','{{hash{{getvar::hashTail}}}}']]},
  {id:'hash-temporary-reset',input:'x y',rules:[['x','{{hash::{{settempvar::x::yes}}}}'],['y','{{hash::{{gettempvar::x}}}}']]},
  {id:'array-generated-next-rule',input:'x',rules:[['x','{{arrayelement::{{arraypush::[]::{{getvar::arrayNext}}}}::0}}'],['missing','']]},
  {id:'array-cross-boundary',input:'{{arrayelement::TOKEN',rules:[['TOKEN','{{arraypush::[]::value}}::0}}']]},
  {id:'array-temporary-reset',input:'x y',rules:[['x','{{arraypush::[]::{{settempvar::x::yes}}}}'],['y','{{arraypush::[]::{{gettempvar::x}}}}']]},
 ];
 checkpoints.push(
  {id:'scalar-generated-next-rule',input:'x',rules:[['x','{{getvar::scalarNext}}'],['missing','']]},
  {id:'scalar-cross-boundary',input:'{{fixnum::TOKEN',rules:[['TOKEN','1.005::2}}']]},
  {id:'scalar-temporary-reset',input:'x y',rules:[['x','{{round::{{settempvar::x::1}}}}'],['y','{{tonumber::{{gettempvar::x}}}}']]},
 );
 for(const op of scalarOperations)
  checkpoints.push({id:op+'-scalar-checkpoint',input:'x',rules:[['x','{{'+op+'::1.5::2}}'],['missing','']]});
 for(const op of ['arrayshift','arraypop','arraypush','arraysplice'])
  checkpoints.push({id:op+'-whole-checkpoint',input:'x',rules:[['x','{{arrayelement::{{'+op+'::["{{user}}","{{user}}"]::0::1::{{user}}}}::0}}'],['missing','']]});
 checkpoints.push(
  {id:'aggregate-generated-next-rule',input:'x',rules:[['x','{{getvar::aggregateNext}}'],['missing','']]},
  {id:'aggregate-cross-boundary',input:'{{sum::TOKEN',rules:[['TOKEN','[true,2]}}']]},
  {id:'aggregate-temporary-reset',input:'x y',rules:[['x','{{sum::{{settempvar::x::1}}::2}}'],['y','{{sum::{{gettempvar::x}}::3}}']]},
 );
 for(const op of aggregateOperations)checkpoints.push({id:op+'-aggregate-checkpoint',input:'x',rules:[['x','{{'+op+'::[1,2]}}'],['missing','']]});
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
 r=await native.send({op:'register',id:'failed-state',text:toUnits('{{settempvar::x::1}}{{random::a::b}}{{char}}')});
 r=await native.send({op:'eval',id:'failed-state',context:wire});assert('failed-eval-fallback',r.fallback==='unsupported-operator');
 r=await native.send({op:'evaluate',text:'{{tempvar::x}}{{random::a::b}}',context:wire});
 const isolated=await page.evaluate(()=>window.__rustCbsParse('{{tempvar::x}}{{random::a::b}}'));assert('failed-state-discard',fromUnits(r.output)===isolated.output&&r.draws===isolated.draws);
 for(let i=0;i<62;i++){r=await native.send({op:'register',id:'bounded-'+i,text:'x'});assert('bounded-register-'+i,!r.fallback)}
 r=await native.send({op:'register',id:'overflow',text:'x'});assert('entry-limit',r.fallback==='resident-limit');
 r=await native.send({op:'release'});assert('release',r.programs===0&&r.retainedBytes===0&&r.variables===0);
 r=await native.send({op:'eval',id:'state'});assert('released-program',r.fallback==='missing-program');
 const fixtureReport=await replayFixtures();
 const report={total:texts.length,fixtureReport,equal,fallback,mismatch,foldedEqual,foldedFallback,foldedMismatch,fallbackReasons,effectRows,effectRefusals,effectInterruptions,effectCheckpoints,historyRows,historyRefusals,historyInterruptions,historyCheckpoints,historyInvalidation,programRows,programRefusals,programInterruptions,programCheckpoints,aggregateAdmission:{start:requiredAggregateStart,count:aggregateCases.length},aggregateDelimiterControls,aggregateGuardRows,scalarAdmission:{start:requiredScalarStart,count:scalarCases.length,unsupportedPow:unsupportedScalarCases.length},hashCoreRows,runtimeRows,checkpoints,lifecycle,rows};await fs.writeFile(out,JSON.stringify(report,null,2));console.log(JSON.stringify({...report,lifecycle:lifecycle.length,checkpoints:checkpoints.map(({id,ok,draws})=>({id,ok,draws})),rows:rows.filter(r=>!r.ok&&!r.fallback).slice(0,20)},null,2));if(mismatch||foldedMismatch)throw Error("Differential mismatch");
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
 const fallbackText=Array.from({length:200},()=>'{{getvar::score}}{{equal::{{getvar::score}}::7}}{{random::a::b}}').join('')+'{{char}}';
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
