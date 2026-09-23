import {calendarDay, resolvePeriod} from './list-filters.js';

export const analyticsKinds = [
  {key:'new', label:'Новые', color:'#67a887'},
  {key:'reply', label:'Закрыты с ответом', color:'#6695d0'},
  {key:'no_reply', label:'Закрыты без ответа', color:'#a4abb5'},
  {key:'deleted', label:'Удалены', color:'#d87c80'},
];
const emptyCounts = () => Object.fromEntries(analyticsKinds.map(({key})=>[key,0]));
const isOpen = record => ['attention','prepared','waiting'].includes(record.state.view);
const kindFor = record => record.state.view==='closed'
  ? (record.state.closure?.outcome==='reply' ? 'reply' : 'no_reply')
  : record.state.view==='deleted' ? 'deleted' : 'new';

// Each imported comment belongs to one current-state segment, dated by its
// creation. These cohorts do not claim historical processing-event totals.
// Display grouping combines all closed items without a confirmed reply;
// the source closure evidence remains unchanged.
export function analyticsSnapshot(records, filters = {}, now = new Date()) {
  filters=resolvePeriod(filters,now);
  const dated=records.map(record=>({record,day:calendarDay(record.createdAt)}));
  const selected=dated.filter(({day})=>day && (!filters.from || day>=filters.from) && (!filters.to || day<=filters.to));
  const days=selected.map(({day})=>day).sort();
  const from=filters.from || days[0] || '', to=filters.to || days.at(-1) || '';
  const monthly=!!from && !!to && (Date.parse(to)-Date.parse(from))/86400000>62;
  const buckets=new Map(), counts=emptyCounts();
  if(from && to && from<=to) for(let day=from;day<=to;){
    const key=monthly ? day.slice(0,7) : day;
    if(!buckets.has(key))buckets.set(key,{day:key,...emptyCounts(),total:0});
    const next=new Date(`${day}T12:00:00Z`);
    if(monthly){next.setUTCDate(1);next.setUTCMonth(next.getUTCMonth()+1);}else next.setUTCDate(next.getUTCDate()+1);
    day=next.toISOString().slice(0,10);
  }
  for(const {record,day} of selected){
    const kind=kindFor(record), bucket=buckets.get(monthly ? day.slice(0,7) : day);
    counts[kind]++;if(bucket){bucket[kind]++;bucket.total++;}
  }
  const nowMs=now.getTime();
  return {from,to,monthly,series:[...buckets.values()],counts,total:selected.length,
    open:counts.new,closed:counts.reply+counts.no_reply,
    last24:records.filter(record=>{const at=Date.parse(record.createdAt);return at>nowMs-86400000 && at<=nowMs;}).length,
    allOpen:records.filter(isOpen).length,undated:dated.filter(({day})=>!day).length};
}
