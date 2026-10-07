import { randomUUID } from 'node:crypto';
import { CliError, UnknownMutationError, localAdmissionPayloadHash, waitForJob } from './client.mjs';
import { connectionFailure } from './conductor-connection.mjs';

export const EDITORIAL_PATH = '/api/proposals/editorial-review';
const refKey = ref => JSON.stringify([ref?.id, ref?.revision]);
const validId = value => typeof value === 'string' && value.length > 0 && value.length <= 160
  && value.trim() === value && !/[\u0000-\u001f\u007f]/u.test(value);
const invalid = message => { throw new CliError(message, { code: 'INVALID_EDITORIAL_REVIEW' }); };
const unknown = () => new UnknownMutationError('POST', EDITORIAL_PATH, { code: 'INVALID_EDITORIAL_ADMISSION' });
async function retainUnposted(state, save, path, work) {
  try { return await work(); }
  catch (error) {
    if (connectionFailure(error)?.beforeMutationPath === path) await save({ ...state, phase: 'unposted' });
    throw error;
  }
}

export function editorialReferences(references) {
  if (!Array.isArray(references) || !references.length || references.length > 100
    || references.some(ref => !validId(ref?.id) || !Number.isSafeInteger(ref.revision) || ref.revision < 1)
    || new Set(references.map(ref => ref.id)).size !== references.length) invalid('Editorial review requires 1 to 100 distinct exact proposal revisions');
  return references.map(({ id, revision }) => ({ id, revision }));
}

export function editorialOutcome(value, references) {
  if (!value || !Array.isArray(value.accepted) || !Array.isArray(value.reused) || !Array.isArray(value.held)) invalid('Editorial outcome is missing its exact partition');
  const expected = new Set(references.map(refKey)); const seen = new Set();
  for (const ref of value.accepted) {
    const key = refKey(ref);
    if (!expected.has(key) || seen.has(key)) invalid('Editorial accepted reference differs from reviewed scope');
    seen.add(key);
  }
  const accepted = new Set(seen); const reused = new Set();
  for (const ref of value.reused) {
    const key = refKey(ref);
    if (!accepted.has(key) || reused.has(key)) invalid('Editorial reused reference is not a unique accepted reference');
    reused.add(key);
  }
  for (const held of value.held) {
    const key = refKey(held?.reference);
    if (!expected.has(key) || seen.has(key) || !['revise', 'hold'].includes(held?.decision)
      || typeof held.reason !== 'string' || !held.reason.trim()
      || held.suggestedText !== undefined && typeof held.suggestedText !== 'string') invalid('Editorial hold is not bound to an exact reviewed reference');
    seen.add(key);
  }
  if (seen.size !== expected.size) invalid('Editorial outcome omitted a reviewed reference');
  return value;
}

function admission(result, requestId) {
  if (!validId(result?.jobId) || result.requestId !== requestId || typeof result.replayed !== 'boolean') throw unknown();
  return result;
}

