import {pathToFileURL} from 'node:url';
import path from 'node:path';
import {mkdir,open,unlink} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {ADAPTER_CONTRACT_VERSION,accountDefinition,repo,resolveAdapterPaths,SAFE_ID,validateScope,reject} from './config.mjs';
import {stdinRequest,safeError} from './bridge.mjs';
import {runProcess} from './process.mjs';
import {providerOfficial,messageRole} from './provider-message-role.mjs';

const loadProviderModule = (providerRepo,name) => import(pathToFileURL(path.join(providerRepo,'src',name)).href);
const str = (v,max=20000) => typeof v==='string'?v.slice(0,max):'';
const iso = v => {if(v==null||v==='')return null;const t=typeof v==='number'?v*(v<1e10?1000:1):v;const d=new Date(t);return Number.isFinite(d.getTime())?d.toISOString():null;};
const platformName = p => ({vk:'VK',vkontakte:'VK',instagram:'Instagram',youtube:'YouTube',tiktok:'TikTok'})[p] || p || 'unknown';

// Only explicit UTC calendar timestamps are safe for excluding a queue item.
export function strictUtc(value) {
  if(typeof value!=='string'||!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,3})?Z$/.test(value))return null;
  const parsed=iso(value);if(!parsed)return null;
  return parsed===value.replace(/(?:\.(\d{1,3}))?Z$/,(_,f)=>`.${(f??'').padEnd(3,'0')}Z`)?parsed:null;
}
export function readWindow(window) {
  if(window===undefined)return null;
  if(!window||Array.isArray(window)||Object.keys(window).sort().join(',')!=='since,until'||!strictUtc(window.since)||!strictUtc(window.until)||Date.parse(window.since)>Date.parse(window.until))reject('INVALID_WINDOW');
  return {since:window.since,until:window.until};
}
function stableJson(value) {
  if(value===null||typeof value!=='object')return JSON.stringify(value);
  if(Array.isArray(value))return `[${value.map(stableJson).join(',')}]`;
  return `{${Object.keys(value).sort().map(k=>`${JSON.stringify(k)}:${stableJson(value[k])}`).join(',')}}`;
}
export function readCursor(req,accountObjectIds,account='likeavto') {
  accountDefinition(account);
  const mode=req.mode??'open';if(!['open','closed'].includes(mode))reject('INVALID_READ');
  if(req.reverse!==undefined&&typeof req.reverse!=='boolean')reject('INVALID_READ');
  if(req.pageSize!==undefined&&![10,100].includes(req.pageSize))reject('INVALID_READ');
  const window=readWindow(req.window),binding=req.binding??null,accountScope=[...accountObjectIds].sort();
  const reverse=req.reverse??(window?false:mode==='closed');
  const contract={version:2,mode,window,account,accountScope,binding,reverse,...(req.pageSize!==undefined?{pageSize:req.pageSize}:{})};
  let positions=Object.fromEntries(accountObjectIds.map(id=>[id,''])),priorIncompleteCount=0;
  if(req.cursor){try{
    if(typeof req.cursor!=='string'||req.cursor.length>16000||!/^[A-Za-z0-9_-]+$/.test(req.cursor))throw 0;
    const v=JSON.parse(Buffer.from(req.cursor,'base64url').toString('utf8'));
    if(v.version===2){for(const k of Object.keys(contract))if(stableJson(v[k])!==stableJson(contract[k]))throw 0;if(v.pageSize!==contract.pageSize)throw 0;}
    else if(v.version!==undefined||req.window!==undefined||req.binding!==undefined||req.reverse!==undefined||v.mode!==mode)throw 0;
    if(!v.positions||Array.isArray(v.positions)||Object.keys(v.positions).sort().join(',')!==accountScope.join(',')||Object.values(v.positions).some(x=>x!==null&&(typeof x!=='string'||x.length>2048)))throw 0;
    if(v.incompleteCount!==undefined&&(!Number.isSafeInteger(v.incompleteCount)||v.incompleteCount<0))throw 0;
    priorIncompleteCount=v.incompleteCount??0;positions=v.positions;
  }catch{reject('INVALID_CURSOR');}}
  return {contract,positions,priorIncompleteCount};
}
export function queueCreatedAt(item) {
  const raw=item?.created_at??item?.createdAt??item?.timestamp;
  return typeof raw==='number'&&Number.isFinite(raw)?iso(raw):strictUtc(raw);
}
export function windowDisposition(item,window) {
  if(!window)return 'inside';
  const date=queueCreatedAt(item);
  if(!date)return 'unknown';
  return Date.parse(date)<Date.parse(window.since)||Date.parse(date)>Date.parse(window.until)?'outside':'inside';
}
export function encodeReadCursor(contract,positions,incompleteCount=0) {return Object.values(positions).some(v=>v!==null)?Buffer.from(JSON.stringify({...contract,positions,incompleteCount})).toString('base64url'):null;}

