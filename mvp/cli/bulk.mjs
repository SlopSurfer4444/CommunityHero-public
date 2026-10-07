import { readFile, open, mkdir, rm } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { CliError, UnknownMutationError, checkpointError, localAdmissionPayloadHash, TERMINAL_JOBS,
  EXECUTE_ADMISSION_PROTOCOL, executeAdmissionResult, recoverExecuteAdmission, waitForPoll } from './client.mjs';
import { readCheckpoint, writeCheckpoint } from './workflow.mjs';
import { editorialOutcome, settleEditorialReview } from './editorial.mjs';
import { bindWorkflowGeneration, createReadObserver, nativeProgress } from './read-observer.mjs';
import { connectionFailure, verifyUnpostedIntent } from './conductor-connection.mjs';

const KIND = 'communityhero-reviewed-bulk';
const canonical = value => String(value || '').toLowerCase().replace(/[^a-zа-я0-9]+/giu, '');
const exactId = value => typeof value === 'string' && value.length > 0 && value.length <= 128 && value.trim() === value && !/[\u0000-\u001f\u007f,]/u.test(value);
const refKey = ref => JSON.stringify([ref?.id, ref?.revision]);
const refsOnly = refs => refs.map(({ id, revision }) => ({ id, revision }));
const terminal = new Set(['succeeded', 'failed', 'stale']);
const fail = (message, code = 'INVALID_CHECKPOINT') => { throw new CliError(message, { code }); };

export function reviewedReferences(value) {
  const raw = Array.isArray(value) ? value : value?.proposals;
  if (!Array.isArray(raw) || !raw.length) fail('Bulk requires a nonempty exact reviewed proposals array', 'USAGE');
  const refs = raw.map(ref => {
    if (!exactId(ref?.id) || !Number.isSafeInteger(ref.revision) || ref.revision < 1
      || (ref.itemId !== undefined && !exactId(ref.itemId))) fail('Reviewed references require exact id, positive revision and optional itemId', 'USAGE');
    return { id: ref.id, revision: ref.revision, ...(ref.itemId ? { itemId: ref.itemId } : {}) };
  });
  if (new Set(refs.map(ref => ref.id)).size !== refs.length) fail('Reviewed proposals must be distinct', 'USAGE');
  return refs;
}

export async function readReviewedReferences(path) {
  try { return reviewedReferences(JSON.parse(await readFile(path, 'utf8'))); }
  catch (error) { if (error instanceof CliError) throw error; fail('Cannot read reviewed proposals file', 'USAGE'); }
}

const approvalReferences = slice => slice.approvalReferences ?? slice.references;

function independentEditorialReferences(slice, state) {
  if (!state.partialAdmission || state.continuationPolicy !== 'continue-independent' || !slice.editorialReview?.outcome) return null;
  const outcome = editorialOutcome(slice.editorialReview.outcome, slice.references);
  if (!outcome.held.length) return null;
  const accepted = new Set(outcome.accepted.map(refKey));
  return slice.references.filter(ref => accepted.has(refKey(ref)));
}

function approvalPayload(slice, state) {
  return { proposals: refsOnly(approvalReferences(slice)), ...(state.partialAdmission ? { admissionMode: 'partial' } : {}) };
}

function admitResult(result, slice, state) {
  const invalid = () => { throw new UnknownMutationError('POST', '/api/approvals', { code: 'INVALID_LOCAL_ADMISSION_RESPONSE' }); };
  if (!result || result.requestId !== slice.approvalRequestId) invalid();
  if (!state.partialAdmission) {
    if (!exactId(result.id)) invalid();
    return { ...result, accepted: refsOnly(approvalReferences(slice)), held: [] };
  }
  if (!Array.isArray(result.accepted) || !Array.isArray(result.held)) invalid();
  const expected = new Set(approvalReferences(slice).map(refKey)); const seen = new Set();
  for (const ref of result.accepted) {
    const key = refKey(ref); if (!expected.has(key) || seen.has(key)) invalid(); seen.add(key);
  }
  for (const hold of result.held) {
    const key = refKey(hold?.reference);
    if (!expected.has(key) || seen.has(key) || typeof hold.reason !== 'string' || !hold.reason
      || typeof hold.message !== 'string' || !Number.isInteger(hold.httpStatus)) invalid();
    seen.add(key);
  }
  if (seen.size !== expected.size || (result.accepted.length ? !exactId(result.id) || result.status !== 'approved' : result.id !== null || result.status !== 'held')) invalid();
  return result;
}