// The caller persists each state transition. Existing keys are inspection-only;
// a missing receipt cannot turn a lost model-job response into another POST.
export async function settleEditorialReview(client, references, saved, {
  save = async () => {}, pollMs, maxPolls, signal, fresh = false, onProgress = () => {},
  restartInterruptedFresh = false, maxInterruptedRestarts = 1
} = {}) {
  if (typeof restartInterruptedFresh !== 'boolean' || !Number.isInteger(maxInterruptedRestarts)
    || maxInterruptedRestarts < 0 || maxInterruptedRestarts > 2) invalid('Invalid editorial restart policy');
  const proposals = editorialReferences(references);
  const payloadHash = localAdmissionPayloadHash({ proposals, ...(fresh ? { fresh: true } : {}) });
  let state = saved;
  if (state) {
    if (!/^[A-Za-z0-9_-]{1,160}$/u.test(state.requestId || '') || state.payloadHash !== payloadHash
      || JSON.stringify(state.proposals) !== JSON.stringify(proposals)) invalid('Saved editorial request differs from exact proposal revisions');
    if (state.outcome) { editorialOutcome(state.outcome, proposals); return state; }
  }
  if (!state || state.phase === 'unposted') {
    state = state ? { ...state, phase: 'admitting' } : { phase: 'admitting', requestId: randomUUID(), payloadHash, proposals, ...(fresh ? { fresh: true } : {}) };
    await save(state);
    onProgress({ event: 'editorial.request', requestId: state.requestId, proposals });
    if (fresh && client.editorialReviewSupportsFresh !== true) invalid('Client cannot request a fresh independent review');
    const result = admission(await retainUnposted(state, save, EDITORIAL_PATH,
      () => client.editorialReview(proposals, state.requestId, { fresh })), state.requestId);
    state = { ...state, phase: 'reviewing', jobId: result.jobId }; await save(state);
  } else if (!state.jobId) {
    try {
      const receipt = await client.localAdmission('editorial', state.requestId);
      if (receipt?.kind !== 'editorial' || receipt.requestId !== state.requestId
        || receipt.status !== 'committed' || receipt.payloadHash !== payloadHash) throw unknown();
      const result = admission(receipt.result, state.requestId);
      state = { ...state, phase: 'reviewing', jobId: result.jobId }; await save(state);
    } catch (error) { throw error?.code === 'UNKNOWN_MUTATION_OUTCOME' ? error : unknown(); }
  }
  let result;
  try { result = await waitForJob(client, state.jobId, { pollMs, maxPolls, signal, includeSnapshot: false, onPoll: job => {
    if (job.id !== state.jobId || job.kind !== 'editorial_review' || job.refId !== state.requestId)
      invalid('Editorial job is not bound to the saved request');
    onProgress({ event: 'editorial.poll', requestId: state.requestId, jobId: job.id, status: job.status });
  } }); }
  catch (error) {
    if (!restartInterruptedFresh || !fresh || error.code !== 'JOB_FAILED'
      || !['interrupted', 'failed', 'error', 'cancelled'].includes(error.details?.status)) throw error;
    if (signal?.aborted) throw new CliError('Stopped before fresh editorial readmission', { code: 'STOPPED' });
    const job = error.details;
    if (job.id !== state.jobId || job.kind !== 'editorial_review' || job.refId !== state.requestId
      || job.editorialFresh !== true || JSON.stringify(job.editorialReferences) !== JSON.stringify(proposals))
      invalid('Interrupted editorial job differs from exact fresh request');
    const receipt = await client.localAdmission('editorial', state.requestId);
    if (receipt?.kind !== 'editorial' || receipt.requestId !== state.requestId || receipt.status !== 'committed'
      || receipt.payloadHash !== payloadHash || admission(receipt.result, state.requestId).jobId !== state.jobId) throw unknown();
    const history = state.restartHistory || [];
    if (!Array.isArray(history) || history.length > 2 || history.some(row => row.status !== 'interrupted'
      || row.payloadHash !== payloadHash || JSON.stringify(row.proposals) !== JSON.stringify(proposals)
      || !validId(row.jobId) || !validId(row.requestId))) invalid('Editorial restart lineage changed');
    // A failure/cancellation/denial never becomes another model call. Exhausted
    // interruption is an explicit transport hold, with no accepted refs.
    if (job.status !== 'interrupted' || history.length >= maxInterruptedRestarts
      || job.editorialOutcome != null || job.result != null) {
      const reason = 'Fresh editorial review has no completed judgment; recipient remains held';
      state = { ...state, phase: 'held', recoveryHold: { jobId: job.id, status: job.status, reason },
        outcome: { accepted: [], reused: [], held: proposals.map(reference => ({ reference, decision: 'hold', reason })) } };
      await save(state); return state;
    }
    const itemIds = [...new Set(references.map(ref => ref.itemId))];
    if (itemIds.some(id => !validId(id))) invalid('Fresh editorial recovery requires exact recipient bindings');
    const current = await client.reviewItems(itemIds);
    if (current.coverage?.operationsComplete !== true) invalid('Editorial recovery operation coverage is incomplete');
    for (const ref of references) {
      const rows = (current.proposals || []).filter(row => row.id === ref.id);
      if (rows.length !== 1 || rows[0].revision !== ref.revision || rows[0].status !== 'draft'
        || rows[0].itemId !== ref.itemId || rows[0].kind !== ref.kind || rows[0].text !== ref.text
        || (current.operations || []).some(op => op.itemId === ref.itemId
          && ['pending', 'queued', 'dispatching', 'unknown', 'succeeded'].includes(op.status)))
        invalid('Fresh editorial recovery candidate or original operation changed');
    }
    if (signal?.aborted) throw new CliError('Stopped before fresh editorial readmission', { code: 'STOPPED' });
    if (client.editorialReviewSupportsFresh !== true) invalid('Client cannot request a fresh independent review');
    // No repaired draft or approval changes here. Rust plan_fresh validates the
    // current route/context/rules again; only its new judgment can permit action.
    const next = { phase: 'admitting', requestId: randomUUID(), payloadHash, proposals, fresh: true,
      restartHistory: [...history, { requestId: state.requestId, jobId: state.jobId, payloadHash, proposals, status: 'interrupted' }] };
    await save(next);
    if (signal?.aborted) {
      // This turn knows the new POST never began. Restore the original cursor;
      // a real crash/ACK-loss retains the admitting key for receipt-only reads.
      await save(state);
      throw new CliError('Stopped before fresh editorial readmission', { code: 'STOPPED' });
    }
    const admitted = admission(await retainUnposted(next, save, EDITORIAL_PATH,
      () => client.editorialReview(proposals, next.requestId, { fresh: true })), next.requestId);
    const reviewing = { ...next, phase: 'reviewing', jobId: admitted.jobId }; await save(reviewing);
    // An ACK-loss on this new admission remains receipt-only on later restart.
    return settleEditorialReview(client, references, reviewing, { save, pollMs, maxPolls, signal, fresh,
      onProgress, restartInterruptedFresh, maxInterruptedRestarts });
  }
  const outcome = editorialOutcome(result.job.editorialOutcome ?? result.job.result, proposals);
  state = { ...state, phase: outcome.held.length ? 'held' : 'complete', outcome }; await save(state);
  return state;
}

