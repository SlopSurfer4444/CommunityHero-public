// Rebuild when a refreshed snapshot replaces a collection or a fixture appends to it.
// Entries themselves remain live, so ordinary field edits need no invalidation.
export function createEntityIndex(getData) {
  const caches = new Map();
  function lookup(collection, field, value) {
    const entries = getData()[collection];
    const key = `${collection}:${field}`;
    let cache = caches.get(key);
    if (!cache || cache.entries !== entries || cache.length !== entries.length) {
      const index = new Map();
      for (const entry of entries) if (!index.has(entry[field])) index.set(entry[field], entry);
      cache = {entries, length: entries.length, index};
      caches.set(key, cache);
    }
    return cache.index.get(value);
  }
  return {
    item: id => lookup('items', 'id', id),
    messageOwner: id => lookup('items', 'targetId', id),
    branch: id => lookup('branches', 'id', id),
    post: id => lookup('posts', 'id', id),
  };
}
