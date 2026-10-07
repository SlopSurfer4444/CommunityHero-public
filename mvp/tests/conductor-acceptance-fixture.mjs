// Synthetic model/provider only. No network, credentials or actual Codex process.
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,admitSinglePassResult,singlePassMetadata,
  editorialMetadata,editorialInstructions,validateEditorialResult,generationMetadata} from '../adapters/assistant.mjs';
const sha=value=>createHash('sha256').update(value).digest('hex');
export const accountKeys=['likeavto','baw-russia'];
export const displayAccount=key=>key==='likeavto'?'LikeAvto':'BAW Russia';
export function fixtureSnapshot(account,count=1200) {
  if(!accountKeys.includes(account)||!Number.isSafeInteger(count)||count<1||count>5000)throw Error('Invalid isolated fixture scope');
  const posts=[],branches=[],items=[];
  for(let index=0;index<count;index++) {
    const family=Math.floor(index/200),postId=`post-${account}-${family}`,postKey=`${account}:post-${family}`;
    const id=`item-${account}-${index}`,text=`Synthetic factual question ${index}`;
    if(!posts.some(post=>post.id===postId))posts.push({id:postId,postKey,title:`Synthetic public post ${family}`,
      text:family<2?'Synthetic mirrored public source':`Synthetic public source ${family}`,channel:family===1?'YOUTUBE':'VK',
      sourceUrl:`https://fixture.example/${account}/post-${family}`});
    const item={id,itemId:`${account}-provider-${index}`,objectId:`${account}-object-${family}`,postId,postKey,
      conversationKey:`${account}:conversation-${index}`,contextEvidenceDigest:'a'.repeat(64),branchId:`branch-${account}-${index}`,
      targetId:`comment-${account}-${index}`,createdAt:new Date(Date.UTC(2026,9,1,0,0,index)).toISOString(),providerStatus:'new',
      status:'new',platform:family===1?'youtube':'vk',workflow:'attention',draft:'',revision:1,author:`Synthetic author ${index}`,
      text,commentText:text,expectedStatuses:['new']};
    items.push(item);branches.push({id:item.branchId,postId,contextComplete:true,missingParentIds:[],contextTruncated:false,
      messages:[{id:item.targetId,author:item.author,text,role:'customer',createdAt:item.createdAt}]});
  }
  return {account,posts,branches,items,hasMore:false,cursor:null,coverage:'complete synthetic fixture',observedCount:count,skipped:[],
    queueAccounting:{version:1,observations:items.map(({objectId,itemId})=>({objectId,itemId,contextRequired:true})),duplicateQueueCount:0},
    live:{account:displayAccount(account),fetchedAt:new Date().toISOString(),hasMore:false,observedCount:count,readOnly:true}};
}
export function syntheticModel(request,scenario={}) {
  const prepared=prepareAssistantRequest(request);
  if(request.purpose==='editorial_review') {
    const value={text:'Synthetic independent exact editorial decision',sources:[],proposals:[],editorial:request.editorialCandidates.map(candidate=>{
      const revise=(scenario.reviseItemIds??[]).includes(candidate.itemId)&&!candidate.text.startsWith('Repaired synthetic reply');
      return {...Object.fromEntries(['proposalId','proposalRevision','itemId','textSha256','contextDigest','rulesDigest'].map(key=>[key,candidate[key]])),
        decision:revise?'revise':'accept',reason:revise?'Synthetic exact wording correction':'Synthetic fresh final revision accepted',
        proposedText:revise?`Repaired synthetic reply ${candidate.itemId}`:null,
        checks:{intent:revise?'fail':'pass',companyRules:'pass',factualScope:'pass'}};
    })};
    return {...validateEditorialResult(value,prepared),runMetadata:{...generationMetadata(prepared.input,false,1,prepared.account.accountKey),
      ...editorialMetadata(prepared,editorialInstructions(prepared.account.accountKey))}};
  }
  if(request.purpose!=='triage'||!prepared.singlePass)throw Error('Fixture admits only captured single-pass preparation/editorial');
  const held=new Set(scenario.heldItemIds??[]),checks={intent:'pass',companyRules:'pass',factualScope:'pass'};
  const proposals=request.items.filter(item=>!held.has(item.id)).map(item=>({itemId:item.id,kind:'reply_and_close',text:`Synthetic initial reply ${item.id}`}));
  const value={text:'Synthetic full selected preparation',sources:[],proposals,
    assessments:request.items.map(item=>({itemId:item.id,outcome:held.has(item.id)?'needs_attention':'reply',
      reason:held.has(item.id)?'Synthetic private current stock needs owner evidence':'Synthetic supplied evidence is sufficient',tags:held.has(item.id)?['needs_fact']:[]})),
    ...(request.factDependencyContract==='targeted_public_v1'?{factDependencies:[]}:{}),
    ...(prepared.visualNeeds?{visualNeeds:[]}:{}),
    evidence:[],generationEditorial:proposals.map(proposal=>({...proposal,decision:'accept',reason:'Synthetic generation checks passed',checks})),
    decisionEvidence:request.items.map(item=>({itemId:item.id,basis:held.has(item.id)?'unresolved':'context',evidenceIndices:[],dependsOnItemIds:[]})),moderationEvidence:[]};
  const admitted=admitSinglePassResult(value,prepared,{calls:0,openedUrls:[],completedActivity:[],webCallLimit:null});
  return {...admitted.admitted,runMetadata:singlePassMetadata(prepared,admitted,1)};
}
export const fixtureHash=sha;
