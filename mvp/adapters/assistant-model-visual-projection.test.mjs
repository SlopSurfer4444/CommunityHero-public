import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest} from './assistant.mjs';

const sha = text => createHash('sha256').update(text).digest('hex');

function fixture() {
  const aggregate = Array.from({length: 266}, (_, groupIndex) => ({
    observation: {
      scene: `Сцена ${groupIndex}: показан автомобиль.`,
      text: groupIndex === 0 ? ['На экране: цена от 3 240 000 ₽'] : [],
      numbers: groupIndex === 0 ? [{raw:'от 3 240 000 ₽',value:'3240000',unit:null,currency:'₽',uncertain:false}] : [],
      uncertainties: groupIndex === 1 ? ['Текст на табличке не читается.'] : []
    },
    sources: []
  }));
  for (let index = 0; index < 974; index++) aggregate[index % aggregate.length].sources.push({
    frameIndex:index,pts:String(index * 1000),timestampMs:index * 2000,pixelSha256:sha(`frame-${index}`)
  });
  const evidence = {
    schemaVersion:2,
    source:{account:'LikeAvto',postKey:'post:one',mediaSha256:sha('source video'),durationMs:2_000_000},
    sourcePostVersion:sha('post version'),
    finalEvidence:{sha256:sha('final proof'),bytes:500_000},
    coverage:{kind:'all_frames_fast_selected_neural',selectionPolicySha256:sha('selection policy'),frameCount:1000,
      selectedFrameCount:974,coveredSelectedFrameCount:974,uniqueReviewedFrames:974},
    aggregateOverflow:false,aggregate
  };
  const request = {
    account:'likeavto',purpose:'triage',
    items:[{id:'comment-1',postId:'post',text:'Какая цена?'},{id:'comment-2',postId:'post',text:'Что на табличке?'}],
    posts:[{id:'post',postKey:'post:one',title:'Пост с видео'}],
    materials:[
      {id:'asr',kind:'transcript',postKey:'post:one',text:'Полная запись речи.',transcription:{partial:false,coverage:'full'}},
      {id:'ocr',kind:'ocr',postKey:'post:one',text:'Цена от 3 240 000 ₽; покрытие OCR частичное.'},
      {id:'visual',kind:'visual_context',trust:'source_only',postKey:'post:one',knowledgeEntryId:'visual-entry',
        knowledgeVersionId:'visual-version',text:'Старый обзор кадров; отсутствие детали не доказано.',visualEvidence:evidence}
    ],
    knowledgeManifest:[{entryId:'visual-entry',versionId:'visual-version',kind:'visual_context',trust:'source_only',hash:sha('manifest')}]
  };
  return {request,evidence};
}

test('validated visual proof projects observations and exact offsets without per-frame bookkeeping', t => {
  const {request,evidence}=fixture();
  const {payload,input}=prepareAssistantRequest(request);
  const projected=payload.materials[2].visualEvidence;
  assert.equal(projected.evidenceSha256,evidence.finalEvidence.sha256);
  assert.equal(projected.sourcePostVersion,evidence.sourcePostVersion);
  assert.deepEqual(projected.coverage,{kind:evidence.coverage.kind,frameCount:1000,
    selectedFrameCount:974,coveredSelectedFrameCount:974,uniqueReviewedFrames:974});
  assert.deepEqual(projected.aggregate.map(group=>group.sourceTimestampsMs).flat().sort((a,b)=>a-b),
    Array.from({length:974},(_,index)=>index*2000));
  assert.deepEqual(projected.aggregate[0].observation,evidence.aggregate[0].observation);
  assert.deepEqual(projected.aggregate[1].observation.uncertainties,['Текст на табличке не читается.']);
  assert.equal(projected.aggregate[0].id,'group-1');
  assert.equal(projected.aggregate[1].id,'group-2');
  assert.equal(payload.materials[0].text,'Полная запись речи.');
  assert.equal(payload.materials[1].text,'Цена от 3 240 000 ₽; покрытие OCR частичное.');
  assert.equal(projected.source.account,'LikeAvto');
  assert.ok(!input.includes(sha('frame-0')));
  assert.ok(!input.includes('pixelSha256'));
  assert.ok(!input.includes('frameIndex'));
  assert.ok(!input.includes('selectionPolicySha256'));
  assert.ok(!input.includes('"pts"'));
  // Reconstruct the previous assistant payload shape for an exact synthetic
  // before/after UTF-8 comparison. This is not a measured live D13 request.
  const previousPayload=structuredClone(payload);
  previousPayload.materials[2].visualEvidence=evidence;
  const beforeBytes=Buffer.byteLength(JSON.stringify(previousPayload));
  const afterBytes=Buffer.byteLength(input);
  t.diagnostic(`synthetic 974-frame source fixture: ${beforeBytes} -> ${afterBytes} UTF-8 JSON bytes`);
  assert.ok(beforeBytes-afterBytes>100_000,{beforeBytes,afterBytes});
});

