// Only the Rust parent may supply configuration. This process reuses the
// existing workflow; it never calls a provider or maintains an effect ledger.
import { pathToFileURL } from 'node:url';
import { isAbsolute, relative, resolve, sep } from 'node:path';
import { ConductorClient, validateConfig } from './conductor-client.mjs';
import { createMediaDependencies } from './conductor-media.mjs';
import { createFactDependencies } from './conductor-facts.mjs';
import { CliError, waitForPoll } from './client.mjs';
import { runQueue, summarizeQueue } from './queue.mjs';
import { PREPARE_REVIEW_ONLY, READY_FOR_OWNER_APPROVAL } from './read-observer.mjs';
import { readCheckpoint } from './workflow.mjs';
import { connectionFailure, hasUnconfirmedAdmission, validateConnectionDependency } from './conductor-connection.mjs';

// Public progress is a bounded explanation; full evidence remains in the
// original workflow checkpoint. Bound bytes, including non-ASCII explanations.
const safeHold = (itemId, reason, stage = 'campaign') => ({ itemId,
  reason: Buffer.from(String(reason || 'Recipient requires review')).subarray(0, 1024).toString('utf8'), stage });

// Queue summaries may predate a child's final atomic save. Inspect the actual
// owned journals before attributing a dependency disposition to an admission.
export async function connectionCheckpointTree(rootPath) {
  const root = resolve(rootPath); const visited = new Set(); const nodes = [];
  const visit = async (path, depth, pending = false) => {
    path = resolve(path);
    if (depth > 5 || visited.size >= 10000 || path !== root && ![`${root}.slices${sep}`, `${root}.ready${sep}`].some(prefix => path.startsWith(prefix)))
      throw new CliError('Connection checkpoint is outside its original owned tree', { code: 'INVALID_CHECKPOINT' });
    if (visited.has(path)) return; visited.add(path);
    let value;
    try { value = await readCheckpoint(path); }
    catch (error) { if (pending && error.code === 'INVALID_CHECKPOINT' && error.details?.cause === 'ENOENT') return; throw error; }
    const slices = value.slices || [];
    const current = value.currentSlice ? [value.currentSlice] : [];
    const pendingSlices = value.pendingSlices || [];
    if (current.some(slice => !slice.childPath)) throw new CliError('Original current child journal is missing', { code: 'INVALID_CHECKPOINT' });
    nodes.push({ ...value, slices: slices.map(row => row.childPath ? { ...row, child: undefined, error: undefined } : row),
      pendingSlices: pendingSlices.map(row => row.childPath ? { ...row, child: undefined, error: undefined } : row),
      currentSlice: undefined, readyBatches: (value.readyBatches || []).map(row => ({ ...row, child: undefined })) });
    for (const slice of [...current, ...slices, ...pendingSlices]) if (slice.childPath) {
      const local = relative(root, resolve(slice.childPath));
      if (isAbsolute(local)) throw new CliError('Foreign checkpoint drive', { code: 'INVALID_CHECKPOINT' });
      await visit(slice.childPath, depth + 1, pendingSlices.includes(slice) && !slice.preparationReady);
    }
    for (const batch of value.readyBatches || []) {
      if (!/^[a-f0-9]{64}$/u.test(batch.id || '')) throw new CliError('Ready journal identity changed', { code: 'INVALID_CHECKPOINT' });
      await visit(resolve(`${path}.ready`, `${batch.id}.json`), depth + 1, batch.mode === 'pending');
    }
  };
  await visit(root, 0); return nodes;
}

export async function emitCampaignResult(output, emit) {
  await emit({ type: 'report', report: { event: 'holds-reset' } });
  for (let offset = 0; offset < output.itemHolds.length; offset += 100)
    await emit({ type: 'report', report: { event: 'holds-page', itemHolds: output.itemHolds.slice(offset, offset + 100) } });
  // Each page was durably accepted before this disposition. Rust reconstructs
  // the final partition from canonical operations and those scoped hold rows.
  await emit({ type: 'report', report: { event: 'holds-complete', mode: output.mode, summary: output.summary } });
  await emit({ type: 'result', result: { mode: output.mode, summary: output.summary } });
}