const statuses = new Set(['new','inprogress','closed','deleted']);
const safeReadCode = error => /^[A-Z0-9_]{1,80}$/.test(error?.code??'') ? error.code : 'PROVIDER_UNAVAILABLE';
export function statusTargets(req, accountObjectIds) {
  if(!Array.isArray(req.targets)||req.targets.length>100)reject('INVALID_TARGETS');
  const seen=new Set();
  return req.targets.map(target=>{
    if(!target||!accountObjectIds.includes(target.objectId)||!SAFE_ID.test(target.itemId??'')||typeof target.itemId!=='string')reject('INVALID_TARGET');
    const key=`${target.objectId}:${target.itemId}`;if(seen.has(key))reject('DUPLICATE_TARGET');seen.add(key);
    return {objectId:target.objectId,itemId:target.itemId};
  });
}
function statusRow(item,objectId,observedAt) {
  if(!item||typeof item.id!=='string'||!SAFE_ID.test(item.id)||!statuses.has(item.status))reject('RESPONSE_SCHEMA_ERROR');
  return {objectId,itemId:item.id,status:item.status,observedAt};
}
export async function readStatuses(req,accountObjectIds,reader) {
  const targets=statusTargets(req,accountObjectIds),items=[],errors=[];
  const observedAt=new Date().toISOString();let index=0;
  await Promise.all(Array.from({length:Math.min(3,targets.length)},async()=>{
    while(index<targets.length){const target=targets[index++],at=new Date().toISOString();
      try{const item=await reader(target.objectId).getItem(target.itemId);if(item.id!==target.itemId)reject('TARGET_IDENTITY_MISMATCH');items.push(statusRow(item,target.objectId,at));}
      catch(error){errors.push({...target,code:safeReadCode(error)});}
    }
  }));
  items.sort((a,b)=>`${a.objectId}:${a.itemId}`.localeCompare(`${b.objectId}:${b.itemId}`));
  return {kind:'exact-status-refresh',observedAt,items,errors};
}
export async function readHead(req,accountObjectIds,reader) {
  const rows=new Map(),errors=[],coverage=[];const observedAt=new Date().toISOString();
  // Queue summaries contain authoritative statuses without downloading each
  // conversation. A partial head is never evidence that an absent item closed.
  for(const objectId of accountObjectIds){
    try{
      const at=new Date().toISOString();
      const page=await reader(objectId).listQueue({statuses:['new','inprogress'],limit:100,reverse:false});
      for(const item of page.items){const row=statusRow(item,objectId,at);rows.set(`${objectId}:${row.itemId}`,row);}
      coverage.push({objectId,hasMore:!!page.nextCursor,count:page.count??null,observedAt:at});
      if(page.nextCursor){
        const reverseAt=new Date().toISOString();
        const reverse=await reader(objectId).listQueue({statuses:['new','inprogress'],limit:100,reverse:true});
        for(const item of reverse.items){const row=statusRow(item,objectId,reverseAt);rows.set(`${objectId}:${row.itemId}`,row);}
      }
    }catch(error){errors.push({objectId,code:safeReadCode(error)});}
  }
  return {kind:'open-status-head',observedAt,items:[...rows.values()],errors,coverage,hasMore:errors.length>0||coverage.some(c=>c.hasMore)};
}

