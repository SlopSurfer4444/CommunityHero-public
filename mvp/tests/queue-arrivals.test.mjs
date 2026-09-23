import test from 'node:test';
import assert from 'node:assert/strict';
import {ARRIVAL_MAX_HOLD_MS,arrivalAnimationIds,arrivalCandidateIds,arrivalGenerationSignature,arrivalIsProcessing,projectQueueArrivals} from '../workshop/queue-arrivals.js';

const now=Date.parse('2026-09-22T12:00:00Z');
const record=(id,status='queued',changes={})=>({
  item:{id,workflow:'attention',providerStatus:'new',draft:'',draftEdited:false,
    autoPreparation:status?{status}:null,...changes.item},
  state:{view:'attention',draft:'',...changes.state}
});

test('only explicitly new pending arrivals are held',()=>{
  const queued=record('queued'),baseline=record('baseline');
  const result=projectQueueArrivals([queued,baseline],{candidateIds:['queued'],now});
  assert.deepEqual(result.visible.map(row=>row.item.id),['baseline']);
  assert.deepEqual(result.processing.map(row=>row.item.id),['queued']);
  assert.equal(result.arrivals.queued.firstSeenAt,now);
  assert.equal(result.arrivals.queued.afterBaseline,true);
});

test('a pending arrival remains held across reload with its original deadline',()=>{
  const arrivals={new:{firstSeenAt:now-1000,afterBaseline:true}};
  const result=projectQueueArrivals([record('new')],{arrivals,now,establishBaseline:true});
  assert.equal(result.processing.length,1);
  assert.equal(result.arrivals.new.firstSeenAt,now-1000);
  assert.notEqual(result.arrivals,arrivals);
});

test('terminal preparation outcomes and unknown states fail open',()=>{
  for(const status of ['prepared','needs_attention','error','stale','unexpected']){
    const result=projectQueueArrivals([record(status,status)],{arrivals:{[status]:{firstSeenAt:now-1000}},now});
    assert.deepEqual(result.visible.map(row=>row.item.id),[status]);
    assert.deepEqual(result.admittedIds,[status]);
    assert.equal(result.arrivals[status],undefined);
  }
});

test('media/source waits and not-yet-queued new provider comments are bounded',()=>{
  const media=record('media',null,{item:{preparationMediaWait:{until:'2026-09-22T12:10:00Z'}}});
  const unqueued=record('unqueued',null);
  assert.equal(arrivalIsProcessing(media),true);
  assert.equal(arrivalIsProcessing(unqueued),true);
  const held=projectQueueArrivals([media,unqueued],{candidateIds:['media','unqueued'],now});
  assert.equal(held.processing.length,2);
  const released=projectQueueArrivals([media,unqueued],{arrivals:held.arrivals,now:now+ARRIVAL_MAX_HOLD_MS});
  assert.deepEqual(released.visible.map(row=>row.item.id),['media','unqueued']);
  assert.deepEqual(released.admittedIds,['media','unqueued']);
});

test('selection, manual work, proposals and saved revalidation are never hidden',()=>{
  const cases=[
    [record('selected'),'selected'],
    [record('manual','running',{state:{manualEdited:true}}),''],
    [record('draft','running',{item:{draft:'Сохранено',draftEdited:true}}),''],
    [record('proposal','running',{state:{_sourceProposalId:'proposal'}}),''],
    [record('held','running',{item:{autoPreparation:{status:'running',requiresReview:true,savedProposalId:'old'}}}),''],
    [record('revalidation','running',{item:{autoRevalidation:{status:'running'}}}),'']
  ];
  for(const [row,selectedId] of cases){
    const id=row.item.id,result=projectQueueArrivals([row],{arrivals:{[id]:{firstSeenAt:now}},selectedId,now});
    assert.deepEqual(result.visible.map(entry=>entry.item.id),[id]);
    assert.equal(result.arrivals[id],undefined);
  }
});

test('remote generation signature ignores observation clocks but catches eligibility changes',()=>{
  const base={id:'one',revision:1,workflow:'attention',providerObservedAt:'old',autoPreparation:{status:'queued'}};
  assert.equal(arrivalGenerationSignature([base]),arrivalGenerationSignature([{...base,providerObservedAt:'new'}]));
  assert.notEqual(arrivalGenerationSignature([base]),arrivalGenerationSignature([{...base,autoPreparation:{status:'prepared'}}]));
  assert.notEqual(arrivalGenerationSignature([base]),arrivalGenerationSignature([base,{...base,id:'two'}]));
  assert.notEqual(arrivalGenerationSignature([base]),arrivalGenerationSignature([{...base,initialState:{_sourceProposalId:'proposal'}}]));
});

test('entry animation requires a remote admission in the currently listed view',()=>{
  const projection={admittedIds:['ready','filtered-out']};
  const options={remoteChanged:true,listedIds:['ready']};
  assert.deepEqual(arrivalAnimationIds(projection,options),['ready']);
  assert.deepEqual(arrivalAnimationIds(projection,{...options,remoteChanged:false}),[]);
  assert.deepEqual(arrivalAnimationIds(projection,{...options,reducedMotion:true}),[]);
  assert.deepEqual(arrivalAnimationIds(projection,{...options,inQueueView:false}),[]);
});

test('first bootstrap establishes the visible baseline; only later remote IDs are arrivals',()=>{
  const initial=[{id:'backlog-a'},{id:'backlog-b'}];
  assert.deepEqual(arrivalCandidateIds(initial,{initial:true,knownIds:[]}),[]);
  assert.deepEqual(arrivalCandidateIds([...initial,{id:'new'}],{knownIds:initial.map(item=>item.id)}),['new']);
  const baseline=projectQueueArrivals(initial.map(item=>record(item.id)),{
    candidateIds:arrivalCandidateIds(initial,{initial:true}),now
  });
  assert.deepEqual(baseline.visible.map(row=>row.item.id),['backlog-a','backlog-b']);
  assert.equal(baseline.processing.length,0);
  const migrated=projectQueueArrivals(initial.map(item=>record(item.id)),{
    arrivals:{'backlog-a':{firstSeenAt:now}},establishBaseline:true,now
  });
  assert.deepEqual(migrated.visible.map(row=>row.item.id),['backlog-a','backlog-b']);
  assert.deepEqual(migrated.arrivals,{});
});
