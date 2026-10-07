// Wire projection only. The validated full payload remains the admission input.
// These references describe exact applicability sets, never new policy authority.
export const SHARED_MODERATION_CONTEXT = 'shared_moderation_v1';
const bytes = value => Buffer.byteLength(JSON.stringify(value));
const validIds = value => Array.isArray(value) && value.every(id => typeof id === 'string' && id.length > 0)
  && new Set(value).size === value.length;

export function projectModerationRuleSets(payload) {
  if (payload.modelContextContract !== SHARED_MODERATION_CONTEXT || !Array.isArray(payload.items)) return payload;
  const groups = new Map();
  for (const item of payload.items) {
    if (item.moderationRuleEntryIds === undefined) continue;
    // Unknown shapes are retained, not shortened by guessing their semantics.
    if (!validIds(item.moderationRuleEntryIds)) return payload;
    const key = JSON.stringify(item.moderationRuleEntryIds);
    const group = groups.get(key) ?? {entryIds:item.moderationRuleEntryIds,count:0};
    group.count++; groups.set(key,group);
  }
  const shared = new Map(), sets = [];
  for (const [key,group] of [...groups].sort(([a],[b]) => a < b ? -1 : a > b ? 1 : 0)) {
    if (group.count < 2) continue;
    const id = `mrs${sets.length + 1}`, set = {id,entryIds:[...group.entryIds]};
    if (bytes(set) + group.count * bytes({moderationRuleSetId:id})
      >= group.count * bytes({moderationRuleEntryIds:group.entryIds})) continue;
    shared.set(key,id); sets.push(set);
  }
  if (!sets.length) return payload;
  const items = payload.items.map(item => {
    const id = shared.get(JSON.stringify(item.moderationRuleEntryIds));
    if (!id) return item;
    const {moderationRuleEntryIds,...rest} = item;
    return {...rest,moderationRuleSetId:id};
  });
  const result = {...payload,moderationRuleSets:sets,items};
  // Include table/property punctuation in the final benefit check too.
  return bytes(result) < bytes(payload) ? result : payload;
}

// Deterministic round-trip/integration check. Never use model-provided sets to
// authorize an action: runtime admission already retains the original lists.
export function expandModerationRuleSets(projected) {
  const invalid = () => { throw new TypeError('Invalid shared moderation applicability'); };
  const items = projected.items;
  if (!Array.isArray(items)) return invalid();
  if (projected.moderationRuleSets === undefined) {
    if (items.some(item => item.moderationRuleSetId !== undefined)) return invalid();
    return projected;
  }
  if (projected.modelContextContract !== SHARED_MODERATION_CONTEXT || !Array.isArray(projected.moderationRuleSets)) return invalid();
  const sets = new Map();
  for (const set of projected.moderationRuleSets) {
    if (!set || Object.keys(set).length !== 2 || typeof set.id !== 'string' || !set.id
      || !validIds(set.entryIds) || sets.has(set.id)) return invalid();
    sets.set(set.id,set.entryIds);
  }
  const restored = items.map(item => {
    if (item.moderationRuleSetId === undefined) return item;
    if (item.moderationRuleEntryIds !== undefined || !sets.has(item.moderationRuleSetId)) return invalid();
    const {moderationRuleSetId,...rest} = item;
    return {...rest,moderationRuleEntryIds:[...sets.get(moderationRuleSetId)]};
  });
  const {moderationRuleSets,...rest} = projected;
  return {...rest,items:restored};
}
