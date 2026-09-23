import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export const ACTIONS = ['reply', 'close', 'needs_attention'];
export const RUNTIME_STATUSES = ['completed', 'source_changed', 'error'];
export const OPERATOR_EVIDENCE = ['not_observed', 'presented_only', 'confirmed_unchanged', 'confirmed_with_changes', 'rejected'];
const hash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const assert = (ok, message) => { if (!ok) throw new Error(message); };
export function validateCases(data) {
  assert(Array.isArray(data?.cases) && data.cases.length > 0, 'cases must be nonempty');
  const ids = new Set();
  for (const c of data.cases) {
    assert(typeof c.id === 'string' && c.id && !ids.has(c.id), 'unique case id required');
    assert(typeof c.postKey === 'string' && c.postKey && typeof c.itemId === 'string' && c.itemId, 'postKey and itemId required');
    if (c.artifactCoverage !== undefined) {
      assert(c.artifactCoverage && ['complete', 'partial', 'unknown'].includes(c.artifactCoverage.status), 'invalid artifactCoverage status');
      assert(Array.isArray(c.artifactCoverage.missing) && c.artifactCoverage.missing.every(x => typeof x === 'string' && x), 'artifactCoverage missing must be string array');
    }
    ids.add(c.id);
  }
  return data.cases;
}
// Account/platform disambiguate identities, but all cases sharing the same post stay together.
export function split(c) { return parseInt(hash([c.account ?? '', c.platform ?? '', c.postKey]).slice(0, 8), 16) % 5 === 0 ? 'control' : 'development'; }
export function caseHash(c) { return hash(c); }
export function outputHash(output) { return hash(output); }
export function template(data) {
  return { schemaVersion: 1, notice: 'Pending analyst cases are not human gold. Reviewer is a supplied identity, not authenticated by this offline tool.', labels: validateCases(data).map(c => ({ caseId: c.id, caseHash: caseHash(c), split: split(c), status: 'pending', reviewer: '', approved: false, allowedActions: [], requiredSourceIds: [], incompleteContext: null, requireAttentionIfIncomplete: false, notes: '' })) };
}
export function validateLabels(data, labels) {
  const cases = new Map(validateCases(data).map(c => [c.id, c]));
  assert(labels?.schemaVersion === 1 && Array.isArray(labels.labels), 'label schemaVersion 1 required');
  const seen = new Set();
  for (const label of labels.labels) {
    const c = cases.get(label.caseId);
    assert(c && !seen.has(c.id), 'unknown or duplicate label case'); seen.add(c.id);
    assert(label.caseHash === caseHash(c) && label.split === split(c), 'case changed or split mismatch');
    assert(['pending', 'approved'].includes(label.status), 'invalid label status');
    assert(Array.isArray(label.allowedActions) && label.allowedActions.every(a => ACTIONS.includes(a)), 'invalid allowedActions');
    assert(Array.isArray(label.requiredSourceIds) && label.requiredSourceIds.every(s => typeof s === 'string' && s), 'invalid requiredSourceIds');
    const known = new Set([...(c.expected?.candidateSourceIds ?? []), ...(c.expected?.relevantRuleIds ?? []), ...(c.sourceIds ?? [])]);
    assert(label.requiredSourceIds.every(s => known.has(s)), 'required source absent from case evidence');
    assert(label.incompleteContext === null || typeof label.incompleteContext === 'boolean', 'invalid incompleteContext');
    assert(typeof label.requireAttentionIfIncomplete === 'boolean', 'invalid incomplete-context policy');
    if (label.status === 'approved') {
      assert(label.approved === true && typeof label.reviewer === 'string' && label.reviewer.trim() && label.allowedActions.length, 'approved labels require explicit approval, reviewer and allowed actions');
      assert(label.incompleteContext !== null, 'approved labels require context assessment');
    } else assert(label.approved === false, 'pending label cannot assert approval');
  }
  return new Map(labels.labels.map(l => [l.caseId, l]));
}
export function evaluate(data, predictions, labels) {
  const cases = validateCases(data), byId = new Map(cases.map(c => [c.id, c]));
  const approved = validateLabels(data, labels);
  assert(predictions?.schemaVersion === 1 && Array.isArray(predictions.outputs), 'prediction schemaVersion 1 required');
  const outputs = new Map();
  for (const p of predictions.outputs) {
    assert(byId.has(p.caseId) && !outputs.has(p.caseId), 'unknown or duplicate output case');
    assert(p.caseHash === caseHash(byId.get(p.caseId)), 'output case hash mismatch');
    assert(RUNTIME_STATUSES.includes(p.runtimeStatus ?? 'completed'), 'invalid runtimeStatus');
    assert(OPERATOR_EVIDENCE.includes(p.operatorEvidence ?? 'not_observed'), 'invalid operatorEvidence');
    outputs.set(p.caseId, p);
  }
  const reviews = new Map();
  assert(labels.outputReviews === undefined || Array.isArray(labels.outputReviews), 'outputReviews must be array');
  for (const review of labels.outputReviews ?? []) {
    const p = outputs.get(review.caseId);
    assert(p && !reviews.has(review.caseId) && review.outputHash === outputHash(p), 'unknown, duplicate or stale output review');
    assert(review.approved === true && typeof review.reviewer === 'string' && review.reviewer.trim(), 'output review requires explicit reviewer approval');
    const dimensions = ['factualAccuracy', 'recipientAppropriate', 'replyNecessary', 'complaintHandled', 'toneAppropriate'];
    assert(review.assessments && dimensions.every(k => ['pass', 'fail', 'not_applicable', 'unreviewed'].includes(review.assessments[k])), 'all subjective dimensions must be explicitly classified');
    reviews.set(review.caseId, review);
  }
  const results = cases.map(c => {
    const p = outputs.get(c.id), label = approved.get(c.id), human = label?.status === 'approved';
    if (!p) return { caseId: c.id, split: split(c), status: 'missing_output', humanLabel: human, contractFailures: [], labelFailures: [] };
    const failures = [], runtimeStatus = p.runtimeStatus ?? 'completed';
    if (p.itemId !== c.itemId) failures.push('wrong_recipient');
    if (runtimeStatus === 'completed' && !ACTIONS.includes(p.action)) failures.push('unsupported_action');
    if (runtimeStatus !== 'completed' && p.action !== undefined && p.action !== null && !ACTIONS.includes(p.action)) failures.push('unsupported_action');
    if (p.action === 'reply' && (typeof p.text !== 'string' || !p.text.trim())) failures.push('empty_reply');
    if (p.executed === true) failures.push('offline_output_claims_execution');
    const sourceIds = p.sourceIds;
    if (!Array.isArray(sourceIds) || sourceIds.some(s => typeof s !== 'string')) failures.push('invalid_source_references');
    const known = new Set([...(c.expected?.candidateSourceIds ?? []), ...(c.expected?.relevantRuleIds ?? []), ...(c.sourceIds ?? [])]);
    if (Array.isArray(sourceIds) && sourceIds.some(s => !known.has(s))) failures.push('unknown_source_reference');
    const labelFailures = [];
    if (human && runtimeStatus === 'completed') {
      if (!label.allowedActions.includes(p.action)) labelFailures.push('action_disagrees_with_review');
      if (label.incompleteContext && label.requireAttentionIfIncomplete && p.action !== 'needs_attention') labelFailures.push('incomplete_context_requires_attention');
      if (label.requiredSourceIds.some(s => !sourceIds?.includes(s))) labelFailures.push('missing_required_source_reference');
    }
    const review = reviews.get(c.id);
    const subjectiveFailures = review ? Object.entries(review.assessments).filter(([, v]) => v === 'fail').map(([k]) => k) : [];
    const subjectiveCompletePass = review ? Object.values(review.assessments).every(value => value === 'pass' || value === 'not_applicable') : false;
    const artifactCoverage = p.artifactCoverage ?? c.artifactCoverage ?? {status:'unknown', missing:[]};
    const actionFailures = [...failures, ...labelFailures];
    const status = actionFailures.length || subjectiveFailures.length ? 'failed'
      : runtimeStatus !== 'completed' ? 'not_evaluable_runtime'
      : artifactCoverage.status === 'partial' ? 'not_evaluable_coverage'
      : human ? 'contract_and_label_checks_passed' : 'unlabelled';
    const operatorEvidence = p.operatorEvidence ?? 'not_observed';
    return { caseId: c.id, split: split(c), humanLabel: human, status, contractFailures: failures, labelFailures,
      subjectiveReview: review ? { reviewer: review.reviewer, reviewerAuthenticated: false, assessments: review.assessments, notes: review.notes ?? '', outputHash: review.outputHash } : null,
      subjectiveFailures,
      dimensions: {
        coverage: {status: artifactCoverage.status, missing: artifactCoverage.missing ?? []},
        runtime: {status: runtimeStatus, errorCode: p.runtimeErrorCode ?? null, retryable: p.runtimeRetryable ?? null},
        actionCorrectness: {status: runtimeStatus !== 'completed' ? 'not_evaluable' : actionFailures.length ? 'failed' : human ? 'reviewed_pass' : 'pending_human_label', failures: actionFailures},
        styleAndOperatorAcceptance: {status: subjectiveFailures.length ? 'failed' : subjectiveCompletePass ? 'reviewed_pass' : 'pending_human_review', operatorEvidence, acceptanceInferred: false}
      },
      origin: Object.fromEntries(['feedbackEventId', 'sourceProposalId', 'prepareRunId', 'prepareBundleId', 'bundleDigest', 'model', 'instructionsHash'].filter(k => typeof p[k] === 'string').map(k => [k, p[k]])) };
  });
  const counts = subset => ({ cases: subset.length, outputs: subset.filter(r => r.status !== 'missing_output').length, approvedLabels: subset.filter(r => r.humanLabel).length, failed: subset.filter(r => r.status === 'failed').length, unlabelled: subset.filter(r => !r.humanLabel).length, passed: subset.filter(r => r.status === 'contract_and_label_checks_passed').length });
  const dimensionCounts = {
    coverage: Object.fromEntries(['complete','partial','unknown'].map(k => [k, results.filter(r => r.dimensions?.coverage.status === k).length])),
    runtime: Object.fromEntries(RUNTIME_STATUSES.map(k => [k, results.filter(r => r.dimensions?.runtime.status === k).length])),
    actionCorrectness: Object.fromEntries(['reviewed_pass','failed','pending_human_label','not_evaluable'].map(k => [k, results.filter(r => r.dimensions?.actionCorrectness.status === k).length])),
    styleAndOperatorAcceptance: Object.fromEntries(['reviewed_pass','failed','pending_human_review'].map(k => [k, results.filter(r => r.dimensions?.styleAndOperatorAcceptance.status === k).length]))
  };
  return { schemaVersion: 1, qualityProven: false, notice: 'Coverage, runtime disposition, action correctness and style/operator acceptance are reported separately. Contract checks and reviewer-labelled action/source checks only. Source presence is not factual entailment; no automated judgement of tone, factual correctness or complaint handling. Presented output is not operator acceptance. Missing labels and outputs are not successes. Reviewer identity is not authenticated.', summary: {...counts(results), dimensions: dimensionCounts}, splits: { development: counts(results.filter(r => r.split === 'development')), control: counts(results.filter(r => r.split === 'control')) }, results };
}

