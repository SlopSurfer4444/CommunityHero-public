import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {
  CONTEXT_GROUNDING_GUIDANCE,
  NATURAL_CONTRIBUTION_GUIDANCE,
  REPLY_QUALITY_GUIDANCE
} from '../adapters/assistant-reply-quality-guidance.mjs';

// These are instruction-contract checks, not a model-behavior acceptance suite.
// Their requirements come from the four rejected-reply failure modes and the
// scoped evidence/closure contract. No generated positive reply is a golden.
const context=CONTEXT_GROUNDING_GUIDANCE.replace(/\s+/g,' ');
const natural=NATURAL_CONTRIBUTION_GUIDANCE.replace(/\s+/g,' ');
const guidance=REPLY_QUALITY_GUIDANCE.replace(/\s+/g,' ');
const requires=(text,requirements)=>{
  for(const requirement of requirements)assert.match(text,requirement);
};

test('one shared block carries both contracts exactly once',()=>{
  assert.equal(REPLY_QUALITY_GUIDANCE,CONTEXT_GROUNDING_GUIDANCE+'\n'+NATURAL_CONTRIBUTION_GUIDANCE);
  assert.ok(REPLY_QUALITY_GUIDANCE.length<7000,'Keep reusable guidance bounded');
  for(const marker of ['Resolve the recipient','Make a useful contribution'])
    assert.equal(REPLY_QUALITY_GUIDANCE.split(marker).length-1,1);
});

test('playful factual questions retain their actual video evidence dependency',()=>{
  requires(context,[
    /exact attached post.*branch and admitted media evidence/,
    /premise is not proof.*what a video contains/,
    /Do not invent a scene.*joke origin/,
    /question about a video detail or inside joke requires relevant bound evidence/,
    /emoji or playful wording does not remove.*factual dependency/,
    /Retain transcript and visual coverage limits/
  ]);
});

test('missing essential context preserves scoped hold and authorized recovery',()=>{
  requires(context,[
    /indispensable context is missing.*affected item needs_attention with missing_context and no actionable proposal/,
    /exact missing referent.*operator reason/,
    /recovery step available and authorized in this run.*never claim it already ran/,
    /Do not close an unanswered substantive question or complaint/,
    /Missing incidental detail does not block an independently useful supported reply/
  ]);
});

test('criticism cannot automatically become a fabricated brand admission',()=>{
  requires(natural,[
    /Do not automatically agree with criticism.*brand self-condemnation/,
    /admission of poor work or unsolicited apology/,
    /without endorsing unverified allegations/,
    /substantiated mistake or actual complaint may warrant.*apology or escalation/,
    /under applicable current-account rules.*supported accountability is valid/,
    /Do not reflexively defend the brand/
  ]);
});

test('performance reactions cannot be converted into irrelevant interviews',()=>{
  requires(natural,[
    /performance or presentation into a therapeutic interview/,
    /ask for feelings.*merely to prolong engagement/,
    /clarification must identify a real answer-changing ambiguity/,
    /not manufacture a question from an opinion, joke or context that is already sufficient/
  ]);
  requires(context,[/clarification only if its answer would change.*cannot be resolved from supplied context or available evidence/]);
});

test('sarcastic wishes do not mandate stock humor or invented assurances',()=>{
  requires(natural,[
    /Do not require a joke, emoji, sales route or follow-up question/,
    /Humor must follow the supplied conversational setup/,
    /rather than importing a stock punchline, imagined video detail or unrelated metaphor/,
    /political or duty-related sarcasm.*does not by itself require.*invented promise or an unrelated joke/
  ]);
});

test('thanks and grounded jokes remain possible without blanket reply or close',()=>{
  requires(natural,[
    /Keep thanks, friendly acknowledgement and jokes available.*apt warmth or wit/,
    /None of those intents automatically requires either reply or close/,
    /Close only when the exact branch and current-account rules establish no further public response is needed/,
    /do not use close as a substitute for a held factual answer or complaint/
  ]);
});

test('review judges whole final public text and keeps operator reasoning separate',()=>{
  requires(natural,[
    /Review every sentence, including the ending/,
    /Remove generic filler.*preserving useful supported content/,
    /verification and recovery deliberation in the operator reason/,
    /substantive need remains, hold the item/,
    /generic reaction cannot resolve that need/,
    /Assess the exact final wording/,
    /semantic guidance, not a phrase blacklist or response template/
  ]);
});

test('shared quality guidance remains subordinate and provider/company independent',()=>{
  requires(guidance,[
    /existing current-account policy, recipient, evidence and approval gates/,
    /Never borrow another company's rules, facts, channels or decisions/,
    /neither create company policy nor grant tools or action authority/,
    /examples and prior candidates never become policy or verified facts/
  ]);
  assert.doesNotMatch(guidance,/LikeAvto|BAW|Angry\.Space|likeavto|reply_and_close|lookup=/);
  assert.doesNotMatch(guidance,/С попугаем то что|В ролике про него шутят|Про гусей и кукурузу|Что именно показалось перебором|Трёх желаний джинна/);
});

test('module is static guidance with no source imports or execution surface',async()=>{
  const source=await readFile(new URL('../adapters/assistant-reply-quality-guidance.mjs',import.meta.url),'utf8');
  assert.doesNotMatch(source,/\bimport\s|\bfetch\s*\(|\bprocess\.|\bfunction\s|\bclass\s|\bfs\./);
  assert.equal((source.match(/export const /g)||[]).length,3);
});
