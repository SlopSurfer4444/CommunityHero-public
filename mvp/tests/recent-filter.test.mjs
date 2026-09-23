import test from 'node:test';
import assert from 'node:assert/strict';
import {filterList, dateBasis, recordDate, validDate, initializeRecentListFilters, hasAppliedListConditions, resetListConditions} from '../workshop/list-filters.js';

const now = new Date('2026-09-21T12:30:00Z');
const record = (id, createdAt, view = 'attention') => ({
  item:{id,targetId:id}, createdAt, postId:'post', channel:'vk',
  state:{view,closure:{at:now.toISOString(),outcome:'reply'},deletion:{at:now.toISOString()}},
  messages:[{id:'old-parent',text:'Earlier context',createdAt:'2020-01-01T00:00:00Z'},
    {id,author:'Author',text:'Comment',createdAt},
    {id:'old-sibling',text:'Sibling context',createdAt:'2020-01-01T00:00:00Z'}]
});
const options = {view:'attention',filters:{period:'recent48'},now};

test('removed legacy period cannot hide older comments', () => {
  const records=[record('old','2020-01-01'),record('recent',now.toISOString())];
  assert.deepEqual(filterList(records,{...options,filters:{period:'recent48',from:'2026-09-21',to:'2026-09-21'}}).map(r=>r.item.id),['recent','old']);
});
test('newest and oldest ordering use selected date basis and keep unknown dates last', () => {
  const records=[record('old','2020-01-01'),record('new',now.toISOString()),record('unknown',null)];
  assert.deepEqual(filterList(records,{...options,filters:{period:'all'}}).map(r=>r.item.id),['new','old','unknown']);
  assert.deepEqual(filterList(records,{...options,filters:{period:'all'},order:'oldest'}).map(r=>r.item.id),['old','new','unknown']);
});

test('closed rows with missing closure dates are unknown and sort by comment date in both directions', () => {
  const early=record('early','2026-09-19T10:00:00Z','closed');early.state.closure.at=null;
  const late=record('late','2026-09-22T10:00:00Z','closed');late.state.closure.at=null;
  const known=record('known','2026-09-01T10:00:00Z','closed');known.state.closure.at='2026-09-23T10:00:00Z';
  const records=[early,known,late];
  assert.equal(validDate(early.state.closure.at),null);
  const base={view:'closed',filters:{period:'all',dateField:'closed'},outcome:'all'};
  assert.deepEqual(filterList(records,{...base,order:'newest'}).map(r=>r.item.id),['known','late','early']);
  assert.deepEqual(filterList(records,{...base,order:'oldest'}).map(r=>r.item.id),['known','early','late']);
  assert.equal(early.state.closure.at,null);
});

test('legacy display fallback cannot turn a missing source comment date into evidence',()=>{
  const imported=record('imported','2026-09-08T00:00:00Z');
  imported.sourceCreatedAt=null;
  assert.equal(recordDate(imported,'created'),null);
  assert.equal(validDate(recordDate(imported,'created')),null);
});

test('list filtering retains every branch message without mutation', () => {
  const recent=record('recent',now.toISOString()), old=record('old','2020-01-01T00:00:00Z');
  const original=structuredClone([recent,old]);
  const result=filterList([recent,old],{...options,filters:{period:'custom',from:'2026-09-21',to:'2026-09-21'}});
  assert.equal(result.length,1);
  assert.equal(result[0],recent);
  assert.equal(result[0].messages.length,3);
  assert.deepEqual([recent,old],original);
});

test('one-time default preserves explicit periods and subsequent all choice', () => {
  const saved={listFilters:{attention:{period:'all',channel:'vk'},
    prepared:{period:'week'},waiting:{period:'custom',from:'2026-01-01'},
    closed:{from:'2026-02-01'},deleted:{period:'all',to:'2026-03-01'}}};
  const views=['attention','prepared','waiting','closed','deleted','newQueue'];
  initializeRecentListFilters(saved,views);
  assert.deepEqual(saved.listFilters.attention,{period:'all',channel:'vk'});
  assert.equal(saved.listFilters.newQueue.period,'all');
  assert.equal(saved.listFilters.prepared.period,'week');
  assert.equal(saved.listFilters.waiting.period,'custom');
  assert.equal(saved.listFilters.closed.from,'2026-02-01');
  assert.equal(saved.listFilters.closed.period,undefined);
  assert.equal(saved.listFilters.deleted.period,'all');
  saved.listFilters.attention={period:'all'};
  const persisted=JSON.parse(JSON.stringify(saved));
  initializeRecentListFilters(persisted,views);
  assert.equal(persisted.listFilters.attention.period,'all');
});

test('old automatic48h default is removed from open queues once without erasing explicit filters',()=>{
  const saved={openQueueDefaultVersion:2,recent48DefaultApplied:true,listFilters:{attention:{period:'recent48',channel:'vk'},prepared:{period:'recent48'},waiting:{period:'custom',from:'2020-01-01'},closed:{period:'recent48'}}};
  initializeRecentListFilters(saved,['attention','prepared','waiting','closed']);
  assert.equal(saved.listFilters.attention.period,'all');
  assert.equal(saved.listFilters.attention.channel,'vk');
  assert.equal(saved.listFilters.prepared.period,'all');
  assert.equal(saved.listFilters.waiting.from,'2020-01-01');
  assert.equal(saved.listFilters.closed.period,'all');
  saved.listFilters.prepared.period='week';
  initializeRecentListFilters(saved,['prepared']);
  assert.equal(saved.listFilters.prepared.period,'week');
});

test('existing all, custom and week filters retain their date semantics', () => {
  const records=[record('older','2026-09-15T00:00:00Z'),record('today',now.toISOString())];
  assert.equal(filterList(records,{...options,filters:{period:'all'}}).length,2);
  assert.equal(filterList(records,{...options,filters:{period:'week'}}).length,2);
  assert.deepEqual(filterList(records,{...options,filters:{period:'custom',from:'2026-09-15',to:'2026-09-15'}}).map(r=>r.item.id),['older']);
});

test('answer section is not presented as a filter condition', () => {
  const saved={search:'',filter:'reply',listFilters:{prepared:{period:'all'}}};
  assert.equal(hasAppliedListConditions(saved.search,saved.listFilters.prepared),false);
  saved.filter='no_reply';
  assert.equal(hasAppliedListConditions(saved.search,saved.listFilters.prepared),false);
  assert.equal(hasAppliedListConditions('текст',{period:'all'}),true);
  assert.equal(hasAppliedListConditions('',{period:'all',channel:'vk'}),true);
});

test('reset clears search and actual filters while preserving answer section', () => {
  const saved={view:'prepared',search:'найти',filter:'no_reply',listFilters:{prepared:{period:'week',channel:'vk'},closed:{period:'all'}}};
  resetListConditions(saved);
  assert.equal(saved.search,'');
  assert.equal(saved.filter,'no_reply');
  assert.deepEqual(saved.listFilters.prepared,{period:'all'});
  assert.deepEqual(saved.listFilters.closed,{period:'all'});
});