export async function collectReadPage(req,accountObjectIds,reader,helpers,binding={accountKey:'likeavto',providerAccountId:'likeavto',displayName:'LikeAvto'}) {
  const {contract,positions,priorIncompleteCount}=readCursor(req,accountObjectIds,binding.accountKey),{mode,window,reverse}=contract;
  const rows=[],skipped=[];let observedCount=0,scannedCount=0,unknownDateCount=0,outsideWindowCount=0;
  const pageDateBounds={min:null,max:null};
  for(const objectId of accountObjectIds){
    if(positions[objectId]===null)continue;
    const r=reader(objectId),page=await r.listQueue({statuses:mode==='closed'?['closed']:['new','inprogress'],limit:contract.pageSize??10,reverse,...(positions[objectId]?{cursor:positions[objectId]}:{})});
    observedCount+=page.count??page.items.length;scannedCount+=page.items.length;
    for(const item of page.items){const date=queueCreatedAt(item);if(date){if(pageDateBounds.min===null||date<pageDateBounds.min)pageDateBounds.min=date;if(pageDateBounds.max===null||date>pageDateBounds.max)pageDateBounds.max=date;}}
    const next=page.nextCursor??null;if(next&&(typeof next!=='string'||next.length>2048||next===positions[objectId]))reject('INVALID_PROVIDER_CURSOR');positions[objectId]=next;
    let n=0;await Promise.all(Array.from({length:Math.min(3,page.items.length)},async()=>{while(n<page.items.length){
      const item=page.items[n++],disposition=windowDisposition(item,window);
      if(disposition==='outside'){outsideWindowCount++;continue;}
      if(disposition==='unknown')unknownDateCount++;
      try{
        const observedAt=new Date().toISOString(),c=await r.getThreadContext(item.id),contextDisposition=windowDisposition(c.item,window);
        if(contextDisposition==='outside'){outsideWindowCount++;continue;}
        if(contextDisposition==='unknown'&&disposition!=='unknown')unknownDateCount++;
        // Queue membership can change before context is read; ingest the newer
        // observed status instead of silently losing a close/reopen transition.
        if(['new','inprogress','closed','deleted'].includes(c.item.status))rows.push(projectContext(c,objectId,helpers,observedAt));
      }catch(e){if(e.code!=='RESPONSE_SCHEMA_ERROR')throw e;skipped.push({objectId,itemId:item.id,code:'RESPONSE_SCHEMA_ERROR'});}
    }}));
  }
  rows.sort((a,b)=>(mode==='closed'?-1:1)*String(a.createdAt??'').localeCompare(String(b.createdAt??''))||String(a.itemId).localeCompare(String(b.itemId)));
  const incompleteCount=priorIncompleteCount+skipped.length+unknownDateCount,cursor=encodeReadCursor(contract,positions,incompleteCount),hasMore=cursor!==null;
  return mapRead(rows,{mode,window,reverse,pageDateBounds,observedCount,scannedCount,unknownDateCount,outsideWindowCount,hasMore,cursor,skipped,coverage:{kind:'bounded-provider-pages',perObjectLimit:contract.pageSize??10,objectCount:accountObjectIds.length,incompleteCount,complete:!hasMore&&incompleteCount===0}},binding);
}

