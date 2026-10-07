import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { template, validateLabels, evaluate, split, caseHash, outputHash } from './evaluate.mjs';

const data = { cases: [{ id: 'synthetic-1', itemId: 'item-1', postKey: 'post-1', account: 'test', sourceIds: ['fact-1'] }, { id: 'synthetic-2', itemId: 'item-2', postKey: 'post-1', account: 'test', sourceIds: [] }] };
const predictions = () => ({ schemaVersion: 1, outputs: data.cases.map(c => ({ caseId: c.id, caseHash: caseHash(c), itemId: c.itemId, action: 'reply', text: 'Synthetic response', sourceIds: [], prepareRunId: 'synthetic-run-1', feedbackEventId: 'synthetic-event-1' })) });
const approved = () => { const labels = template(data); labels.labels.forEach(l => Object.assign(l, { status: 'approved', reviewer: 'synthetic-test-reviewer', approved: true, allowedActions: ['reply'], incompleteContext: false })); return labels; };
test('post groups cannot leak between development and control; deterministic', () => {
  assert.equal(split(data.cases[0]), split(data.cases[1]));
  const cases = JSON.parse(readFileSync(new URL('../fixtures/knowledge-real-cases.json', import.meta.url)));
  const groups = new Map();
  for (const c of cases.cases) { const key = `${c.account}:${c.platform}:${c.postKey}`; if (groups.has(key)) assert.equal(groups.get(key), split(c)); groups.set(key, split(c)); }
});
test('ten synthetic replacement cases stay pending; analyst expectations are not gold', () => {
  const data = JSON.parse(readFileSync(new URL('../fixtures/knowledge-real-cases.json', import.meta.url)));
  const labels = template(data);
  assert.equal(labels.labels.length, 10);
  assert(labels.labels.every(l => l.status === 'pending' && l.approved === false));
  assert.equal(validateLabels(data, labels).size, 10);
});
test('positive contract does not turn absent human labels into quality claims', () => {
  const report = evaluate(data, predictions(), template(data));
  assert.equal(report.summary.unlabelled, 2); assert.equal(report.summary.passed, 0);
  assert.equal(report.qualityProven, false);
  assert.equal(report.results[0].origin.feedbackEventId, 'synthetic-event-1');
});
test('explicit reviewed labels check action and required evidence', () => {
  const labels = approved(), p = predictions(); labels.labels[0].requiredSourceIds = ['fact-1']; p.outputs[0].sourceIds = ['fact-1'];
  assert.equal(evaluate(data, p, labels).summary.passed, 2);
  p.outputs[0].sourceIds = [];
  assert.deepEqual(evaluate(data, p, labels).results[0].labelFailures, ['missing_required_source_reference']);
});
test('bad recipient, unsupported action, invented source and execution are rejected', () => {
  const p = predictions(); Object.assign(p.outputs[0], { itemId: 'wrong', action: 'delete', sourceIds: ['invented'], executed: true });
  assert.deepEqual(evaluate(data, p, approved()).results[0].contractFailures, ['wrong_recipient', 'unsupported_action', 'offline_output_claims_execution', 'unknown_source_reference']);
});
test('human assessment of missing context can demand attention; no keyword judge', () => {
  const labels = approved(); Object.assign(labels.labels[0], { incompleteContext: true, requireAttentionIfIncomplete: true, allowedActions: ['needs_attention'] });
  const r = evaluate(data, predictions(), labels).results[0];
  assert.deepEqual(r.labelFailures, ['action_disagrees_with_review', 'incomplete_context_requires_attention']);
});
test('missing output is not pass; duplicate/unknown cases cannot distort denominators', () => {
  const p = predictions(); p.outputs.pop(); assert.equal(evaluate(data, p, approved()).results[1].status, 'missing_output');
  p.outputs.push(p.outputs[0]); assert.throws(() => evaluate(data, p, approved()), /duplicate/);
});
test('labels require explicit approval, reviewer, valid source, immutable case and split', () => {
  for (const patch of [{ reviewer: '' }, { approved: false }, { requiredSourceIds: ['invented'] }, { caseHash: 'changed' }, { split: 'bogus' }, { incompleteContext: null }]) {
    const labels = approved(); Object.assign(labels.labels[0], patch); assert.throws(() => validateLabels(data, labels));
  }
});
test('saved outputs are bound to exact case revision', () => {
  const p = predictions(); p.outputs[0].caseHash = 'old'; assert.throws(() => evaluate(data, p, approved()), /hash/);
});
test('subjective findings require human review of exact output, never inferred from keywords', () => {
  const labels = approved(), p = predictions();
  labels.outputReviews = [{ caseId: 'synthetic-1', outputHash: outputHash(p.outputs[0]), reviewer: 'synthetic-reviewer', approved: true, assessments: { factualAccuracy: 'fail', recipientAppropriate: 'pass', replyNecessary: 'unreviewed', complaintHandled: 'not_applicable', toneAppropriate: 'unreviewed' } }];
  assert.deepEqual(evaluate(data, p, labels).results[0].subjectiveFailures, ['factualAccuracy']);
  p.outputs[0].text = 'changed after review'; assert.throws(() => evaluate(data, p, labels), /stale output review/);
});
test('unreviewed subjective dimensions never become a style or acceptance pass', () => {
  const labels = approved(), p = predictions();
  labels.outputReviews = [{ caseId:'synthetic-1', outputHash:outputHash(p.outputs[0]), reviewer:'synthetic-reviewer', approved:true,
    assessments:{factualAccuracy:'pass',recipientAppropriate:'pass',replyNecessary:'unreviewed',complaintHandled:'pass',toneAppropriate:'pass'} }];
  assert.equal(evaluate(data,p,labels).results[0].dimensions.styleAndOperatorAcceptance.status,'pending_human_review');
  labels.outputReviews[0].assessments.replyNecessary='not_applicable';
  assert.equal(evaluate(data,p,labels).results[0].dimensions.styleAndOperatorAcceptance.status,'reviewed_pass');
});
test('coverage, runtime, action and style acceptance remain separate dimensions', () => {
  const cases = {cases:[{id:'runtime-1',itemId:'item-1',postKey:'post-1',artifactCoverage:{status:'complete',missing:[]}}]};
  const labels = template(cases);
  const outputs = {schemaVersion:1,outputs:[{caseId:'runtime-1',caseHash:caseHash(cases.cases[0]),itemId:'item-1',runtimeStatus:'source_changed',action:null,text:'',sourceIds:[],operatorEvidence:'presented_only'}]};
  const report = evaluate(cases, outputs, labels);
  assert.equal(report.results[0].status, 'not_evaluable_runtime');
  assert.deepEqual(report.results[0].dimensions, {
    coverage:{status:'complete',missing:[]},
    runtime:{status:'source_changed',errorCode:null,retryable:null},
    actionCorrectness:{status:'not_evaluable',failures:[]},
    styleAndOperatorAcceptance:{status:'pending_human_review',operatorEvidence:'presented_only',acceptanceInferred:false}
  });
  assert.equal(report.summary.dimensions.runtime.source_changed, 1);
  assert.equal(report.summary.passed, 0);
});
test('partial artifact coverage cannot be counted as a quality pass', () => {
  const cases = {cases:[{id:'partial-1',itemId:'item-1',postKey:'post-1',artifactCoverage:{status:'partial',missing:['post']}}]};
  const labels = template(cases); Object.assign(labels.labels[0], {status:'approved',reviewer:'reviewer',approved:true,allowedActions:['reply'],incompleteContext:false});
  const outputs = {schemaVersion:1,outputs:[{caseId:'partial-1',caseHash:caseHash(cases.cases[0]),itemId:'item-1',runtimeStatus:'completed',action:'reply',text:'Draft',sourceIds:[]}]};
  const report = evaluate(cases, outputs, labels);
  assert.equal(report.results[0].status, 'not_evaluable_coverage');
  assert.equal(report.summary.passed, 0);
  outputs.outputs[0].operatorEvidence='accepted';
  assert.throws(()=>evaluate(cases,outputs,labels),/operatorEvidence/);
});
