// Restart cursors only. Canonical Rust dependencies retain source evidence,
// bounded research attempts and consumption by a NEW ordinary preparation.
import { CliError, waitForPoll } from './client.mjs';
import { readCheckpoint, writeCheckpoint } from './workflow.mjs';
import { connectionFailure } from './conductor-connection.mjs';

const ACTIVE = new Set(['queued', 'running']);
const PHASES = new Set(['new', 'admitting', 'observing', 'uncertain', 'held']);
const invalid = message => new CliError(message, { code: 'INVALID_CHECKPOINT' });
const key = row => JSON.stringify([row?.prepareJobId, row?.id, row?.itemId]);
const nonempty = value => typeof value === 'string' && value.length > 0;

export async function createFactDependencies(client, config, { signal, pollMs = 1000, maxPolls = 86400 } = {}) {
  const path = `${config.checkpointPath}.facts.json`;
  let saved;
  try { saved = await readCheckpoint(path); }
  catch (error) { if (error.code !== 'INVALID_CHECKPOINT' || error.details?.cause !== 'ENOENT') throw error; }
  if (saved && (saved.kind !== 'communityhero-conductor-facts' || saved.runId !== config.runId
    || saved.account !== config.account || saved.baseUrl !== client.baseUrl
    || JSON.stringify(saved.scopeItemIds) !== JSON.stringify(config.scopeItemIds)))
    throw invalid('Fact dependency checkpoint binding changed');
  let state = saved || { kind: 'communityhero-conductor-facts', runId: config.runId, account: config.account,
    baseUrl: client.baseUrl, scopeItemIds: config.scopeItemIds, dependencies: [] };
  const scope = new Set(config.scopeItemIds); const owned = new Set();
  if (!Array.isArray(state.dependencies)) throw invalid('Fact dependency checkpoint has no dependencies');
  for (const row of state.dependencies) {
    if (!row || !nonempty(row.prepareJobId) || !nonempty(row.id) || !scope.has(row.itemId)
      || row.kind !== 'missing_public_fact' || !PHASES.has(row.phase) || owned.has(key(row))
      || row.lastResearchJobId != null && !nonempty(row.lastResearchJobId))
      throw invalid('Fact dependency checkpoint ownership changed');
    owned.add(key(row));
  }
  const save = async () => { state = await writeCheckpoint(path, state); };
  const checkStopped = () => {
    if (signal?.aborted) throw new CliError('Stopped while observing fact dependency', { code: 'STOPPED' });
  };
  const parentEntries = async parentId => {
    checkStopped();
    const parent = await client.getJob(parentId, { signal });
    if (!parent || parent.id !== parentId || parent.purpose !== 'engine_prepare'
      || parent.conductorRunId !== config.runId || !Number.isSafeInteger(parent.grantGeneration)
      || parent.grantGeneration < 1 || parent.grantGeneration > config.leaseGeneration
      || !Array.isArray(parent.factDependencies)) throw invalid('Fact parent/current grant binding changed');
    const ids = new Set(); const items = new Set();
    for (const row of parent.factDependencies) {
      if (!row || !nonempty(row.id) || !nonempty(row.itemId) || ids.has(row.id) || items.has(row.itemId))
        throw invalid('Canonical fact dependency ownership changed');
      ids.add(row.id); items.add(row.itemId);
    }
    return parent.factDependencies;
  };
  let observing = false;
  const observe = async ({ factDependencies = [] }, once = false) => {
    if (observing) throw invalid('Fact dependency observation already has an owner');
    if (!Array.isArray(factDependencies) || new Set(factDependencies.map(key)).size !== factDependencies.length
      || new Set(factDependencies.map(row => row.itemId)).size !== factDependencies.length
      || factDependencies.some(row => !row || !nonempty(row.prepareJobId) || !nonempty(row.id) || !scope.has(row.itemId)))
      throw new CliError('Fact dependencies differ from exact held scope', { code: 'INVALID_DEPENDENCY_SCOPE' });
    observing = true;
    try {
      checkStopped();
      // Private, media and owner holds never become lookup requests.
      const requested = factDependencies.filter(row => row.kind === 'missing_public_fact');
      if (!requested.length) return once ? { readyItemIds: [], pending: false } : [];
      const cursors = requested.map(row => {
        let cursor = state.dependencies.find(old => key(old) === key(row));
        if (!cursor) { cursor = { prepareJobId: row.prepareJobId, id: row.id, itemId: row.itemId, kind: row.kind, phase: 'new' }; state.dependencies.push(cursor); }
        return cursor;
      });
      await save();
      const groups = [];
      for (const parentId of new Set(cursors.map(row => row.prepareJobId))) {
        const rows = cursors.filter(row => row.prepareJobId === parentId);
        for (let offset = 0; offset < rows.length; offset += 100) groups.push({ parentId, cursors: rows.slice(offset, offset + 100) });
      }
      for (let poll = 0; poll < maxPolls; poll++) {
        const ready = []; let pending = false;
        for (const group of groups) {
          const entries = await parentEntries(group.parentId); const candidates = [];
          for (const cursor of group.cursors) {
            const current = entries.find(row => row.id === cursor.id && row.itemId === cursor.itemId);
            if (!current || current.kind !== cursor.kind) throw invalid('Fact dependency was retargeted');
            if (current.consumedByJobId != null || ['held', 'stale'].includes(current.status)) continue;
            if (!['pending', 'researching', 'resolved'].includes(current.status)) throw invalid('Unknown canonical fact dependency status');
            if (current.lastResearchJobId != null) {
              if (!nonempty(current.lastResearchJobId)) throw invalid('Invalid canonical research identity');
              // Recover original acknowledgement from the exact parent first.
              cursor.lastResearchJobId = current.lastResearchJobId;
              const job = await client.getJob(current.lastResearchJobId, { signal });
              if (!job || job.id !== current.lastResearchJobId || job.purpose !== 'public_fact_followup'
                || job.parentPrepareJobId !== group.parentId || job.conductorRunId !== config.runId
                || !Number.isSafeInteger(job.grantGeneration) || job.grantGeneration < 1 || job.grantGeneration > config.leaseGeneration
                || !Array.isArray(job.requestedItemIds) || !job.requestedItemIds.includes(cursor.itemId)
                || !Array.isArray(job.factDependencyIds) || !job.factDependencyIds.includes(cursor.id))
                throw invalid('Original research job binding changed');
              if (ACTIVE.has(job.status)) { cursor.phase = 'observing'; pending = true; continue; }
              if (current.status !== 'resolved' && job.status !== 'interrupted') continue;
              // Rust permits a maximum of two read-only attempts. An observed
              // interrupted original is necessary before admitting a new one.
            } else if (cursor.phase !== 'new') {
              // Missing ACK with no original canonical identity stays held.
              cursor.phase = 'uncertain'; continue;
            }
            candidates.push(cursor);
          }
          await save();
          if (!candidates.length) continue;
          checkStopped();
          const priorPhases = candidates.map(cursor => cursor.phase);
          for (const cursor of candidates) cursor.phase = 'admitting';
          await save();
          let result;
          try { result = await client.resolvePublicFacts(group.parentId, candidates.map(row => row.itemId)); }
          catch (error) {
            if (connectionFailure(error)?.beforeMutationPath === '/api/engine/prepare/facts/resolve') {
              candidates.forEach((cursor, index) => { cursor.phase = priorPhases[index]; });
              await save(); throw error;
            }
            for (const cursor of candidates) cursor.phase = error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'uncertain' : 'held';
            await save();
            if (signal?.aborted || ['STOPPED', 'WRONG_ACCOUNT', 'FORBIDDEN'].includes(error.code) || error.status === 403) throw error;
            if (error.code === 'UNKNOWN_MUTATION_OUTCOME') pending = true;
            continue;
          }
          const selected = new Set(candidates.map(row => row.itemId));
          if (!result || !Array.isArray(result.jobIds) || result.jobIds.some(id => !nonempty(id))
            || new Set(result.jobIds).size !== result.jobIds.length || !Array.isArray(result.readyItemIds)
            || new Set(result.readyItemIds).size !== result.readyItemIds.length || result.readyItemIds.some(id => !selected.has(id))
            || !Array.isArray(result.held) || result.held.some(row => !selected.has(row?.itemId) || !nonempty(row.reason)))
            throw new CliError('Fact resolver widened or changed its exact scope', { code: 'INVALID_DEPENDENCY_SCOPE' });
          const fresh = await parentEntries(group.parentId);
          if (result.jobIds.some(id => !fresh.some(row => candidates.some(cursor => cursor.id === row.id
            && cursor.itemId === row.itemId) && row.lastResearchJobId === id)))
            throw invalid('Resolver research identity differs from original parent');
          for (const cursor of candidates) {
            const row = fresh.find(row => row.id === cursor.id && row.itemId === cursor.itemId && row.kind === cursor.kind);
            if (!row) throw invalid('Resolved fact dependency binding changed');
            cursor.lastResearchJobId = row.lastResearchJobId;
            cursor.phase = 'observing';
            if (result.readyItemIds.includes(cursor.itemId)) {
              if (row.status !== 'resolved' || row.consumedByJobId != null) throw invalid('Fact readiness lacks unconsumed canonical evidence');
              ready.push(cursor.itemId);
            }
          }
          if (result.jobIds.length) pending = true;
          await save();
        }
        if (ready.length) return once ? { readyItemIds: ready, pending } : ready;
        if (!pending) return once ? { readyItemIds: [], pending: false } : [];
        if (once) return { readyItemIds: [], pending };
        if (poll + 1 === maxPolls) throw new CliError('Fact dependency observation reached its bound; resume the same checkpoints', { code: 'POLL_LIMIT' });
        await waitForPoll(pollMs, signal);
      }
      return [];
    } finally { observing = false; }
  };
  observe.observeOnce = context => observe(context, true);
  return observe;
}