export function validateActions(actions, config) {
  if(!Array.isArray(actions)||!actions.length||actions.length>100)reject('INVALID_ACTIONS');
  const seen=new Set(),targets=new Set();
  return actions.map(a=>{
    if(!a || !['close','delete','hide','reply_and_close'].includes(a.action)||!SAFE_ID.test(a.actionId)||seen.has(a.actionId)||!config.accountObjectIds.includes(a.objectId)||!SAFE_ID.test(a.itemId)||typeof a.conversationKey!=='string'||!a.conversationKey.startsWith(a.objectId+':')||!/^[a-f0-9]{64}$/.test(a.contextEvidenceDigest)||!Array.isArray(a.expectedStatuses)||!a.expectedStatuses.length||a.expectedStatuses.some(s=>!['new','inprogress','closed'].includes(s))||a.action!=='reply_and_close'&&a.expectedStatuses.includes('closed')||!Number.isInteger(a.workTime)||a.workTime<0||a.workTime>86400)reject('INVALID_ACTIONS');
    if(a.action==='reply_and_close'&&(typeof a.reply!=='string'||!a.reply.trim()||a.reply.length>20000))reject('INVALID_REPLY');
    let readbackEvidence;
    if(a.readbackEvidence!==undefined){
      if(a.action!=='reply_and_close'||!a.readbackEvidence||Array.isArray(a.readbackEvidence)||typeof a.readbackEvidence!=='object')reject('INVALID_ACTIONS');
      const keys=Object.keys(a.readbackEvidence);
      if(!keys.length||keys.some(key=>!['expectedReplyId','baselineReplyIds'].includes(key)))reject('INVALID_ACTIONS');
      readbackEvidence={};
      if(a.readbackEvidence.expectedReplyId!==undefined){if(typeof a.readbackEvidence.expectedReplyId!=='string'||!SAFE_ID.test(a.readbackEvidence.expectedReplyId))reject('INVALID_ACTIONS');readbackEvidence.expectedReplyId=a.readbackEvidence.expectedReplyId;}
      if(a.readbackEvidence.baselineReplyIds!==undefined){if(!Array.isArray(a.readbackEvidence.baselineReplyIds)||a.readbackEvidence.baselineReplyIds.length>1000||new Set(a.readbackEvidence.baselineReplyIds).size!==a.readbackEvidence.baselineReplyIds.length||a.readbackEvidence.baselineReplyIds.some(id=>typeof id!=='string'||!SAFE_ID.test(id)))reject('INVALID_ACTIONS');readbackEvidence.baselineReplyIds=[...a.readbackEvidence.baselineReplyIds];}
    }
    if(targets.has(`${a.objectId}:${a.itemId}`))reject('DUPLICATE_ACTION_TARGET');
    seen.add(a.actionId);targets.add(`${a.objectId}:${a.itemId}`);
    return {actionId:a.actionId,objectId:a.objectId,itemId:a.itemId,conversationKey:a.conversationKey,action:a.action,...(a.action==='reply_and_close'?{reply:a.reply,...(readbackEvidence?{readbackEvidence}:{})}:{}),contextEvidenceDigest:a.contextEvidenceDigest,expectedStatuses:a.expectedStatuses,workTime:a.workTime};
  });
}

