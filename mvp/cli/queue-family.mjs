import { CliError } from './client.mjs';

// Campaign scope is finite metadata. Only returned windows become preparation
// requests; membership is resolved by Rust from current company media evidence.
export function familySelectionInput(itemIds, batchSize, maxBatches) {
  if (!Array.isArray(itemIds) || !itemIds.length || itemIds.length > 5000
    || new Set(itemIds).size !== itemIds.length
    || itemIds.some(id => typeof id !== 'string' || !id || id.length > 128 || id.trim() !== id || /[,\u0000-\u001f\u007f]/u.test(id))
    || !Number.isInteger(batchSize) || batchSize < 1 || batchSize > 100
    || !Number.isInteger(maxBatches) || maxBatches < 1 || maxBatches > 8)
    throw new CliError('Invalid family selection bounds', { code: 'USAGE' });
  return { itemIds, batchSize, maxBatches };
}

export function validateFamilyWindows(value, itemIds, batchSize, maxBatches) {
  const invalid = () => { throw new CliError('Invalid preparation family selection', { code: 'INVALID_FAMILY_SELECTION' }); };
  if (value?.advisory !== true || !Array.isArray(value.selectedItemIds)
    || value.selectedItemIds.length !== itemIds.length || value.selectedItemIds.some((id, i) => id !== itemIds[i])
    || !Array.isArray(value.windows) || !value.windows.length || value.windows.length > maxBatches) invalid();
  const scope = new Set(itemIds); const seen = new Set();
  for (const window of value.windows) {
    if (!Array.isArray(window) || !window.length || window.length > batchSize) invalid();
    for (const id of window) {
      if (!scope.has(id) || seen.has(id)) invalid();
      seen.add(id);
    }
  }
  return value.windows;
}
