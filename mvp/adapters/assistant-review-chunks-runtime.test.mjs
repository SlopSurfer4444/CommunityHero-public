import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {reviewRuntimeSha256,reviewProfile} from './assistant.mjs';

const root=path.dirname(fileURLToPath(import.meta.url));
const entry=path.join(root,'assistant.mjs');
const absolute=label=>path.resolve(root,label);
const label=file=>path.relative(root,file).replaceAll('\\','/');
const expected=[
  'assistant.mjs','process.mjs','config.mjs','codex-model-policy.mjs','assistant-invocation-budget.mjs',
  'assistant-research.mjs','assistant-fact-followup.mjs','assistant-evidence-quality.mjs',
  'assistant-reply-quality-guidance.mjs','assistant-question-preservation.mjs',
  'assistant-media-analysis-reuse.mjs','assistant-images.mjs','assistant-materials.mjs',
  'assistant-rule-semantics.mjs','assistant-research-repair.mjs','assistant-stage-budget.mjs',
  'assistant-failure-evidence.mjs','assistant-model-visual-projection.mjs','assistant-model-context.mjs',
  'assistant-moderation-context.mjs','assistant-process-events.mjs','assistant-volume-observation.mjs',
  '../cli/trace-recorder.mjs','../cli/trace-contract.mjs','../cli/trace-envelope-v1.schema.json',
].sort();

// Overlay bytes only while hashing; modified source is never imported/executed
// and no canonical source file, private workspace or credential is written/read.
function observe(t,{target,transform,redirect,link}={}){
  const readFile=fs.readFile,lstat=fs.lstat,realpath=fs.realpath;
  const reads=[],checks=[];
  t.mock.method(fs,'readFile',async(file,...args)=>{
    reads.push(label(file));
    assert.ok(expected.includes(label(file)),'hashing opened an unapproved source');
    const bytes=await readFile(file,...args);
    return file===target&&transform?Buffer.from(transform(bytes.toString('utf8'))):bytes;
  });
  t.mock.method(fs,'lstat',async(file,...args)=>{
    checks.push(file);
    const stat=await lstat(file,...args);
    return file===link?{isSymbolicLink:()=>true}:stat;
  });
  t.mock.method(fs,'realpath',async(file,...args)=>file===redirect?absolute('../unapproved-canonical-target.mjs'):realpath(file,...args));
  return {reads,checks};
}

test('review runtime reads exactly the canonical transitive modules and trace schema once',async t=>{
  const {reads}=observe(t);
  const first=await reviewRuntimeSha256();
  assert.match(first,/^[a-f0-9]{64}$/);
  assert.deepEqual([...reads].sort(),expected);
  reads.length=0;
  assert.equal(await reviewRuntimeSha256(),first);
  assert.deepEqual([...reads].sort(),expected);
  const profile=await reviewProfile();
  assert.equal(profile.runtimeSha256,first);
});

for(const dependency of ['../cli/trace-recorder.mjs','../cli/trace-contract.mjs','../cli/trace-envelope-v1.schema.json','assistant-materials.mjs','assistant-invocation-budget.mjs'])
test(`review runtime digest changes when ${dependency} bytes change`,async t=>{
  const before=await reviewRuntimeSha256();
  observe(t,{target:absolute(dependency),transform:source=>source+'\n'});
  assert.notEqual(await reviewRuntimeSha256(),before);
});

for(const candidate of [
  absolute('../cli/trace-recorder.mjs'),absolute('../cli/trace-envelope-v1.schema.json'),
  absolute('../cli/unapproved-helper.mjs'),absolute('./unapproved-helper.mjs'),
  absolute('../../.env'),absolute('./assistant.mjs:private'),
  path.join(os.tmpdir(),'unapproved-user-workspace.mjs'),null,
])test(`foreign review entry rejected before filesystem access: ${candidate===null?'null':path.basename(candidate)}`,async t=>{
  const observed=observe(t);
  await assert.rejects(reviewRuntimeSha256(candidate),{code:'ASSISTANT_UNAVAILABLE'});
  assert.deepEqual(observed.reads,[]);assert.deepEqual(observed.checks,[]);
});

for(const statement of [
  "import '../cli/unapproved-helper.mjs';",
  "export {value} from '../cli/unapproved-helper.mjs';",
  "import'../cli/unapproved-helper.mjs';",
  "export*from'../cli/unapproved-helper.mjs';",
  "import/* gap */'../cli/unapproved-helper.mjs';",
  "export{value}from/* gap */'../cli/unapproved-helper.mjs';",
  "import '../../.env';",
  "import './config.json';",
  "import './unapproved-helper.mjs';",
  "import './assistant.mjs:private';",
  "import './assistant.mjs?alias';",
  "import 'file:///C:/unapproved-workspace.mjs';",
  "import 'unapproved-package';",
  "import 'fs';",
  "import './assistant.mjs/../assistant.mjs';",
  "import('./assistant-materials.mjs');",
  "import/* gap */('./assistant-materials.mjs');",
  "import // gap\n('./assistant-materials.mjs');",
  "import(`./assistant-materials.mjs`);",
])test(`review closure refuses unpinned or dynamic dependency: ${statement}`,async t=>{
  const observed=observe(t,{target:entry,transform:source=>statement+'\n'+source});
  await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
  assert.deepEqual(observed.reads,['assistant.mjs']);
  assert.deepEqual(observed.checks,[path.dirname(root),root,entry]);
});