test('proof and source binding validation still runs before model projection', () => {
  for (const change of [
    evidence=>{evidence.aggregate[0].sources[0].pixelSha256='bad';},
    evidence=>{evidence.aggregate[0].sources[0].pixelSha256=evidence.aggregate[1].sources[0].pixelSha256;},
    evidence=>{evidence.coverage.coveredSelectedFrameCount=973;},
    evidence=>{evidence.aggregate[0].observation.numbers[0].uncertain=true;},
    evidence=>{evidence.source.account='BAW Russia';}
  ]) {
    const {request,evidence}=fixture();
    change(evidence);
    assert.throws(()=>prepareAssistantRequest(request),{code:'ASSISTANT_INVALID_REQUEST'});
  }
});

test('legacy V1 proof keeps sampled observations and uncertainty without manifest bookkeeping', () => {
  const source={account:'LikeAvto',postKey:'post:legacy',mediaSha256:sha('legacy media'),durationMs:1100};
  const coverage={kind:'sampled_frames',samplingVersion:1,durationMs:1100,regularIntervalMs:2000,
    tailWindowMs:10000,tailIntervalMs:1000,maxGapMs:1000,tailStartMs:0,endingFrameId:'frame-2'};
  const manifest={schemaVersion:1,workId:'media-00000000-0000-4000-8000-000000000001',
    createdAtUtc:'2026-09-28T00:00:00Z',source,coverage,frames:[
      {id:'frame-1',sha256:sha('first frame'),timestampMs:0},
      {id:'frame-2',sha256:sha('second frame'),timestampMs:1000}
    ]};
  const stableJson=value=>Array.isArray(value)?`[${value.map(stableJson).join(',')}]`:
    value&&typeof value==='object'?`{${Object.keys(value).sort().map(key=>`${JSON.stringify(key)}:${stableJson(value[key])}`).join(',')}}`:
      JSON.stringify(value);
  const evidence={schemaVersion:1,manifest,durableManifestSha256:sha(stableJson(manifest)),
    result:{schemaVersion:1,status:'incomplete',source,manifestSha256:sha('manifest'),coverage,
      frames:[
        {id:'frame-1',sha256:manifest.frames[0].sha256,timestampMs:0,status:'readable',
          scene:'Табличка с ценой.',text:['12 900 ₽'],
          numbers:[{raw:'12 900 ₽',value:'12900',unit:null,currency:'₽',uncertain:false}],uncertainties:[]},
        {id:'frame-2',sha256:manifest.frames[1].sha256,timestampMs:1000,status:'unreadable',
          scene:'Нечёткая надпись.',text:[],numbers:[],uncertainties:['Надпись не читается.']}
      ],summary:'Выбранные кадры; часть надписи не читается.',
      provenance:{backend:'local_ollama',model:'synthetic-local',instructionSha256:sha('instruction')}}};
  const request={account:'likeavto',items:[{id:'item',postId:'post',text:'Какая цена?'}],
    posts:[{id:'post',postKey:'post:legacy'}],
    materials:[{id:'legacy',kind:'visual_context',trust:'source_only',postKey:'post:legacy',
      text:evidence.result.summary,knowledgeEntryId:'entry',knowledgeVersionId:'version',visualEvidence:evidence}],
    knowledgeManifest:[{entryId:'entry',versionId:'version',kind:'visual_context',trust:'source_only',hash:sha('entry')}]};
  const {payload,input}=prepareAssistantRequest(request);
  const projected=payload.materials[0].visualEvidence;
  assert.equal(projected.modelProjectionVersion,1);
  assert.equal(projected.schemaVersion,1);
  assert.equal(projected.evidenceSha256,evidence.durableManifestSha256);
  assert.deepEqual(projected.source,source);
  assert.deepEqual(projected.coverage,coverage);
  assert.deepEqual(projected.frames.map(frame=>frame.timestampMs),[0,1000]);
  assert.deepEqual(projected.frames[0].text,['12 900 ₽']);
  assert.deepEqual(projected.frames[0].numbers,evidence.result.frames[0].numbers);
  assert.deepEqual(projected.frames[1].uncertainties,['Надпись не читается.']);
  assert.equal(projected.frames[1].status,'unreadable');
  assert.equal(projected.frames[1].scene,'Нечёткая надпись.');
  assert.equal(projected.manifest,undefined);
  assert.equal(projected.durableManifestSha256,undefined);
  assert.equal(projected.frames[0].sha256,undefined);
  assert.ok(!input.includes(manifest.frames[0].sha256));
  assert.ok(!input.includes('createdAtUtc'));
  const invalid=structuredClone(request);
  invalid.materials[0].visualEvidence.result.frames[0].sha256=sha('tampered');
  assert.throws(()=>prepareAssistantRequest(invalid),{code:'ASSISTANT_INVALID_REQUEST'});
});
