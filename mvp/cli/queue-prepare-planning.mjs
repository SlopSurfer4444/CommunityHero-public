// Bounded advisory readers. The queue is the sole consumer and journal writer.
// Successful groups become visible on completion; joining all readers belongs
// only to cleanup, including read failures after a paid child was admitted.
import { CliError } from './client.mjs';

export function createQueuePreparePlanning(client, windows, instruction, { parallelism = 1, signal } = {}) {
  if (!Number.isInteger(parallelism) || parallelism < 1 || parallelism > 8
    || !Array.isArray(windows) || windows.length < 1 || windows.length > 8
    || windows.some(ids => !Array.isArray(ids) || !ids.length || ids.length > 100
      || ids.some(id => typeof id !== 'string' || !id)))
    throw new CliError('Invalid independent preparation plan windows', { code: 'USAGE' });
  const selected = windows.map(ids => [...ids]);
  if (new Set(selected.flat()).size !== selected.flat().length)
    throw new CliError('Independent preparation plan windows overlap', { code: 'USAGE' });
  const ready = [], waiters = new Set();
  let cursor = 0, active = 0, stopped = false, failure;
  const notify = () => { for (const resolve of waiters) resolve(); waiters.clear(); };
  const worker = async () => {
    active++;
    try {
      while (!stopped && cursor < selected.length) {
        const window = cursor++;
        try {
          if (signal?.aborted) throw new CliError('Stopped before preparation planning', { code: 'STOPPED' });
          const plan = await client.planPrepare(selected[window], instruction);
          if (!stopped) ready.push({ ...plan, window });
          notify();
        } catch (error) { failure ??= error; stopped = true; notify(); }
      }
    } finally { active--; notify(); }
  };
  const readers = Array.from({ length: Math.min(parallelism, selected.length) }, worker);
  const waitReady = async () => {
    while (!failure && !ready.length && active) await new Promise(resolve => waiters.add(resolve));
  };
  return {
    get done() { return !failure && !active && !ready.length; },
    waitReady,
    async next({ wait = true } = {}) {
      if (wait) await waitReady();
      if (failure) throw failure;
      if (signal?.aborted) throw new CliError('Stopped during preparation planning', { code: 'STOPPED' });
      return ready.shift();
    },
    async joinAll({ stop = false } = {}) {
      if (stop) { stopped = true; notify(); }
      await Promise.all(readers);
    }
  };
}