export function main(args) {
  const [command, casesPath, inputPath, labelsPath, reportPath] = args;
  const read = path => JSON.parse(readFileSync(path, 'utf8').replace(/^\uFEFF/, ''));
  if (command === 'template' && casesPath && inputPath) { writeFileSync(inputPath, JSON.stringify(template(read(casesPath)), null, 2) + '\n'); return; }
  if (command === 'validate-labels' && casesPath && inputPath) { const labels = validateLabels(read(casesPath), read(inputPath)); console.log(JSON.stringify({ valid: true, labels: labels.size, approved: [...labels.values()].filter(l => l.status === 'approved').length, reviewerAuthenticated: false })); return; }
  if (command === 'evaluate' && casesPath && inputPath && labelsPath && reportPath) { const report = evaluate(read(casesPath), read(inputPath), read(labelsPath)); writeFileSync(reportPath, JSON.stringify(report, null, 2) + '\n'); console.log(JSON.stringify(report.summary)); if (report.summary.failed || report.summary.outputs !== report.summary.cases) process.exitCode = 1; return; }
  throw new Error('Usage: evaluate.mjs template CASES LABELS | validate-labels CASES LABELS | evaluate CASES OUTPUTS LABELS REPORT');
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) { try { main(process.argv.slice(2)); } catch (e) { console.error(e.message); process.exitCode = 2; } }
