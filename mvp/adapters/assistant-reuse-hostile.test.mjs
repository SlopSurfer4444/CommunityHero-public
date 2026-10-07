import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {validateExactFileAnalysisBinding} from './assistant-media-analysis-reuse.mjs';

// Source-shaped parser checks only. Locator-array field order is taken from
// native media_analysis_reuse::attachment_identity; no native execution claimed.
const sha=value=>createHash('sha256').update(JSON.stringify(value)).digest('hex');
const digest=c=>c.repeat(64);
function data(rawAttachment) {
  const attachment=JSON.parse(rawAttachment);
  const account={displayName:'LikeAvto',providerAccountId:'likeavto'};
  const connector={id:'connection',workspaceId:'isolated',accountId:'LikeAvto',connector:'angryspace',revision:1,providerAccountId:'likeavto'};
  const material={kind:'transcript',account:'LikeAvto',trust:'source_only',postKey:'donor',mediaSha256:digest('f'),text:'Source-shaped speech',
    knowledgeEntryId:'e',knowledgeVersionId:'v',transcription:{partial:false,sourceVersion:digest('b'),sourcePostKey:'donor',
      coverage:'full_audio',mediaDurationSeconds:1,audioDurationSeconds:1}};
  const entry={kind:'transcript',trust:'source_only',entryId:'e',versionId:'v',hash:digest('c')};
  const post={id:'target',postKey:'target',attachments:[attachment],mediaPolicy:{sourceVersion:digest('a')}};
  const edge={schemaVersion:1,match:'verified_exact_file_analysis_reuse',postKey:'target',targetPostId:'target',sourcePostKey:'donor',
    companyId:'LikeAvto',connectorBinding:connector,target:{connectorBinding:connector,postId:'target',postKey:'target',sourceVersion:digest('a'),
      attachmentIndex:0,aliasRevision:1,attachmentIdentity:sha(['video','https://fixture.invalid/native', 'https://fixture.invalid/legacy','https://fixture.invalid/file',null,null])},
    verifiedFile:{sha256:digest('f'),bytes:10,receiptSha256:digest('d'),probeSha256:digest('e')},proofSha256:digest('1'),resultSha256:digest('2'),
    specSha256:digest('3'),normalizedOutput:{sha256:digest('4'),bytes:10},transcript:{entryId:'e',versionId:'v',hash:digest('c')},
    originalSourceVersion:digest('b'),coverage:{kind:'full_audio',durationMs:1000},screenReuse:false};
  return {edge,entry,material,posts:[post],connector,account};
}
const raw=n=>`{"sourceUrl":"https://fixture.invalid/native","source_url":"https://fixture.invalid/legacy","type":"video","url":"https://fixture.invalid/file","duration":${n}}`;
const admit=input=>validateExactFileAnalysisBinding(input.edge,input.entry,input.material,input.posts,input.connector,input.account);

test('mixed locator keys and ancillary 1.0 metadata use the native ordered string/null identity',()=>{
  for(const number of ['1.0','1','1e0'])assert.deepEqual(admit(data(raw(number))),data(raw(number)).edge);
});

test('locator equality cannot excuse changed native source-version or attachment-index pins',()=>{
  for(const mutation of [
    input=>input.posts[0].mediaPolicy.sourceVersion=digest('9'),
    input=>input.edge.target.attachmentIndex=1,
    input=>input.posts[0].attachments[0].url='https://fixture.invalid/changed',
    input=>input.edge.screenReuse=true,
    input=>input.connector.revision=0
  ]) {
    const input=data(raw('1.0'));mutation(input);
    assert.throws(()=>admit(input),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_ANALYSIS_REUSE'});
  }
});