export function campaignHolds(checkpoint, scopeItemIds) {
  const holds = new Map();
  for (const row of checkpoint.scopeHolds || []) if (scopeItemIds.includes(row.itemId)) holds.set(row.itemId, safeHold(row.itemId, row.reason));
  for (const slice of checkpoint.slices || []) {
    if (slice.dependencyResolved) continue;
    const resolved = new Set(slice.dependencyResolvedItemIds || []);
    if (slice.planHoldReason) for (const itemId of slice.itemIds || []) if (!resolved.has(itemId)) holds.set(itemId, safeHold(itemId, slice.planHoldReason, 'preparation'));
    const child = slice.child || {};
    if (slice.status === 'editorial-held' && child.kind === 'communityhero-reviewed-bulk') {
      const sent = new Set((child.slices || []).flatMap(row => row.operations || []).map(row => row.itemId));
      for (const proposal of slice.adoptedDrafts || []) if (!sent.has(proposal.itemId))
        holds.set(proposal.itemId, safeHold(proposal.itemId, 'Draft retained with an editorial-held batch', 'editorial'));
    }
    for (const bulk of child.kind === 'communityhero-reviewed-bulk' ? child.slices || [] : []) {
      for (const held of [...(bulk.editorialReview?.outcome?.held || []), ...(bulk.admission?.held || [])]) {
        const proposal = (slice.adoptedDrafts || []).find(row => row.id === held.reference?.id);
        if (proposal) holds.set(proposal.itemId, safeHold(proposal.itemId, held.reason, 'editorial'));
      }
    }
    for (const held of child.editorialReview?.outcome?.held || []) {
      const proposal = [...(child.originProposals || []), ...(child.proposals || [])].find(row => row.id === held.reference?.id);
      if (proposal?.itemId && !resolved.has(proposal.itemId)) holds.set(proposal.itemId, safeHold(proposal.itemId, held.reason, 'editorial'));
    }
    for (const batch of child.readyBatches || []) {
      if (['editorial-held', 'complete-with-holds', 'quarantined-scope'].includes(batch.mode)) {
        for (const proposal of batch.proposals || []) if (!resolved.has(proposal.itemId) && !(batch.operations || []).some(op => op.itemId === proposal.itemId && op.status === 'succeeded'))
          holds.set(proposal.itemId, batch.mode === 'quarantined-scope'
            ? safeHold(proposal.itemId, 'Original UNKNOWN operation quarantines this conversation', 'quarantine')
            : safeHold(proposal.itemId, 'Editorial review retained this recipient', 'editorial'));
      }
    }
    if (slice.status === 'held' || slice.status === 'prepare-partial-failed') {
      const covered = new Set((child.operations || []).map(op => op.itemId));
      for (const itemId of slice.itemIds || []) if (!covered.has(itemId) && !resolved.has(itemId)) holds.set(itemId, safeHold(itemId, 'Preparation retained this recipient', 'preparation'));
    }
  }
  return [...holds.values()].filter(hold => scopeItemIds.includes(hold.itemId));
}

