import {buildPostDiscussions} from './post-topics.js';
const normalize = text => String(text || '').normalize('NFKC').toLocaleLowerCase('ru').replaceAll('ё','е').trim();
const dayFormatter = new Intl.DateTimeFormat('en-CA',{timeZone:'Europe/Moscow',year:'numeric',month:'2-digit',day:'2-digit'});
export function initializeRecentListFilters(saved, views) {
  if (saved.openQueueDefaultVersion === 3) return;
  saved.listFilters ||= {};
  for (const view of views) {
    const filters = saved.listFilters[view] ||= {};
    if (!filters.period && !filters.from && !filters.to || filters.period === 'recent48') {
      filters.period = 'all';
      delete filters.from; delete filters.to;
      filters.dateField = 'created';
    }
  }
  saved.openQueueDefaultVersion = 3;
}
export function resolvePeriod(filters = {}, now = new Date()) {
  if(filters.period==='recent48'){const result={...filters,period:'all'};delete result.from;delete result.to;return result;}
  if (filters.period !== 'week') return {...filters};
  const to = calendarDay(now), start = new Date(`${to}T12:00:00Z`);
  start.setUTCDate(start.getUTCDate()-6);
  return {...filters,from:start.toISOString().slice(0,10),to};
}
export function dateBasis(view, filters = {}) {
  return filters.dateField === 'created' ? 'created' : view === 'closed' ? 'closed' : view === 'deleted' ? 'deleted' : 'created';
}
export function recordDate(record, basis) {
  return basis === 'closed' ? record.state.closure?.at : basis === 'deleted' ? record.state.deletion?.at
    : Object.hasOwn(record,'sourceCreatedAt') ? record.sourceCreatedAt : record.createdAt;
}
export function validDate(value) {
  return value != null && value !== '' && Number.isFinite(Date.parse(value)) ? value : null;
}
export function calendarDay(value) {
  if (!value || !Number.isFinite(Date.parse(value))) return '';
  const parts = dayFormatter.formatToParts(new Date(value));
  return ['year','month','day'].map(type => parts.find(part => part.type === type).value).join('-');
}
// Display grouping only: imported closure evidence is never rewritten.
export function presentedOutcome(state) {
  return state.view==='closed' ? (state.closure?.outcome==='reply'?'reply':'no_reply') : state.decision;
}
export function matchesList(record, {view, outcome = 'all', query = '', filters = {}, now = new Date()}) {
  filters = resolvePeriod(filters, now);
  if (record.state.view !== view) return false;
  const actualOutcome = presentedOutcome(record.state);
  if (['prepared','closed'].includes(view) && outcome !== 'all' && actualOutcome !== outcome) return false;
  if (filters.postId && filters.postId !== record.postId) return false;
  if (filters.channel && filters.channel !== record.channel) return false;
  if (query.trim()) {
    const target = record.messages.find(message => message.id === record.item.targetId);
    const searchable = normalize(`${target?.author || ''} ${target?.textUnavailable ? '' : target?.text || ''}`);
    if (normalize(query).split(/\s+/).filter(Boolean).some(word => !searchable.includes(word))) return false;
  }
  if (filters.from || filters.to) {
    if (filters.from && filters.to && filters.from > filters.to) return false;
    const day = calendarDay(recordDate(record,dateBasis(view,filters)));
    if (!day || filters.from && day < filters.from || filters.to && day > filters.to) return false;
  }
  return true;
}
export function filterList(records, options) {
  const now = options.now ?? new Date();
  const resolved = resolvePeriod(options.filters, now);
  options = {...options, now, filters:resolved.period==='week'?{...resolved,period:'custom'}:resolved};
  const basis = dateBasis(options.view,options.filters);
  const direction=options.order==='oldest'?1:-1;
  const created=record=>validDate(Object.hasOwn(record,'sourceCreatedAt')?record.sourceCreatedAt:record.createdAt);
  return records.filter(record => matchesList(record,options)).sort((a,b) => {
    const first=validDate(recordDate(a,basis)),second=validDate(recordDate(b,basis));
    // Keep undated closures last, and order that group by the actual comment date.
    // This is a sorting fallback only; it does not create a closure timestamp.
    if(Boolean(first)!==Boolean(second))return first ? -1 : 1;
    const firstSort=Date.parse(first || created(a)),secondSort=Date.parse(second || created(b));
    if(Number.isFinite(firstSort)&&Number.isFinite(secondSort))return direction*(firstSort-secondSort)||a.item.id.localeCompare(b.item.id);
    if(Number.isFinite(firstSort)!==Number.isFinite(secondSort))return Number.isFinite(firstSort) ? -1 : 1;
    return a.item.id.localeCompare(b.item.id);
  });
}

export function hasAppliedListConditions(search = '', filters = {}) {
  return Boolean(search || filters.postId || filters.channel
    || filters.period==='week' || filters.from || filters.to);
}

export function resetListConditions(saved, view = saved.view) {
  saved.listFilters ||= {};
  saved.listFilters[view] = {period:'all'};
  saved.search = '';
  return saved;
}

export function overviewSnapshot(records, posts, topicsFor, period='week', now=new Date()) {
  const open=record=>['attention','prepared','waiting'].includes(record.state.view);
  const filters={...resolvePeriod(typeof period==='string'?{period}:period,now),period:'custom',dateField:'created'};
  const retained=records.filter(r=>r.state.view!=='deleted');
  const current=retained.filter(r=>matchesList(r,{view:r.state.view,filters}));
  const currentIds=new Set(current.map(r=>r.item.id));
  const groups=buildPostDiscussions(current,posts);
  return {groups,records:current,open:current.filter(open).length,attention:current.filter(r=>r.state.view==='attention').length,outside:retained.filter(r=>open(r)&&!currentIds.has(r.item.id)).length};
}

export function overviewActivity(records, filters = {}) {
  const days = new Map();
  const add = (at, kind) => {
    const day = calendarDay(at);
    if (!day || filters.from && day < filters.from || filters.to && day > filters.to) return;
    if (!days.has(day)) days.set(day, {new:0, reply:0, no_reply:0, deleted:0});
    days.get(day)[kind]++;
  };
  for (const record of records) {
    add(record.createdAt,'new');
    for (const event of record.state.events || []) {
      if (event.type === 'deleted') add(event.at,'deleted');
      else if (event.type === 'closed' && ['reply','no_reply'].includes(event.outcome)) add(event.at,event.outcome);
    }
  }
  const keys = [...days.keys()].sort();
  const from = filters.from || keys[0], to = filters.to || keys.at(-1);
  if (!from || !to || from > to) return [];
  // Daily buckets for short ranges; aggregate longer ones to readable months.
  const monthly = (Date.parse(to)-Date.parse(from))/86400000 > 62;
  const buckets = new Map();
  for (let day=from; day<=to;) {
    const key=monthly ? day.slice(0,7) : day;
    if (!buckets.has(key)) buckets.set(key,{new:0,reply:0,no_reply:0,deleted:0});
    const date=new Date(`${day}T12:00:00Z`);
    if(monthly){date.setUTCDate(1);date.setUTCMonth(date.getUTCMonth()+1);}else date.setUTCDate(date.getUTCDate()+1);
    day=date.toISOString().slice(0,10);
  }
  for(const [day,counts] of days){const bucket=buckets.get(monthly?day.slice(0,7):day);if(bucket)for(const kind of Object.keys(counts))bucket[kind]+=counts[kind];}
  return [...buckets].map(([day,counts])=>({day,...counts,total:Object.values(counts).reduce((a,b)=>a+b,0)}));
}