test('an unapproved transitive trace import is rejected without reading its target',async t=>{
  const observed=observe(t,{target:absolute('../cli/trace-recorder.mjs'),
    transform:source=>"import './unapproved-helper.mjs';\n"+source});
  await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
  assert.ok(observed.reads.includes('../cli/trace-recorder.mjs'));
  assert.ok(!observed.checks.includes(absolute('../cli/unapproved-helper.mjs')));
});

const clauseComments=[
  '/* ; */',
  `/* " ' ; from './assistant-materials.mjs' */`,
  `/* } from 'node:fs'; import { */`,
  `// ; " ' from './assistant-materials.mjs'\n`,
  `// ; from 'node:fs'\u2028`,
];
for(const keyword of ['import','export'])for(const comment of clauseComments){
  const clause=`${keyword} {value ${comment}} from `;
  test(`named clause comment cannot hide a foreign dependency: ${clause}`,async t=>{
    const observed=observe(t,{target:entry,transform:source=>clause+"'./unapproved-helper.mjs';\n"+source});
    await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
    assert.deepEqual(observed.reads,['assistant.mjs']);
    assert.deepEqual(observed.checks,[path.dirname(root),root,entry]);
  });
  test(`named clause comment preserves the real allowed dependency: ${clause}`,async t=>{
    const observed=observe(t,{target:entry,transform:()=>clause+"'../cli/trace-recorder.mjs';"});
    assert.match(await reviewRuntimeSha256(),/^[a-f0-9]{64}$/);
    assert.deepEqual(observed.reads,['assistant.mjs','../cli/trace-recorder.mjs',
      '../cli/trace-contract.mjs','../cli/trace-envelope-v1.schema.json']);
  });
}

for(const statement of [
  `import {"name ; from" as value /* ; ' */} from './unapproved-helper.mjs';`,
  `export {value as "name ; from" /* ; ' */} from './unapproved-helper.mjs';`,
  `import value, {other /* ; */} from './unapproved-helper.mjs';`,
  `import value, * /* ; */ as ns from './unapproved-helper.mjs';`,
  `export * /* ; */ as ns from './unapproved-helper.mjs';`,
  String.raw`import \u0076alue, {other} from './unapproved-helper.mjs';`,
])test(`static declaration tokens keep a foreign module visible: ${statement}`,async t=>{
  const observed=observe(t,{target:entry,transform:source=>statement+'\n'+source});
  await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
  assert.deepEqual(observed.reads,['assistant.mjs']);
  assert.deepEqual(observed.checks,[path.dirname(root),root,entry]);
});

test('named-clause comments cannot hide an unapproved transitive trace dependency',async t=>{
  const observed=observe(t,{target:absolute('../cli/trace-recorder.mjs'),
    transform:source=>"export {value /* ; from './trace-contract.mjs' */} from './unapproved-helper.mjs';\n"+source});
  await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
  assert.ok(observed.reads.includes('../cli/trace-recorder.mjs'));
  assert.ok(!observed.checks.includes(absolute('../cli/unapproved-helper.mjs')));
});

for(const statement of [
  "import source wasm from './unapproved.wasm';",
  "import source /* ; */ wasm from './unapproved.wasm';",
  "import /* ; */ source /* from 'node:fs' */ wasm /* ; */ from './unapproved.wasm';",
  "import source wasm from './assistant-materials.mjs';",
  "import source from from './unapproved.wasm';",
  "import.source('./unapproved.wasm');",
  "import /* ; */ . /* ; */ source /* ; */ ('./unapproved.wasm');",
  "import.source(`./unapproved.wasm`);",
  "import.source(modulePath);",
  "import // ;\u2028. source // ;\u2028('./unapproved.wasm');",
])test(`unsupported source-phase module form is rejected before any dependency read: ${statement}`,async t=>{
  const observed=observe(t,{target:entry,transform:source=>statement+'\n'+source});
  await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
  assert.deepEqual(observed.reads,['assistant.mjs']);
  assert.deepEqual(observed.checks,[path.dirname(root),root,entry]);
});

for(const statement of [
  "import source from 'node:fs';",
  "import /* ; */ source /* ; */ from 'node:fs';",
  "import {source as value} from 'node:fs';",
])test(`a normal imported binding named source remains a static module: ${statement}`,async t=>{
  const observed=observe(t,{target:entry,transform:()=>statement});
  assert.match(await reviewRuntimeSha256(),/^[a-f0-9]{64}$/);
  assert.deepEqual(observed.reads,['assistant.mjs']);
});

test('source-phase dependencies are also refused in a transitive trace module',async t=>{
  const observed=observe(t,{target:absolute('../cli/trace-recorder.mjs'),
    transform:source=>"import source wasm from './unapproved.wasm';\n"+source});
  await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
  assert.ok(observed.reads.includes('../cli/trace-recorder.mjs'));
  assert.ok(!observed.checks.includes(absolute('../cli/unapproved.wasm')));
});

for(const linked of [path.dirname(root),root,entry,absolute('../cli'),absolute('../cli/trace-recorder.mjs'),absolute('../cli/trace-envelope-v1.schema.json')])
test(`linked source or source directory is rejected: ${label(linked)}`,async t=>{
  const observed=observe(t,{link:linked});
  await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
  assert.ok(!observed.reads.includes(label(linked)));
});

for(const redirected of [entry,absolute('../cli'),absolute('../cli/trace-envelope-v1.schema.json')])
test(`canonical redirect is rejected before bytes are read: ${label(redirected)}`,async t=>{
  const observed=observe(t,{redirect:redirected});
  await assert.rejects(reviewRuntimeSha256(),{code:'ASSISTANT_UNAVAILABLE'});
  assert.ok(!observed.reads.includes(label(redirected)));
});