const digest = value => typeof value === 'string' && /^[a-f0-9]{64}$/u.test(value);
export function repairExpected(held) {
  const value = held?.repairExpected;
  if (held?.decision !== 'revise' || !value || value.proposalId !== held.reference?.id
    || value.proposalRevision !== held.reference?.revision || !validId(value.proposalId)
    || !Number.isSafeInteger(value.proposalRevision) || value.proposalRevision < 1
    || !['textSha256', 'contextDigest', 'rulesDigest', 'receiptSha256'].every(key => digest(value[key])))
    invalid('Repair requires exact canonical revise proof');
  return Object.fromEntries(['proposalId', 'proposalRevision', 'textSha256', 'contextDigest', 'rulesDigest', 'receiptSha256'].map(key => [key, value[key]]));
}

export function repairResult(value, parentReviewJobId, expected, requestId) {
  const oldRefs = expected.map(ref => ({ id: ref.proposalId, revision: ref.proposalRevision }));
  if (!value || value.requestId !== requestId || typeof value.replayed !== 'boolean'
    || value.status !== 'repaired' || !validId(value.repairId) || value.parentReviewJobId !== parentReviewJobId
    || JSON.stringify(value.oldRefs) !== JSON.stringify(oldRefs) || !Array.isArray(value.newRefs)
    || value.newRefs.length !== oldRefs.length || value.newRefs.some((ref, index) => ref?.id !== oldRefs[index].id
      || ref.revision !== oldRefs[index].revision + 1)) invalid('Repair receipt differs from exact revision transition');
  return value;
}

// Persist intent before POST. A saved intent is inspection-only: absence of a
// committed canonical receipt never permits repeating an uncertain mutation.
export async function settleEditorialRepair(client, parentReviewJobId, expected, saved, { save = async () => {}, signal } = {}) {
  if (!validId(parentReviewJobId) || !Array.isArray(expected) || !expected.length || expected.length > 100
    || new Set(expected.map(ref => ref?.proposalId)).size !== expected.length) invalid('Invalid repair scope');
  const payloadHash = localAdmissionPayloadHash({ reviewJobId: parentReviewJobId, expected });
  let state = saved;
  if (state && (state.parentReviewJobId !== parentReviewJobId || state.payloadHash !== payloadHash
    || JSON.stringify(state.expected) !== JSON.stringify(expected))) invalid('Saved repair scope changed');
  if (state?.result) return { ...state, result: repairResult(state.result, parentReviewJobId, expected, state.requestId) };
  if (!state || state.phase === 'unposted') {
    if (signal?.aborted) throw new CliError('Stopped before repair admission', { code: 'STOPPED' });
    state = state ? { ...state, phase: 'admitting' } : { requestId: randomUUID(), parentReviewJobId, expected, payloadHash, phase: 'admitting' };
    await save(state);
    if (signal?.aborted) throw new CliError('Stopped before repair admission', { code: 'STOPPED' });
    const result = repairResult(await retainUnposted(state, save, `/api/editorial-reviews/${encodeURIComponent(parentReviewJobId)}/repairs`,
      () => client.editorialRepair(parentReviewJobId, expected, state.requestId)), parentReviewJobId, expected, state.requestId);
    state = { ...state, phase: 'repaired', result }; await save(state); return state;
  }
  const receipt = await client.localAdmission('editorial-repair', state.requestId);
  if (receipt?.kind !== 'editorial-repair' || receipt.requestId !== state.requestId || receipt.status !== 'committed'
    || receipt.payloadHash !== payloadHash) throw new UnknownMutationError('POST', '/api/editorial-reviews/repairs', { code: 'LOCAL_ADMISSION_UNCONFIRMED' });
  const result = repairResult(receipt.result, parentReviewJobId, expected, state.requestId);
  state = { ...state, phase: 'repaired', result }; await save(state); return state;
}

export async function verifyRepairChain(client, state) {
  let refs = editorialReferences(state.originProposals || state.proposals);
  for (const repair of state.repairs || []) {
    const receipt = await client.localAdmission('editorial-repair', repair.requestId);
    if (receipt?.kind !== 'editorial-repair' || receipt.requestId !== repair.requestId || receipt.status !== 'committed'
      || receipt.payloadHash !== repair.payloadHash || repair.payloadHash !== localAdmissionPayloadHash({ reviewJobId: repair.parentReviewJobId, expected: repair.expected }))
      invalid('Repair chain has no canonical admission proof');
    const result = repairResult(receipt.result, repair.parentReviewJobId, repair.expected, repair.requestId);
    for (let index = 0; index < result.oldRefs.length; index++) {
      const old = result.oldRefs[index]; const position = refs.findIndex(ref => ref.id === old.id && ref.revision === old.revision);
      if (position < 0) invalid('Repair chain retargets an origin revision');
      refs[position] = result.newRefs[index];
    }
  }
  if (JSON.stringify(refs) !== JSON.stringify(editorialReferences(state.proposals))) invalid('Current revisions are not proven by repair chain');
}
