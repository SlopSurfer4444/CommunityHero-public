// Synthetic SQLite bootstrap preparation only. Startup uses the production loader.
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir,stat} from 'node:fs/promises';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash,randomUUID} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
const sha=b=>createHash('sha256').update(b).digest('hex');
const selector='wave_execution_fixture_tests::emit_production_bootstrap_fixture';
function required(name){const value=process.env[name];assert.ok(value&&path.isAbsolute(value),`${name}: absolute fixture input required`);return value;}
async function pinned(file,expected){assert.match(expected??'',/^[a-f0-9]{64}$/);const bytes=await readFile(file);assert.equal(sha(bytes),expected,`${file}: pin changed`);return bytes;}
async function native(file){
 const binary=required('COMMUNITYHERO_WAVE_TEST_EXECUTABLE');
 await pinned(binary,process.env.COMMUNITYHERO_WAVE_TEST_SHA256);
 const env=Object.fromEntries(Object.entries(process.env).filter(([k])=>!k.toUpperCase().startsWith('COMMUNITYHERO_')&&!k.toUpperCase().startsWith('PG')&&!['DATABASE_URL','CODEX_HOME','OPENAI_API_KEY','NODE_OPTIONS'].includes(k.toUpperCase())));
 if(file)env.COMMUNITYHERO_WAVE_SYNTHETIC_LEDGER_FILE=file;
 const module=required('COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_CONTROL_MODULE');await pinned(module,process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_CONTROL_SHA256);
 const control=await (await import(pathToFileURL(module).href)).getAcceptanceControl();
 const {exitCode,stdout,stderr}=await control.nativeFixture(binary,[selector,'--exact','--ignored','--nocapture','--test-threads=1'],{env,timeoutMs:30000});
 assert.equal(exitCode,0,stderr);assert.match(stdout,/test result: ok\. 1 passed; 0 failed; 0 ignored;/);await pinned(binary,process.env.COMMUNITYHERO_WAVE_TEST_SHA256);return stdout;
}
function one(stdout,prefix){const lines=stdout.split(/\r?\n/).filter(s=>s.startsWith(prefix));assert.equal(lines.length,1,`One ${prefix} marker required`);return lines[0].slice(prefix.length);}
let seeds;
export async function prepareLifecycleFixture(f,{binary,mvp,output}){
 if(process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_PG==='isolated-candidate'){
  assert.equal(f.account,'baw-russia');const module=required('COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_CONTROL_MODULE');await pinned(module,process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_CONTROL_SHA256);const control=await (await import(pathToFileURL(module).href)).getAcceptanceControl();return control.prepareLifecycleFixture(f,{binary,mvp,output});
 }
 assert.ok(path.isAbsolute(f.data)&&path.relative(output,f.data)&&!path.relative(output,f.data).startsWith('..'),'Only harness-owned data');
 await pinned(required('COMMUNITYHERO_WAVE_TEST_EXECUTABLE'),process.env.COMMUNITYHERO_WAVE_TEST_SHA256);
 const corePath=required('COMMUNITYHERO_WAVE_CORE_PATH'),coreHash=process.env.COMMUNITYHERO_WAVE_CORE_SHA256;
 const core=JSON.parse(await pinned(corePath,coreHash));
 assert.equal(core.kind,'company-independent-immutable-core');assert.equal(core.schemaVersion,1);
 const resolve=p=>path.resolve(path.dirname(corePath),p);
 assert.equal(resolve(core.binary).toLowerCase(),path.resolve(binary).toLowerCase(),'Core must bind actual server');
 await pinned(binary,core.binarySha256);
 assert.equal(resolve(core.runtimeRoot).toLowerCase(),path.resolve(mvp).toLowerCase(),'Fixture bridge and production core must use the same runtime source');
 assert.ok(Array.isArray(core.assets)&&core.assets.length);
 const names=new Set();for(const a of core.assets){assert.match(a.path,/^(adapters|cli|connectors|web)\//);assert.ok(!a.path.split('/').some(s=>!s||s==='.'||s==='..')&&!a.path.includes('\\'));assert.ok(!names.has(a.path.toLowerCase()));names.add(a.path.toLowerCase());await pinned(path.join(mvp,a.path),a.sha256);}
 await mkdir(f.data,{recursive:true});const dbPath=path.join(f.data,'workspace.sqlite');
 let existing=true;try{await stat(dbPath);}catch(e){if(e.code==='ENOENT')existing=false;else throw e;}
 const db=new DatabaseSync(dbPath);let digest,workspace;
 try{
  if(!existing){seeds??=JSON.parse(one(await native(),'WAVE_E2E_BOOTSTRAP_SEEDS='));const seed=seeds.find(s=>s.accountKey===f.account);assert.ok(seed,'Native initialized account seed missing');workspace=seed.workspace;digest=seed.ledgerSha256;
   db.exec('CREATE TABLE workspace(id INTEGER PRIMARY KEY CHECK(id=1),payload TEXT NOT NULL)');db.prepare('INSERT INTO workspace(id,payload) VALUES(1,?)').run(JSON.stringify(workspace));
  }else{const row=db.prepare('SELECT payload FROM workspace WHERE id=1').get();assert.ok(row);workspace=JSON.parse(row.payload);const file=path.join(f.folder,'synthetic-ledger.json');await writeFile(file,row.payload);digest=one(await native(file),'WAVE_E2E_LEDGER_DIGEST=');}
 }finally{db.close();}
 assert.match(digest,/^[a-f0-9]{64}$/);
 f.lifecycleRuntimeId??=`e2e-${randomUUID()}`;
 const startup={kind:existing?'same-owner-recovery':'bootstrap',receiptSha256:sha(JSON.stringify({kind:'isolated-synthetic-bootstrap-intent',fixture:f.folder,account:f.account,binary:core.binarySha256,coreHash,digest})),expectedLedgerSha256:digest};
 const envelope={schemaVersion:1,kind:'root-reviewed-native-runtime-lifecycle-startup',fixedOwner:{account:workspace.account,runtimeId:f.lifecycleRuntimeId,releaseSha256:coreHash},installedCore:{path:corePath,sha256:coreHash},startup,admittedTargets:[]};
 const bytes=Buffer.from(JSON.stringify(envelope));const admission=path.join(f.folder,`lifecycle-admission-${(f.launchNumber??0)+1}.json`);await writeFile(admission,bytes,{flag:'wx'});
 return {COMMUNITYHERO_LIFECYCLE_ADMISSION_PATH:admission,COMMUNITYHERO_LIFECYCLE_ADMISSION_SHA256:sha(bytes)};
}