export function summarizeBulk(state) {
  const operations = [...new Map(state.slices.flatMap(slice => slice.operations || []).map(op => [op.id, op])).values()];
  return { submitted: state.references.length, accepted: state.slices.reduce((n, slice) => n + (slice.admission?.accepted.length || 0), 0),
    held: state.slices.flatMap(slice => slice.admission?.held || []),
    editorialHeld: state.slices.flatMap(slice => slice.editorialReview?.outcome?.held || []),
    succeeded: operations.filter(op => op.status === 'succeeded').length,
    failed: operations.filter(op => op.status === 'failed').length, stale: operations.filter(op => op.status === 'stale').length,
    unknown: operations.filter(op => op.status === 'unknown').length,
    unresolvedSlices: state.slices.filter(slice => ['unknown', 'needs-reconciliation'].includes(slice.phase)).map(slice => slice.index),
    remaining: state.slices.filter(slice => slice.phase === 'pending').reduce((n, slice) => n + slice.references.length, 0) };
}

function validateState(client, state) {
  if (state.kind !== KIND || canonical(state.account) !== canonical(client.account) || state.baseUrl !== client.baseUrl)
    fail('Bulk checkpoint account/server binding differs', 'WRONG_ACCOUNT');
  const refs = reviewedReferences(state.references);
  if (state.freshEditorial !== undefined && typeof state.freshEditorial !== 'boolean') fail('Bulk editorial policy is invalid');
  if (state.scopeHash !== localAdmissionPayloadHash({ references: refs }) || !Array.isArray(state.slices)
    || !['stop-on-mixed', 'continue-independent'].includes(state.continuationPolicy) || typeof state.partialAdmission !== 'boolean') fail('Bulk checkpoint scope or policy is invalid');
  const flattened = state.slices.flatMap(slice => slice.references || []);
  if (JSON.stringify(flattened) !== JSON.stringify(refs) || state.slices.some((slice, index) => slice.index !== index || !slice.references.length || slice.references.length > 100))
    fail('Bulk checkpoint slices differ from reviewed scope');
  for (const slice of state.slices) {
    if (!['pending', 'editorial-admitting', 'editorial-unposted', 'editorial-reviewing', 'editorial-reviewed', 'editorial-held', 'approval-admitting', 'approved', 'execute-admitting', 'executing', 'complete', 'mixed', 'held', 'unknown', 'needs-reconciliation', 'failed'].includes(slice.phase)) fail('Bulk checkpoint slice phase is invalid');
    if ((slice.phase === 'unknown' || slice.error?.code === 'UNKNOWN_MUTATION_OUTCOME')
      && (slice.connectionUnpostedIntent || slice.editorialReview?.phase === 'unposted'))
      fail('An unconfirmed bulk admission cannot become unposted', 'UNKNOWN_MUTATION_OUTCOME');
    if (slice.connectionUnpostedIntent) {
      const intent = slice.connectionUnpostedIntent;
      if (intent.kind === 'approval' && !slice.admission && slice.phase === 'editorial-reviewed')
        verifyUnpostedIntent(slice, 'approval', slice.approvalRequestId, slice.payloadHash);
      else if (intent.kind === 'execute' && !slice.executeJobId && slice.phase === 'approved')
        verifyUnpostedIntent(slice, 'execute', slice.executeAttemptId, slice.executePayloadHash);
      else fail('Bulk unposted intent differs from its original admission');
    }
    if (slice.editorialReview) {
      if (slice.editorialReview.payloadHash !== localAdmissionPayloadHash({ proposals: refsOnly(slice.references), ...(state.freshEditorial ? { fresh: true } : {}) })
        || JSON.stringify(slice.editorialReview.proposals) !== JSON.stringify(refsOnly(slice.references))) fail('Bulk editorial payload binding differs');
      if (slice.editorialReview.outcome) editorialOutcome(slice.editorialReview.outcome, slice.references);
    }
    if (slice.approvalReferences !== undefined) {
      const derived = independentEditorialReferences(slice, state);
      if (!derived?.length || JSON.stringify(slice.approvalReferences) !== JSON.stringify(derived)
        || slice.approvalScopeHash !== localAdmissionPayloadHash({ references: derived })) fail('Bulk approval subset differs from editorial-accepted scope');
    } else if (slice.approvalScopeHash !== undefined
      || slice.approvalRequestId && independentEditorialReferences(slice, state)?.length) fail('Bulk approval subset is missing');
    if (slice.admission && (!slice.approvalRequestId || !slice.payloadHash)) fail('Bulk approval is missing its admission identity');
    if (slice.approvalRequestId && slice.payloadHash !== localAdmissionPayloadHash(approvalPayload(slice, state))) fail('Bulk admission payload binding differs');
    if (slice.admission) admitResult(slice.admission, slice, state);
    if (slice.executeAttemptId && !slice.admission?.accepted.length) fail('Bulk execution has no accepted scope');
    if (slice.executeAdmissionProtocol !== undefined || slice.executePayloadHash !== undefined) {
      if (slice.executeAdmissionProtocol !== EXECUTE_ADMISSION_PROTOCOL || !exactId(slice.executeAttemptId)
        || slice.executePayloadHash !== localAdmissionPayloadHash({ approvalId: slice.admission?.id })) fail('Bulk execution payload binding differs');
    }
  }
}