const psLiteral=value=>`'${String(value).replaceAll("'","''")}'`;
export function mutationResourceKeys(account,actions) {
  accountDefinition(account);
  const keys=new Set();
  for(const action of actions){
    keys.add(`${account}\nitem\n${action.objectId}\n${action.itemId}`);
    if(action.action==='reply_and_close')keys.add(`${account}\nconversation\n${action.conversationKey}`);
  }
  return [...keys].sort();
}
export function mutationGuardOptions(native,override={}) {
  if(typeof native?.lockDirectory!=='string'||!path.isAbsolute(native.lockDirectory))reject('SCOPE_UNAVAILABLE');
  return {lockRoot:path.join(native.lockDirectory,'communityhero-resource-locks'),...override};
}
export async function guardMutation(account,configFile,actions,task,{runProcessFn=runProcess,lockRoot=path.join(repo,'mvp/data/locks')}={}) {
  const folder=lockRoot;await mkdir(folder,{recursive:true});const handles=[];
  try {
    for(const resource of mutationResourceKeys(account,actions)){
      const digest=createHash('sha256').update(resource).digest('hex'),lock=path.join(folder,`${account}-${digest}.execute.lock`);
      try{const handle=await open(lock,'wx');handles.push({handle,lock});await handle.writeFile(JSON.stringify({pid:process.pid,at:new Date().toISOString(),account,resourceDigest:digest}));}
      catch{reject('ACCOUNT_EXECUTION_BUSY');}
    }
    // This rejects known concurrent conveyor processes. It is not a shared lease with the unmodified conveyor.
    const ps=path.join(process.env.SystemRoot || 'C:/Windows','System32/WindowsPowerShell/v1.0/powershell.exe');
    const script=`$account=${psLiteral(account)};$config=${psLiteral(path.resolve(configFile))};$p=@(Get-CimInstance Win32_Process -ErrorAction Stop | Where-Object { $_.Name -match '^(python|pythonw|node)(\\.exe)?$' -and $_.CommandLine -and (($_.CommandLine -match 'commentops_fast' -and $_.CommandLine -match '(?i)\\brun\\b' -and $_.CommandLine -match [regex]::Escape($account)) -or ($_.CommandLine -match 'fast-conveyor-cli\\.ts' -and $_.CommandLine -match [regex]::Escape($config))) }); if($p.Count -gt 0){[Console]::Out.Write('busy')}else{[Console]::Out.Write('clear')}`;
    const {stdout}=await runProcessFn(ps,['-NoProfile','-NonInteractive','-Command',script],{timeoutMs:15000,maxOutputBytes:1024});
    if(stdout.trim()!=='clear')reject('CONCURRENT_CONVEYOR_ACTIVE');
    return await task();
  } finally {
    for(const {handle,lock} of handles.reverse()){await handle.close().catch(()=>{});await unlink(lock).catch(()=>{});}
  }
}

function attachmentState(value) { return Array.isArray(value) ? (value.length ? 'present' : 'none') : 'unknown'; }
function projectedActor(item,authorId=null,normalize=()=>[]) {return {attachments:normalize(item?.attachments),attachmentsState:attachmentState(item?.attachments),authorId,id:str(item?.id,200),author:str(item?.author?.name,160)||'Автор неизвестен',providerOfficial:providerOfficial(item?.official),text:str(item?.text),status:str(item?.status,30),createdAt:iso(item?.created_at??item?.createdAt??item?.timestamp),replyToId:str(item?.reply_to_item_id,200)||null};}

export function projectContext(context,objectId,helpers,observedAt=new Date().toISOString()) {
  const {fastCommentAttachments,fastConveyorPublicSourceUrl,fastConveyorAuthorId,computeThreadContextEvidenceDigest}=helpers;
  const item=context.item,platform=str(item.provider||context.object?.provider||item.object?.provider,40)||'unknown';
  return {itemId:item.id,objectId,observedAt,status:str(item.status,30),platform,createdAt:iso(item.created_at??item.createdAt??item.timestamp),conversationKey:`${objectId}:${context.replyTo?.id??item.id}`,postKey:`${objectId}:${context.parent?.id??item.id}`,commentText:str(item.text),authorId:fastConveyorAuthorId(item)??null,commentAttachmentsPresent:Array.isArray(item.attachments)&&item.attachments.length>0,commentAttachments:fastCommentAttachments(item.attachments),attachmentsState:attachmentState(item.attachments),parentText:str(context.parent?.text),parentTitle:str(context.parent?.title,180),sourceUrl:fastConveyorPublicSourceUrl(context.parent),replyToText:str(context.replyTo?.text),officialReplyTexts:context.officialReplies.map(r=>str(r.text)),officialReplyIds:context.officialReplies.map(r=>str(r.id,200)).filter(Boolean),attachments:fastCommentAttachments(context.parent?.attachments),contextEvidenceDigest:computeThreadContextEvidenceDigest(context),item:projectedActor(item,fastConveyorAuthorId(item)??null,fastCommentAttachments),replyTo:context.replyTo?projectedActor(context.replyTo,fastConveyorAuthorId(context.replyTo)??null,fastCommentAttachments):null,officialReplies:context.officialReplies.map(a=>projectedActor(a,fastConveyorAuthorId(a)??null,fastCommentAttachments))};
}

