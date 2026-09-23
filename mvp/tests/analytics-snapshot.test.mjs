import test from 'node:test';
import assert from 'node:assert/strict';
import {analyticsSnapshot, analyticsKinds} from '../workshop/analytics-snapshot.js';

const now=new Date('2026-09-22T12:00:00Z');
const record=(view='attention',createdAt='2026-09-22T09:00:00Z',outcome)=>({createdAt,state:{view,closure:{outcome}}});

test('all open queues count toward new total while chart segments never overlap',()=>{
  const report=analyticsSnapshot(['attention','prepared','waiting','closed','deleted'].map(view=>record(view)),{},now);
  assert.equal(report.open,3);assert.equal(report.allOpen,3);
  assert.equal(report.counts.new,3);assert.equal('working' in report.counts,false);
  assert.equal(report.series[0].new,3);assert.equal(analyticsKinds.find(kind=>kind.key==='new').label,'Новые');
  assert.equal(report.total,5);assert.equal(report.series[0].total,5);
  assert.equal(analyticsKinds.reduce((n,{key})=>n+report.counts[key],0),5);
});

test('closed items without a confirmed reply share the display bucket without changing source evidence',()=>{
  const rows=[record('closed'),record('closed',undefined,'unknown'),record('closed',undefined,'reply'),record('closed',undefined,'no_reply')];
  const original=structuredClone(rows);
  const report=analyticsSnapshot(rows,{},now);
  assert.equal('unknown' in report.counts,false);assert.equal(report.counts.reply,1);assert.equal(report.counts.no_reply,3);
  assert.equal(report.closed,4);assert.equal(report.total,4);
  assert.equal(report.series[0].no_reply,3);assert.equal(report.series[0].total,4);
  assert.equal(analyticsKinds.some(kind=>kind.key==='unknown'),false);
  assert.equal(analyticsKinds.reduce((n,{key})=>n+report.counts[key],0),4);
  assert.deepEqual(rows,original);
});

test('current state is dated by creation and repeated historical events do not inflate counts',()=>{
  const row=record('closed','2026-09-21T20:59:00Z','reply');
  row.state.events=[{type:'closed',at:now.toISOString(),outcome:'reply'},{type:'reopened',at:now.toISOString()},{type:'closed',at:now.toISOString(),outcome:'reply'}];
  const report=analyticsSnapshot([row],{from:'2026-09-21',to:'2026-09-22'},now);
  assert.equal(report.series.length,2);assert.equal(report.series[0].reply,1);assert.equal(report.series[1].total,0);
  assert.equal(report.total,1);
});

test('selected range uses Moscow creation dates while backlog and rolling 24h retain their scopes',()=>{
  const report=analyticsSnapshot([record('attention','2026-09-21T21:00:00Z'),record('waiting','2026-09-01T12:00:00Z'),record('closed','2026-09-21T12:00:00Z','reply'),record('prepared','2026-09-23T12:00:00Z')],{from:'2026-09-22',to:'2026-09-22'},now);
  assert.equal(report.total,1);assert.equal(report.open,1);assert.equal(report.allOpen,3);assert.equal(report.last24,1);
  assert.equal(report.from,'2026-09-22');assert.equal(report.to,'2026-09-22');
});

test('30 day ranges include empty daily buckets and deleted comments',()=>{
  const report=analyticsSnapshot([record('deleted','2026-09-22T01:00:00Z')],{from:'2026-08-24',to:'2026-09-22'},now);
  assert.equal(report.series.length,30);assert.equal(report.monthly,false);
  assert.equal(report.series.at(-1).deleted,1);assert.equal(report.series[0].total,0);
});

test('empty, undated and inverted periods stay finite and explicit',()=>{
  const missing=analyticsSnapshot([record('attention','invalid')],{},now);
  assert.equal(missing.undated,1);assert.equal(missing.allOpen,1);assert.equal(missing.total,0);assert.deepEqual(missing.series,[]);
  const reversed=analyticsSnapshot([record()],{from:'2026-09-23',to:'2026-09-21'},now);
  assert.equal(reversed.total,0);assert.deepEqual(reversed.series,[]);
});

test('long imported ranges aggregate to months without losing or duplicating comments',()=>{
  const report=analyticsSnapshot([record('attention','2026-01-15T12:00:00Z'),record('deleted','2026-09-22T12:00:00Z')],{},now);
  assert.equal(report.monthly,true);assert.equal(report.series.length,9);
  assert.equal(report.series.reduce((n,day)=>n+day.total,0),2);
});