async function operationsFor(client, slice) {
  const accepted = new Set(slice.admission.accepted.map(refKey));
  const selected = slice.references.filter(ref => accepted.has(refKey(ref)));
  const review = await client.reviewItems([...new Set(selected.map(ref => ref.itemId))]);
  const expected = new Set(selected.map(ref => ref.id));
  const operations = (review.operations || []).filter(op => op.approvalId === slice.admission.id && expected.has(op.proposalId));
  const counts = new Map(); for (const op of operations) counts.set(op.proposalId, (counts.get(op.proposalId) || 0) + 1);
  const complete = review.coverage?.operationsComplete === true && [...expected].every(id => counts.get(id) === 1)
    && operations.every(op => exactId(op.id)) && new Set(operations.map(op => op.id)).size === operations.length
    && operations.every(op => op.itemId === selected.find(ref => ref.id === op.proposalId)?.itemId);
  return { operations: operations.map(({ id, proposalId, itemId, approvalId, status, providerRetryAllowed }) => ({ id, proposalId, itemId, approvalId, status, ...(providerRetryAllowed !== undefined ? { providerRetryAllowed } : {}) })), complete, progress: nativeProgress(review.progress) };
}

// Finite operator-driven coordinator. It never schedules a future process or retries
// an external execution attempt, even when a failed operation says retry is allowed.
export async function runBulk(client, references = [], options = {}) {
  const { checkpointPath, resumePath, execute = false, partialAdmission = false,
    continuationPolicy = 'stop-on-mixed', batchSize = 100, pollMs = 1000, maxPolls,
    signal, onProgress = () => {} } = options;
  const path = checkpointPath || resumePath;
  if (options.freshEditorial !== undefined && typeof options.freshEditorial !== 'boolean') fail('Invalid bulk editorial policy', 'USAGE');
  if (!path) fail('Bulk requires --checkpoint or --resume', 'USAGE');
  if (!Number.isSafeInteger(batchSize) || batchSize < 1 || batchSize > 100
    || maxPolls !== undefined && (!Number.isSafeInteger(maxPolls) || maxPolls < 1)
    || !Number.isSafeInteger(pollMs) || pollMs < 0 || pollMs > 2_147_483_647 || !['stop-on-mixed', 'continue-independent'].includes(continuationPolicy)) fail('Invalid bulk bounds or continuation policy', 'USAGE');
  if (checkpointPath && resumePath && resolve(checkpointPath) !== resolve(resumePath)) fail('Bulk resume must update its original checkpoint', 'USAGE');
  await mkdir(dirname(resolve(path)), { recursive: true });
  let lock;
  try { lock = await open(`${resolve(path)}.lock`, 'wx', 0o600); }
  catch (error) { if (error.code === 'EEXIST') fail('Bulk checkpoint is locked; inspect the previous process before removing the lock', 'CHECKPOINT_LOCKED'); throw error; }
  try {
    await lock.writeFile(`${JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString(), account: client.account, baseUrl: client.baseUrl })}\n`);
    let state;
    if (resumePath) {
      state = await readCheckpoint(resumePath); validateState(client, state);
      state = await bindWorkflowGeneration(client, state, { resuming: true });
      if (references.length && JSON.stringify(reviewedReferences(references)) !== JSON.stringify(state.references)) fail('Resume cannot replace reviewed scope', 'USAGE');
      if (options.partialAdmission !== undefined && partialAdmission !== state.partialAdmission
        || options.continuationPolicy !== undefined && continuationPolicy !== state.continuationPolicy) fail('Resume cannot replace saved admission/continuation policy', 'USAGE');
      if (options.freshEditorial !== undefined && options.freshEditorial !== (state.freshEditorial ?? false)) fail('Resume cannot replace saved editorial policy', 'USAGE');
    } else {
      try { await readFile(path); fail('Checkpoint already exists; use --resume', 'USAGE'); } catch (error) { if (error.code !== 'ENOENT') throw error; }
      const refs = reviewedReferences(references);
      if (refs.some(ref => !ref.itemId)) {
        const snapshot = await client.bootstrap();
        for (const ref of refs) if (!ref.itemId) {
          const matches = (snapshot.proposals || []).filter(row => row.id === ref.id && row.revision === ref.revision);
          if (matches.length !== 1 || !exactId(matches[0].itemId)) fail(`Exact item binding missing for ${ref.id}; include reviewed itemId`, 'USAGE');
          ref.itemId = matches[0].itemId;
        }
      }
      state = { kind: KIND, account: client.account, baseUrl: client.baseUrl, phase: 'ready', references: refs,
        scopeHash: localAdmissionPayloadHash({ references: refs }), partialAdmission, continuationPolicy,
        ...(options.freshEditorial === true ? { freshEditorial: true } : {}),
        slices: Array.from({ length: Math.ceil(refs.length / batchSize) }, (_, index) => ({ index, references: refs.slice(index * batchSize, (index + 1) * batchSize), phase: 'pending' })) };
      state = await bindWorkflowGeneration(client, state);
      state = await writeCheckpoint(path, state);
    }
    const save = async () => { state = await writeCheckpoint(path, state); };
    const result = () => ({ mode: state.phase, checkpoint: state, summary: summarizeBulk(state) });
    if (state.phase === 'complete' || state.phase === 'complete-with-holds') return result();
    if (!execute) { state.phase = 'execution-required'; state.stopReason = 'explicit-execute-required'; await save(); return result(); }
    state.phase = 'running'; state.stopReason = null; await save();
    const stop = async (reason, error) => { state.phase = 'stopped'; state.stopReason = reason; if (error) state.error = checkpointError(error); await save(); return result(); };
    for (const slice of state.slices) {
      if (signal?.aborted) return stop('operator-stopped');
      // A crash may leave the durable mixed verdict before its derived approval
      // scope was saved. Only explicit partial + independent mode can resume it.
      const resumeEditorialSubset = slice.phase === 'editorial-held' && !slice.approvalRequestId && !slice.admission
        && independentEditorialReferences(slice, state)?.length;
      if (['complete', 'mixed', 'held', 'editorial-held'].includes(slice.phase) && !resumeEditorialSubset) {
        if (slice.phase === 'editorial-held' && state.continuationPolicy !== 'continue-independent') return stop('editorial-review-held');
        if (slice.phase !== 'complete' && state.continuationPolicy !== 'continue-independent') return stop(slice.phase === 'held' ? 'admission-held' : 'operation-failures');
        if (slice.admission?.held.length && state.continuationPolicy !== 'continue-independent'
          && state.slices.some(row => row.index > slice.index && row.phase === 'pending')) return stop('partial-admission-held');
        continue;
      }
      if (slice.phase === 'failed') return stop(slice.stopReason || 'slice-failed');
      try {
        // Previously admitted approvals/executions retain their original recovery path.
        if (!slice.approvalRequestId && !slice.admission) {
          await settleEditorialReview(client, slice.references, slice.editorialReview, { pollMs, maxPolls, signal, onProgress, fresh: state.freshEditorial === true,
            save: async editorialReview => {
              slice.editorialReview = editorialReview;
              slice.phase = editorialReview.phase === 'complete' ? 'editorial-reviewed'
                : editorialReview.phase === 'held' ? 'editorial-held' : `editorial-${editorialReview.phase}`;
              slice.error = null; await save();
            } });
          if (slice.editorialReview.outcome.held.length) {
            const accepted = independentEditorialReferences(slice, state);
            if (!accepted?.length) {
              slice.phase = 'editorial-held'; await save();
              if (state.continuationPolicy === 'continue-independent') continue;
              return stop('editorial-review-held');
            }
            // Preserve the original requested refs and all holds. Persist the
            // exact derived subset before giving it a separate admission key.
            slice.approvalReferences = accepted;
            slice.approvalScopeHash = localAdmissionPayloadHash({ references: accepted });
            slice.phase = 'editorial-reviewed'; await save();
            onProgress({ event: 'bulk.editorial.partition', slice: slice.index,
              accepted: refsOnly(accepted), held: slice.editorialReview.outcome.held });
          }
        }
        if (slice.approvalRequestId && !slice.admission && !slice.connectionUnpostedIntent) {
          const receipt = await client.localAdmission('approval', slice.approvalRequestId);
          if (receipt?.kind !== 'approval' || receipt.requestId !== slice.approvalRequestId || receipt.status !== 'committed'
            || receipt.payloadHash !== slice.payloadHash) throw new UnknownMutationError('POST', '/api/approvals', { code: 'LOCAL_ADMISSION_UNCONFIRMED' });
          slice.admission = admitResult(receipt.result, slice, state); slice.phase = 'approved'; slice.error = null; await save();
        }
        if (!slice.admission) {
          const requestId = slice.connectionUnpostedIntent ? slice.approvalRequestId : randomUUID();
          const payloadHash = localAdmissionPayloadHash(approvalPayload(slice, state));
          verifyUnpostedIntent(slice, 'approval', requestId, payloadHash);
          slice.approvalRequestId = requestId; slice.payloadHash = payloadHash; slice.phase = 'approval-admitting'; await save();
          onProgress({ event: 'bulk.approval.request', slice: slice.index, proposals: refsOnly(approvalReferences(slice)) });
          slice.admission = admitResult(await client.mutate('/api/approvals', { ...approvalPayload(slice, state), requestId: slice.approvalRequestId }), slice, state);
          slice.phase = 'approved'; if (slice.connectionUnpostedIntent) slice.connectionUnpostedIntent = null; await save();
        }
        if (!slice.admission.accepted.length) { slice.phase = 'held'; await save(); if (state.continuationPolicy === 'continue-independent') continue; return stop('admission-held'); }
        if (slice.executeAttemptId && !slice.executeJobId && !slice.connectionUnpostedIntent) {
          if (slice.executeAdmissionProtocol === EXECUTE_ADMISSION_PROTOCOL) {
            const launched = await recoverExecuteAdmission(client, { approvalId: slice.admission.id,
              requestId: slice.executeAttemptId, payloadHash: slice.executePayloadHash });
            slice.executeJobId = launched.jobId; slice.phase = 'executing'; slice.error = null; await save();
          } else {
            // Historical attempt IDs were never sent to the server: they are not receipt keys.
            const snapshot = await client.bootstrap();
            const jobs = (snapshot.jobs || []).filter(job => job.kind === 'execute' && job.refId === slice.admission.id);
            if (jobs.length === 1 && exactId(jobs[0].id)) { slice.executeJobId = jobs[0].id; slice.phase = 'executing'; await save(); }
            else {
              const evidence = await operationsFor(client, slice); Object.assign(slice, evidence);
              slice.phase = 'needs-reconciliation'; slice.stopReason = 'execution-admission-unconfirmed'; await save();
              // Absence from bounded history proves neither dispatch nor safe retry.
              return stop('execution-admission-unconfirmed');
            }
          }
        }
        if (!slice.executeAttemptId || slice.connectionUnpostedIntent) {
          const requestId = slice.connectionUnpostedIntent ? slice.executeAttemptId : randomUUID();
          verifyUnpostedIntent(slice, 'execute', requestId, localAdmissionPayloadHash({ approvalId: slice.admission.id }));
          slice.executeAttemptId = requestId; slice.executeAdmissionProtocol = EXECUTE_ADMISSION_PROTOCOL;
          slice.executePayloadHash = localAdmissionPayloadHash({ approvalId: slice.admission.id });
          slice.phase = 'execute-admitting'; await save();
          onProgress({ event: 'bulk.execute.request', slice: slice.index, approvalId: slice.admission.id, attemptId: slice.executeAttemptId });
          const launched = executeAdmissionResult(await client.execute(slice.admission.id, slice.executeAttemptId), slice.admission.id, slice.executeAttemptId);
          slice.executeJobId = launched.jobId; slice.phase = 'executing'; if (slice.connectionUnpostedIntent) slice.connectionUnpostedIntent = null; await save();
        }
        let settled = false;
        const reads = createReadObserver(client, { signal, pollMs, onObservation: async observation => {
          slice.observation = observation; await save();
        } });
        for (let poll = 0; maxPolls === undefined || poll < maxPolls; poll += 1) {
          if (signal?.aborted) return stop('operator-stopped');
          const job = await reads.getJob(slice.executeJobId, { signal });
          if (!job || job.id !== slice.executeJobId || job.kind !== 'execute' || job.refId !== slice.admission.id) fail('Execution job binding is absent or invalid', 'JOB_NOT_FOUND');
          if (!['queued', 'running', ...TERMINAL_JOBS].includes(String(job.status).toLowerCase())) fail('Execution job status is invalid', 'INVALID_RESPONSE');
          const status = String(job.status).toLowerCase();
          slice.lastJob = { id: job.id, status, kind: job.kind, refId: job.refId }; slice.polls = (slice.polls || 0) + 1;
          if (TERMINAL_JOBS.has(status)) slice.observation = { ...reads.observation, lastDurableOutcomeAt: new Date().toISOString() };
          await save();
          onProgress({ event: 'bulk.job.poll', slice: slice.index, jobId: job.id, status: job.status });
          if (signal?.aborted) return stop('operator-stopped');
          if (TERMINAL_JOBS.has(status)) {
            settled = true;
            const evidence = await operationsFor(reads, slice); Object.assign(slice, evidence);
            const unresolved = !evidence.complete || evidence.operations.some(op => !terminal.has(op.status));
            slice.phase = unresolved ? 'needs-reconciliation' : status !== 'completed' || evidence.operations.some(op => op.status !== 'succeeded') ? 'mixed' : 'complete';
            slice.stopReason = status !== 'completed' ? 'execution-job-failed' : unresolved ? 'operation-outcomes-unresolved' : slice.phase === 'mixed' ? 'operation-failures' : null;
            slice.observation = { ...reads.observation, state: unresolved ? 'unresolved' : 'complete',
              lastDurableOutcomeAt: new Date().toISOString(), coverage: { operationsComplete: evidence.complete } };
            await save();
            if (slice.phase !== 'complete' && (state.continuationPolicy !== 'continue-independent' || unresolved && !evidence.complete)) return stop(slice.stopReason);
            if (slice.admission.held.length && state.continuationPolicy !== 'continue-independent'
              && state.slices.some(row => row.index > slice.index && row.phase === 'pending')) return stop('partial-admission-held');
            break;
          }
          if (maxPolls === undefined || poll + 1 < maxPolls) await waitForPoll(pollMs, signal);
        }
        if (!settled) { slice.phase = 'needs-reconciliation'; slice.stopReason = 'poll-limit'; await save(); return stop('poll-limit'); }
      } catch (error) {
        if (typeof client.connectionRejection === 'function') error = await client.connectionRejection(error);
        const failure = connectionFailure(error);
        if (failure) {
          // This exact private before-call observation is the sole source of
          // an unposted marker. Existing receipt-only/UNKNOWN keys stay intact.
          if (!failure.rejectionError && (slice.phase === 'unknown' || slice.error?.code === 'UNKNOWN_MUTATION_OUTCOME'))
            throw new UnknownMutationError('POST', '/api/approvals', { code: 'LOCAL_ADMISSION_UNCONFIRMED' });
          if (failure.rejectionError) { slice.error = checkpointError(failure.rejectionError); slice.phase = 'execute-admitting'; }
          if (failure.beforeMutationPath === '/api/approvals' && slice.phase === 'approval-admitting') {
            slice.connectionUnpostedIntent = { kind: 'approval', requestId: slice.approvalRequestId, payloadHash: slice.payloadHash, path: failure.beforeMutationPath };
            slice.phase = 'editorial-reviewed';
          } else if (failure.beforeMutationPath === `/api/approvals/${encodeURIComponent(slice.admission?.id)}/execute`
            && slice.phase === 'execute-admitting') {
            slice.connectionUnpostedIntent = { kind: 'execute', requestId: slice.executeAttemptId, payloadHash: slice.executePayloadHash, path: failure.beforeMutationPath };
            slice.phase = 'approved';
          }
          state.connectionDependency = failure.dependency;
          await save(); throw error;
        }
        if (error.code === 'STOPPED' && signal?.aborted && slice.executeJobId) return stop('operator-stopped');
        slice.error = checkpointError(error); slice.phase = error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown'
          : (slice.executeJobId && error.code !== 'REJECTED_LOCAL_ADMISSION')
            || (['POLL_LIMIT', 'NETWORK_ERROR', 'READ_TIMEOUT', 'STOPPED'].includes(error.code) && slice.editorialReview?.jobId && !slice.approvalRequestId) ? 'needs-reconciliation' : 'failed';
        slice.stopReason = error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'mutation-outcome-unknown'
          : slice.editorialReview && !slice.approvalRequestId ? error.code === 'POLL_LIMIT' ? 'editorial-review-poll-limit' : 'editorial-review-failed'
            : error.code === 'JOB_NOT_FOUND' ? 'execution-job-missing' : slice.executeAttemptId ? 'execution-dependency-failed' : 'approval-dependency-failed';
        await save(); return stop(slice.stopReason, error);
      }
    }
    const summary = summarizeBulk(state);
    state.phase = summary.unknown || summary.unresolvedSlices.length || summary.failed || summary.stale ? 'stopped' : summary.held.length || summary.editorialHeld.length ? 'complete-with-holds' : 'complete';
    state.stopReason = summary.unknown || summary.unresolvedSlices.length ? 'operation-outcomes-unresolved' : summary.failed || summary.stale ? 'operation-failures' : null;
    await save(); return result();
  } finally { await lock.close(); await rm(`${resolve(path)}.lock`); }
}