export function mapRead(rows,meta,binding={accountKey:'likeavto',providerAccountId:'likeavto',displayName:'LikeAvto'}) {
  const definition=accountDefinition(binding.accountKey);
  const account={accountKey:definition.accountKey,providerAccountId:definition.providerAccountId,displayName:definition.displayName,...(binding.primaryObjectId?{primaryObjectId:binding.primaryObjectId}:{}),...(binding.objectIds?{objectIds:[...binding.objectIds]}:{})};
  const posts=new Map(),branches=[],items=[];
  for(const r of rows){const {objectId,itemId}=r,postId=`post-${r.postKey}`,branchId=`branch-${objectId}-${itemId}`,targetId=`comment-${objectId}-${itemId}`;
    if(!posts.has(postId))posts.set(postId,{id:postId,title:r.parentTitle||`Публикация ${definition.displayName}`,excerpt:r.parentText.slice(0,180),text:r.parentText,channel:platformName(r.platform),sourceUrl:r.sourceUrl,postKey:r.postKey,objectId,itemId,attachments:r.attachments,mediaNote:'Содержание видео доступно после обработки медиа.'});
    const messages=[],known=new Set();const add=(a,role,id,parentId=null)=>{if(!a||known.has(id))return;known.add(id);messages.push({id,parentId,author:a.author,authorId:a.authorId??null,role:messageRole(a,role),providerOfficial:providerOfficial(a.providerOfficial),roleEvidence:providerOfficial(a.providerOfficial)?'provider-official':role==='brand'?'provider-official-replies':null,text:a.text,createdAt:a.createdAt,time:a.createdAt?new Intl.DateTimeFormat('ru-RU',{hour:'2-digit',minute:'2-digit',timeZone:'Europe/Moscow'}).format(new Date(a.createdAt)):'',providerItemId:a.id,providerObjectId:objectId,replyToProviderItemId:a.replyToId??null,attachments:id===targetId?r.commentAttachments:(a.attachments??[]),attachmentsState:id===targetId?(r.attachmentsState??'unknown'):(a.attachmentsState??'unknown')});};
    const parent=r.replyTo?.id?`comment-${objectId}-${r.replyTo.id}`:null;if(parent&&r.replyTo.id!==itemId)add(r.replyTo,'participant',parent);add(r.item,'customer',targetId,parent);
    const messageIds=new Map(r.officialReplies.map(a=>[a.id,`official-${objectId}-${a.id}`]));
    if(r.replyTo?.id)messageIds.set(r.replyTo.id,`comment-${objectId}-${r.replyTo.id}`);
    messageIds.set(itemId,targetId);
    for(const a of r.officialReplies)add(a,'brand',`official-${objectId}-${a.id}`,a.replyToId?messageIds.get(a.replyToId)??`comment-${objectId}-${a.replyToId}`:null);
    branches.push({id:branchId,postId,messages,contextComplete:false,unavailableReason:'Provider returns only target, direct parent and official replies; full branch is unavailable.'});
    items.push({id:`item-${objectId}-${itemId}`,itemId,objectId,providerItemId:itemId,providerObjectId:objectId,replyToProviderItemId:r.item.replyToId??null,postId,postKey:r.postKey,conversationKey:r.conversationKey,contextEvidenceDigest:r.contextEvidenceDigest,branchId,targetId,title:r.item.author,author:r.item.author,authorId:r.authorId??null,preview:r.commentText.slice(0,180),text:r.commentText,view:['closed','deleted'].includes(r.status)?r.status:'attention',workflow:['closed','deleted'].includes(r.status)?r.status:'attention',decision:'unknown',reason:'Решение оператора не задано.',contextNote:'API показывает комментарий, родителя и официальные ответы. Полнота ветки не подтверждена.',contextMessageIds:messages.map(m=>m.id),draft:'',suggestions:[],createdAt:r.createdAt,providerStatus:r.status,providerStatusObservedAt:r.observedAt,contextObservedAt:r.observedAt,platform:platformName(r.platform),sourceUrl:r.sourceUrl,attachments:r.commentAttachments,attachmentsState:r.attachmentsState??'unknown'});
  }
  return {items,posts:[...posts.values()],branches,exercises:[],notice:`Живые данные ${definition.displayName}. Полнота ветки не подтверждена.`,accountBinding:account,...meta,live:{account:definition.displayName,accountKey:definition.accountKey,providerAccountId:definition.providerAccountId,accountBinding:account,fetchedAt:new Date().toISOString(),contextComplete:false,readOnly:true,...meta}};
}

