// Offline by default; --check-heads adds only GET /api/knowledge. No activation path.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
const dir=path.dirname(fileURLToPath(import.meta.url));
const read=n=>JSON.parse(fs.readFileSync(path.join(dir,n),'utf8'));
const hash=s=>crypto.createHash('sha256').update(s).digest('hex');
const c=read('rules.v1.json'),coverage=read('coverage.v1.json'),plan=read('activation-plan.v1.json'),suite=read('scenario-suite.v1.json'),metrics=read('metrics.v1.json');
const all=[...c.rules,...c.contracts,...c.constraints],ids=new Set(all.map(r=>r.id));
assert.equal(ids.size,all.length);assert.equal(c.sources.length,31);assert.equal(c.constraints.length,3);
assert.equal(new Set(all.map(r=>r.text)).size,all.length,'Exact duplicate normalized text');
assert.equal(new Set(plan.rules.map(r=>r.title)).size,plan.rules.length,'Rule titles must be unique');
for(const r of plan.rules)assert(r.title&&r.title===c.rules.find(x=>x.id===r.id)?.title,'Plan title differs from catalog');
assert(!plan.rules.find(r=>r.id==='LA-S02').text.includes('Редакторское руководство'),'Attribution note leaked into active rule text');
assert.equal(c.status,'pending_owner_review');assert.equal(c.trust,'imported_policy');
assert.deepEqual(plan.unresolvedAmbiguityIds,c.ambiguities.filter(a=>a.status==='unresolved').map(a=>a.id));
assert.equal(plan.unresolvedAmbiguityIds.length,0);
assert.equal(plan.ownerDecisions.length,2);
for(const ref of plan.ownerDecisions){const file=ref.id.includes('REACTION')?'owner-decision-reaction.v1.json':'owner-decision-cta.v1.json';assert.equal(hash(fs.readFileSync(path.join(dir,file))),ref.sha256);const decision=read(file);assert.deepEqual(decision.resolvedAmbiguityIds,ref.resolvedAmbiguityIds);assert.equal(decision.changeType,'owner_clarification');assert.equal(decision.trustElevation,false);}
for(const r of all){assert(r.when&&r.action&&r.text&&r.category);assert.equal(r.scope.account,'LikeAvto');assert.equal(r.trust,'imported_policy');assert.equal(r.grantsExecutionAuthority??false,false);assert(r.sourceClauseIds.length);for(const id of r.sourceClauseIds)assert(coverage.clauses.some(c=>c.id===id&&c.targetRuleIds.includes(r.id)));}
for(const source of c.sources){const clauses=coverage.clauses.filter(x=>x.entryId===source.entryId);assert(clauses.length);let end=0;for(const cl of clauses){assert.equal(cl.versionId,source.versionId);assert(cl.startByte>=end&&cl.endByte>cl.startByte);end=cl.endByte;assert(cl.targetRuleIds.every(id=>ids.has(id)));const mapped=plan.coverage.find(x=>x.clauseId===cl.id);assert(mapped);if(source.disposition==='retain')assert.equal(mapped.retainedEntryId,source.entryId);else {assert.deepEqual(mapped.ruleIds??[],cl.targetRuleIds.filter(id=>c.rules.some(r=>r.id===id)));const archived=cl.targetRuleIds.filter(id=>c.contracts.some(r=>r.id===id));if(archived.length){assert(mapped.archivedLegacy);assert(archived.every(id=>mapped.archivedLegacy.sourceContractIds.includes(id)));}assert((mapped.ruleIds?.length??0)>0||mapped.archivedLegacy);}}}
assert.equal(new Set(suite.cases.map(s=>s.id)).size,suite.cases.length);
for(const s of suite.cases){assert(s.expect.length&&s.reject.length);for(const id of [...s.ruleIds,...s.excludedRuleIds??[]])assert(ids.has(id),`${s.id} references unknown ${id}`);}
const uncovered=suite.ruleCoverage.filter(r=>!r.scenarioIds.length).map(r=>r.ruleId);
assert.equal(uncovered.length,0,'Every final atomic rule/contract needs a scenario reference');
assert.equal(plan.sources.filter(s=>s.disposition==='replace').length,28);
assert.equal(plan.sources.filter(s=>s.disposition==='retain').length,3);
assert.equal(plan.rules.length,41);
for(const r of plan.rules){assert(!c.contracts.some(x=>x.id===r.id));for(const ref of r.ownerDecisionIds??[])assert(plan.ownerDecisions.some(d=>d.id===ref));for(const id of r.sourceClauseIds)assert(plan.coverage.find(c=>c.clauseId===id)?.ruleIds?.includes(r.id));}
for(const row of plan.coverage){if(row.archivedLegacy){assert(row.archivedLegacy.reason.length<=2000);assert(row.archivedLegacy.sourceContractIds.length);assert(row.archivedLegacy.evidenceRefs.length);}for(const id of row.ruleIds??[])assert(plan.rules.some(r=>r.id===id&&r.sourceClauseIds.includes(row.clauseId)));}
const constraintExpected={
 'LA-C01':['bawrussia.ru','bawofficial.ru','baw-official.ru','t.me/bawrussia','t.me/baw_support','t.me/bawrussia_sales_bot','бав россия','baw russia'],
 'LA-C02':['https://likeavto.ru/','https://likeavto.ru/calc','https://t.me/likeavto_op_bot','https://max.ru/id753613695767_biz','https://vk.com/likeavto_import'],
 'LA-C03':['спасибо за комментарий']
};
for(const r of c.constraints){assert.deepEqual(r.values,constraintExpected[r.id]);assert.equal(r.disposition,'retain_original_entry');assert(plan.sources.some(s=>s.entryId===r.retainedEntryId&&s.disposition==='retain'));}
// Bounded examples of preserved historical operators, not a replacement runtime validator.
const forbidden=c.constraints.find(r=>r.id==='LA-C01').values;
const badSubstring=s=>forbidden.some(v=>s.toLowerCase().includes(v.toLowerCase()));
assert(badSubstring('Ссылка BAWOfficial.ru/page'));assert(!badSubstring('LikeAvto'));
const urls=c.constraints.find(r=>r.id==='LA-C02').values;
const badUrl=s=>(s.match(/https?:\/\/[^\s<>]+/g)??[]).map(v=>v.replace(/[.,!?;:)\]}]+$/u,'')).some(v=>!urls.includes(v));
assert(!badUrl('Расчёт: https://likeavto.ru/calc).'));assert(badUrl('https://likeavto.ru/calc?x=1'));assert(badUrl('https://likeavto.ru/other'));assert(badUrl('https://LIKEAVTO.ru/'));
const badPrefix=s=>s.trim().toLowerCase().startsWith('спасибо за комментарий');
assert(badPrefix('  СПАСИБО ЗА КОММЕНТАРИЙ!'));assert(!badPrefix('За добрые слова спасибо за комментарий'));
const normalized=[...c.rules,...c.constraints].map(r=>r.text).join('\n\n');assert.equal(normalized.length,metrics.normalizedCandidateText.utf16Units);assert.equal(Buffer.byteLength(normalized),metrics.normalizedCandidateText.utf8Bytes);
let headCheck='not_requested';
if(process.argv.includes('--check-heads')){
 const d=await(await fetch('http://127.0.0.1:4199/api/knowledge',{signal:AbortSignal.timeout(30000)})).json();
 const active=d.entries.filter(e=>e.kind==='rule'&&e.status==='active'&&e.scope?.account==='LikeAvto');assert.equal(active.length,c.sources.length,'Active set changed');
 for(const s of c.sources){const e=active.find(e=>e.id===s.entryId),v=d.versions.find(v=>v.id===s.versionId);assert(e&&v&&e.currentVersionId===s.versionId,'Head changed');assert.equal(v.hash,s.versionHash);assert.equal(v.sourceHash,s.sourceHash);assert.equal(hash(v.text),s.textSha256Utf8);assert.deepEqual(e.scope,s.scope);const bytes=Buffer.from(v.text);let end=0;for(const cl of coverage.clauses.filter(x=>x.entryId===e.id)){assert(!bytes.subarray(end,cl.startByte).toString('utf8').trim(),'Uncovered source text');assert.equal(hash(bytes.subarray(cl.startByte,cl.endByte)),cl.textSha256Utf8);end=cl.endByte;}assert(!bytes.subarray(end).toString('utf8').trim(),'Uncovered tail');}
 headCheck='31 exact current heads and all byte spans verified';
}
console.log(JSON.stringify({ok:true,sourceVersions:c.sources.length,clauses:coverage.clauses.length,newActiveRules:plan.rules.length,retainedTypedConstraints:3,archivedContracts:c.contracts.length,archivedCoverageRows:plan.coverage.filter(c=>c.archivedLegacy).length,scenarioCases:suite.cases.length,scenarioUncoveredRuleIds:uncovered,structuralChecks:'passed',semanticScenarioExecution:'not_run',headCheck,activation:'not_performed',unresolved:plan.unresolvedAmbiguityIds,planSha256:hash(fs.readFileSync(path.join(dir,'activation-plan.v1.json'))),ownerDecisions:plan.ownerDecisions}));
