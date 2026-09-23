import test from 'node:test';
import assert from 'node:assert/strict';
import {projectContext,mapRead} from './provider.mjs';

const media=(type,name)=>({type,url:`https://media.example/${name}`});
const helpers={fastCommentAttachments:value=>Array.isArray(value)?value:[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>undefined,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)};
test('observed branch actors retain their own media and never inherit the publication media',()=>{
 const photo=media('photo','comment.jpg'),sticker=media('sticker','sticker.webp'),video=media('video','reply.mp4'),parent=media('video','publication.mp4');
 const row=projectContext({item:{id:'c',text:'',status:'new',attachments:[photo]},replyTo:{id:'older',text:'',attachments:[sticker]},parent:{id:'post',attachments:[parent]},officialReplies:[{id:'brand',text:'',attachments:[video]}]},'11391',helpers);
 const mapped=mapRead([row],{});
 assert.deepEqual(mapped.items[0].attachments,[photo]);
 assert.deepEqual(mapped.branches[0].messages.map(m=>m.attachments),[[sticker],[photo],[video]]);
 assert.deepEqual(mapped.branches[0].messages.map(m=>m.attachmentsState),['present','present','present']);
 assert.deepEqual(mapped.posts[0].attachments,[parent]);
 assert.ok(!JSON.stringify(mapped.branches).includes('publication.mp4'));
});
test('missing, explicitly empty and unsupported media stay distinct evidence',()=>{
 const mappedFor=extra=>mapRead([projectContext({item:{id:'c',status:'new',...extra},parent:{id:'p'},officialReplies:[]},'11391',helpers)],{});
 for(const [extra,state] of [[{},'unknown'],[{attachments:[]},'none'],[{attachments:[{type:'unsupported'}]},'present']]){
  const mapped=mappedFor(extra);
  assert.equal(mapped.items[0].attachmentsState,state);
  assert.equal(mapped.branches[0].messages[0].attachmentsState,state);
 }
});
