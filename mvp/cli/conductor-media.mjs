// Dependency cursors are not an external-effect ledger. Rust's canonical plan
// remains the only readiness proof, and uncertain admissions are inspection-only.
import { CliError, waitForPoll } from './client.mjs';
import { readCheckpoint, writeCheckpoint, DEFAULT_INSTRUCTION } from './workflow.mjs';
import { connectionFailure } from './conductor-connection.mjs';

const MEDIA_HOLDS = new Set(['media_wait', 'media_pending', 'media_unavailable']);
const ACTIVE = new Set(['queued', 'running']);
const PHASES = new Set(['new', 'admitting', 'observing', 'advancing', 'uncertain', 'held']);
const invalid = message => new CliError(message, { code: 'INVALID_CHECKPOINT' });

export async function createMediaDependencies(client, config, { signal, pollMs = 1000, maxPolls = 86400 } = {}) {
  const path = `${config.checkpointPath}.dependencies.json`;
  let saved;
  try { saved = await readCheckpoint(path); }
  catch (error) { if (error.code !== 'INVALID_CHECKPOINT' || error.details?.cause !== 'ENOENT') throw error; }
  const scope = new Set(config.scopeItemIds);
  if (saved && (saved.kind !== 'communityhero-conductor-dependencies' || saved.runId !== config.runId
    || saved.account !== config.account || saved.baseUrl !== client.baseUrl
    || JSON.stringify(saved.scopeItemIds) !== JSON.stringify(config.scopeItemIds)))
    throw invalid('Media dependency checkpoint binding changed');
  let state = saved || { kind: 'communityhero-conductor-dependencies', runId: config.runId, account: config.account,
    baseUrl: client.baseUrl, scopeItemIds: config.scopeItemIds, dependencies: [] };
  const itemOwners = new Map(); const posts = new Set();
  if (!Array.isArray(state.dependencies)) throw invalid('Media dependency checkpoint has no dependencies');
  for (const dependency of state.dependencies) {
    if (!dependency || typeof dependency.postId !== 'string' || !dependency.postId || posts.has(dependency.postId)
      || !PHASES.has(dependency.phase) || !Array.isArray(dependency.itemIds) || !dependency.itemIds.length
      || new Set(dependency.itemIds).size !== dependency.itemIds.length
      || dependency.itemIds.some(id => !scope.has(id) || itemOwners.has(id))
      || dependency.jobId != null && (typeof dependency.jobId !== 'string' || !dependency.jobId))
      throw invalid('Media dependency checkpoint ownership changed');
    posts.add(dependency.postId);
    for (const id of dependency.itemIds) itemOwners.set(id, dependency.postId);
  }
  const save = async () => { state = await writeCheckpoint(path, state); };
  const checkStopped = () => {
    if (signal?.aborted) throw new CliError('Stopped while observing media dependency', { code: 'STOPPED' });
  };
  const plan = async ids => {
    const ready = []; const waiting = [];
    for (let offset = 0; offset < ids.length; offset += 100) {
      checkStopped();
      const selected = ids.slice(offset, offset + 100);
      const result = await client.planPrepare(selected, DEFAULT_INSTRUCTION);
      ready.push(...result.batches.flatMap(batch => batch.itemIds).filter(id => selected.includes(id)));
      waiting.push(...result.held.filter(row => selected.includes(row.itemId) && MEDIA_HOLDS.has(row.reason)).map(row => row.itemId));
    }
    return { ready: [...new Set(ready)], waiting: [...new Set(waiting)] };
  };
  // Creating the observer never scans the manifest or admits media. The queue
  // drains independent ready work first and supplies only its owned typed holds.
  let observing = false;
  const observe = async ({ heldItemIds }, once = false) => {
    if (observing) throw invalid('Media dependency observation already has an owner');
    if (!Array.isArray(heldItemIds) || new Set(heldItemIds).size !== heldItemIds.length
      || heldItemIds.some(id => !scope.has(id)))
      throw new CliError('Media dependency readiness differs from exact held scope', { code: 'INVALID_DEPENDENCY_SCOPE' });
    observing = true;
    try {
      checkStopped();
      const initial = await plan(heldItemIds);
      if (initial.ready.length) return { readyItemIds: initial.ready, pending: false };
      for (let offset = 0; offset < initial.waiting.length; offset += 100) {
        checkStopped();
        const selected = initial.waiting.slice(offset, offset + 100);
        const review = await client.reviewItems(selected);
        for (const item of review.items) {
          if (!selected.includes(item.id) || typeof item.postId !== 'string' || !item.postId) continue;
          if (itemOwners.has(item.id) && itemOwners.get(item.id) !== item.postId)
            throw invalid('Media dependency post allocation changed');
          let dependency = state.dependencies.find(row => row.postId === item.postId);
          if (!dependency) {
            dependency = { postId: item.postId, itemIds: [], phase: 'new' }; state.dependencies.push(dependency);
          }
          if (!dependency.itemIds.includes(item.id)) dependency.itemIds.push(item.id);
          itemOwners.set(item.id, item.postId);
        }
      }
      if (initial.waiting.length) await save();
      const requested = new Set(initial.waiting);
      const relevant = () => state.dependencies.filter(row => row.itemIds.some(id => requested.has(id)));
      for (const dependency of relevant()) {
        checkStopped();
        if (dependency.phase !== 'new') continue;
        dependency.phase = 'admitting'; await save();
        try {
          const admitted = await client.media(dependency.postId);
          dependency.jobId = admitted.jobId || null; dependency.status = admitted.status;
          dependency.phase = 'observing'; await save();
        } catch (error) {
          if (connectionFailure(error)?.beforeMutationPath === '/api/materials/process') {
            dependency.phase = 'new'; await save(); throw error;
          }
          dependency.phase = error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'uncertain' : 'held';
          dependency.reason = error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'media-admission-unresolved' : 'media-preparation-unavailable';
          await save();
          if (signal?.aborted || ['STOPPED', 'WRONG_ACCOUNT', 'FORBIDDEN'].includes(error.code) || error.status === 403) throw error;
        }
      }
      for (let poll = 0; poll < maxPolls; poll++) {
        let pending = false; const continuations = [];
        // Read every dependency once before waiting or advancing queued work.
        // An earlier running job cannot consume all polls before B is checked.
        for (const dependency of relevant()) {
          checkStopped();
          const status = await client.mediaStatus(dependency.postId);
          if (!Array.isArray(status.jobs)) throw new CliError('Media observation has no canonical jobs', { code: 'INVALID_RESPONSE' });
          if (!dependency.jobId) {
            const active = status.jobs.filter(job => ACTIVE.has(job.status));
            if (active.length === 1) { dependency.jobId = active[0].id; await save(); }
          }
          const observed = dependency.jobId ? status.jobs.find(job => job.id === dependency.jobId) : null;
          const readiness = status.readiness;
          if (readiness?.ready === true || readiness?.state === 'ready' || readiness?.status === 'ready') continue;
          if (observed && ACTIVE.has(observed.status)) {
            pending = true;
            if (observed.status === 'queued' && dependency.phase === 'observing') continuations.push(dependency);
          }
        }
        const current = await plan(heldItemIds);
        if (current.ready.length) return { readyItemIds: current.ready, pending };
        if (!pending) return { readyItemIds: [], pending: false };
        for (const dependency of continuations) {
          checkStopped();
          // A crash or lost continuation ACK must never replay its POST.
          dependency.phase = 'advancing'; await save();
          try {
            const result = await client.media(dependency.postId);
            if (result.jobId && result.jobId !== dependency.jobId) {
              dependency.phase = 'held'; dependency.reason = 'media-source-allocation-changed';
            } else dependency.phase = 'observing';
            await save();
          } catch (error) {
            if (connectionFailure(error)?.beforeMutationPath === '/api/materials/process') {
              dependency.phase = 'observing'; await save(); throw error;
            }
            dependency.phase = error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'uncertain' : 'held';
            dependency.reason = error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'media-admission-unresolved' : 'media-preparation-unavailable';
            await save(); throw error;
          }
        }
        if (once) return { readyItemIds: [], pending: true };
        if (poll + 1 === maxPolls)
          throw new CliError('Media dependency observation reached its bound; resume the same checkpoints', { code: 'POLL_LIMIT' });
        await waitForPoll(pollMs, signal);
      }
      return { readyItemIds: [], pending: false };
    } finally { observing = false; }
  };
  const observer = async context => (await observe(context)).readyItemIds;
  // A conductor can fairly interleave media with other typed dependency lanes
  // without leaving either observer running when a ready subset is returned.
  observer.observeOnce = context => observe(context, true);
  return observer;
}
