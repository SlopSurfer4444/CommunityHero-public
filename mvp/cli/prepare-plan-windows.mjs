// Independent advisory plans only. No admission, checkpoint, approval, model
// request or provider action is created by this scheduler.
import { CliError } from './client.mjs';

export async function planPrepareWindows(client, windows, instruction, { parallelism = 1, signal } = {}) {
  if (!Number.isInteger(parallelism) || parallelism < 1 || parallelism > 8
    || !Array.isArray(windows) || windows.length < 1 || windows.length > 8
    || windows.some(ids => !Array.isArray(ids) || !ids.length || ids.length > 100
      || ids.some(id => typeof id !== 'string' || !id)))
    throw new CliError('Invalid independent preparation plan windows', { code: 'USAGE' });
  const flattened = windows.flat();
  if (new Set(flattened).size !== flattened.length)
    throw new CliError('Independent preparation plan windows overlap', { code: 'USAGE' });
  const selected = windows.map(ids => [...ids]);
  const results = new Array(selected.length);
  const errors = new Array(selected.length);
  let cursor = 0; let stopped = false;
  const worker = async () => {
    while (!stopped && cursor < selected.length) {
      const index = cursor++;
      try {
        if (signal?.aborted) throw new CliError('Stopped before preparation planning', { code: 'STOPPED' });
        results[index] = { ...await client.planPrepare(selected[index], instruction), window: index };
      } catch (error) { errors[index] = { error }; stopped = true; }
    }
  };
  // Even after a failed read, observe all started siblings before returning.
  // A consumer gets a complete ordered plan set or the original read failure.
  await Promise.all(Array.from({ length: Math.min(parallelism, selected.length) }, worker));
  const failed = errors.findIndex(error => error !== undefined);
  if (failed >= 0) throw errors[failed].error;
  if (signal?.aborted) throw new CliError('Stopped after preparation planning', { code: 'STOPPED' });
  return results;
}