export async function readContext(req,accountObjectIds,reader,helpers,binding={accountKey:'likeavto',providerAccountId:'likeavto',displayName:'LikeAvto'}) {
  if(!SAFE_ID.test(req.itemId)||!accountObjectIds.includes(req.objectId))reject('INVALID_TARGET');
  if(req.snapshot!==undefined&&typeof req.snapshot!=='boolean')reject('INVALID_READ');
  const observedAt=new Date().toISOString(),context=await reader(req.objectId).getThreadContext(req.itemId);
  if(context.item?.id!==req.itemId)reject('TARGET_IDENTITY_MISMATCH');
  const projected=projectContext(context,req.objectId,helpers,observedAt);
  return req.snapshot===true?mapRead([projected],{kind:'exact-target-refresh',hasMore:false,cursor:null,coverage:{kind:'exact-target',complete:false},target:{objectId:req.objectId,itemId:req.itemId}},binding):projected;
}

export function resolveMaxInFlight(req,actionCount,env=process.env) {
  const raw=req.maxInFlight??env.COMMUNITYHERO_MAX_IN_FLIGHT??actionCount;
  const value=typeof raw==='string'&&/^\d+$/.test(raw)?Number(raw):raw;
  if(!Number.isSafeInteger(value)||value<1||value>100)reject('INVALID_MAX_IN_FLIGHT');
  return value;
}

function scanRequest(req,account) {
  return {version:1,operation:'scan',account,...(req.statuses!==undefined?{statuses:req.statuses}:{}),...(req.pageSize!==undefined?{pageSize:req.pageSize}:{}),...(req.maxPages!==undefined?{maxPages:req.maxPages}:{}),...(req.maxItems!==undefined?{maxItems:req.maxItems}:{}),...(req.maxElapsedMs!==undefined?{maxElapsedMs:req.maxElapsedMs}:{}),...(req.resume!==undefined?{resume:req.resume}:{})};
}