export function createCampaignDependencies(mediaObserver, factObserver, { signal, pollMs = 1000, maxPolls = 86400 } = {}) {
  let firstLane = 0;
  return async context => {
    const mediaIds = (context.heldSlices || []).filter(slice => ['media_wait', 'media_pending', 'media_unavailable'].includes(slice.planHoldReason))
      .flatMap(slice => slice.itemIds || []).filter(id => context.heldItemIds.includes(id));
    const facts = context.factDependencies || [];
    const lanes = [
      { observer: mediaObserver, ids: mediaIds, context: { ...context, heldItemIds: mediaIds } },
      { observer: factObserver, ids: facts.map(row => row.itemId), context: { ...context, factDependencies: facts } }
    ];
    for (let poll = 0; poll < maxPolls; poll++) {
      let pending = false;
      for (let offset = 0; offset < lanes.length; offset++) {
        if (signal?.aborted) throw new CliError('Stopped before dependency observation', { code: 'STOPPED' });
        const index = (firstLane + offset) % lanes.length; const lane = lanes[index];
        if (!lane.ids.length || !lane.observer) continue;
        const result = await lane.observer.observeOnce(lane.context);
        if (!result || !Array.isArray(result.readyItemIds) || typeof result.pending !== 'boolean'
          || new Set(result.readyItemIds).size !== result.readyItemIds.length || result.readyItemIds.some(id => !lane.ids.includes(id)))
          throw new CliError('Dependency readiness differs from its exact typed scope', { code: 'INVALID_DEPENDENCY_SCOPE' });
        if (result.readyItemIds.length) {
          firstLane = (index + 1) % lanes.length;
          return result.readyItemIds;
        }
        pending ||= result.pending;
      }
      if (!pending) return [];
      if (poll + 1 === maxPolls) throw new CliError('Dependency observation reached its bound; resume the same checkpoints', { code: 'POLL_LIMIT' });
      await waitForPoll(pollMs, signal);
    }
    return [];
  };
}

export async function runConductor(config, { client, signal, onOutput = () => {}, pollMs = 1000, maxPolls = 86400 } = {}) {
  validateConfig(config); client ||= new ConductorClient(config, { signal });
  const emit = value => onOutput(value);
  const mediaObserver = await createMediaDependencies(client, config, { signal, pollMs, maxPolls });
  const factObserver = await createFactDependencies(client, config, { signal, pollMs, maxPolls });
  const waitForDependencies = createCampaignDependencies(mediaObserver, factObserver, { signal, pollMs, maxPolls });
  let lastReport = 0;
  let result;
  try { result = await runQueue(client, { ...(config.resume ? { resumePath: config.checkpointPath } : { checkpointPath: config.checkpointPath }),
    scopeItemIds: config.scopeItemIds, ...(config.cutoffUtc ? { cutoffUtc: config.cutoffUtc } : {}),
    workflowId: config.runId,
    batchSize: config.batchSize, maxCycles: config.maxCycles, autonomous: config.mode === 'execute', execute: config.mode === 'execute',
    ...(config.mode === 'prepare' ? { workflowMode: PREPARE_REVIEW_ONLY } : {}),
    adoptCurrentDrafts: config.mode === 'execute',
    freshEditorial: true, maxRepairRounds: config.mode === 'prepare' ? 0 : config.maxRepairRounds, continueHeld: true, waitForDependencies,
    signal, pollMs, maxPolls, onProgress: event => {
      if (Date.now() - lastReport < 2000) return;
      lastReport = Date.now(); emit({ type: 'report', report: { event: /^[a-zA-Z0-9_.-]{1,80}$/u.test(event.event || '') ? event.event : 'progress' } });
    } }); }
  catch (error) {
    const failure = connectionFailure(error); if (!failure) throw error;
    const checkpoint = await readCheckpoint(config.checkpointPath);
    const journals = await connectionCheckpointTree(config.checkpointPath);
    if (journals.some(hasUnconfirmedAdmission)) throw new CliError('Original admission remains unconfirmed', { code: 'UNKNOWN_MUTATION_OUTCOME' });
    const dependency = validateConnectionDependency(checkpoint.connectionDependency, config);
    if (JSON.stringify(dependency) !== JSON.stringify(failure.dependency))
      throw new CliError('Dependency disposition was not saved in the original checkpoint', { code: 'INVALID_CHECKPOINT' });
    const admissionJournals = journals.flatMap(journal => journal.kind === 'communityhero-reviewed-bulk' ? [journal, ...(journal.slices || [])] : [journal]);
    if (dependency.executeRejection && !admissionJournals.some(journal => {
      const negative = dependency.executeRejection;
      const original = journal.admission ? journal.admission.id === negative.approvalId
        && journal.executeAttemptId === negative.requestId && journal.executeAdmissionProtocol === 'local-admission-v1'
        && journal.executePayloadHash === negative.payloadHash : journal.approvalId === negative.approvalId
        && journal.executeRequestId === negative.requestId && journal.pendingLocalAdmission?.kind === 'execute'
        && journal.pendingLocalAdmission.requestId === negative.requestId && journal.pendingLocalAdmission.payloadHash === negative.payloadHash;
      return original && !journal.executeJobId
        && journal.error?.code === 'REJECTED_LOCAL_ADMISSION'
        && ['requestId', 'approvalId', 'payloadHash', 'evaluationId', 'receiptSha256'].every(key => journal.error.rejection?.[key] === negative[key]);
    })) throw new CliError('Original negative admission was not saved in its owned journal', { code: 'INVALID_CHECKPOINT' });
    await emit({ type: 'dependency_wait', dependency });
    return { mode: 'waiting_dependency', dependency };
  }
  const { adoptedDrafts, ...counts } = summarizeQueue(result.checkpoint);
  const summary = { ...counts, total: config.scopeItemIds.length };
  const itemHolds = campaignHolds(result.checkpoint, config.scopeItemIds);
  const mode = result.mode === READY_FOR_OWNER_APPROVAL ? itemHolds.length ? 'complete-with-holds' : 'prepared'
    : result.mode === 'complete' && itemHolds.length ? 'complete-with-holds' : result.mode;
  const output = { mode, summary, itemHolds };
  await emitCampaignResult(output, emit);
  return output;
}

async function main() {
  if (process.argv.length !== 2) throw new Error('Conductor accepts only private parent configuration');
  const controller = new AbortController(); let resolveConfig; let rejectConfig;
  const initial = new Promise((resolve, reject) => { resolveConfig = resolve; rejectConfig = reject; });
  let config; let buffered = ''; let heartbeat = Date.now();
  const input = (async () => {
    for await (const chunk of process.stdin) {
      buffered += chunk.toString('utf8');
      if (buffered.length > 2 * 1024 * 1024) throw new Error('Parent input exceeds bound');
      let end;
      while ((end = buffered.indexOf('\n')) >= 0) {
        const line = buffered.slice(0, end); buffered = buffered.slice(end + 1);
        const value = JSON.parse(line);
        if (!config) { config = validateConfig(value); resolveConfig(config); }
        else if (value.type === 'heartbeat' && value.leaseGeneration === config.leaseGeneration
          && Object.keys(value).length === 2) heartbeat = Date.now();
        else throw new Error('Invalid parent heartbeat');
      }
    }
    controller.abort(); rejectConfig(new Error('Parent closed before configuration'));
  })().catch(error => { controller.abort(); rejectConfig(error); });
  const timer = setInterval(() => { if (Date.now() - heartbeat > 15_000) controller.abort(); }, 1000);
  let writes = Promise.resolve();
  const emit = value => {
    writes = writes.then(() => new Promise((resolve, reject) => {
      const line = `${JSON.stringify(value)}\n`;
      if (Buffer.byteLength(line) > 256 * 1024) return reject(new Error('Conductor report exceeds bound'));
      process.stdout.write(line, error => error ? reject(error) : resolve());
    }));
    return writes;
  };
  try { await runConductor(await initial, { signal: controller.signal, onOutput: emit }); }
  catch {
    if (config) await emit({ type: 'result', result: { mode: 'blocked', summary: { total: config.scopeItemIds.length }, itemHolds: [] } });
    process.exitCode = 1;
  } finally { clearInterval(timer); await writes; process.stdin.destroy(); await input; }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(() => { process.exitCode = 1; });
}