export async function runProvider(req,options={}) {
  const definition=accountDefinition(req?.account),env=options.env??process.env;
  const paths=options.paths??resolveAdapterPaths(definition.accountKey,{env});
  const executionEnabled=options.executionEnabled??env.COMMUNITYHERO_EXTERNAL_WRITES==='enabled';
  if(req.op==='execute'&&!executionEnabled)reject('EXECUTION_DISABLED');
  const {config,binding}=await validateScope(definition.accountKey,{paths,readFileFn:options.readFileFn,requireExecution:req.op==='execute'});
  // Reject mismatched checkpoints and targets before creating credential-backed readers.
  if(req.op==='read')readCursor(req,config.accountObjectIds,definition.accountKey);
  if(req.op==='status')statusTargets(req,config.accountObjectIds);
  const moduleLoader=options.moduleLoader??(name=>loadProviderModule(paths.providerRepo,name));
  if(req.op==='caps'){
    const [loader,gateway]=await Promise.all([moduleLoader('transport/windows-native-companion-launcher.ts'),moduleLoader('transport/fast-conveyor-gateway.ts')]);
    const native=await loader.loadWindowsNativeCompanionConfig(paths.configFile);
    const providerCaps=await gateway.runFastConveyorRequest(native,{version:1,operation:'capabilities',account:definition.providerAccountId},{});
    return {version:1,operation:'caps',account:definition.accountKey,contractVersion:ADAPTER_CONTRACT_VERSION,binding,local:{operations:['caps','scan','read','context','head','status','execute','readback'],actions:['close','delete','hide','reply_and_close'],runtimeSideEffects:executionEnabled?'explicitly-enabled':'disabled',maxActions:100,maxInFlight:{min:1,max:100}},provider:providerCaps};
  }
  const [loader,credentials,runtime,provider,gateway]=await Promise.all([moduleLoader('transport/windows-native-companion-launcher.ts'),moduleLoader('transport/windows-credential-manager-store.ts'),moduleLoader('transport/credential-runtime.ts'),moduleLoader('provider/read-only-provider.ts'),moduleLoader('transport/fast-conveyor-gateway.ts')]);
  const native=await loader.loadWindowsNativeCompanionConfig(paths.configFile),credentialStore=new credentials.WindowsCredentialManagerStore(),deps={credentialStore};
  const readers=new Map();const reader=objectId=>{if(!config.accountObjectIds.includes(objectId))reject('ACCOUNT_SCOPE_MISMATCH');if(!readers.has(objectId)){const scope={stableAccountKey:definition.accountKey,objectId};const rt=runtime.createCredentialBackedAngrySpaceRuntime({scope,credentialStore,tokenReference:native.tokenReference,oauthClientReference:native.oauthClientReference,lockDirectory:native.lockDirectory,executionMode:{mode:'disabled'}});readers.set(objectId,new provider.AngrySpaceReadOnlyProvider(rt.transport,scope));}return readers.get(objectId);};
  const catalogue=await reader(binding.primaryObjectId).listAuthorizedObjects();if(config.accountObjectIds.some(id=>!catalogue.objectIds.includes(id)))reject('ACCOUNT_SCOPE_MISMATCH');
  const helpers={...gateway,computeThreadContextEvidenceDigest:provider.computeThreadContextEvidenceDigest};
  const bound=result=>({...result,account:definition.accountKey,accountBinding:binding});
  if(req.op==='scan')return bound(await gateway.runFastConveyorRequest(native,scanRequest(req,definition.providerAccountId),deps));
  if(req.op==='head')return bound(await readHead(req,config.accountObjectIds,reader));
  if(req.op==='status')return bound(await readStatuses(req,config.accountObjectIds,reader));
  if(req.op==='context')return bound(await readContext(req,config.accountObjectIds,reader,helpers,binding));
  if(req.op==='execute'||req.op==='readback') {
    const actions=validateActions(req.actions,config),request={version:1,operation:req.op,account:definition.providerAccountId,actions,...(req.op==='execute'?{maxInFlight:resolveMaxInFlight(req,actions.length,env)}:{})};
    const call=()=>gateway.runFastConveyorRequest(native,request,deps);
    return bound(await (req.op==='execute'?guardMutation(definition.accountKey,paths.configFile,actions,call,mutationGuardOptions(native,options.guardOptions)):call()));
  }
  if(req.op!=='read'||!['open','closed'].includes(req.mode??'open'))reject('INVALID_READ');
  return bound(await collectReadPage(req,config.accountObjectIds,reader,helpers,binding));
}

if(process.argv[1]&&import.meta.url===pathToFileURL(path.resolve(process.argv[1])).href){try{process.stdout.write(JSON.stringify({ok:true,result:await runProvider(await stdinRequest())}));}catch(e){process.stdout.write(JSON.stringify(safeError(e)));}}
