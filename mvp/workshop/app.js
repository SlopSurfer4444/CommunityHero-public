import {bindEmojiPicker,emojiButton} from './emoji-picker.js';
import {mediaPreparationHold,replyReadiness,updateMediaActionControls} from './preparation-readiness.js';
import {requireOperator,revokeOperatorSession} from './operator-session.js';
import {buildAssistantContext} from './assistant-context.js';
import {bindAnalyticsTooltips} from './analytics-tooltip.js';
import {analyticsKinds, analyticsSnapshot} from './analytics-snapshot.js';
import {channelBadge} from './social-icons.js';
import {sortDiscussions} from './post-topics.js';
import {authorHistory} from './author-history.js';
import {createMvpConnection} from './mvp-connection.js';
import {displayInstructions} from './active-instructions.js';
import {createEntityIndex} from './entity-index.js';
import {chronologicalSiblings} from './thread-order.js';
import {outcomeCounts,queueTags,postThumbnailCandidates,initializeOverviewPeriod} from './workspace-presentation.js';
import {isOpen, replyFor, closeRecord, reopenRecord, previewClosure, applyClosure, reconcileFixtureState} from './lifecycle.js';
import {initializeRecentListFilters, filterList, matchesList, dateBasis, recordDate, validDate, calendarDay, resolvePeriod, overviewSnapshot, hasAppliedListConditions, resetListConditions} from './list-filters.js';
import {LIST_PAGE, LIST_MAX_ROWS, ESTIMATED_ROW_HEIGHT, initialListWindow, listWindowAfterSelection, listWindowAfterConditionsChange, listWindowAfterWidthChange, nextListWindow, previousListWindow} from './list-window.js';
import {resolveColumns, resizeColumns, maximumColumnWidth, COLUMN_LIMITS} from './column-layout.js';
import {restoreInPrototype, undoPrototypeRestore} from './assistant-actions.js';
import {concepts as capeConcepts} from './hero-cape-combinations.js';
import {arrivalAnimationIds,arrivalCandidateIds,arrivalGenerationSignature,projectQueueArrivals} from './queue-arrivals.js';
import {bindCommentVideoErrors,commentMediaHtml} from './comment-media.js';
const liveReadMode = false;
let mvp;
let assistantNavigationRevision = 0;
const shell = document.querySelector('#shell');
const operator = await requireOperator(shell);
const storageKey = operator.id==='local-owner' ? 'communityhero-mvp-original-workshop-v1' : `communityhero-operator-${operator.id}-v1`;
const demoHeader=document.querySelector('.app-header'),brandNode=demoHeader.querySelector('.wordmark');
brandNode.setAttribute('aria-label','CommunityHero — обзор');brandNode.title='CommunityHero — обзор';
brandNode.innerHTML=`<span class="brand-emblem" aria-hidden="true">${capeConcepts.find(({id})=>id==='cape-2').svg.replace('<svg ', '<svg focusable="false" ')}</span><span class="brand-name">CommunityHero</span>`;
let bar = null;
if (false) {
  bar = document.createElement('div'); bar.id = 'study-bar'; bar.className = 'study-bar';
  bar.setAttribute('aria-label','Учебные задания'); document.querySelector('.app-header').after(bar);
}
let data, icons, saved = {}, editSession = false, readingObserver, composerObserver;
try { saved = JSON.parse(localStorage.getItem(storageKey) || '{}'); } catch {}
let queueArrivalProjection={visible:[],processing:[],arrivals:{}},queueArrivalSignature=null,knownRemoteItemIds=new Set(),queueArrivalEntryIds=new Set(),queueArrivalTimer=null;
const esc = value => String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));
const icon = name => icons[name] || '';
const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
const profileMotion=new URLSearchParams(location.search).get('motion-profile')==='1';
const motionTokens=getComputedStyle(document.documentElement);
const motionValue=(name,fallback)=>parseFloat(motionTokens.getPropertyValue(name))||fallback;
const motion={
  fast:motionValue('--motion-fast',160),
  normal:motionValue('--motion-normal',240),
  layout:motionValue('--motion-layout',420),
  ease:motionTokens.getPropertyValue('--motion-ease').trim()||'cubic-bezier(.32,0,.2,1)'
};
let layoutFrame = 0, composerMotion = null;
let threadMotion = null;
function finishThreadMotion() {
  if(!threadMotion)return;
  const running=threadMotion;threadMotion=null;
  cancelAnimationFrame(running.frame);
  running.animations.forEach(animation=>animation.cancel());
  running.content.style.opacity=running.opacity;
  running.post?.style.removeProperty("overflow");
  running.layer.remove();
}
function threadKey(item) {
  const state=stateFor(item);
  return JSON.stringify([item.id,!!state.postPaneOpen,Object.hasOwn(state,'threadRoot')?state.threadRoot:threadWindowRoot(item,item.targetId)]);
}
function captureThreadTransition(item,direction) {
  const pane=shell.querySelector('.thread-scroll'),content=pane?.querySelector('.thread-content');
  if(!item || reducedMotion.matches || !pane?.clientHeight || pane.dataset.threadKey===threadKey(item)) {
    finishThreadMotion();return null;
  }
  const rect=pane.getBoundingClientRect(),style=getComputedStyle(pane),textStyle=getComputedStyle(content);
  const composer=shell.querySelector('.composer')?.getBoundingClientRect();
  // Freeze the current visible frame before cancelling an interrupted transition.
  const snapshot={pane,rect,scroll:pane.scrollTop,direction,
    height:Math.max(0,Math.min(rect.bottom,composer?.top??rect.bottom)-rect.top),
    padding:style.padding,opacity:textStyle.opacity,transform:textStyle.transform};
  finishThreadMotion();
  return snapshot.height>0?snapshot:null;
}
function enterThread(snapshot) {
  const pane=shell.querySelector('.thread-scroll'),content=pane?.querySelector('.thread-content');
  if(!snapshot || !pane?.clientHeight)return;
  const layer=document.createElement('div');layer.className='thread-exit';
  layer.inert=true;layer.setAttribute('aria-hidden','true');
  const {rect}=snapshot;
  layer.style.cssText=`left:${rect.left}px;top:${rect.top}px;width:${rect.width}px;height:${snapshot.height}px`;
  // A visual copy has no scroll/toggle handlers that could write into live state.
  const old=snapshot.pane.cloneNode(true),oldContent=old.querySelector('.thread-content');
  // IDs must belong exclusively to the live branch.
  old.querySelectorAll('[id]').forEach(node=>node.removeAttribute('id'));
  old.style.cssText=`height:${rect.height}px;min-height:0;flex:none;padding:${snapshot.padding};overflow:hidden`;
  oldContent.style.opacity=snapshot.opacity;oldContent.style.transform=snapshot.transform;
  layer.append(old);document.body.append(layer);old.scrollTop=snapshot.scroll;
  const running={layer,content,opacity:content.style.opacity,frame:0,animations:[]};
  threadMotion=running;content.style.opacity='0';
  // Scroll restoration and focus run first, so the text never slides to a wrong anchor.
  running.frame=requestAnimationFrame(()=>{
    if(threadMotion!==running)return;
    if(snapshot.direction==='post-open'||snapshot.direction==='post-close'){
      const opening=snapshot.direction==='post-open';
      const timing={duration:motion.layout,easing:motion.ease,fill:'both'};
      // Unfold the actual publication surface without stretching its text.
      const post=opening?content.querySelector('.post-pane'):oldContent.querySelector('.post-pane');
      const extent=Math.min(pane.clientHeight,post?.getBoundingClientRect().height||pane.clientHeight);
      if(post){
        const height=post.getBoundingClientRect().height;
        post.style.overflow='hidden';
        running.animations.push(post.animate(opening?[
          {height:'0px',opacity:0,transform:'translateY(-10px)'},
          {height:`${height}px`,opacity:1,transform:'none'}
        ]:[{height:`${height}px`,opacity:1},{height:'0px',opacity:0}],timing));
        running.post=post;
      }
      running.animations.push(layer.animate(opening?[
        {transform:'none',opacity:1},{transform:`translateY(${extent*.35}px)`,opacity:0}
      ]:[{opacity:1},{opacity:0}],timing));
      const incoming=content.animate(opening?[{opacity:1},{opacity:1}]:[
        {opacity:0,transform:'translateY(16px)'},{opacity:1,transform:'none'}
      ],timing);
      running.animations.push(incoming);content.style.opacity=running.opacity;
      const arrow=shell.querySelector('.source-chevron');
      if(arrow)running.animations.push(arrow.animate([
        {transform:`rotate(${opening?0:180}deg)`},{transform:`rotate(${opening?180:0}deg)`}
      ],timing));
      incoming.finished.then(()=>{if(threadMotion===running)finishThreadMotion();},()=>{});
      return;
    }
    const back=snapshot.direction==='back',nested=snapshot.direction!=='next';
    const enter=nested?`translateX(${back?-18:18}px)`:'translateY(12px)';
    const exit=nested?`translateX(${back?10:-10}px)`:'translateY(-6px)';
    running.animations.push(oldContent.animate([
      {opacity:snapshot.opacity,transform:snapshot.transform},
      {opacity:0,transform:`${snapshot.transform==='none'?'':snapshot.transform} ${exit}`}
    ],{duration:motion.normal,easing:motion.ease,fill:'both'}));
    const incoming=content.animate([{opacity:0,transform:enter},{opacity:1,transform:'none'}],
      {duration:motion.normal,easing:motion.ease,fill:'both'});
    running.animations.push(incoming);content.style.opacity=running.opacity;
    incoming.finished.then(()=>{if(threadMotion===running)finishThreadMotion();},()=>{});
  });
}
window.addEventListener('resize',finishThreadMotion);
reducedMotion.addEventListener('change',finishThreadMotion);
function screenKey() {
  return isOverview()?`overview:${overviewSection()}:${saved.overviewTopic||''}`:
    `${saved.view}:${window.innerWidth<=820?(saved.listMode?'list':'detail'):'split'}`;
}
window.addEventListener('resize',()=>{if(shell.dataset.screenKey)shell.dataset.screenKey=screenKey();});
const assistantLayoutListeners=new WeakMap();
let geometryUntil=0,geometryTimer=0,settleWorkspaceMeasurements=()=>{};
const layoutMotionActive=()=>performance.now()<geometryUntil;
function finishGeometryMotion() {
  if(!geometryTimer && !geometryUntil)return;
  clearTimeout(geometryTimer);geometryTimer=0;geometryUntil=0;
  shell.classList.remove('is-layout-animating');
  const anchor=readingAnchor();settleWorkspaceMeasurements();restoreReadingAnchor(anchor,true);
}
const readingMotions=new Map();
function finishReadingMotions() { for(const running of readingMotions.values())running.finish(); }
function animateReadingText(text,expanded,button,onSettled) {
  const from=text.getBoundingClientRect().height;
  readingMotions.get(text)?.finish();
  text.classList.toggle('text-collapsed',!expanded);
  const to=text.getBoundingClientRect().height;
  if(reducedMotion.matches || Math.abs(from-to)<1) {onSettled();return;}
  text.classList.remove('text-collapsed');
  const overflow=text.style.overflow;text.style.overflow='hidden';text.classList.add('is-unfolding');
  const animation=text.animate([{height:`${from}px`},{height:`${to}px`}],{duration:motion.layout,easing:motion.ease,fill:'both'});
  const running={frame:0,finish:()=>{
    if(readingMotions.get(text)!==running)return;
    readingMotions.delete(text);cancelAnimationFrame(running.frame);animation.cancel();
    text.classList.toggle('text-collapsed',!expanded);text.classList.remove('is-unfolding');text.style.overflow=overflow;
    onSettled();
  }};
  readingMotions.set(text,running);
  if(!expanded) {
    const pane=text.closest('.thread-scroll');
    const follow=()=>{
      if(readingMotions.get(text)!==running)return;
      const delta=button.getBoundingClientRect().top-pane.getBoundingClientRect().top-12;
      if(delta<0)pane.scrollTop+=delta;
      running.frame=requestAnimationFrame(follow);
    };
    running.frame=requestAnimationFrame(follow);
  }
  animation.finished.then(running.finish,()=>{});
}
function stopLayoutMotion() {
  cancelAnimationFrame(layoutFrame); layoutFrame=0; shell.classList.remove('is-moving');
  if(composerMotion) cancelAnimationFrame(composerMotion.frame);
  for(const running of readingMotions.values())cancelAnimationFrame(running.frame);
}
function readingAnchor() {
  const pane=shell.querySelector('.thread-scroll');
  if(!pane?.clientHeight) return null;
  const top=pane.getBoundingClientRect().top;
  const message=[...pane.querySelectorAll('[data-message-id]')].find(node=>node.getBoundingClientRect().bottom>top+4);
  return message ? {node:message,offset:message.getBoundingClientRect().top-top,pane} : null;
}
function captureSurfaceAnchor() {
  if(shell.dataset.actorId!==(saved.mvpActorId||operator.id))return null;
  const detail=shell.querySelector('.detail[data-context-item]'),pane=detail?.querySelector('.thread-scroll');
  if(!pane?.clientHeight)return null;
  const bounds=pane.getBoundingClientRect();
  const node=[...pane.querySelectorAll('[data-message-id],[data-reading-block]')]
    .find(entry=>{const rect=entry.getBoundingClientRect();return rect.bottom>bounds.top+4&&rect.top<bounds.bottom;});
  if(!node)return null;
  return {itemId:detail.dataset.contextItem,surface:pane.dataset.readingSurface,
    id:node.dataset.messageId||node.dataset.readingBlock,kind:node.dataset.messageId?'message':'block',
    offset:node.getBoundingClientRect().top-bounds.top};
}
function restoreSurfaceAnchor(pane,anchor,item) {
  if(!anchor||!item||anchor.itemId!==item.id||anchor.surface!==pane.dataset.readingSurface)return;
  const selector=anchor.kind==='message'?`[data-message-id="${CSS.escape(anchor.id)}"]`:`[data-reading-block="${CSS.escape(anchor.id)}"]`;
  const node=pane.querySelector(selector);if(!node)return;
  const offset=node.getBoundingClientRect().top-pane.getBoundingClientRect().top;
  restoreReadingPosition(pane,pane.scrollTop+offset-anchor.offset);
}
function moveLayout(change) {
  finishThreadMotion();
  const requested=performance.now();
  stopLayoutMotion();finishGeometryMotion();const anchor=readingAnchor();
  geometryUntil=performance.now()+(reducedMotion.matches?0:motion.layout);
  shell.classList.toggle('is-layout-animating',!reducedMotion.matches);
  shell.classList.add('is-moving'); change();
  geometryTimer=setTimeout(finishGeometryMotion,reducedMotion.matches?0:motion.layout);
  const started=performance.now();
  const intervals=[];let previousFrame=0;
  const follow=now=>{
    if(profileMotion && previousFrame)intervals.push(now-previousFrame);
    previousFrame=now;
    if(anchor?.node.isConnected && anchor.pane.clientHeight) {
      const offset=anchor.node.getBoundingClientRect().top-anchor.pane.getBoundingClientRect().top;
      const delta=offset-anchor.offset;
      if(Math.abs(delta)>.5)anchor.pane.scrollTop+=delta;
    }
    if(!reducedMotion.matches && performance.now()-started<motion.layout) layoutFrame=requestAnimationFrame(follow);
    else {
      stopLayoutMotion();rememberReading();persist();
      if(profileMotion && intervals.length) {
        const sorted=[...intervals].sort((a,b)=>a-b);
        shell.dataset.motionProfile=JSON.stringify({frames:intervals.length,median:sorted[Math.floor(sorted.length/2)],p95:sorted[Math.floor(sorted.length*.95)],max:Math.max(...intervals),over25:intervals.filter(n=>n>25).length,setup:started-requested,elapsed:performance.now()-started});
      }
    }
  };
  layoutFrame=requestAnimationFrame(follow);
}
let assistantMotion=null, pendingAssistantRender=null;
function finishAssistantMotion(focus=false) {
  if(!assistantMotion)return;
  const motion=assistantMotion;assistantMotion=null;
  motion.animations.forEach(animation=>animation.cancel());
  motion.head.prepend(...motion.label.childNodes);
  motion.panel.style.cssText=motion.panelStyle;
  motion.slot.prepend(motion.panel);
  motion.surface.remove();
  motion.opener.style.opacity=motion.openerOpacity;
  shell.classList.remove('assistant-morphing');
  motion.slot.inert=!saved.ai;
  motion.slot.setAttribute('aria-hidden',String(!saved.ai));
  motion.panel.dispatchEvent(new Event('assistant-layout-settled'));
  if(focus && (document.activeElement===document.body || document.activeElement===motion.opener || motion.panel.contains(document.activeElement))) {
    shell.querySelector(motion.focusControl)?.focus({preventScroll:true});
  }
  motion.onSettled?.();
}
function setAssistantOpen(open, focusControl = open ? '#ai-input' : '#toggle-ai') {
  mvp.rememberAssistantReading();
  finishThreadMotion();
  finishReadingMotions();
  finishComposerMotion();
  const slot=shell.querySelector('.assistant-slot'),panel=shell.querySelector('.ai'),opener=shell.querySelector('#toggle-ai');
  if(!panel || !opener)return;
  rememberReading();
  const frameFor=(rect,style)=>({transform:`translate3d(${rect.left}px,${rect.top}px,0)`,width:`${rect.width}px`,height:`${rect.height}px`,borderRadius:style.borderRadius,backgroundColor:style.backgroundColor});
  const labelFrameFor=(rect,glyph,text,parent)=>{
    const bounds=glyph.getBoundingClientRect(),style=getComputedStyle(text);
    return {transform:`translate3d(${bounds.left-rect.left}px,${bounds.top-rect.top}px,0)`,color:style.color};
  };
  // Keep the visible shape and its label continuous when direction changes.
  const interrupted=assistantMotion ? frameFor(assistantMotion.surface.getBoundingClientRect(),getComputedStyle(assistantMotion.surface)) : null;
  const contentOpacities=interrupted ? [...panel.children].map(node=>getComputedStyle(node).opacity) : null;
  const labelStyle=interrupted ? getComputedStyle(assistantMotion.label) : null;
  const interruptedLabel=labelStyle ? {transform:labelStyle.transform,color:labelStyle.color} : null;
  const buttonBefore=opener.getBoundingClientRect();
  const buttonStart=frameFor(buttonBefore,getComputedStyle(opener));
  const buttonLabelStart=labelFrameFor(buttonBefore,opener.querySelector('svg'),opener.querySelector('span'),opener);
  const columns=[...shell.querySelectorAll('.navigation,.list,.assistant-slot')];
  const widths=columns.map(node=>getComputedStyle(node).width);
  finishAssistantMotion();
  const head=panel.querySelector('.ai-head'),glyph=head.querySelector(':scope > svg'),title=head.querySelector('strong');
  const animate=!reducedMotion.matches;
  let panelRect,buttonFrame,panelFrame,buttonLabelFrame,panelLabelFrame;
  moveLayout(()=>{
    // Measure both endpoints before paint. Only the surrounding columns move;
    // the visible surface starts at the button, not at the right-hand slot.
    shell.classList.add('resize-settle');
    shell.classList.toggle('assistant-morphing',animate);
    saved.ai=open;
    shell.classList.toggle('has-ai',open);
    // Keep the outgoing pane visible until its exit reaches the opener.
    if(!pendingAssistantRender)shell.classList.toggle('list-mode',!!saved.listMode);
    opener.setAttribute('aria-expanded',String(open));
    updateNavigationLayout();
    panelRect=panel.getBoundingClientRect();
    panelFrame=frameFor(panelRect,getComputedStyle(panel));
    buttonFrame=open?buttonStart:frameFor(opener.getBoundingClientRect(),getComputedStyle(opener));
    buttonLabelFrame=open?buttonLabelStart:labelFrameFor(opener.getBoundingClientRect(),opener.querySelector('svg'),opener.querySelector('span'),opener);
    panelLabelFrame=labelFrameFor(panelRect,glyph,title,head);
    if(animate && window.innerWidth>820) columns.forEach((node,index)=>node.style.width=widths[index]);
    // Commit the starting widths with transitions disabled before enabling travel.
    void shell.offsetWidth;
    shell.classList.remove('resize-settle');
    void shell.offsetWidth;
    columns.forEach(node=>node.style.removeProperty('width'));
    slot.inert=animate || !open;slot.setAttribute('aria-hidden',String(!open));
    persist();
  });
  if(!animate) {mvp.restoreAssistantReading();shell.querySelector(focusControl)?.focus({preventScroll:true});return;}
  const surface=document.createElement('div');surface.className='assistant-morph-surface';surface.inert=true;surface.setAttribute('aria-hidden','true');
  const label=document.createElement('div');label.className='assistant-morph-label';
  // Carry the actual heading icon and title into their new position without a fade gap.
  label.append(glyph,title);
  const panelStyle=panel.style.cssText,openerOpacity=opener.style.opacity;
  // Reuse the actual panel nodes: no duplicate chat, draft, or scaled glyphs.
  Object.assign(panel.style,{position:'absolute',inset:'0px auto auto 0px',width:`${panelRect.width}px`,height:`${panelRect.height}px`,margin:'0px',borderRadius:'0px',background:'transparent',boxShadow:'none'});
  surface.append(panel,label);shell.querySelector('.work-surface').append(surface);
  opener.style.opacity='0';
  const start=interrupted || (open?buttonFrame:panelFrame),end=open?panelFrame:buttonFrame;
  // Reversals share the column timeline, so reparenting cannot outrun its slot.
  const duration=motion.layout;
  const shape=surface.animate([start,end],{duration,easing:motion.ease,fill:'both'});
  const labelMotion=label.animate([interruptedLabel || (open?buttonLabelFrame:panelLabelFrame),open?panelLabelFrame:buttonLabelFrame],{duration,easing:motion.ease,fill:'both'});
  const contents=[...panel.children].map((node,index)=>node.animate(
    [{opacity:contentOpacities?.[index] ?? (open?0:1)},{opacity:open?1:0}],
    {duration:open?motion.normal:motion.fast,delay:open&&!interrupted?motion.fast:0,easing:motion.ease,fill:'both'}
  ));
  const running={open,animations:[shape,labelMotion,...contents],surface,label,head,slot,panel,panelStyle,opener,openerOpacity,focusControl};
  assistantMotion=running;
  const settle=()=>{if(assistantMotion===running)finishAssistantMotion(true);};
  shape.finished.then(settle,settle);
  if(!open && focusControl!=='#toggle-ai')shell.querySelector(focusControl)?.focus({preventScroll:true});
}
function finishComposerMotion(focus=false) {
  if(!composerMotion)return;
  const running=composerMotion;composerMotion=null;
  cancelAnimationFrame(running.frame);
  running.animations.forEach(animation=>animation.cancel());
  running.outgoing.remove();
  running.surface.classList.remove('is-morphing');
  running.surface.style.cssText=running.surfaceStyle;
  running.surface.inert=false;
  shell.querySelector('.ai')?.dispatchEvent(new Event('assistant-layout-settled'));
  const pane=shell.querySelector('.thread-scroll');
  if(pane) {
    const top=pane.scrollTop;
    pane.style.removeProperty('--reading-slack');
    restoreReadingPosition(pane,top);
  }
  if(focus && document.activeElement===document.body) shell.querySelector(running.focusControl)?.focus({preventScroll:true});
}
function transitionComposer(change,focusControl) {
  finishReadingMotions();
  finishComposerMotion();finishAssistantMotion();
  const outgoing=shell.querySelector('.editor-surface'),anchor=readingAnchor();
  const from=outgoing?.getBoundingClientRect().height;
  const anchorId=anchor?.node.dataset.messageId;
  rememberReading();change();persist();render();
  const surface=shell.querySelector('.editor-surface');
  if(reducedMotion.matches || !from || !surface) {shell.querySelector(focusControl)?.focus({preventScroll:true});return;}
  const to=surface.getBoundingClientRect().height,surfaceStyle=surface.style.cssText;
  const contents=[...surface.children];
  // Keep the old contents inside the shared bottom-anchored shape while it changes mode.
  // Strip IDs from the detached outgoing layer; only the new controls own state/focus.
  outgoing.querySelectorAll('[id]').forEach(node=>node.removeAttribute('id'));
  outgoing.classList.add('composer-outgoing');outgoing.inert=true;outgoing.setAttribute('aria-hidden','true');
  Object.assign(outgoing.style,{height:`${from}px`,minHeight:'0px',background:'transparent',boxShadow:'none',backdropFilter:'none'});
  surface.append(outgoing);surface.classList.add('is-morphing');surface.style.minHeight='0px';surface.inert=true;
  const shape=surface.animate([{height:`${from}px`},{height:`${to}px`}],{duration:motion.layout,easing:motion.ease,fill:'both'});
  const exit=outgoing.animate([{opacity:1},{opacity:0}],{duration:motion.fast,easing:motion.ease,fill:'both'});
  const entering=contents.map(node=>node.animate([{opacity:0},{opacity:1}],{duration:motion.normal,delay:motion.fast,easing:motion.ease,fill:'both'}));
  const running={surface,outgoing,surfaceStyle,focusControl,animations:[shape,exit,...entering],frame:0};
  composerMotion=running;
  const nextAnchor=anchorId ? {node:shell.querySelector(`[data-message-id="${CSS.escape(anchorId)}"]`),pane:shell.querySelector('.thread-scroll'),offset:anchor.offset} : null;
  const follow=()=>{if(composerMotion!==running)return;restoreReadingAnchor(nextAnchor,true);running.frame=requestAnimationFrame(follow);};
  running.frame=requestAnimationFrame(follow);
  shape.finished.then(()=>{if(composerMotion===running){finishComposerMotion(true);rememberReading();persist();}},()=>{});
}
const disclosureMotion = new WeakMap();
function animateDisclosure(details) {
  const previous=disclosureMotion.get(details), open=!(previous?.open ?? details.open);
  const reasonBody=details.querySelector('.reason-body');
  if(reasonBody){
    const from=details.open?reasonBody.getBoundingClientRect().height:0;
    previous?.animation.cancel();disclosureMotion.delete(details);
    if(reducedMotion.matches){details.open=open;details.classList.remove('is-closing');return;}
    details.open=true;details.classList.toggle('is-closing',!open);
    const to=open?reasonBody.scrollHeight:0;
    const animation=reasonBody.animate([{height:`${from}px`,opacity:open?0:1},{height:`${to}px`,opacity:open?1:0}],{duration:motion.normal,easing:motion.ease});
    disclosureMotion.set(details,{animation,open});
    animation.onfinish=()=>{if(disclosureMotion.get(details)?.animation!==animation)return;details.open=open;details.classList.remove('is-closing');disclosureMotion.delete(details);};
    return;
  }
  const from=details.getBoundingClientRect().height;
  previous?.animation.cancel(); disclosureMotion.delete(details);
  details.classList.remove('is-closing');
  if(reducedMotion.matches) {details.style.removeProperty('overflow');details.open=open;return;}
  details.open=true;
  const to=open ? details.getBoundingClientRect().height : details.querySelector('summary').getBoundingClientRect().height;
  details.style.overflow='hidden';
  const branch=details.classList.contains('reply-branch');
  const duration=branch?motion.layout:motion.normal;
  details.style.setProperty('--disclosure-duration',`${duration}ms`);
  details.classList.toggle('is-closing',!open);
  // Branches use the layout cadence; compact disclosures use the normal cadence.
  const animation=details.animate([{height:`${from}px`},{height:`${to}px`}],{duration,easing:motion.ease});
  disclosureMotion.set(details,{animation,open});
  animation.onfinish=()=>{
    if(disclosureMotion.get(details)?.animation!==animation) return;
    details.open=open;details.style.removeProperty('overflow');details.classList.remove('is-closing');disclosureMotion.delete(details);
  };
}
const closingDialogs = new WeakSet();
function closeDialog(dialog) {
  if(!dialog.open || closingDialogs.has(dialog)) return;
  closingDialogs.add(dialog); dialog.inert=true;
  if(reducedMotion.matches) {dialog.close();return;}
  dialog.animate([{opacity:1,transform:'translateY(0)'},{opacity:0,transform:'translateY(4px)'}],{duration:motion.fast,easing:motion.ease}).finished.then(()=>dialog.close(),()=>dialog.close());
}
function openDialog(dialog) {
  dialog.addEventListener('cancel',event=>{if(event.defaultPrevented)return;event.preventDefault();closeDialog(dialog);});
  dialog.showModal();
}
function openCommentImage(url,label,trigger) {
  if(document.querySelector('#comment-image-dialog'))return;
  const dialog=document.createElement('dialog');
  dialog.id='comment-image-dialog';dialog.className='comment-image-dialog';
  dialog.setAttribute('aria-label',label||'Изображение комментария');
  const close=document.createElement('button');close.type='button';close.className='icon-button';
  close.setAttribute('aria-label','Закрыть изображение');close.textContent='×';
  const image=document.createElement('img');image.src=url;image.alt=label||'Изображение комментария';
  image.referrerPolicy='no-referrer';
  image.addEventListener('error',()=>{image.replaceWith(Object.assign(document.createElement('p'),{textContent:'Изображение недоступно.'}));});
  close.addEventListener('click',()=>closeDialog(dialog));
  dialog.addEventListener('close',()=>{dialog.remove();trigger?.focus({preventScroll:true});});
  dialog.append(close,image);document.body.append(dialog);openDialog(dialog);
}
function bindCommentMedia() {
  bindCommentVideoErrors(shell);
  shell.querySelectorAll('.comment-media img,.comment-media-queue img').forEach(image=>image.addEventListener('error',()=>{
    const fallback=document.createElement('span');fallback.className='comment-media-unavailable';fallback.textContent='Медиа недоступно';image.replaceWith(fallback);
  }));
  shell.querySelectorAll('[data-comment-image]').forEach(button=>button.addEventListener('click',()=>openCommentImage(button.dataset.commentImage,button.dataset.commentImageLabel,button)));
}
reducedMotion.addEventListener('change',()=>{
  if(!reducedMotion.matches) return;
  finishAssistantMotion(true);
  finishComposerMotion(true);
  finishReadingMotions();
  stopLayoutMotion();
  for(const animation of document.getAnimations()) {try {animation.finish();} catch {animation.cancel();}}
});
shell.addEventListener('wheel',stopLayoutMotion,{passive:true});
shell.addEventListener('touchstart',stopLayoutMotion,{passive:true});
shell.addEventListener('pointerdown',stopLayoutMotion,{passive:true});
shell.addEventListener('keydown',event=>{
  if(!event.target.closest('input,textarea,[contenteditable="true"]') && ['PageUp','PageDown','Home','End','ArrowUp','ArrowDown',' '].includes(event.key)) stopLayoutMotion();
});
function sendButton(destination, disabled = false, disabledReason = '') {
  const label = destination === 'reply' ? 'Проверить ответ перед отправкой' : 'Отправить ассистенту';
  const hint = destination === 'reply' && disabled ? `${label} — ${disabledReason || 'сначала напишите ответ'}` : label;
  return `<button class="send-button" type="${destination === 'reply' ? 'button' : 'submit'}" aria-label="${label}" title="${esc(hint)}" ${disabled ? 'disabled' : ''}>${icon('ArrowUp')}</button>`;
}
function fitTextInput(input) {
  if(!input.clientWidth)return;
  input.style.height='0px';
  input.style.height=input.scrollHeight+'px'; // CSS owns the shared minimum and maximum.
}
const labels = {attention:'Нужно участие',prepared:'Подготовлено',waiting:'Ждём',closed:'Закрытые',deleted:'Удалённые'};
const topicReading=new Map();
const overviewReportReading=new Map();
let pendingTopicFocus=null;
function rememberTopicReading() {
  if(shell.dataset.actorId!==(saved.mvpActorId||operator.id))return;
  const panel=shell.querySelector('.topic-panel[data-topic-key]'),key=panel?.dataset.topicKey;
  pendingTopicFocus=null;
  if(!key)return;
  const pane=panel.querySelector('.topic-scroll');
  const details=[...panel.querySelectorAll('.topic-instructions details[data-rule-version]')];
  const previous=topicReading.get(key)||{top:0,open:[],sources:[]};
  topicReading.set(key,{top:pane?.scrollTop||0,open:details.length?details.filter(node=>node.open).map(node=>node.dataset.ruleVersion):previous.open,
    sources:details.length?details.filter(node=>node.querySelector('.instruction-source')?.open).map(node=>node.dataset.ruleVersion):previous.sources});
  const active=document.activeElement;
  if(active&&panel.contains(active))pendingTopicFocus={key,version:active.closest('details[data-rule-version]')?.dataset.ruleVersion||null,id:active.id||null};
}
function restoreTopicReading(topic) {
  const panel=shell.querySelector('.topic-panel[data-topic-key]');
  if(!topic||!panel)return;
  const state=topicReading.get(topic.key),pane=panel.querySelector('.topic-scroll');
  if(state){
    const opened=new Set(state.open);
    const sources=new Set(state.sources);
    panel.querySelectorAll('.topic-instructions details[data-rule-version]').forEach(node=>{
      node.open=opened.has(node.dataset.ruleVersion);
      const source=node.querySelector('.instruction-source');if(source)source.open=sources.has(node.dataset.ruleVersion);
    });
    if(pane)pane.scrollTop=state.top;
  }
  pane?.addEventListener('scroll',()=>{if(pane.isConnected&&shell.querySelector('.topic-scroll')===pane)rememberTopicReading();},{passive:true});
  panel.querySelectorAll('.topic-instructions details[data-rule-version]').forEach(node=>{
    const remember=()=>{if(node.isConnected&&shell.querySelector('.topic-panel')===panel)rememberTopicReading();};
    node.addEventListener('toggle',remember);
    node.querySelector('.instruction-source')?.addEventListener('toggle',remember);
  });
  if(pendingTopicFocus?.key===topic.key){
    const target=pendingTopicFocus.version?panel.querySelector(`details[data-rule-version="${CSS.escape(pendingTopicFocus.version)}"] summary`):pendingTopicFocus.id?panel.querySelector(`#${CSS.escape(pendingTopicFocus.id)}`):null;
    target?.focus({preventScroll:true});
  }
  pendingTopicFocus=null;
}
const dateLabel = value => new Date(value).toLocaleString('ru-RU',{day:'numeric',month:'short',hour:'2-digit',minute:'2-digit'});
const basisLabels = {created:'Дата комментария',closed:'Дата закрытия',deleted:'Дата удаления'};
let listWindow = initialListWindow(0);
let listObserver = null;
let listResizeObserver = null;
let listViewKey = '';
let listPageHeights = new Map();
let listMeasuredWidth = 0;
let listMeasuredMean = 0;
let skipNextReadingSnapshot = false;
let listOrderMotion = false;
function listKey() { return JSON.stringify([saved.view,saved.filter,saved.search,filtersFor(),saved.listOrder?.[saved.view]]); }
function syncListWindow(items = viewItems()) {
  const key=listKey();
  if(key!==listViewKey) { listViewKey=key;listWindow=initialListWindow(items.length);listPageHeights=new Map();listMeasuredWidth=0;listMeasuredMean=0; }
  listWindow.end=Math.min(items.length,Math.max(listWindow.start,listWindow.end));
  if(listWindow.start>=items.length)listWindow=initialListWindow(items.length);
  return listWindow;
}
function filtersFor(view = saved.view) { return (saved.listFilters ||= {})[view] ||= {period:'all',dateField:'created'}; }
function listOptions() { return {view:saved.view,outcome:saved.filter,query:saved.search,order:saved.listOrder?.[saved.view]||'newest',filters:filtersFor()}; }
const announce = text => { document.querySelector('#live').textContent = text; };
function persist() {
  try { localStorage.setItem(storageKey, JSON.stringify(saved)); }
  catch { announce('Не удалось сохранить локальные правки в браузере. Не перезагружайте страницу.'); }
}
const entities = createEntityIndex(() => data);
const itemById = id => entities.item(id);
const itemForMessage = id => entities.messageOwner(id);
const branchFor = item => entities.branch(item.branchId);
const postFor = item => entities.post(branchFor(item).postId);
const branchState = branch => saved.branches[branch.id] ||= {extra:false};
const contextVersion = item => Number(branchState(branchFor(item)).extra);
function stateFor(item) {
  return saved.items[item.id] ||= {draft:item.draft, draftContext:0, revision:0, history:[], redo:[], chat:[], proposal:null,
    expanded:[], scroll:0, postOpen:false, aiInput:'', decision:item.decision, view:item.view, note:'',
    ...(item.initialState || {})};
}
function messagesFor(item) {
  const branch = branchFor(item);
  const messages=branchState(branch).extra && branch.extraReply ? [...branch.messages, branch.extraReply] : branch.messages;
  return messages.map(message=>{
    const owner=itemForMessage(message.id),state=owner&&stateFor(owner);
    return state?.localRecovery && state.view!=='deleted' ? {...message,deleted:false,restoredLocally:true} : message;
  });
}
function selectedItem() { return itemById(saved.selected); }
function currentAssistantContext() {
  if(isOverview() && overviewSection()==='discussions' && saved.overviewTopic) {
    const snapshot=overviewSnapshot(allRecords(),data.posts,overviewTopics,overviewPeriodFilters());
    const group=snapshot.groups.find(g=>g.topics.some(t=>t.key===saved.overviewTopic));
    const topic=group?.topics.find(t=>t.key===saved.overviewTopic);
    if(topic)return buildAssistantContext({kind:'topic',itemId:null,itemIds:[...new Set(topic.itemIds||topic.open.map(r=>r.item.id))],key:'topic:'+topic.key,topicKey:topic.key,postId:group.post.id,label:'Тема · '+(topic.title||topic.label)+' · '+topic.open.length+' в работе'});
  }
  const item=isOverview()||listOwnsAssistantToggle()?null:selectedItem();
  const queue=!item&&!isOverview()?viewItems():[];
  return buildAssistantContext({kind:item?'comment':isOverview()?overviewSection():'queue',itemId:item?.id||null,itemIds:item?[item.id]:queue.slice(listWindow.start,listWindow.end).map(i=>i.id),totalCount:item?1:queue.length,query:saved.search,filters:{...filtersFor(),workflow:saved.view,outcome:saved.filter},order:saved.listOrder?.[saved.view],key:item?`comment:${item.id}`:isOverview()?`overview:${overviewSection()}`:`queue:${saved.view}`,label:item?`Комментарий · ${messagesFor(item).find(m=>m.id===item.targetId)?.author||''}`:isOverview()?`Обзор · ${{analytics:'Аналитика',history:'История действий',discussions:'Обсуждения'}[overviewSection()]}`:`Раздел · ${labels[saved.view]}`});
}
function assistantSession() {
  if(!saved.assistantSession){
    const context=currentAssistantContext(),old=context.itemId?stateFor(itemById(context.itemId)):saved.assistantContexts?.[context.key];
    saved.assistantSession={context,chat:[...(old?.chat||[])],aiInput:old?.aiInput||'',aiScroll:old?.aiScroll||0};
  }
  return saved.assistantSession;
}
function assistantItem() { return itemById(assistantSession().context.itemId); }
function assistantScopeKey() { return assistantSession().context.key; }
function assistantState(item) {
  const session=assistantSession(),base=item?stateFor(item):{};
  return new Proxy(base,{get:(target,key)=>['chat','aiInput','aiScroll'].includes(key)?session[key]:target[key],set:(target,key,value)=>{if(['chat','aiInput','aiScroll'].includes(key))session[key]=value;else target[key]=value;return true;}});
}
function assistantToggleHtml() { return `<button class="toggle-ai" id="toggle-ai" aria-label="Ассистент" title="Открыть ассистента" aria-expanded="${!!saved.ai}">${icon('MessagesSquare')}<span>Ассистент</span></button>`; }
function listOwnsAssistantToggle() { return saved.listMode && window.innerWidth<=820; }
function retainedSelection() { return saved.retainedSelection?.itemId===saved.selected && saved.retainedSelection.view===saved.view; }
function restoreLinkedItem(id) {
  const item = itemById(id); if (!item) return;
  delete saved.retainedSelection;
  const previousView = saved.view;
  const changed = saved.selected !== id;
  saved.selected = id; saved.view = stateFor(item).view;
  if (changed) { stateFor(item).threadRoot=threadWindowRoot(item,item.targetId); stateFor(item).scroll=0; stateFor(item).threadTrail=[]; }
  if (previousView !== saved.view) {
    saved.search = '';
    const state = stateFor(item);
    saved.filter = state.view === 'prepared' ? state.decision : state.view === 'closed' ? (state.closure?.outcome === 'reply' ? 'reply' : 'no_reply') : 'all';
  }
  if (!matchesList(recordFor(item),listOptions())) {
    const f = filtersFor(), bounds = resolvePeriod(f), state = stateFor(item);
    if (f.postId && f.postId !== postFor(item).id) delete f.postId;
    if (f.channel && f.channel !== postFor(item).channel) delete f.channel;
    const day = calendarDay(recordDate(recordFor(item),dateBasis(saved.view,f)));

    if ((bounds.from || bounds.to) && (!day || bounds.from && day < bounds.from || bounds.to && day > bounds.to)) {delete f.from;delete f.to;f.period='all';}
    if (!matchesList(recordFor(item),{view:saved.view,query:saved.search})) saved.search = '';
    if (!matchesList(recordFor(item),{view:saved.view,outcome:saved.filter})) {
      saved.filter = state.view === 'closed' ? (state.closure?.outcome === 'reply' ? 'reply' : 'no_reply') : state.view === 'prepared' ? state.decision : 'all';
    }
    requestAnimationFrame(() => announce('Условия, скрывавшие выбранный комментарий, сняты. Ветка показана полностью.'));
  }
  if (changed || previousView !== saved.view) saved.listMode = false;
  const items=viewItems();
  syncListWindow(items);
  const previousStart=listWindow.start;
  listWindow=listWindowAfterSelection(listWindow,items.length,items.findIndex(entry=>entry.id===id));
  if(previousStart!==listWindow.start){saved.queueScroll=listWindow.topHeight;skipNextReadingSnapshot=true;}
}
function viewItems() {
  return filterList(queueRecords(),listOptions()).map(record=>record.item);
}
function rememberReading() {
  if(shell.dataset.actorId!==(saved.mvpActorId||operator.id))return;
  // During an exit, saved.selected already points at the requested destination.
  // Measurements still belong to the visible outgoing conversation.
  const item = itemById(shell.querySelector('.detail')?.dataset.contextItem) || selectedItem();
  const thread = shell.querySelector('.thread-scroll'), queue = shell.querySelector('.queue-scroll');
  if (item && thread?.clientHeight) stateFor(item)[thread.dataset.readingSurface==='post'?'postScroll':'scroll'] = thread.scrollTop;
  const chat=shell.querySelector('.ai-scroll'),panel=chat?.closest('.ai');
  if(chat?.clientHeight)assistantState(itemById(panel.dataset.contextItem),panel.dataset.assistantScope).aiScroll=chat.scrollTop;
  if (!pendingAssistantRender && queue?.clientHeight) saved.queueScroll = queue.scrollTop;
  const overview=shell.querySelector('.overview-posts');
  if(overview?.clientHeight)saved.overviewScroll=overview.scrollTop;
}
function rememberVisibleDisclosures() {
  const detail=shell.querySelector('.detail[data-context-item]');
  if(!detail||shell.dataset.actorId!==(saved.mvpActorId||operator.id))return;
  const item=itemById(detail.dataset.contextItem);if(!item)return;
  const state=stateFor(item),pane=detail.querySelector('.thread-scroll');
  const wanted=node=>disclosureMotion.get(node)?.open??node.open;
  if(pane?.dataset.readingSurface==='post'){
    if(pane.clientHeight)state.postScroll=pane.scrollTop;
    state.postTranscriptOpen=[...pane.querySelectorAll('.post-transcript[data-transcript-id]')]
      .filter(wanted).map(node=>node.dataset.transcriptId);
  }
  const opened=new Set(state.treeOpen||[]),closed=new Set(state.treeClosed||[]);
  detail.querySelectorAll('.reply-branch[data-branch-id]').forEach(node=>{
    if(wanted(node)){opened.add(node.dataset.branchId);closed.delete(node.dataset.branchId);}
    else{const explicitlyClosed=opened.has(node.dataset.branchId)||node.dataset.pathOpen==='true';
      opened.delete(node.dataset.branchId);if(explicitlyClosed)closed.add(node.dataset.branchId);}
  });
  state.treeOpen=[...opened];state.treeClosed=[...closed];
  const reason=detail.querySelector('.selected-reason');if(reason)state.selectedReasonOpen=wanted(reason);
  const decision=detail.querySelector('.decision-reason');if(decision)state.reasonOpen=wanted(decision);
  const completion=detail.querySelector('.completion details');if(completion)state.completionDraftOpen=wanted(completion);
}
function rememberOverviewReport() {
  const report=shell.querySelector('.overview-report'),section=shell.querySelector('[data-overview-section]')?.dataset.overviewSection;
  if(report?.clientHeight&&section&&shell.dataset.actorId===(saved.mvpActorId||operator.id))
    overviewReportReading.set(section,report.scrollTop);
}
function revealSelectedPath(item,id) {
  if(!item||!id)return;
  const byId=new Map(messagesFor(item).map(message=>[message.id,message]));
  const closed=new Set(stateFor(item).treeClosed||[]),seen=new Set();let message=byId.get(id);
  while(message&&!seen.has(message.id)){
    seen.add(message.id);closed.delete(message.id);message=byId.get(message.parentId);
  }
  stateFor(item).treeClosed=[...closed];
}
function selectItem(id, reveal = true) {
  if (!itemById(id)) return;
  assistantNavigationRevision++;
  rememberReading(); restoreLinkedItem(id); saved.listMode = false;
  if (reveal) { const item=itemById(id); stateFor(item).threadRoot=threadWindowRoot(item,item.targetId); stateFor(item).scroll=0; }
  saved.navOpen = true;
  const hash = `#item/${encodeURIComponent(id)}`;
  if (location.hash !== hash) history.pushState(null,'',hash);
  
  persist(); render({focusMessage:reveal ? itemById(id).targetId : null, focusControl:reveal ? `#message-${itemById(id).targetId}` : null});
  shell.querySelector('[data-item][aria-current="true"]')?.scrollIntoView({block:'nearest'});
}
function selectExercise(id) {
  rememberReading();
  const exercise = data.exercises.find(entry => entry.id === id) || data.exercises[0];
  const item = itemById(exercise.itemId); saved.exercise = exercise.id; saved.view = stateFor(item).view;
  saved.filter = stateFor(item).decision === 'no_reply' ? 'no_reply' : 'all'; saved.search = '';
  selectItem(item.id);
}
function renderStudy() {
  if (!bar) return; bar.hidden=false;
  const exercise = data.exercises.find(entry => entry.id === saved.exercise) || data.exercises[0];
  const item = selectedItem();
  const canSimulate = data.exercises.indexOf(exercise) === 3 && item && branchFor(item).extraReply;
  bar.innerHTML = `<label class="sr-only" for="exercise">Учебное задание</label><select id="exercise">${data.exercises.map((entry,index) => `<option value="${esc(entry.id)}" ${entry.id === exercise.id ? 'selected' : ''}>${index + 1}. ${esc(entry.title)}</option>`).join('')}</select><p>${esc(exercise.instruction)}</p>${canSimulate ? `<button id="simulate" ${contextVersion(item) ? 'disabled' : ''}>${contextVersion(item) ? 'Реплика добавлена' : '+ Новая реплика'}</button>` : ''}<button id="reset" title="Сбросить только макет 04; учебные правки будут очищены">Сбросить</button>`;
  bar.querySelector('#exercise').addEventListener('change', event => selectExercise(event.target.value));
  bar.querySelector('#reset').addEventListener('click', () => {
    const exerciseId = saved.exercise;
    saved = {items:{},branches:{},navOpen:true,filter:'all',view:'attention',search:'',ai:false};
    selectExercise(exerciseId); announce('Состояние макета 04 сброшено.');
  });
  bar.querySelector('#simulate')?.addEventListener('click', () => {
    rememberReading(); branchState(branchFor(item)).extra = true;
    data.items.filter(entry => entry.branchId === item.branchId).forEach(entry => { stateFor(entry).proposal = null; });
    persist(); render(); announce('Появилась новая реплика. Ручной черновик сохранён и требует перепроверки.');
  });
}
function navigationCollapsed() {
  return saved.navCollapsed ?? window.matchMedia('(max-width:1150px)').matches;
}
const columnNames={navigation:'Ширина навигации',list:'Ширина списка',assistant:'Ширина ассистента'};
let columnDrag=null, columnFrame=0;
function columnLayout(widths=saved.columnWidths,priority=null) {
  return resolveColumns(shell.clientWidth,{navigationCollapsed:navigationCollapsed(),assistantOpen:shell.classList.contains('has-ai'),widths,priority});
}
function columnHandle(name) {
  return `<div class="column-resizer" data-resize="${name}" role="separator" tabindex="0" aria-orientation="vertical" aria-label="${columnNames[name]}" title="Потяните для изменения ширины · двойной щелчок — сброс"></div>`;
}
function columnMaximum(name) {
  return Math.floor(maximumColumnWidth(shell.clientWidth,name,{navigationCollapsed:navigationCollapsed(),assistantOpen:shell.classList.contains('has-ai'),widths:saved.columnWidths}));
}
function resizedColumnWidths(name,width,before=saved.columnWidths) {
  const layout=resizeColumns(shell.clientWidth,name,width,{navigationCollapsed:navigationCollapsed(),assistantOpen:shell.classList.contains('has-ai'),widths:before});
  const next={...before,[name]:layout[name],workspace:layout.workspace};
  for(const peer of ['list','assistant'])if(layout[peer])next[peer]=layout[peer];
  if(name==='navigation')next.navigation=layout.navigation;
  return next;
}
function updateColumnLayout() {
  const layout=columnLayout();
  if(layout.resizable) {
    shell.style.setProperty('--navigation-width',`${layout.navigation}px`);
    shell.style.setProperty('--list-width',`${layout.list}px`);
    const assistant=layout.assistant || resolveColumns(shell.clientWidth,{navigationCollapsed:navigationCollapsed(),assistantOpen:true,widths:saved.columnWidths}).assistant;
    shell.style.setProperty('--assistant-width',`${assistant}px`);
  }
  shell.querySelectorAll('[data-resize]').forEach(handle=>{
    const name=handle.dataset.resize;
    handle.hidden=!layout.resizable || !layout[name] || name==='navigation' && navigationCollapsed();
    handle.setAttribute('aria-valuemin',COLUMN_LIMITS[name].min);
    handle.setAttribute('aria-valuemax',columnMaximum(name));
    handle.setAttribute('aria-valuenow',Math.round(layout[name]));
    handle.setAttribute('aria-valuetext',`${Math.round(layout[name])} пикселей`);
  });
}
function restoreReadingPosition(pane,top) {
  // A shorter decision surface must not clamp a reader already near the end.
  const missing=top-(pane.scrollHeight-pane.clientHeight);
  if(missing>0) pane.style.setProperty('--reading-slack',`${Math.ceil(parseFloat(pane.style.getPropertyValue('--reading-slack')||0)+missing)}px`);
  pane.scrollTop=top;
}
function restoreReadingAnchor(anchor, keepRoom=false) {
  if(anchor?.node?.isConnected && anchor.pane.clientHeight) {
    const offset=anchor.node.getBoundingClientRect().top-anchor.pane.getBoundingClientRect().top;
    const top=anchor.pane.scrollTop+offset-anchor.offset;
    if(keepRoom) restoreReadingPosition(anchor.pane,top); else anchor.pane.scrollTop=top;
  }
}
function finishColumnDrag(cancel=false) {
  if(!columnDrag) return;
  const drag=columnDrag;columnDrag=null;cancelAnimationFrame(columnFrame);
  drag.handle.classList.remove('is-dragging');
  if(cancel) saved.columnWidths=drag.before;
  updateColumnLayout();restoreReadingAnchor(drag.anchor);
  if(drag.handle.hasPointerCapture(drag.pointer)) drag.handle.releasePointerCapture(drag.pointer);
  shell.classList.remove('is-resizing');shell.classList.add('resize-settle');
  requestAnimationFrame(()=>{
    restoreReadingAnchor(drag.anchor);rememberReading();persist();
    requestAnimationFrame(()=>shell.classList.remove('resize-settle'));
  });
  announce(cancel?'Изменение ширины отменено.':`${columnNames[drag.name]}: ${Math.round(columnLayout()[drag.name])} пикселей.`);
}
function bindColumnResizers() {
  shell.querySelectorAll('[data-resize]').forEach(handle=>{
    const name=handle.dataset.resize,direction=name==='assistant'?-1:1;
    handle.addEventListener('blur',()=>handle.classList.remove('pointer-resize-focus'));
    handle.addEventListener('pointerdown',event=>{
      if(event.button!==0 || handle.hidden) return;
      handle.classList.add('pointer-resize-focus');
      finishThreadMotion();
      finishAssistantMotion();
      finishComposerMotion();
      finishReadingMotions();
      event.preventDefault();stopLayoutMotion();finishColumnDrag();rememberReading();
      const layout=columnLayout();
      columnDrag={name,handle,pointer:event.pointerId,startX:event.clientX,x:event.clientX,width:layout[name],max:columnMaximum(name),before:{...saved.columnWidths},anchor:readingAnchor()};
      handle.classList.add('is-dragging');handle.focus({preventScroll:true});handle.setPointerCapture(event.pointerId);shell.classList.add('is-resizing');
      const follow=()=>{
        const drag=columnDrag;if(!drag)return;
        const width=Math.round(Math.max(COLUMN_LIMITS[name].min,Math.min(drag.max,drag.width+(drag.x-drag.startX)*direction)));
        // A click alone is not a new width preference.
        if(drag.x!==drag.startX) {saved.columnWidths=resizedColumnWidths(name,width,drag.before);updateColumnLayout();}
        else if(drag.moved) {saved.columnWidths={...drag.before};updateColumnLayout();}
        restoreReadingAnchor(drag.anchor);columnFrame=requestAnimationFrame(follow);
      };
      columnFrame=requestAnimationFrame(follow);
    });
    handle.addEventListener('pointermove',event=>{
      if(columnDrag?.pointer!==event.pointerId || columnDrag.handle!==handle)return;
      columnDrag.x=event.clientX;columnDrag.moved ||= event.clientX!==columnDrag.startX;
    });
    handle.addEventListener('pointerup',event=>{
      if(columnDrag?.pointer!==event.pointerId)return;
      const drag=columnDrag;
      if(event.clientX!==drag.startX) saved.columnWidths=resizedColumnWidths(name,Math.round(Math.max(COLUMN_LIMITS[name].min,Math.min(drag.max,drag.width+(event.clientX-drag.startX)*direction))),drag.before);
      else saved.columnWidths={...drag.before};
      finishColumnDrag();
    });
    const cancelDrag=event=>{if(columnDrag?.pointer===event.pointerId && columnDrag.handle===handle)finishColumnDrag(true);};
    handle.addEventListener('pointercancel',cancelDrag);
    handle.addEventListener('lostpointercapture',cancelDrag);
    handle.addEventListener('dblclick',()=>{
      finishColumnDrag();moveLayout(()=>{delete (saved.columnWidths||={})[name];delete saved.columnWidths.workspace;updateColumnLayout();});
      announce(`${columnNames[name]}: исходный размер.`);
    });
    handle.addEventListener('keydown',event=>{
      handle.classList.remove('pointer-resize-focus');
      if(event.key==='Escape' && columnDrag){event.preventDefault();finishColumnDrag(true);return;}
      if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;
      event.preventDefault();
      finishAssistantMotion();
      finishComposerMotion();
      finishReadingMotions();
      const layout=columnLayout(),{min}=COLUMN_LIMITS[name],max=columnMaximum(name);
      const delta=(event.key==='ArrowRight'?1:-1)*direction*(event.shiftKey?32:16);
      const width=event.key==='Home'?min:event.key==='End'?max:Math.max(min,Math.min(max,layout[name]+delta));
      moveLayout(()=>{saved.columnWidths=resizedColumnWidths(name,width);updateColumnLayout();});
    });
  });
}
window.addEventListener('blur',()=>finishColumnDrag(true));
function updateNavigationLayout() {
  const collapsed = navigationCollapsed();
  shell.classList.toggle('nav-collapsed',collapsed);
  shell.classList.toggle('nav-expanded',!collapsed);
  const overview=shell.querySelector('#toggle-overview'),overviewViews=shell.querySelector('#overview-views');
  if(overview&&overviewViews){
    const open=saved.overviewOpen!==false;
    overviewViews.classList.toggle('is-collapsed',!open);overviewViews.inert=!open;
    overviewViews.setAttribute('aria-hidden',String(!open));overview.setAttribute('aria-expanded',String(open));
    const label=`${open?'Свернуть':'Развернуть'} раздел «Обзор»`;
    overview.setAttribute('aria-label',label);overview.title=label;
  }
  const comments=shell.querySelector('#toggle-comments'),views=shell.querySelector('#work-views');
  if(views) {
    views.classList.toggle('is-collapsed',!saved.navOpen);
    views.inert=!saved.navOpen;
    views.setAttribute('aria-hidden',String(!saved.navOpen));
  }
  if(comments) {
    const label=`${saved.navOpen?'Свернуть':'Развернуть'} раздел «Комментарии»`;
    comments.setAttribute('aria-expanded',String(!!saved.navOpen));comments.setAttribute('aria-label',label);comments.title=label;
  }
  const queue=shell.querySelector('.list');
  if(queue) queue.inert=!!saved.ai && window.innerWidth<=1150;
  const toggle = shell.querySelector('#toggle-nav');
  if (toggle) {
    toggle.setAttribute('aria-expanded',String(!collapsed));
    toggle.setAttribute('aria-label',collapsed ? 'Развернуть навигацию' : 'Свернуть навигацию');
    toggle.title = collapsed ? 'Развернуть навигацию' : 'Свернуть навигацию';
  }
  updateColumnLayout();
  const opener=shell.querySelector('#toggle-ai');
  if(opener && !isOverview()) {
    const home=shell.querySelector(listOwnsAssistantToggle()?'.list-title':'.detail-head, .empty-head');
    if(home && opener.parentElement!==home)home.append(opener);
  }
}
function toggleNavigation(collapse = !navigationCollapsed()) {
  finishReadingMotions();
  finishAssistantMotion();
  finishComposerMotion();
  moveLayout(()=>{saved.navCollapsed = collapse; persist(); updateNavigationLayout();});
  shell.querySelector('#toggle-nav')?.focus({preventScroll:true});
}
window.addEventListener('resize',()=>{
  if(!data)return;
  finishGeometryMotion();
  finishReadingMotions();
  finishAssistantMotion();
  finishComposerMotion();
  finishColumnDrag(true);
  stopLayoutMotion();shell.classList.add('resize-settle');updateNavigationLayout();
  if(!isOverview() && shell.querySelector('.ai')?.dataset.contextItem!==(assistantItem()?.id||'')) {
    rememberReading();render();
  }
  requestAnimationFrame(()=>requestAnimationFrame(()=>shell.classList.remove('resize-settle')));
});
window.addEventListener('keydown',event=>{
  if(event.key==='Escape' && document.querySelector('#comment-image-dialog[open]'))return;
  if(event.key==='Escape' && assistantMotion && saved.ai) {event.preventDefault();setAssistantOpen(false);return;}
  if(event.key === 'Escape' && !navigationCollapsed() && window.innerWidth <= 820) {
    event.preventDefault(); toggleNavigation(true);
  }
});
function navigationHtml() {
  const records = queueRecords();
  const counts = Object.fromEntries(Object.keys(labels).map(view => [view,filterList(records,{view,outcome:'all',query:'',filters:filtersFor(view)}).length]));
  const queues=Object.entries(labels).map(([view,label], index) => `<a class="nav-view" href="#view/${view}" data-view="${view}" aria-label="${label}" title="${label}" ${!isOverview() && saved.view === view ? 'aria-current="page"' : ''}>${icon(['CircleAlert','Check','Clock3','CheckCheck','X'][index])}<span class="nav-text">${label}</span><span class="count">${counts[view]}</span></a>`).join('');
  return `<button class="nav-backdrop" aria-label="Закрыть навигацию" tabindex="-1"></button><aside class="navigation" aria-label="Навигация"><div class="nav-brand-slot"></div><nav id="primary-navigation"><button class="nav-group overview-nav" id="toggle-overview" aria-expanded="${saved.overviewOpen!==false}" aria-controls="overview-views"><span class="comments-glyph" aria-hidden="true">${icon('Layers')}<span class="comments-disclosure">${icon('ChevronDown')}</span></span><span class="nav-text">Обзор</span></button><div class="work-views ${saved.overviewOpen===false?'is-collapsed':''}" id="overview-views" role="group" aria-label="Разделы обзора" ${saved.overviewOpen===false?'inert aria-hidden="true"':''}><div class="work-views-content">${[['discussions','Обсуждения','MessagesSquare'],['analytics','Аналитика','Layers'],['history','История действий','Clock3']].map(([key,label,glyph])=>`<a class="nav-view" href="#overview${key==='discussions'?'':'/'+key}" aria-label="${label}" title="${label}" ${isOverview()&&overviewSection()===key?'aria-current="page"':''}>${icon(glyph)}<span class="nav-text">${label}</span></a>`).join('')}</div></div><button class="nav-group" id="toggle-comments" aria-expanded="${!!saved.navOpen}" aria-controls="work-views"><span class="comments-glyph" aria-hidden="true">${icon('Inbox')}<span class="comments-disclosure">${icon('ChevronDown')}</span></span><span class="nav-text">Комментарии</span></button><div class="work-views ${saved.navOpen ? '' : 'is-collapsed'}" id="work-views" role="group" aria-label="Очереди комментариев" ${saved.navOpen ? '' : 'inert aria-hidden="true"'}><div class="work-views-content">${queues}</div></div></nav><div class="nav-footer"><span class="avatar">О</span><div>Оператор</div><button class="icon-button nav-collapse" id="toggle-nav" aria-controls="primary-navigation" aria-expanded="${!navigationCollapsed()}" aria-label="${navigationCollapsed()?'Развернуть навигацию':'Свернуть навигацию'}" title="${navigationCollapsed()?'Развернуть навигацию':'Свернуть навигацию'}">${icon('PanelRight')}</button></div></aside>`;
}
function queueTagsHtml(state,item) {
  const tags=queueTags(state,item,{stale:state.draftContext!==contextVersion(item),currentView:saved.view,currentOutcome:saved.filter});
  return tags.length?`<span class="queue-tags">${tags.map(tag=>`<span class="queue-tag tag-${tag.tone}" ${tag.inferred?'title="Подсказка по объяснению; откройте подробности решения"':''}>${esc(tag.label)}</span>`).join('')}</span>`:'';
}
function rowsHtml() {
  const items = viewItems();
  syncListWindow(items);
  const rows=items.slice(listWindow.start,listWindow.end).map(item => {
    const post = postFor(item), state = stateFor(item), target = messagesFor(item).find(message => message.id === item.targetId);
    const basis=dateBasis(saved.view,filtersFor()),at=validDate(recordDate(recordFor(item),basis));
    const date=at?new Date(at).toLocaleDateString('ru-RU',{day:'numeric',month:'short',timeZone:'Europe/Moscow'}):'Дата неизвестна';
    const dateHtml=at?`<time datetime="${esc(at)}" title="${esc(basisLabels[basis])} · МСК">${esc(date)}</time>`:`<span class="queue-date" title="${esc(basisLabels[basis])} неизвестна">${esc(date)}</span>`;
    return `<article class="queue-row ${queueArrivalEntryIds.has(item.id)?'is-arriving':''}" ${saved.selected === item.id ? 'aria-current="true"' : ''}><div class="row-meta">${channelBadge(post.channel)}<button class="queue-author" type="button" data-author-item="${esc(item.id)}" title="История комментариев автора">${esc(target.author)}</button>${dateHtml}</div><a class="queue-main" href="#item/${esc(item.id)}" data-item="${esc(item.id)}" ${saved.selected===item.id?'aria-current="true"':''}><p>${target.deleted && !target.textUnavailable ? `<s>${esc(target.text)}</s>` : esc(target.textUnavailable ? 'Текст недоступен' : target.text)}</p>${commentMediaHtml(Array.isArray(target.attachments)&&target.attachments.length?target.attachments:item.attachments,{compact:true})}${queueTagsHtml(state,item)}<span class="row-post">${esc(post.title)}</span></a></article>`;
  }).join('');
  if(!items.length)return `<div class="empty">${hasListConditions() ? 'Ничего не найдено. Попробуйте убрать часть условий.' : processingRecordsForView().length ? 'Готовим новые комментарии. Они появятся здесь после разбора.' : saved.view === 'waiting' ? 'Сейчас никого не ждём.' : 'В этом виде нет комментариев.'}${hasListConditions() ? '<button data-clear-list>Сбросить условия</button>' : ''}</div>`;
  return `${listWindow.start?`<div class="list-top-spacer" style="height:${listWindow.topHeight}px"><button class="load-more" id="load-previous">Показать предыдущие ${Math.min(LIST_PAGE,listWindow.start)}</button></div>`:''}${rows}${listWindow.end<items.length?`<button class="load-more" id="load-more">Показать ещё ${Math.min(LIST_PAGE,items.length-listWindow.end)} · осталось ${items.length-listWindow.end}</button>`:''}`;
}
function listHtml() {
  const counts=outcomeCounts(queueRecords(),listOptions());
  return `<section class="list" aria-label="Список"><div class="list-head"><div class="list-title"><h1>${labels[saved.view]}</h1></div><div class="search-tools"><label class="search">${icon('Search')}<input type="search" id="search" placeholder="Текст или автор" aria-label="Текст или автор" value="${esc(saved.search)}"></label></div><div class="list-toolbar"><button id="list-order" class="text-action chronological-order" title="Поменять порядок комментариев">${icon('ArrowDownUp')}${saved.listOrder?.[saved.view]==='oldest'?'Сначала старые':'Сначала новые'}</button><button id="list-filter-button" title="Настроить фильтры" aria-haspopup="dialog">${icon('ListFilter')}<span>Фильтры</span>${filterCount()?`<small aria-label="Условий: ${filterCount()}">${filterCount()}</small>`:''}</button>${['closed','deleted'].includes(saved.view) ? '' : '<button id="bulk-close" class="quiet-button" aria-haspopup="dialog" title="Выбрать группу комментариев для закрытия">Закрыть группу</button>'}</div>${['prepared','closed'].includes(saved.view) ? `<div class="filters" aria-label="${saved.view === 'closed' ? 'Результат обработки' : 'Предложенное действие'}">${[['all','Все'],['reply','С ответом'],['no_reply','Без ответа']].map(([value,label]) => `<button data-filter="${value}" aria-pressed="${saved.filter === value}">${label}<span class="outcome-count">${counts[value]}</span></button>`).join('')}</div>` : ''}${chipsHtml()}</div><div class="queue-scroll">${rowsHtml()}</div></section>`;
}
function threadWindowRoot(item, id) {
  const byId = new Map(messagesFor(item).map(m=>[m.id,m]));
  let m = byId.get(id);
  for (let i=0; i<2 && byId.has(m?.parentId); i++) m=byId.get(m.parentId);
  return byId.has(m?.parentId) ? m.id : null;
}
function messagesHtml(item) {
  const messages = messagesFor(item), state = stateFor(item);
  const byId = new Map(messages.map(m => [m.id,m])), children = new Map();
  for (const m of messages) {
    const parent = byId.has(m.parentId) && m.parentId !== m.id ? m.parentId : null;
    if (!children.has(parent)) children.set(parent,[]);
    children.get(parent).push(m);
  }
  for(const [parent,siblings] of children)children.set(parent,chronologicalSiblings(siblings));
  if (!Object.hasOwn(state,'threadRoot')) { state.threadRoot=threadWindowRoot(item,item.targetId); state.scroll=0; }
  if (state.threadRoot && !byId.has(state.threadRoot)) state.threadRoot=null;
  function descendantCount(id, seen=new Set()) {
    if (seen.has(id)) return 0;
    seen.add(id);
    return (children.get(id)||[]).reduce((n,m)=>n+1+descendantCount(m.id,seen),0);
  }
  const path = new Set();
  for (const id of [item.targetId,...state.expanded]) {
    let m = byId.get(id); const seen = new Set();
    while(m && !seen.has(m.id)) { seen.add(m.id); path.add(m.id); m=byId.get(m.parentId); }
  }
  const rendered = new Set();
  function renderNode(message, depth) {
    if (rendered.has(message.id)) return '';
    rendered.add(message.id);
    const parent = byId.get(message.parentId), messageItem = itemForMessage(message.id);
    const isTarget = message.id === item.targetId, isNew = message.id === branchFor(item).extraReply?.id;
    const article = `<article class="message ${isTarget ? 'is-target' : ''} ${isNew ? 'is-new' : ''} ${message.role === 'brand' ? 'brand' : ''} ${message.deleted ? 'is-deleted' : ''}" id="message-${esc(message.id)}" data-message-id="${esc(message.id)}" tabindex="-1" ${messageItem ? `data-select-comment="${esc(message.id)}"` : ''} aria-label="${message.deleted ? 'Удалён. ' : ''}${isTarget ? 'Выбранный комментарий. ' : ''}${esc(message.author)}: ${esc(message.text)}"><div class="message-meta"><span class="avatar" aria-hidden="true">${esc(message.author.slice(0,1))}</span>${message.role!=='brand' ? `<button class="message-author" data-author-item="${esc(messageItem?.id||item.id)}" data-author-message="${esc(message.id)}" title="История комментариев автора">${esc(message.author)}</button>` : `<strong>${esc(message.author)}</strong>`}${isTarget ? '<span class="message-tag">Выбранный комментарий</span>' : message.role === 'brand' ? '<span class="message-tag">Наш ответ</span>' : ''}${message.deleted ? '<span class="message-tag deleted-tag">Удалён</span>' : ''}<time>${esc(message.time)}${isNew ? ' · Новое' : ''}</time></div>${parent ? `<button class="reply-parent" data-open-comment="${esc(parent.id)}">↳ ${esc(parent.author)}</button>` : message.parentId ? '<p class="message-tag">Исходный ответ недоступен</p>' : ''}<p id="body-${esc(message.id)}" class="message-text ${!isTarget && !state.readExpanded?.includes(message.id) ? 'text-collapsed' : ''}">${message.deleted && !message.textUnavailable ? `<s>${esc(message.text)}</s>` : esc(message.textUnavailable ? 'Текст комментария недоступен' : message.text)}</p>${commentMediaHtml(isTarget&&(!Array.isArray(message.attachments)||!message.attachments.length)?item.attachments:message.attachments)}${!isTarget ? `<button class="text-action read-more" data-read-message="${esc(message.id)}" aria-controls="body-${esc(message.id)}" aria-expanded="${!!state.readExpanded?.includes(message.id)}" hidden>${state.readExpanded?.includes(message.id)?'Свернуть':'Читать дальше'}</button>` : ''}</article>`;
    const replies = children.get(message.id) || [];
    const count = descendantCount(message.id);
    if (depth === 2 && replies.length) return `<div class="thread-node">${article}<button class="thread-continue" data-thread-root="${esc(message.id)}">Продолжить ветку · ${count} ${icon('ChevronRight')}</button></div>`;
    const content = replies.map(m=>renderNode(m,depth+1)).join('');
    const expandable = count > 4 && (depth === 0 || replies.length > 1);
    const open = !state.treeClosed?.includes(message.id) && (path.has(message.id) || state.treeOpen?.includes(message.id));
    const group = content ? `<div class="thread-replies " data-parent-id="${esc(message.id)}">${content}</div>` : '';
    return `<div class="thread-node">${article}${expandable ? `<details class="reply-branch" data-branch-id="${esc(message.id)}" data-path-open="${path.has(message.id)}" ${open?'open':''}><summary>Ответы · ${count}</summary>${group}</details>` : group}</div>`;
  }
  const root = byId.get(state.threadRoot);
  const roots = root ? [root] : children.get(null)||[];
  return roots.map(m=>renderNode(m,0)).join('');
}
function composerHtml(item) {
  const state = stateFor(item), target = messagesFor(item).find(message => message.id === item.targetId), stale = state.draftContext !== contextVersion(item), staleGenerated=state._staleGenerated&&!state.manualEdited;
  if (!isOpen(state)) return completionHtml(item);
  const mediaHold=mediaPreparationHold(item),readiness=replyReadiness(item,state);
  const published=replyFor(recordFor(item));
  const closeLabel=published?'Завершить обработку':'Закрыть без ответа';
  const closeButton=`<button id="close-comment" class="close-comment" ${mediaHold?'disabled':''} title="${published?'Переместить в закрытые. Ранее опубликованный ответ сохранится.':'Закрыть комментарий без отправки ответа.'} Черновик сохранится.">${closeLabel}</button>`;
  const returned=state.events?.some(event=>event.type==='reopened');
  const replyStatus=published ? (state.waitingReason ? 'Ответ опубликован · ждём уточнение' : returned ? 'Возвращён в работу · ответ опубликован' : 'Есть предыдущий ответ') : '';
  const draftStatus=mediaHold?.label||(stale||staleGenerated?'Нужна перепроверка':replyStatus||'Черновик · не отправлен');
  const draftStatusTitle=[...new Set([state.waitingReason||replyStatus,draftStatus].filter(Boolean))].join(' · ');
  const recipient=`Кому: ${target.author}`;
  if (state.decision === 'no_reply' || state.editorCollapsed) return `<section class="composer" aria-label="${state.editorCollapsed ? 'Свёрнутый черновик' : 'Решение без ответа'}"><div class="input-surface editor-surface no-reply"><div class="decision-copy"><h3>${icon('Check')} ${state.editorCollapsed ? 'Черновик свёрнут' : published ? 'Ответ уже опубликован' : 'Предложение: без ответа'}</h3>${published ? `<div class="decision-context">${returned?'Возвращён в работу':'Есть предыдущий ответ'} · можно завершить без нового сообщения.</div>` : ''}</div><div class="composer-actions decision-actions">${closeButton}<button id="change-decision" class="primary-close">${state.replyStarted || state.draft ? 'Продолжить ответ' : 'Подготовить ответ'}</button></div></div></section>`;
  return `<section class="composer" aria-label="Черновик ответа"><div class="input-surface editor-surface">${staleGenerated ? `<div class="stale" role="status">${esc(item.autoPreparation?.sourceChangeReason||'Сохранённый ответ требует проверки. Его текст доступен для правок и обсуждения.')}<div><button id="discuss-saved-draft">Обсудить с ассистентом</button><button id="confirm-saved-draft">Я проверил — оставить этот текст</button></div></div>` : ''}${stale ? `<div class="stale" role="status">Новая реплика может изменить ответ. Ваш текст сохранён.<div><button id="see-new">Прочитать реплику</button><button id="update-draft">Предложить обновление</button><button id="confirm-current">Я проверил — оставить мой текст</button></div></div>` : ''}<label class="sr-only" for="draft">Черновик — ${esc(target.author)}</label><textarea id="draft" aria-describedby="draft-status" placeholder="Написать ответ…" spellcheck="false">${esc(state.draft)}</textarea><div class="composer-actions reply-actions"><div class="edit-tools" role="group" aria-label="Правки черновика">${emojiButton('draft-emoji')}<button class="icon-button" id="undo" aria-label="Отменить правку" title="Отменить правку" ${state.history.length ? '' : 'disabled'}>${icon('Undo2')}</button><button class="icon-button" id="redo" aria-label="Повторить правку" title="Повторить правку" ${state.redo.length ? '' : 'disabled'}><span class="redo-glyph">${icon('Undo2')}</span></button>${item.decision === 'no_reply' ? `<button id="collapse-draft" class="icon-button" aria-label="Свернуть черновик" title="Свернуть черновик — текст сохранится">${icon('ChevronDown')}</button>` : ''}</div>${closeButton}<div class="composer-submit">${sendButton('reply',readiness.disabled,readiness.reason)}</div></div>${state.note && state.note!==state._preparationNote ? `<p class="save-note">${esc(state.note)}</p>` : ''}</div><footer class="composer-meta-footer" aria-label="Сведения о черновике"><strong class="composer-recipient" title="${esc(recipient)}">${esc(recipient)}</strong><span id="draft-status" data-media-hold="${mediaHold?'true':''}" title="${esc(draftStatusTitle)}">${esc(draftStatus)}</span></footer></section>`;
}
function updateDecisionReadiness() {
  const item=selectedItem();if(!item)return;
  updateMediaActionControls(shell,item,stateFor(item));
  // A focused draft defers the full render; refresh only its independent queue.
  if(!document.activeElement?.matches('#draft,.mvp-proposal-edit')||isOverview())return;
  const queue=shell.querySelector('.queue-scroll');if(!queue)return;
  const anchor=captureQueueAnchor();refreshQueueArrivalProjection();
  const records=queueRecords(),counts=outcomeCounts(records,listOptions());
  for(const view of Object.keys(labels)){
    const count=shell.querySelector(`[data-view="${view}"] .count`);
    if(count)count.textContent=filterList(records,{view,outcome:'all',query:'',filters:filtersFor(view)}).length;
  }
  for(const key of ['all','reply','no_reply']){
    const count=shell.querySelector(`[data-filter="${key}"] .outcome-count`);if(count)count.textContent=counts[key];
  }
  const count=shell.querySelector('.list-count');
  if(count)count.textContent=`Найдено: ${viewItems().length} · ${basisLabels[dateBasis(saved.view,filtersFor())].toLocaleLowerCase('ru')}`;
  listObserver?.disconnect();queue.innerHTML=rowsHtml();bindRows();
  queue.querySelector('[data-clear-list]')?.addEventListener('click',clearListConditions);
  queue.scrollTop=anchor?.scrollTop||0;
  const retained=anchor?.itemId&&queue.querySelector(`[data-item="${CSS.escape(anchor.itemId)}"]`)?.closest('.queue-row');
  if(retained)queue.scrollTop+=retained.getBoundingClientRect().top-queue.getBoundingClientRect().top-anchor.offset;
  saved.queueScroll=queue.scrollTop;
}
function assistantTopic() {
  const key=assistantSession().context.topicKey;if(!key)return null;
  return overviewSnapshot(allRecords(),data.posts,overviewTopics,overviewPeriodFilters()).groups.flatMap(g=>g.topics).find(t=>t.key===key);
}
function assistantTopicHtml(){
  const topic=assistantTopic();if(!topic)return '';
  const preview=saved.overviewPreviews?.[topic.key],applied=preview&&saved.overviewInstructions?.[topic.key]?.text===preview.instruction;
  return `<section class="topic-preview"><h3>Указание для темы</h3><p>Напишите, что учитывать в ответах, в сообщении ниже.</p><button id="assistant-topic-preview">Посмотреть пример</button>${preview?`<p>${esc(topic.example)}</p><small>Учебный пример · ${esc(preview.instruction)}</small><button id="assistant-topic-apply" ${applied?'disabled':''}>${applied?'Указание применено':'Применить указание'}</button>`:''}</section>`;
}
function aiHtml(item) { return mvp.aiHtml(); }
function threadControlsHtml(item) {
  const state=stateFor(item),messages=messagesFor(item),root=messages.find(m=>m.id===state.threadRoot);
  const backToSubbranch=messages.some(m=>m.id===state.threadTrail?.at(-1)?.root);
  return `<div class="thread-label">${root ? `<button id="thread-back" class="text-action" title="${backToSubbranch?'Вернуться к предыдущему участку':'Вернуться к полной ветке'}">${icon('ArrowLeft')} Назад</button><span class="thread-context">Подветка · ${esc(root.author)}</span>${backToSubbranch?'<button id="thread-full" class="text-action">Полная ветка</button>':''}` : `<span class="thread-context">Ветка · ${messagesFor(item).length} сообщ.</span>`}<button class="return-to-selected is-concealed" aria-hidden="true" title="Прокрутить ветку к выбранному комментарию" data-jump="${esc(item.targetId)}">К выбранной реплике</button></div>`;
}
function workspaceHtml(item) {
  if (!item) return `<main class="workspace empty-workspace" aria-label="Рабочая область"><header class="empty-head">${listOwnsAssistantToggle()?'':assistantToggleHtml()}</header><p class="empty">Выберите комментарий в списке.</p></main>`;
  const state = stateFor(item), post = postFor(item), thumbnails=postThumbnailCandidates(post,data.posts,data.account||'LikeAvto'),thumbnail=thumbnails[0],thumbnailFallbacks=encodeURIComponent(JSON.stringify(thumbnails.slice(1)));
  const reason=item.reason;
  const postSubtitle=(post.excerpt||post.text||'').startsWith(post.title.replace(/…$/,''))?post.channel:(post.excerpt||post.text||post.channel);
  const details=reason?`<details class="selected-reason" ${state.selectedReasonOpen?'open':''}><summary><span>Почему такое решение</span>${icon('ChevronDown')}</summary><div class="reason-body"><p>${esc(reason)}</p></div></details>`:'';
  const sourceLink=/^https?:\/\//i.test(post.sourceUrl||'')?`<a href="${esc(post.sourceUrl)}" target="_blank" rel="noopener noreferrer">Открыть публикацию ${icon('ArrowUpRight')}</a>`:'';
  const content=state.postPaneOpen?`<article class="post-pane" aria-label="Исходная публикация"><button id="return-thread" class="text-action">${icon('ArrowLeft')} Вернуться к обсуждению</button><div class="post-pane-meta">${channelBadge(post.channel)}<span>${esc(post.channel)}</span></div><h2 id="post-pane-title" tabindex="-1">${esc(post.title)}</h2>${thumbnail?`<img class="post-expanded-media" src="${esc(thumbnail)}" data-thumbnail-fallbacks="${thumbnailFallbacks}" alt="Превью публикации" decoding="async" referrerpolicy="no-referrer">`:''}<div class="post-pane-text" data-reading-block="post-text">${esc(post.text||post.excerpt||'Текст публикации недоступен.')}</div>${sourceLink}${postTranscriptHtml(post,item)}</article>`:`${threadControlsHtml(item)}${details}${messagesHtml(item)}`;
  return `<main class="workspace ${saved.ai ? 'with-ai' : ''}" aria-label="Рабочая область"><section class="detail ${state.postPaneOpen?'showing-post':''}" data-context-item="${esc(item.id)}" aria-label="Разговор и решение"><header class="detail-head"><button class="icon-button back" id="back-list" aria-label="К списку">${icon('ArrowLeft')}</button><button class="source source-toggle" id="toggle-post" aria-controls="central-reading-pane" aria-expanded="${!!state.postPaneOpen}" title="${state.postPaneOpen?'Вернуться к обсуждению':'Открыть исходный пост'}"><span class="post-thumbnail-slot">${thumbnail?`<img class="post-thumbnail" src="${esc(thumbnail)}" data-thumbnail-fallbacks="${thumbnailFallbacks}" alt="" loading="lazy" decoding="async" referrerpolicy="no-referrer">`:''}<span class="post-thumbnail-fallback" aria-hidden="true">${icon('FileText')}</span></span><span class="source-copy"><strong>${esc(post.title)}</strong><span>${esc(postSubtitle)}</span></span><span class="source-chevron">${icon('ChevronDown')}</span></button>${listOwnsAssistantToggle()?'':assistantToggleHtml()}</header>${retainedContextHtml(item)}<div class="thread-scroll" id="central-reading-pane" data-reading-surface="${state.postPaneOpen?'post':'thread'}"><div class="thread-content">${content}</div></div>${state.postPaneOpen?'':composerHtml(item)}</section></main>`;
}
function postTranscriptHtml(post,item) {
  if(!post.transcripts?.length)return '<p class="post-media-note">Расшифровка этого видео пока не добавлена.</p>';
  const opened=new Set(stateFor(item).postTranscriptOpen||[]);
  return post.transcripts.map((transcript,index)=>{
    const key=String(transcript.id||transcript.sourceMaterialId||`position-${index}`);
    return `<details class="post-transcript" data-transcript-id="${esc(key)}" data-reading-block="transcript:${esc(key)}" ${opened.has(key)?'open':''}><summary>Расшифровка видео ${icon('ChevronDown')}</summary>${transcript.shared?'<p class="post-media-note">Общая расшифровка этого видео из другой публикации.</p>':''}${transcript.transcription?.partial?'<p class="post-media-note">Обработано начало ролика — до 15 минут. Расшифровка может быть неполной.</p>':!transcript.transcription?'<p class="post-media-note">Полнота этой расшифровки не подтверждена.</p>':''}<div class="post-transcript-text">${esc(transcript.text)}</div></details>`;
  }).join('');
}
function retainedContextHtml(item) {
  const state=stateFor(item),receipt=state.assistantReceipt;
  const canUndo=receipt && undoPrototypeRestore(recordFor(item),receipt,new Date().toISOString());
  if(state.view===saved.view && !viewItems().some(record=>record.id===item.id))return '<div class="retained-context" role="status"><span>Открытый комментарий вне фильтров · разговор сохранён</span></div>';
  if(!retainedSelection() && !state.localRecovery && !canUndo)return '';
  return `<div class="retained-context" role="status"><span>${state.localRecovery?'Восстановлен в макете · ':''}Сейчас в «${esc(labels[state.view])}»</span>${retainedSelection()?'<button class="text-action" id="follow-comment">Перейти</button>':''}${canUndo?'<button class="text-action" id="undo-assistant-action">Отменить восстановление</button>':''}</div>`;
}
function retainAssistantNode(previous) {
  if(!previous)return;
  const fresh=shell.querySelector('.ai');
  previous.replaceChildren(...fresh.childNodes);
  previous.dataset.contextItem=fresh.dataset.contextItem;
  previous.dataset.assistantScope=fresh.dataset.assistantScope;
  fresh.replaceWith(previous);
}
function highlightPublishedReply(node) {
  if(!node)return;
  const style=getComputedStyle(node),rest={backgroundColor:style.backgroundColor,boxShadow:style.boxShadow};
  const lit={backgroundColor:'#cfdef3',boxShadow:'inset 0 0 0 2px #9bafd0'};
  node.classList.add('is-located');
  const cleanup=()=>node.classList.remove('is-located');
  if(reducedMotion.matches) {
    const inline={backgroundColor:node.style.backgroundColor,boxShadow:node.style.boxShadow};
    Object.assign(node.style,lit);
    setTimeout(()=>{Object.assign(node.style,inline);cleanup();},motion.normal);
    return;
  }
  const animation=node.animate([rest,{...lit,offset:.35},rest],{duration:motion.layout,easing:motion.ease});
  animation.finished.then(cleanup,cleanup);
}
function settleAssistantNavigation(open) {
  const pending=pendingAssistantRender;
  const destination=assistantMotion ? assistantMotion.open : shell.classList.contains('has-ai');
  if(destination!==open) {
    // A retargeted motion replaces its completion callback as well as its path.
    if(assistantMotion)assistantMotion.onSettled=null;
    setAssistantOpen(open,null);
  }
  if(!assistantMotion) {
    saved.ai=open;shell.classList.toggle('has-ai',open);
    pendingAssistantRender=null;render(pending.request);return;
  }
  const running=assistantMotion;
  pending.motion=running;
  // The old conversation is visible only for the transition; navigation stays active.
  shell.querySelector('.workspace, .overview').inert=true;
  running.onSettled=()=>queueMicrotask(()=>{
    if(pendingAssistantRender!==pending || pending.motion!==running)return;
    pendingAssistantRender=null;render(pending.request);
  });
}
function render({focusMessage = null, focusControl = null, highlightMessage = null, threadDirection = 'next'} = {}) {
  rememberVisibleDisclosures();
  if(focusMessage)revealSelectedPath(selectedItem(),focusMessage);
  if(!skipNextReadingSnapshot)rememberOverviewReport();
  rememberTopicReading();
  const sameActor=shell.dataset.actorId===(saved.mvpActorId||operator.id);
  const keepReading=!skipNextReadingSnapshot && sameActor && shell.dataset.screenKey===screenKey();
  skipNextReadingSnapshot=false;
  if(keepReading) {
    const queue=shell.querySelector('.queue-scroll'),overview=shell.querySelector('.overview-posts');
    if(queue?.clientHeight)saved.queueScroll=queue.scrollTop;
    if(overview?.clientHeight)saved.overviewScroll=overview.scrollTop;
  }
  mvp.rememberAssistantReading();
  const request={focusMessage,focusControl,highlightMessage,threadDirection};
  if(pendingAssistantRender) {
    pendingAssistantRender.request=request;
    settleAssistantNavigation(pendingAssistantRender.restoreOpen);
    return;
  }
  if(!reducedMotion.matches && assistantMotion) {
    pendingAssistantRender={request,restoreOpen:saved.ai};
    settleAssistantNavigation(saved.ai);
    return;
  }
  finishGeometryMotion();settleWorkspaceMeasurements=()=>{};
  finishReadingMotions();
  finishComposerMotion();
  finishAssistantMotion();
  finishColumnDrag();
  stopLayoutMotion();
  const surfaceAnchor=threadDirection==='next'&&!focusMessage?captureSurfaceAnchor():null;
  const queueAnchor=sameActor?captureQueueAnchor():null;refreshQueueArrivalProjection();
  if(keepReading&&queueAnchor)saved.queueScroll=queueAnchor.scrollTop;
  const nextScreen=screenKey(),screenChanged=shell.dataset.screenKey!==nextScreen;
  shell.dataset.screenKey=nextScreen;
  shell.dataset.actorId=saved.mvpActorId||operator.id;
  if(screenChanged)queueArrivalEntryIds.clear();
  if(screenChanged)finishThreadMotion();
  listObserver?.disconnect();listResizeObserver?.disconnect();
  if(isOverview()) {finishThreadMotion();renderOverview();return;}
  readingObserver?.disconnect(); composerObserver?.disconnect();
  const item = selectedItem(), aiItem=assistantItem();
  const outgoing=screenChanged?null:captureThreadTransition(item,threadDirection);
  const assistant=saved.ai?shell.querySelector('.ai'):null;
  renderStudy(); shell.className = `shell ${navigationCollapsed()?'nav-collapsed':'nav-expanded'} ${saved.listMode ? 'list-mode' : ''} ${saved.ai ? 'has-ai' : ''} ${listOrderMotion&&!reducedMotion.matches?'list-order-changing':''}`;
  listOrderMotion=false;
  shell.innerHTML = navigationHtml() + `<div class="work-surface">${listHtml()}${workspaceHtml(item)}<div class="assistant-slot" ${saved.ai ? '' : 'inert aria-hidden="true"'}>${aiHtml(aiItem)}</div></div>`;
  if(listOwnsAssistantToggle())shell.querySelector('.list-title').insertAdjacentHTML('beforeend',assistantToggleHtml());
  retainAssistantNode(assistant);
  bindNavigation(); bindList(); if (item) bindWorkspace(item); bindCommentMedia();
  bindAi(aiItem);
  const scroll = shell.querySelector('.thread-scroll'); if (scroll && item) restoreReadingPosition(scroll,stateFor(item)[stateFor(item).postPaneOpen?'postScroll':'scroll']||0);
  if(scroll&&item)restoreSurfaceAnchor(scroll,surfaceAnchor,item);
  if(scroll && item)scroll.addEventListener('scroll',()=>{
    if(scroll.isConnected&&shell.querySelector('.thread-scroll')===scroll)
      stateFor(item)[scroll.dataset.readingSurface==='post'?'postScroll':'scroll']=scroll.scrollTop;
  },{passive:true});
  if(scroll && item)scroll.dataset.threadKey=threadKey(item);
  const queue = shell.querySelector('.queue-scroll'); if (queue) {
    queue.scrollTop = saved.queueScroll || 0;
    if(keepReading&&queueAnchor?.itemId){
      const anchor=queue.querySelector(`[data-item="${CSS.escape(queueAnchor.itemId)}"]`)?.closest('.queue-row');
      if(anchor){queue.scrollTop+=anchor.getBoundingClientRect().top-queue.getBoundingClientRect().top-queueAnchor.offset;saved.queueScroll=queue.scrollTop;}
    }
    queue.addEventListener('scroll',()=>{if(queue.isConnected&&shell.querySelector('.queue-scroll')===queue)saved.queueScroll=queue.scrollTop;},{passive:true});
    observeListEdges(queue);
    observeListWidth(queue);
  }
  queueArrivalEntryIds.clear();
  const renderedSurface=shell.querySelector('.work-surface');
  if (focusMessage && focusMessage === item?.targetId) shell.querySelector('[data-item][aria-current="true"]')?.scrollIntoView({block:'nearest'});
  if (focusMessage) requestAnimationFrame(() => {
    if(!renderedSurface.isConnected)return;
    const node=shell.querySelector(`[data-message-id="${CSS.escape(focusMessage)}"]`);
    node?.scrollIntoView({block:'center'});
    if(highlightMessage===focusMessage) highlightPublishedReply(node);
  });
  if (focusControl) requestAnimationFrame(() => {if(renderedSurface.isConnected)shell.querySelector(focusControl)?.focus({preventScroll:true});});
  enterThread(outgoing);
}
function bindNavigation() {
  mvp.bindExtras();
  for(const [name,selector] of Object.entries({navigation:'.navigation',list:'.list',assistant:'.assistant-slot'})) {
    shell.querySelector(selector)?.insertAdjacentHTML('beforeend',columnHandle(name));
  }
  updateNavigationLayout();
  bindColumnResizers();
  shell.querySelector('#toggle-nav').addEventListener('click',()=>toggleNavigation());
  shell.querySelector('.nav-backdrop').addEventListener('click',()=>toggleNavigation(true));
  shell.querySelector('.navigation').addEventListener('click',event=>{
    if (event.target.closest('a') && window.innerWidth <= 820) {
      saved.navCollapsed=true; persist(); updateNavigationLayout();
    }
  });
  shell.querySelector('.nav-brand-slot').append(brandNode);demoHeader.remove();
  shell.querySelector('#toggle-overview').addEventListener('click',()=>{
    saved.overviewOpen=saved.overviewOpen===false;persist();updateNavigationLayout();
  });
  shell.querySelector('#toggle-comments').addEventListener('click', () => {
    saved.navOpen = !saved.navOpen; persist(); updateNavigationLayout();
  });
  shell.querySelectorAll('[data-view]').forEach(link => link.addEventListener('click', event => {
    event.preventDefault(); navigateView(link.dataset.view);
  }));
}
function bindAuthorHistory() {
  shell.querySelectorAll('[data-author-item]').forEach(button=>{
    if(button.dataset.historyBound)return;
    button.dataset.historyBound='true';
    const activate=event=>{event.preventDefault();event.stopPropagation();openAuthorHistory(button.dataset.authorItem,button.dataset.authorMessage);};
    button.addEventListener('click',activate);
    if(button.tagName!=='BUTTON')button.addEventListener('keydown',event=>{if(event.key==='Enter'||event.key===' ')activate(event);});
  });
}
function openAuthorHistory(itemId,messageId) {
  if(document.querySelector('#author-history-dialog'))return;
  const item=data.items.find(i=>i.id===itemId);if(!item)return;
  const message=messageId?messagesFor(item).find(m=>m.id===messageId):undefined;
  const history=authorHistory(data,item,message), dialog=document.createElement('dialog');
  dialog.id='author-history-dialog';dialog.className='author-history-dialog';dialog.setAttribute('aria-labelledby','author-history-title');
  dialog.innerHTML=`<header><div><h2 id="author-history-title">${esc(history.author)}</h2><p>${history.identified?'История в рабочем пространстве · '+history.entries.length:'Личность автора пока не подтверждена'}<br>${history.identified?'Сохранённые комментарии на этой площадке. Архив может быть неполным.':'Показываем только эту реплику. Данные автора обновляются в фоне.'}</p></div><button class="icon-button" data-dismiss-author aria-label="Закрыть историю автора">${icon('X')}</button></header><div class="author-history-controls"><button class="text-action chronological-order" data-author-order></button></div><div class="author-history-scroll"></div>`;
  const body=dialog.querySelector('.author-history-scroll');let limit=20;
  const paint=()=>{
    const ordered=saved.authorHistoryOrder==='oldest'?[...history.entries].reverse():history.entries;
    dialog.querySelector('[data-author-order]').textContent=saved.authorHistoryOrder==='oldest'?'Сначала старые':'Сначала новые';
    body.innerHTML=ordered.slice(0,limit).map(entry=>`<article class="author-history-entry"><div class="author-history-meta"><span>${esc(entry.post?.channel||item.platform||'')}</span><time>${entry.createdAt?esc(new Date(entry.createdAt).toLocaleString('ru-RU',{dateStyle:'medium',timeStyle:'short',timeZone:'Europe/Moscow'})):''}</time></div><p>${entry.deleted||entry.textUnavailable?'Текст недоступен':esc(entry.text)}</p>${entry.replies.map(reply=>`<blockquote><small>Наш ответ</small>${reply.deleted||reply.textUnavailable?'Текст недоступен':esc(reply.text)}</blockquote>`).join('')}<button class="text-action" data-history-item="${esc(entry.itemId)}" data-history-message="${esc(entry.id)}">${esc(entry.post?.title||'Открыть обсуждение')} ${icon('ArrowUpRight')}</button></article>`).join('')||'<p class="empty">Сохранённых комментариев пока нет.</p>';
    if(history.entries.length>limit)body.insertAdjacentHTML('beforeend','<button class="text-action" data-history-more>Показать ещё</button>');
    body.querySelector('[data-history-more]')?.addEventListener('click',()=>{const top=body.scrollTop;limit+=20;paint();body.scrollTop=top;});
    body.querySelectorAll('[data-history-item]').forEach(button=>button.onclick=()=>{dialog.close();selectItem(button.dataset.historyItem);const chosen=data.items.find(i=>i.id===button.dataset.historyItem);if(chosen)revealMessage(chosen,button.dataset.historyMessage,true);});
  };
  dialog.querySelector('[data-author-order]').onclick=()=>{saved.authorHistoryOrder=saved.authorHistoryOrder==='oldest'?'newest':'oldest';limit=20;persist();paint();body.scrollTop=0;};
  dialog.querySelector('[data-dismiss-author]').onclick=()=>closeDialog(dialog);
  dialog.addEventListener('close',()=>{dialog.remove();shell.querySelector(`[data-author-item="${CSS.escape(itemId)}"]`)?.focus({preventScroll:true});});document.body.append(dialog);paint();openDialog(dialog);
}
function bindRows() {
  bindAuthorHistory();
  shell.querySelectorAll('[data-item]').forEach(link => link.addEventListener('click', event => { event.preventDefault(); selectItem(link.dataset.item); }));
  shell.querySelector('#load-more')?.addEventListener('click',()=>loadListWindow('next',true));
  shell.querySelector('#load-previous')?.addEventListener('click',()=>loadListWindow('previous',true));
}
function loadListWindow(direction,manual=false) {
  const queue=shell.querySelector('.queue-scroll');if(!queue)return;
  const items=viewItems(), before=listWindow;
  if(direction==='next' && before.end>=items.length || direction==='previous' && before.start===0)return;
  rememberReading();
  if(direction==='next') {
    const trim=Math.min(items.length,before.end+LIST_PAGE)-before.start>LIST_MAX_ROWS?LIST_PAGE:0;
    const rendered=[...queue.querySelectorAll('.queue-row')];
    // Distance between row starts includes any future margins or row gaps.
    const height=trim?rendered[trim].getBoundingClientRect().top-rendered[0].getBoundingClientRect().top:0;
    if(trim)listPageHeights.set(before.start,height);
    listWindow=nextListWindow(before,items.length,height);
  } else {
    const previousStart=Math.max(0,before.start-LIST_PAGE);
    const height=listPageHeights.get(previousStart)??(before.start-previousStart)*(listMeasuredMean||ESTIMATED_ROW_HEIGHT);
    listWindow=previousListWindow(before,height);
  }
  const focusControl=manual?(direction==='next'?(listWindow.end<items.length?'#load-more':'.queue-scroll .queue-row:last-of-type .queue-main'):(listWindow.start?'#load-previous':'.queue-scroll .queue-row:first-of-type .queue-main')):null;
  persist();render({focusControl});
}
function observeListEdges(queue) {
  if(typeof IntersectionObserver!=='function')return;
  listObserver=new IntersectionObserver(entries=>{
    for(const entry of entries)if(entry.isIntersecting && entry.target.isConnected && document.activeElement!==entry.target) {
      if(entry.target.id==='load-more')loadListWindow('next');
      else if(entry.target.id==='load-previous')loadListWindow('previous');
      break;
    }
  },{root:queue,rootMargin:'160px 0px'});
  for(const id of ['load-more','load-previous']){const target=queue.querySelector(`#${id}`);if(target)listObserver.observe(target);}
}
function observeListWidth(queue) {
  const measure=()=>{
    const rows=queue.querySelectorAll('.queue-row');
    if(!rows.length)return 0;
    return rows.length>1?(rows[rows.length-1].getBoundingClientRect().top-rows[0].getBoundingClientRect().top)/(rows.length-1):rows[0].getBoundingClientRect().height;
  };
  const update=()=>{
    const width=queue.clientWidth,mean=measure();
    if(!width||!mean)return;
    if(listMeasuredWidth&&width!==listMeasuredWidth&&listMeasuredMean){
      const oldTop=listWindow.topHeight;
      const adjusted=listWindowAfterWidthChange(listWindow,listPageHeights,listMeasuredMean,mean);
      listWindow=adjusted.window;listPageHeights=adjusted.pageHeights;
      const spacer=queue.querySelector('.list-top-spacer');
      if(spacer)spacer.style.height=`${listWindow.topHeight}px`;
      queue.scrollTop+=listWindow.topHeight-oldTop;
      saved.queueScroll=queue.scrollTop;
    }
    listMeasuredWidth=width;listMeasuredMean=mean;
  };
  update();
  if(typeof ResizeObserver==='function'){
    listResizeObserver=new ResizeObserver(update);
    listResizeObserver.observe(queue);
  }
}
function bindList() {
  bindRows();
  shell.querySelector('#list-order')?.addEventListener('click',()=>{saved.listOrder||={};saved.listOrder[saved.view]=saved.listOrder[saved.view]==='oldest'?'newest':'oldest';listOrderMotion=true;refreshList('#list-order');});
  shell.querySelector('#bulk-close')?.addEventListener('click', () => openClosureDialog());
  shell.querySelector('#list-filter-button').addEventListener('click',openListFilters);
  shell.querySelectorAll('[data-clear-list]').forEach(button=>button.addEventListener('click',clearListConditions));
  shell.querySelectorAll('[data-edit-condition]').forEach(button=>button.addEventListener('click',()=>{if(button.dataset.editCondition==='query')shell.querySelector('#search').focus();else openListFilters();}));
  shell.querySelectorAll('[data-remove-condition]').forEach(button=>button.addEventListener('click',()=>{
    const key=button.dataset.removeCondition;
    if(key==='query') saved.search='';
    else if(key==='period') {delete filtersFor().from;delete filtersFor().to;filtersFor().period='all';}
    else delete filtersFor()[key];
    refreshList('#list-filter-button');
  }));
  shell.querySelector('#search').addEventListener('input', event => { saved.search = event.target.value; refreshList('#search'); });
  shell.querySelectorAll('[data-filter]').forEach(button => button.addEventListener('click', () => {
    saved.filter = button.dataset.filter; refreshList(`[data-filter="${saved.filter}"]`);
  }));
}
function revealMessage(item,id,highlight=false) {
  assistantNavigationRevision++;
  rememberReading(); const state = stateFor(item); if (!state.expanded.includes(id)) state.expanded.push(id);
  state.threadRoot=threadWindowRoot(item,id); state.scroll=0;
  persist(); render({focusMessage:id,highlightMessage:highlight?id:null});
}
function openComment(item, id) {
  const targetItem = itemForMessage(id);
  if(targetItem?.id===item.id) {revealMessage(item,id);return;}
  if (targetItem) {
    selectItem(targetItem.id);
    announce(`${labels[saved.view]}${saved.filter === 'no_reply' ? ', без ответа' : ''}. Выбран другой комментарий. Его решение открыто; прежний черновик сохранён.`);
  } else {
    revealMessage(item, id);
    announce('Открыто опубликованное сообщение в контексте. Адресат черновика не изменён.');
  }
}
function bindWorkspace(item) {
  const state = stateFor(item);
  shell.querySelector('#follow-comment')?.addEventListener('click',()=>{
    assistantNavigationRevision++;
    rememberReading();restoreLinkedItem(item.id);saved.listMode=false;
    history.replaceState(null,'',`#item/${encodeURIComponent(item.id)}`);persist();render();
    announce(`Открыт раздел «${labels[saved.view]}». Текущий комментарий сохранён.`);
  });
  shell.querySelector('#undo-assistant-action')?.addEventListener('click',()=>{
    rememberReading();const undone=undoPrototypeRestore(recordFor(item),state.assistantReceipt,new Date().toISOString());
    if(!undone)return;
    delete undone.assistantReceipt;undone.restorePrompt=false;
    undone.chat.push({role:'assistant',text:'Учебное восстановление отменено. Комментарий снова в «Удалённых»; переписка и черновик сохранены.'});
    applyWorkflowChange(item,undone,'#ai-input');
  });
  shell.querySelector('#close-comment')?.addEventListener('click', () => {
    completeOne(item);
  });
  shell.querySelector('#reopen-comment')?.addEventListener('click', () => {
    return announce('Возврат закрытого комментария пока не подключён.');
    rememberReading();
    const reopened = reopenRecord(recordFor(item),new Date().toISOString());
    if (!reopened) return;
    applyWorkflowChange(item,reopened,reopened.decision==='no_reply'?'#change-decision':'#draft');
    announce('Комментарий возвращён в работу. Черновик и история сохранены.');
  });
  const readingPane=shell.querySelector('.thread-scroll');
  readingPane.addEventListener('scroll', event => {
    if(readingPane.isConnected&&shell.querySelector('.thread-scroll')===readingPane)
      {state[readingPane.dataset.readingSurface==='post'?'postScroll':'scroll']=event.target.scrollTop;updateSelectedVisibility();}
  }, {passive:true});
  const togglePost=event=>{rememberReading();state.postPaneOpen=!state.postPaneOpen;persist();render({focusControl:event.detail===0?'#toggle-post':null,threadDirection:state.postPaneOpen?'post-open':'post-close'});};
  shell.querySelector('#toggle-post')?.addEventListener('click',togglePost);
  shell.querySelector('#return-thread')?.addEventListener('click',togglePost);
  shell.querySelector('.selected-reason')?.addEventListener('toggle',event=>{if(!event.target.isConnected)return;state.selectedReasonOpen=event.target.open;persist();});
  shell.querySelectorAll('.post-thumbnail,.post-expanded-media').forEach(image=>image.addEventListener('error',()=>{let fallbacks=[];try{fallbacks=JSON.parse(decodeURIComponent(image.dataset.thumbnailFallbacks||''));}catch{}const next=fallbacks.shift();if(next){image.dataset.thumbnailFallbacks=encodeURIComponent(JSON.stringify(fallbacks));image.src=next;}else image.hidden=true;}));
  shell.querySelector('.decision-reason')?.addEventListener('toggle', event => { if(!event.target.isConnected)return;state.reasonOpen = event.target.open; persist(); });
  shell.querySelector('.completion details')?.addEventListener('toggle',event=>{if(!event.target.isConnected)return;state.completionDraftOpen=event.target.open;persist();});
  shell.querySelectorAll('.post-transcript[data-transcript-id]').forEach(node=>node.addEventListener('toggle',()=>{
    if(!node.isConnected)return;
    state.postTranscriptOpen=[...shell.querySelectorAll('.post-transcript[data-transcript-id]')]
      .filter(entry=>entry.open).map(entry=>entry.dataset.transcriptId);
    persist();
  }));
  shell.querySelector('#back-list').addEventListener('click', () => { assistantNavigationRevision++;rememberReading(); saved.listMode = true; persist(); render(); });
  shell.querySelectorAll('details.reply-branch,details.source,details.decision-reason,details.selected-reason,.completion details').forEach(details=>{
    details.querySelector('summary').addEventListener('click',event=>{event.preventDefault();animateDisclosure(details);});
  });
  function switchThread(root, back=false) {
    rememberReading();
    if (!back) (state.threadTrail ||= []).push({root:state.threadRoot,scroll:state.scroll});
    if (root) {
      const opened = new Set(state.treeOpen || []);
      const closed = new Set(state.treeClosed || []);
      opened.add(root);
      messagesFor(item).filter(message=>message.parentId===root).forEach(message=>opened.add(message.id));
      for(const id of opened)closed.delete(id);
      state.treeOpen=[...opened];state.treeClosed=[...closed];
    }
    state.threadRoot=root; state.scroll=0; persist(); render({focusControl:'.thread-label button',threadDirection:root?'forward':'back'});
  }
  shell.querySelectorAll('[data-thread-root]').forEach(button=>button.addEventListener('click',()=>switchThread(button.dataset.threadRoot)));
  shell.querySelector('#thread-full')?.addEventListener('click',()=>switchThread(null));
  shell.querySelector('#thread-back')?.addEventListener('click',()=>{
    const previous=state.threadTrail?.pop();
    state.threadRoot=previous?.root||null; state.scroll=previous?.scroll||0;
    persist(); render({focusControl:'.thread-label button',threadDirection:'back'});
  });
  const readingPairs=[...shell.querySelectorAll('[data-read-message]')].map(button=>({button,text:document.getElementById(button.getAttribute('aria-controls'))}));
  function updateSelectedVisibility() {
    const pane=shell.querySelector('.thread-scroll'),button=pane?.querySelector('.return-to-selected');
    if(!button)return;
    const target=pane.querySelector(`[data-message-id="${CSS.escape(item.targetId)}"]`);
    const bounds=pane.getBoundingClientRect(),rect=target?.getBoundingClientRect();
    const composer=shell.querySelector('.detail .composer')?.getBoundingClientRect();
    // The floating editor occludes the reading area even though it is inside
    // the scroll viewport. A tiny remaining edge is not a readable comment.
    const bottom=Math.min(bounds.bottom,composer ? composer.top-8 : bounds.bottom);
    const visible=rect && rect.width>0 && rect.height>0 &&
      Math.min(rect.bottom,bottom)-Math.max(rect.top,bounds.top)>=Math.min(32,rect.height);
    button.classList.toggle('is-concealed',!!visible);
    button.setAttribute('aria-hidden',String(!!visible));
  }
  function measureReading(force=false) {
    if(layoutMotionActive() && force!==true)return;
    // Read the whole batch before changing visibility; avoid layout between each row.
    const measured=readingPairs.map(({button,text})=>{
      const expanded=button.getAttribute('aria-expanded')==='true';
      return {button,hidden:!expanded && !text.classList.contains('is-unfolding') && text.scrollHeight<=text.clientHeight+1};
    });
    measured.forEach(({button,hidden})=>{if(button.hidden!==hidden)button.hidden=hidden;});
    updateSelectedVisibility();
  }
  shell.querySelectorAll('[data-read-message]').forEach(button=>button.addEventListener('click',()=>{
    const expanded=button.getAttribute('aria-expanded')!=='true';
    const ids=new Set(state.readExpanded||[]);
    if(expanded)ids.add(button.dataset.readMessage);else ids.delete(button.dataset.readMessage);
    state.readExpanded=[...ids];
    const text=document.getElementById(button.getAttribute('aria-controls'));
    button.setAttribute('aria-expanded',String(expanded));button.textContent=expanded?'Свернуть':'Читать дальше';
    animateReadingText(text,expanded,button,()=>{measureReading();rememberReading();persist();});
    persist();measureReading();
  }));
  readingObserver=new ResizeObserver(()=>measureReading());
  shell.querySelectorAll('.message-text').forEach(text=>readingObserver.observe(text));
  readingObserver.observe(shell.querySelector('.thread-scroll'));
  readingObserver.observe(shell.querySelector('.thread-content'));
  measureReading();
  requestAnimationFrame(()=>{if(shell.querySelector('.detail')?.dataset.contextItem===item.id)updateSelectedVisibility();});
  shell.querySelectorAll('[data-jump]').forEach(button => button.addEventListener('click', () => revealMessage(item,button.dataset.jump,!!button.closest('.completion'))));
  bindAuthorHistory();
  shell.querySelectorAll('[data-open-comment]').forEach(link => link.addEventListener('click', event => {
    event.preventDefault(); openComment(item, link.dataset.openComment);
  }));
  shell.querySelectorAll('.reply-branch').forEach(group => group.addEventListener('toggle', () => {
    if(!group.isConnected)return;
    const opened = new Set(state.treeOpen || []);
    const closed = new Set(state.treeClosed || []);
    if (group.open){opened.add(group.dataset.branchId);closed.delete(group.dataset.branchId);}
    else {opened.delete(group.dataset.branchId);closed.add(group.dataset.branchId);}
    state.treeOpen = [...opened];state.treeClosed=[...closed]; persist();
  }));
  shell.querySelectorAll('.message[data-select-comment]').forEach(message => message.addEventListener('click', event => {
    if (event.target.closest('a,button,video,.comment-media') || window.getSelection()?.toString()) return;
    openComment(item, message.dataset.selectComment);
  }));
  shell.querySelectorAll('[data-jump-new]').forEach(button => button.addEventListener('click', () => revealMessage(item,branchFor(item).extraReply.id)));
  const draft = shell.querySelector('#draft'), assistantInput = shell.querySelector('#ai-input');
  shell.querySelector('.composer .send-button')?.addEventListener('click',()=>mvp.prepareReply(item).catch(()=>{}));
  draft?.addEventListener('blur',event=>{
    if(event.relatedTarget?.closest('.emoji-toggle,.emoji-picker'))return;
    mvp.saveDraft(item).catch(()=>{});
  });
  bindEmojiPicker(shell.querySelector('#draft-emoji'),draft);
  const editorSurface = shell.querySelector('.editor-surface'), assistantForm = shell.querySelector('.ai-form');
  function alignInputs() {
    updateSelectedVisibility();
    const detail = shell.querySelector('.detail'), composer = editorSurface?.closest('.composer');
    if (detail && composer?.clientHeight) {
      const cover = `${Math.ceil(detail.getBoundingClientRect().bottom-composer.getBoundingClientRect().top)}px`;
      if (detail.style.getPropertyValue('--composer-cover') !== cover) detail.style.setProperty('--composer-cover',cover);
    }
    if (!assistantForm || shell.classList.contains('assistant-morphing')) return;
    const visible = editorSurface?.clientHeight > 0;
    const rect = visible ? editorSurface.getBoundingClientRect() : null;
    const panel = assistantForm.closest('.ai');
    const bottom = rect ? `${Math.max(8,panel.getBoundingClientRect().bottom-rect.bottom)}px` : '';
    if (assistantForm.style.getPropertyValue('--paired-bottom') !== bottom) {
      if (bottom) assistantForm.style.setProperty('--paired-bottom',bottom);
      else assistantForm.style.removeProperty('--paired-bottom');
    }
  }
  const assistantPanel=assistantForm.closest('.ai');
  const priorLayout=assistantLayoutListeners.get(assistantPanel);
  if(priorLayout)assistantPanel.removeEventListener('assistant-layout-settled',priorLayout);
  assistantPanel.addEventListener('assistant-layout-settled',alignInputs);
  assistantLayoutListeners.set(assistantPanel,alignInputs);
  const fitInput=fitTextInput;
  const inputs = [draft,assistantInput].filter(Boolean);
  if (inputs.length) {
    const widths = new Map(); let lastHeight=window.innerHeight;
    inputs.forEach(input=>{
      fitInput(input);
      input.addEventListener('input',()=>{fitInput(input);alignInputs();});
    });
    inputs.forEach(input=>widths.set(input,input.clientWidth));
    composerObserver=new ResizeObserver(()=>{
      if(layoutMotionActive())return;
      const heightChanged=lastHeight!==window.innerHeight; lastHeight=window.innerHeight;
      inputs.forEach(input=>{
        if(input.clientWidth!==widths.get(input)||heightChanged) {
          widths.set(input,input.clientWidth); fitInput(input);
        }
      });
      alignInputs();
    });
    inputs.forEach(input=>composerObserver.observe(input));
    if(editorSurface) composerObserver.observe(editorSurface);
    if(assistantForm) composerObserver.observe(assistantForm);
    composerObserver.observe(shell);
  }
  settleWorkspaceMeasurements=()=>{inputs.forEach(fitInput);measureReading(true);alignInputs();};
  alignInputs();
  draft?.addEventListener('focus', () => { editSession = false; });
  draft?.addEventListener('input', event => {
    assistantNavigationRevision++;
    if (!editSession) { state.history.push({draft:state.draft,context:state.draftContext}); editSession = true; }
    state.draft = event.target.value; state.revision++; state.redo = []; state.note = ''; state.manualEdited = true; persist(); mvp.scheduleDraft(item);
    shell.querySelector('#undo').disabled = !state.history.length; shell.querySelector('#redo').disabled = true;
    const status=state.draftContext !== contextVersion(item) ? 'Нужна перепроверка' : 'Локальные правки · сохраняются при выходе из поля';
    shell.querySelector('#draft-status').textContent = status; shell.querySelector('#draft-status').title=status;
    updateMediaActionControls(shell,itemById(item.id)||item,state);
    const apply = shell.querySelector('#apply-proposal'); if (apply) { apply.disabled = true; apply.textContent = 'Черновик изменён — нужна новая версия'; }
  });
  const restore = (from,to) => {
    if (!from.length) return; rememberReading(); to.push({draft:state.draft,context:state.draftContext});
    assistantNavigationRevision++;
    const entry = from.pop(); state.draft = entry.draft; state.draftContext = entry.context; state.revision++; state.proposal = null;
    persist(); render({focusControl:'#draft'}); announce('Версия черновика восстановлена.'); mvp.saveDraft(item).catch(()=>{});
  };
  shell.querySelector('#undo')?.addEventListener('click', () => restore(state.history,state.redo));
  shell.querySelector('#redo')?.addEventListener('click', () => restore(state.redo,state.history));
  shell.querySelector('#confirm-saved-draft')?.addEventListener('click',()=>{assistantNavigationRevision++;state.manualEdited=true;state._staleGenerated=false;state.draftContext=contextVersion(item);persist();render();mvp.saveDraft(item).catch(()=>{});});
  shell.querySelector('#discuss-saved-draft')?.addEventListener('click',()=>{if(!saved.mvpAiInput)saved.mvpAiInput='Проверь сохранённый ответ с учётом текущего обсуждения: '+state.draft;persist();render();setAssistantOpen(true);});
  shell.querySelector('#see-new')?.addEventListener('click', () => revealMessage(item,branchFor(item).extraReply.id));
  shell.querySelector('#update-draft')?.addEventListener('click', () => propose(item,'updated'));
  shell.querySelector('#confirm-current')?.addEventListener('click', () => { rememberReading(); state.draftContext = contextVersion(item); state.note = 'Вы подтвердили актуальность своего текста. Отправки не было.'; persist(); render(); });
  shell.querySelector('#change-decision')?.addEventListener('click', () => {
    transitionComposer(()=>{
      if(state.editorCollapsed){state.editorCollapsed=false;state.note='';return;}
      state.decision='reply';state.view='attention';
      if(!state.replyStarted && !state.manualEdited && !state.draft) state.draft=item.correctionDraft || '';
      state.replyStarted=true;state.note='';
      saved.retainedSelection=state.view!==saved.view?{itemId:item.id,view:saved.view}:null;
    },'#draft');
  });
  shell.querySelector('#collapse-draft')?.addEventListener('click', () => {
    transitionComposer(()=>{
      state.editorCollapsed=true;state.note='';
    },'#change-decision');
  });
}
function propose(item,code) {
  rememberReading(); const state = stateFor(item), updated = code === 'updated';
  if (!isOpen(state)) return;
  const suggestion = updated ? {instruction:'Учесть новую реплику',text:item.updatedDraft} : item.suggestions[Number(code)];
  if (!suggestion?.text || (!updated && contextVersion(item))) return;
  assistantSession().chat.push({role:'user',text:suggestion.instruction});
  assistantSession().chat.push({role:'assistant',text:'Подготовленный учебный вариант ниже. Он станет черновиком только после применения.'});
  state.proposal = {text:suggestion.text,revision:state.revision,context:contextVersion(item)}; saved.ai = true; saved.listMode=false;
  persist(); render(); shell.querySelector('.ai-scroll').scrollTop = shell.querySelector('.ai-scroll').scrollHeight;
  announce('Предложена новая версия. Ваш черновик пока не изменён.');
}
function bindAi(item) { mvp.bindAi(); }
function requestPrototypeRestore(item, text='Восстановить в макете') {
  const state=stateFor(item);if(state.view!=='deleted')return;
  state.restorePrompt=true;
  assistantSession().chat.push({role:'user',text},{role:'assistant',text:'Куда вернуть комментарий? Могу выбрать по сохранённому черновику и причине ожидания. Это учебное восстановление, соцсеть не меняется.'});
  persist();render({focusControl:'[data-restore-to="auto"]'});
  shell.querySelector('.ai-scroll').scrollTop=shell.querySelector('.ai-scroll').scrollHeight;
}
function applyPrototypeRestore(item,destination,text='') {
  return announce('Восстановление комментария пока не подключено.');
  rememberReading();const result=restoreInPrototype(recordFor(item),new Date().toISOString(),destination);
  if(!result)return;
  result.state.restorePrompt=false;result.state.assistantReceipt=result.receipt;
  assistantSession().chat.push({role:'user',text},{role:'assistant',text:`В макете комментарий восстановлен в «${labels[result.receipt.target]}». ${result.receipt.reason} Текущий разбор остаётся открыт.`});
  applyWorkflowChange(item,result.state,'#ai-input');
  shell.querySelector('.ai-scroll').scrollTop=shell.querySelector('.ai-scroll').scrollHeight;
}
function applyWorkflowChange(item,state,focusControl) {
  const outgoing=shell.querySelector(`[data-item="${CSS.escape(item.id)}"]`)?.closest('.queue-row');
  const next=outgoing?.nextElementSibling?.dataset.item, height=outgoing?.getBoundingClientRect().height;
  transitionComposer(()=>{
    saved.items[item.id]=state;
    saved.retainedSelection=state.view!==saved.view?{itemId:item.id,view:saved.view}:null;
    saved.selected=item.id;
    history.replaceState(null,'',`#item/${encodeURIComponent(item.id)}`);
  },focusControl);
  if(outgoing && !reducedMotion.matches && !viewItems().some(entry=>entry.id===item.id)) {
    outgoing.removeAttribute('data-item');outgoing.removeAttribute('href');outgoing.inert=true;outgoing.setAttribute('aria-hidden','true');
    const queue=shell.querySelector('.queue-scroll'),following=next&&queue.querySelector(`[data-item="${CSS.escape(next)}"]`)?.closest('.queue-row');
    queue.insertBefore(outgoing,following||queue.firstChild);
    outgoing.style.overflow='hidden';outgoing.style.minHeight='0';
    const animation=outgoing.animate([{height:`${height}px`,opacity:1},{height:'0px',paddingTop:0,paddingBottom:0,marginBottom:0,borderWidth:0,opacity:0}],{duration:motion.layout,easing:motion.ease,fill:'both'});
    animation.finished.then(()=>outgoing.remove(),()=>outgoing.remove());
  }
  announce(`Комментарий сейчас в «${labels[state.view]}». Текущий разбор сохранён.`);
}
try {
  saved.items ||= {}; saved.branches ||= {}; saved.navOpen ??= true;
  saved.filter ||= 'all'; saved.search ||= ''; saved.view ||= 'attention';
  if(saved.view==='closed' && saved.filter==='unknown')saved.filter='no_reply';
  saved.listFilters ||= {}; initializeOverviewPeriod(saved);
  initializeRecentListFilters(saved,Object.keys(labels));
  for(const view of Object.keys(labels))saved.listFilters[view] ||= {period:'all',dateField:'created'};
  mvp=createMvpConnection({getSaved:()=>saved,getData:()=>data,stateFor,render,announce,updateDecisionReadiness,
    rememberReading,setAssistantOpen,currentAssistantContext,assistantSession,selectedItem,
    assistantNavigationRevision:()=>assistantNavigationRevision,
    assistantScreenKey:()=>JSON.stringify([location.hash,saved.view,saved.selected,saved.listMode,saved.search,saved.filter,saved.listFilters?.[saved.view],saved.overviewTopic]),
    navigateAssistant:target=>{
      if(target.kind==='queue'&&labels[target.workflow]){navigateView(target.workflow);return true;}
      if(target.kind==='comment'&&itemById(target.itemId)){selectItem(target.itemId);return true;}
      return false;
    },
    onActorChange:()=>{topicReading.clear();overviewReportReading.clear();persist();mvp.stop();shell.textContent='Пользователь изменился. Перезагружаем рабочее место…';location.reload();return true;},
    icon,esc,persist,filtersFor,operator,logout:async()=>{persist();mvp.stop();await revokeOperatorSession(operator);location.reload();}});
  [data,icons]=await Promise.all([mvp.load(),fetch('/icons.json').then(r=>{if(!r.ok)throw Error('Icons unavailable');return r.json();})]);
  mvp.hydrate(undefined,{repaint:false});
  const hashId=decodeURIComponent(location.hash.replace(/^#item\//,''));
  refreshQueueArrivalProjection({initial:true,selectedId:itemById(hashId)?hashId:saved.selected});
  if(itemById(hashId))saved.selected=hashId;
  if(!itemById(saved.selected))saved.selected=queueRecords().find(record=>record.state.view===saved.view)?.item.id||null;
  const route=location.hash.match(/^#view\/(\w+)$/)?.[1];if(labels[route])saved.view=route;
  persist();render({focusMessage:selectedItem()?.targetId});mvp.start();
  brandNode.addEventListener('click',event=>{event.preventDefault();location.hash='overview';});
  window.addEventListener('hashchange',()=>{
    if(isOverview()){assistantNavigationRevision++;rememberReading();render();return;}
    const id=decodeURIComponent(location.hash.replace(/^#item\//,''));
    if(itemById(id))selectItem(id);
    else {const view=location.hash.match(/^#view\/(\w+)$/)?.[1];if(labels[view])navigateView(view,false);}
  });
  window.addEventListener('pagehide',()=>{rememberReading();persist();mvp.stop();});
  window.addEventListener('pageshow',event=>{if(event.persisted){mvp.start();void mvp.refresh({background:true}).catch(error=>console.error(error));}});
} catch(error) {
  shell.innerHTML='<p class="empty" role="alert">Не удалось загрузить рабочее место LikeAvto. Обновите страницу. '+esc(error.message)+'</p>';
  console.error(error);
}


function recordFor(item) {
  return {item,state:stateFor(item),messages:messagesFor(item),context:contextVersion(item),
    postId:postFor(item).id,channel:postFor(item).channel,sourceCreatedAt:item.createdAt||null,createdAt:item.createdAt || '2026-09-08T00:00:00+03:00'};
}
function allRecords() {
  return data.items.filter(item => !item.onlyAfterExtra || branchState(branchFor(item)).extra).map(recordFor);
}
function queueRecords() { return queueArrivalProjection.visible; }
function processingRecordsForView() { return queueArrivalProjection.processing.filter(record=>record.state.view===saved.view); }
function captureQueueAnchor() {
  const queue=shell.querySelector('.queue-scroll');
  if(!queue?.clientHeight)return null;
  const top=queue.getBoundingClientRect().top;
  const row=[...queue.querySelectorAll('.queue-row')].find(node=>node.getBoundingClientRect().bottom>top);
  const itemId=row?.querySelector('[data-item]')?.dataset.item;
  return {itemId,offset:row?row.getBoundingClientRect().top-top:0,scrollTop:queue.scrollTop};
}
function refreshQueueArrivalProjection({initial=false,selectedId=saved.selected}={}) {
  const records=allRecords(),signature=arrivalGenerationSignature(data.items),currentIds=new Set(data.items.map(item=>item.id));
  const remoteChanged=queueArrivalSignature!==null&&signature!==queueArrivalSignature;
  const candidates=remoteChanged||initial
    ? arrivalCandidateIds(records.map(record=>record.item),{initial,knownIds:knownRemoteItemIds})
    : [];
  const before=JSON.stringify(saved.queueArrivals||{}),now=Date.now();
  const next=projectQueueArrivals(records,{arrivals:saved.queueArrivals||{},candidateIds:candidates,selectedId,now,establishBaseline:initial});
  saved.queueArrivals=next.arrivals;queueArrivalProjection=next;
  queueArrivalSignature=signature;knownRemoteItemIds=currentIds;
  const listedIds=filterList(next.visible,listOptions()).map(record=>record.item.id);
  queueArrivalEntryIds=new Set(arrivalAnimationIds(next,{remoteChanged,reducedMotion:reducedMotion.matches,inQueueView:!isOverview(),listedIds}));
  clearTimeout(queueArrivalTimer);queueArrivalTimer=null;
  if(next.nextReleaseAt!==null)queueArrivalTimer=setTimeout(()=>render(),Math.max(0,next.nextReleaseAt-now)+25);
  if(before!==JSON.stringify(next.arrivals))persist();
  return {remoteChanged};
}
function hasListConditions() {
  return hasAppliedListConditions(saved.search,filtersFor());
}
function chipsHtml() {
  const f=filtersFor(), chips=[];
  if(saved.search) chips.push(['query',`Поиск: ${saved.search}`]);
  if(f.postId) chips.push(['postId',data.posts.find(post=>post.id===f.postId)?.title || f.postId]);
  if(f.channel) chips.push(['channel',f.channel]);
  if(['closed','deleted'].includes(saved.view)&&f.dateField==='created') chips.push(['dateField','По дате комментария']);
  if(f.period==='week') chips.push(['period','Последние 7 дней']);
  else if(f.from||f.to) chips.push(['period',`${basisLabels[dateBasis(saved.view,f)]}: ${f.from || '…'} — ${f.to || '…'} (МСК)`]);
  const processing=processingRecordsForView().length;
  return `<div class="list-conditions">${chips.map(([key,label])=>`<span class="condition-control" role="group" aria-label="${esc(label)}"><button class="condition-value" data-edit-condition="${key}" aria-label="Изменить условие: ${esc(label)}" title="Изменить: ${esc(label)}"><span>${esc(label)}</span></button><button class="condition-remove" data-remove-condition="${key}" aria-label="Убрать условие: ${esc(label)}" title="Убрать условие">${icon('X')}</button></span>`).join('')}${hasListConditions()?'<button class="clear-conditions" data-clear-list>Сбросить</button>':''}</div><p class="list-count" role="status">Найдено: ${viewItems().length} · ${basisLabels[dateBasis(saved.view,f)].toLocaleLowerCase('ru')}${processing?`<span class="list-processing">Готовим: ${processing}</span>`:''}</p>${dateBasis(saved.view,f)==='closed'&&!f.from&&!f.to&&f.period!=='week'?'<p class="list-date-note">Без даты закрытия — в конце, по дате комментария.</p>':''}`;
}
function refreshList(focusControl = '#search') {
  assistantNavigationRevision++;
  const search = shell.querySelector('#search');
  const caret = focusControl === '#search' ? search?.selectionStart : null;
  rememberReading();
  const items=viewItems();
  // The conversation selection is independent of the list's new reading order.
  listViewKey=listKey();listPageHeights=new Map();listMeasuredWidth=0;listMeasuredMean=0;listWindow=listWindowAfterConditionsChange(items.length);
  saved.queueScroll=0;skipNextReadingSnapshot=true;
  history.replaceState(null,'',saved.selected?`#item/${saved.selected}`:`#view/${saved.view}`); 
  persist(); render({focusControl});
  if(caret!=null) requestAnimationFrame(()=>{try{shell.querySelector('#search')?.setSelectionRange(caret,caret);}catch{}});
}
function clearListConditions() {
  resetListConditions(saved,saved.view);refreshList('#search');
}
function filterCount() {
  const f=filtersFor();
  return [f.postId,f.channel,f.period==='week'||f.from||f.to,['closed','deleted'].includes(saved.view)&&f.dateField==='created'].filter(Boolean).length;
}
function overviewPeriodFilters() {
  const period=saved.overviewPeriod||'all';
  return resolvePeriod({period,...(period==='custom'?{from:saved.overviewFrom||'',to:saved.overviewTo||''}:{})});
}
function overviewPeriodLabel() {
  const period=saved.overviewPeriod||'all';
  if(period==='week')return 'Последние 7 дней';
  if(period==='all')return 'Всё время';
  const format=value=>value.split('-').reverse().join('.');
  return saved.overviewFrom&&saved.overviewTo?`${format(saved.overviewFrom)} — ${format(saved.overviewTo)}`:saved.overviewFrom?`С ${format(saved.overviewFrom)}`:`По ${format(saved.overviewTo)}`;
}
function openListFilters({overview=false}={}) {
  if(document.querySelector('#list-filter-dialog'))return;
  const f=overview?overviewPeriodFilters():filtersFor(),dialog=document.createElement('dialog'),isHistory=!overview&&['closed','deleted'].includes(saved.view);
  const draft={period:f.period||(f.from||f.to?'custom':'all'),channel:f.channel||'',postId:f.postId||'',from:f.from||'',to:f.to||'',dateField:f.dateField||'auto'};
  dialog.id='list-filter-dialog';dialog.className='closure-dialog list-filter-dialog';dialog.setAttribute('aria-labelledby','filter-title');
  const choices=(name,entries)=>entries.map(([value,label])=>`<label class="filter-choice"><input type="radio" name="${name}" value="${esc(value)}" ${draft[name]===value?'checked':''}><span>${esc(label)}</span></label>`).join('');
  const dateText=value=>value?value.split('-').reverse().join('.'):'';
  const parseDate=value=>{
    if(!value.trim())return '';
    const parts=value.trim().match(/^(\d{2})\.(\d{2})\.(\d{4})$/);
    if(!parts)return null;
    const iso=`${parts[3]}-${parts[2]}-${parts[1]}`,date=new Date(`${iso}T12:00:00Z`);
    return Number(parts[3])>=1&&Number.isFinite(date.getTime())&&date.toISOString().slice(0,10)===iso?iso:null;
  };
  const calendarIcon='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><rect x="3" y="5" width="18" height="16" rx="3"/><path d="M7 3v4m10-4v4M3 11h18M7 15h3m4 0h3M7 18h3"/></svg>';
  const dateField=(name,label)=>`<div class="date-field"><label for="filter-${name}">${label}</label><div class="date-control"><input id="filter-${name}" type="text" inputmode="numeric" autocomplete="off" name="${name}" placeholder="дд.мм.гггг" aria-label="${name==='from'?'Начало':'Конец'} периода" value="${esc(dateText(draft[name]))}"><button type="button" class="calendar-toggle" data-calendar="${name}" aria-expanded="false" aria-controls="filter-calendar" aria-label="Календарь: ${name==='from'?'начало':'конец'} периода" title="Открыть или закрыть календарь">${calendarIcon}</button></div></div>`;
  dialog.innerHTML=`<form><header><div><h2 id="filter-title">Фильтры</h2><p>${esc(labels[saved.view])} · только список комментариев</p></div><button type="button" id="dismiss-filters" class="icon-button" aria-label="Закрыть фильтры" title="Закрыть фильтры">${icon('X')}</button></header>
    <div class="filter-body"><fieldset class="filter-section"><legend>Где искать</legend><div class="filter-options">${choices('channel',[['','Все каналы'],...[...new Set(data.posts.map(p=>p.channel))].map(c=>[c,c])])}</div>
    <details class="post-picker"><summary><span><small>Публикация</small><strong id="chosen-post"></strong></span>${icon('ChevronDown')}</summary><div class="post-picker-body"><label class="search">${icon('Search')}<input type="search" id="post-search" placeholder="Найти публикацию" aria-label="Найти публикацию"></label><fieldset class="post-options"><legend class="sr-only">Выберите публикацию</legend>${choices('postId',[['','Все публикации'],...data.posts.map(p=>[p.id,p.title])])}</fieldset><p id="post-empty" hidden>По этому названию ничего не найдено.</p></div></details></fieldset>
    <fieldset class="filter-section"><legend>Когда</legend>${isHistory?`<div class="filter-options date-basis-options">${choices('dateField',[['auto',saved.view==='closed'?'Когда закрыли':'Когда удалили'],['created','Когда написали']])}</div>`:overview?'':'<p class="filter-hint">По дате комментария</p>'}<div class="filter-options">${choices('period',[['all','За всё время'],['week','Последние 7 дней'],['custom','Свой период']])}</div><div class="date-range">${dateField('from','С')}${dateField('to','По включительно')}</div><div id="filter-calendar" class="filter-calendar" role="group" aria-label="Календарь" hidden></div><p class="filter-error" id="date-error" role="status"></p><p class="filter-hint">Время — московское</p></fieldset>
    <p class="filter-context" id="filter-context"></p></div><footer><button type="button" id="reset-filter-fields">Сбросить настройки</button><button type="submit" class="primary-close" id="apply-filters">Показать комментарии</button><output id="filter-preview" aria-live="polite"></output></footer></form>`;
  document.body.append(dialog);
  if(overview){
    dialog.querySelector('#filter-title').textContent='Период обзора';
    dialog.querySelector('header p').textContent='Для обсуждений, аналитики и истории действий';
    dialog.querySelector('.filter-section').hidden=true;
    dialog.querySelector('#dismiss-filters').setAttribute('aria-label','Закрыть выбор периода');
    dialog.querySelector('#dismiss-filters').title='Закрыть выбор периода';
    dialog.querySelector('#reset-filter-fields').textContent='За всё время';
  }
  const form=dialog.querySelector('form'),from=form.elements.from,to=form.elements.to;
  const picker=dialog.querySelector('.post-picker'),dateRange=dialog.querySelector('.date-range');
  let dateAnimation=null,dateOpen=null;
  const revealDates=open=>{
    if(dateOpen===open)return;
    const initial=dateOpen===null,start=dateRange.hidden?0:dateRange.getBoundingClientRect().height;
    const opacity=dateRange.hidden?0:Number(getComputedStyle(dateRange).opacity);
    dateOpen=open;dateAnimation?.cancel();dateAnimation=null;
    dateRange.hidden=false;dateRange.inert=!open;
    if(initial||reducedMotion.matches){dateRange.hidden=!open;return;}
    const target=open?dateRange.getBoundingClientRect().height:0;
    const animation=dateRange.animate([{height:`${start}px`,opacity,marginTop:start?'10px':'0px'},{height:`${target}px`,opacity:open?1:0,marginTop:open?'10px':'0px'}],{duration:motion.normal,easing:motion.ease,fill:'both'});
    dateAnimation=animation;
    animation.finished.then(()=>{if(dateAnimation!==animation)return;dateRange.hidden=!open;animation.cancel();dateAnimation=null;},()=>{});
  };
  picker.querySelector('summary').addEventListener('click',event=>{event.preventDefault();animateDisclosure(picker);});
  const hidePosts=()=>{if(disclosureMotion.get(picker)?.open??picker.open)animateDisclosure(picker);picker.querySelector('summary').focus();};
  const calendar=dialog.querySelector('#filter-calendar');
  let calendarField=null,calendarAnimation=null,calendarMonth='',calendarFocus='';
  const isoDay=date=>date.toISOString().slice(0,10);
  const shiftDay=(iso,days)=>{const date=new Date(`${iso}T12:00:00Z`);date.setUTCDate(date.getUTCDate()+days);return isoDay(date);};
  const closeCalendar=(focus=true,instant=false)=>{
    if(!calendarField)return;
    const trigger=dialog.querySelector(`[data-calendar="${calendarField}"]`);
    calendarField=null;trigger.setAttribute('aria-expanded','false');calendar.inert=true;
    calendarAnimation?.cancel();
    if(focus)trigger.focus({preventScroll:true});
    if(instant||reducedMotion.matches){calendar.hidden=true;return;}
    const animation=calendar.animate([{opacity:1,height:`${calendar.getBoundingClientRect().height}px`,marginTop:'10px'},{opacity:0,height:'0px',marginTop:'0px',paddingBlock:'0px',borderWidth:'0px'}],{duration:motion.normal,easing:motion.ease,fill:'both'});
    calendarAnimation=animation;animation.finished.then(()=>{if(calendarAnimation!==animation)return;calendar.hidden=true;animation.cancel();calendarAnimation=null;},()=>{});
  };
  const paintCalendar=()=>{
    const first=new Date(`${calendarMonth}-01T12:00:00Z`),offset=(first.getUTCDay()+6)%7,today=calendarDay(new Date());
    const monthLabel=first.toLocaleDateString('ru-RU',{month:'long',year:'numeric',timeZone:'UTC'});
    const days=Array.from({length:42},(_,index)=>shiftDay(isoDay(first),index-offset));
    calendar.innerHTML=`<div class="calendar-head"><button type="button" class="icon-button" data-month="-1" aria-label="Предыдущий месяц" title="Предыдущий месяц">${icon('ArrowLeft')}</button><strong aria-live="polite">${esc(monthLabel)}</strong><button type="button" class="icon-button" data-month="1" aria-label="Следующий месяц" title="Следующий месяц">${icon('ArrowRight')}</button></div><div class="calendar-week" aria-hidden="true">${['Пн','Вт','Ср','Чт','Пт','Сб','Вс'].map(d=>`<span>${d}</span>`).join('')}</div><div class="calendar-days" role="group" aria-label="Дни месяца">${days.map(day=>`<button type="button" data-day="${day}" tabindex="${day===calendarFocus?'0':'-1'}" class="${day.slice(0,7)!==calendarMonth?'outside-month':''}" aria-label="${esc(new Date(`${day}T12:00:00Z`).toLocaleDateString('ru-RU',{day:'numeric',month:'long',year:'numeric',timeZone:'UTC'}))}" aria-pressed="${draft[calendarField]===day}" ${day===today?'aria-current="date"':''}>${Number(day.slice(-2))}</button>`).join('')}</div><div class="calendar-footer"><button type="button" data-calendar-clear>Очистить</button><button type="button" data-calendar-today>Сегодня</button></div>`;
  };
  const moveMonth=delta=>{
    const date=new Date(`${calendarMonth}-01T12:00:00Z`);date.setUTCMonth(date.getUTCMonth()+delta);calendarMonth=isoDay(date).slice(0,7);calendarFocus=calendarMonth+'-01';paintCalendar();
  };
  const openCalendar=name=>{
    if(calendarField===name){closeCalendar();return;}
    if(calendarField)closeCalendar(false,true);
    calendarAnimation?.cancel();calendarAnimation=null;calendarField=name;
    calendarFocus=draft[name]||calendarDay(new Date());calendarMonth=calendarFocus.slice(0,7);
    const trigger=dialog.querySelector(`[data-calendar="${name}"]`);trigger.setAttribute('aria-expanded','true');
    calendar.setAttribute('aria-label',`Календарь: ${name==='from'?'начало':'конец'} периода`);
    calendar.hidden=false;calendar.inert=false;paintCalendar();
    if(!reducedMotion.matches)calendarAnimation=calendar.animate([{opacity:0,height:'0px',marginTop:'0px',paddingBlock:'0px',borderWidth:'0px'},{opacity:1,height:`${calendar.getBoundingClientRect().height}px`,marginTop:'10px',paddingBlock:'10px',borderWidth:'1px'}],{duration:motion.normal,easing:motion.ease});
    calendar.querySelector(`[data-day="${calendarFocus}"]`)?.focus({preventScroll:true});
  };
  const chooseDate=value=>{
    const name=calendarField;if(!name)return;draft[name]=value;form.elements[name].value=dateText(value);closeCalendar();update();
  };
  dialog.querySelectorAll('[data-calendar]').forEach(button=>button.addEventListener('click',()=>openCalendar(button.dataset.calendar)));
  calendar.addEventListener('click',event=>{
    const button=event.target.closest('button');if(!button)return;
    if(button.dataset.month){const delta=button.dataset.month;moveMonth(Number(delta));calendar.querySelector(`[data-month="${delta}"]`).focus();}
    else if(button.dataset.day)chooseDate(button.dataset.day);
    else if(button.hasAttribute('data-calendar-clear'))chooseDate('');
    else if(button.hasAttribute('data-calendar-today'))chooseDate(calendarDay(new Date()));
  });
  calendar.addEventListener('keydown',event=>{
    const button=event.target.closest('[data-day]');if(!button)return;
    const steps={ArrowLeft:-1,ArrowRight:1,ArrowUp:-7,ArrowDown:7};
    if(Object.hasOwn(steps,event.key)){event.preventDefault();calendarFocus=shiftDay(button.dataset.day,steps[event.key]);calendarMonth=calendarFocus.slice(0,7);paintCalendar();}
    else if(event.key==='PageUp'||event.key==='PageDown'){event.preventDefault();moveMonth((event.key==='PageUp'?-1:1)*(event.shiftKey?12:1));}
    else if(event.key==='Home'||event.key==='End'){event.preventDefault();const weekday=(new Date(`${button.dataset.day}T12:00:00Z`).getUTCDay()+6)%7;calendarFocus=shiftDay(button.dataset.day,event.key==='Home'?-weekday:6-weekday);calendarMonth=calendarFocus.slice(0,7);paintCalendar();}
    else return;
    calendar.querySelector(`[data-day="${calendarFocus}"]`)?.focus();
  });
  dialog.addEventListener('pointerdown',event=>{if(calendarField&&!event.target.closest('#filter-calendar,[data-calendar]'))closeCalendar(false);});
  const read=()=>({...draft,from:draft.period==='custom'?draft.from:'',to:draft.period==='custom'?draft.to:''});
  const updatePosts=()=>{
    const query=dialog.querySelector('#post-search').value.toLocaleLowerCase('ru').replaceAll('ё','е').trim();
    const records=filterList(queueRecords(),{...listOptions(),filters:{...read(),postId:''}});
    let visible=0;
    dialog.querySelectorAll('.post-options .filter-choice').forEach(label=>{
      const input=label.querySelector('input'),post=data.posts.find(p=>p.id===input.value);
      label.hidden=!!query&&!!post&&!post.title.toLocaleLowerCase('ru').replaceAll('ё','е').includes(query);
      if(!label.hidden)visible++;
      let count=label.querySelector('small');if(!count){count=document.createElement('small');label.append(count);}
      count.textContent=String(input.value?records.filter(r=>r.postId===input.value).length:records.length);
    });
    dialog.querySelector('#post-empty').hidden=visible>1||!query;
    dialog.querySelector('#chosen-post').textContent=data.posts.find(p=>p.id===draft.postId)?.title||'Все публикации';
  };
  const update=()=>{
    const custom=draft.period==='custom';if(!custom)closeCalendar(false,true);revealDates(custom);from.disabled=to.disabled=!custom;
    const invalidText=custom&&(parseDate(from.value)===null||parseDate(to.value)===null);
    const invalid=custom&&(invalidText||(!draft.from&&!draft.to)||(draft.from&&draft.to&&draft.from>draft.to));
    dialog.querySelector('#date-error').textContent=invalid?(invalidText?'Введите существующую дату в формате дд.мм.гггг.':!draft.from&&!draft.to?'Укажите начало или конец периода.':'Конец периода должен быть не раньше начала.'):'';
    to.setCustomValidity(invalid?'Проверьте даты периода.':'');
    if(overview){
      dialog.querySelector('#apply-filters').disabled=!!invalid;
      dialog.querySelector('#apply-filters').textContent=invalid?'Проверить даты':'Применить';
      dialog.querySelector('#filter-preview').textContent='';
      dialog.querySelector('#filter-context').textContent='Обсуждения — по дате комментария. История и график обработки — по дате события.';
      return;
    }
    const count=filterList(queueRecords(),{...listOptions(),filters:read()}).length;
    dialog.querySelector('#apply-filters').disabled=!!invalid;
    dialog.querySelector('#apply-filters').textContent=invalid?'Проверить даты':`Показать · ${count}`;
    dialog.querySelector('#filter-preview').textContent=invalid?'':count?'Открытый разговор и черновик сохранятся.':'Совпадений нет. Попробуйте другой период, канал или публикацию.';
    const context=[];if(saved.search)context.push(`Поиск: «${saved.search}»`);if(saved.filter!=='all')context.push(saved.filter==='reply'?'С ответом':'Без ответа');
    dialog.querySelector('#filter-context').textContent=context.length?`Также учитываем: ${context.join(' · ')}. Эти условия меняются над списком.`:'';
    updatePosts();
  };
  form.addEventListener('input',event=>{
    if(event.target.id==='post-search'){updatePosts();return;}
    const name=event.target.name;if(!Object.hasOwn(draft,name))return;
    draft[name]=name==='from'||name==='to'?(parseDate(event.target.value)||''):event.target.value;update();
    if(name==='postId')hidePosts();
  });
  dialog.querySelector('#post-search').addEventListener('keydown',event=>{if(event.key==='Enter')event.preventDefault();});
  dialog.querySelector('#dismiss-filters').onclick=()=>closeDialog(dialog);
  dialog.querySelector('#reset-filter-fields').onclick=()=>{
    Object.assign(draft,{period:'all',channel:'',postId:'',from:'',to:'',dateField:'auto'});
    form.querySelectorAll('input[type="radio"]').forEach(input=>{input.checked=draft[input.name]===input.value;});
    from.value=to.value='';dialog.querySelector('#post-search').value='';update();
  };
  dialog.addEventListener('cancel',event=>{if(calendarField){event.preventDefault();closeCalendar();}else if(picker.open){event.preventDefault();hidePosts();}else{event.preventDefault();closeDialog(dialog);}});
  form.addEventListener('submit',event=>{event.preventDefault();if(!form.reportValidity()||dialog.querySelector('#apply-filters').disabled)return;if(overview){const selected=read();saved.overviewPeriod=selected.period;saved.overviewPeriodExplicit=true;saved.overviewFrom=selected.from;saved.overviewTo=selected.to;persist();closeDialog(dialog);render();}else{saved.listFilters[saved.view]=read();closeDialog(dialog);refreshList('#list-filter-button');}});
  dialog.addEventListener('close',()=>{calendarAnimation?.cancel();dateAnimation?.cancel();disclosureMotion.get(picker)?.animation.cancel();disclosureMotion.delete(picker);dialog.remove();shell.querySelector(overview?'#overview-period-button':'#list-filter-button')?.focus({preventScroll:true});});
  update();openDialog(dialog);
}
function rowOutcome(state,item) {
  if (state.view === 'deleted') return `Удалён · ${esc(state.deletion?.actor || 'автор неизвестен')}`;
  if (state.view === 'closed') return state.closure?.outcome === 'reply' ? 'Закрыт с ответом' : 'Закрыт без ответа';
  return state.decision === 'no_reply' ? 'Без ответа' : state.draftContext !== contextVersion(item) ? 'Изменился контекст' : state.view === 'attention' ? 'Нужно ваше решение' : 'Черновик подготовлен';
}
function completionHtml(item) {
  const state=stateFor(item);
  return `<section class="composer" aria-label="Результат обработки"><div class="input-surface editor-surface completion"><div class="completion-copy"><div class="completion-head"><strong>${icon('CheckCheck')} Комментарий закрыт</strong><span class="completion-meta">Статус получен от источника</span></div>${state.draft ? `<details ${state.completionDraftOpen?'open':''}><summary>Сохранённый черновик · не отправлен</summary><p class="preserved-draft">${esc(state.draft)}</p></details>` : ''}</div><div class="composer-actions completion-actions"><button class="text-action" data-mvp-history>История действий</button></div></div></section>`;
}

function navigateView(view, push = true, outcome = 'all') {
  assistantNavigationRevision++;
  delete saved.retainedSelection;
  rememberReading(); saved.view = view; saved.filter = outcome; saved.search = '';
  listViewKey='';listWindow=initialListWindow(0);listPageHeights=new Map();skipNextReadingSnapshot=true;
  saved.selected = viewItems()[0]?.id || null; saved.listMode = true; saved.navOpen = true; saved.queueScroll = 0;
  if (push && location.hash !== `#view/${view}`) history.pushState(null,'',`#view/${view}`);
   persist(); render();
}
function showFeedback(message) {
  document.querySelector('.local-result')?.remove();
  const node = document.createElement('div'); node.className = 'local-result'; node.setAttribute('role','status');
  node.innerHTML = `<span>${esc(message)}</span><button data-show-closed>В закрытые</button><button aria-label="Скрыть результат">${icon('X')}</button>`;
  document.body.append(node);
  node.querySelector('[data-show-closed]').onclick = () => {navigateView('closed');node.remove();};
  node.querySelector('[aria-label]').onclick = () => node.remove();
}
function afterCompletion(message) {
  delete saved.retainedSelection;
  saved.selected = viewItems()[0]?.id || null;
  const hash = saved.selected ? `#item/${saved.selected}` : `#view/${saved.view}`;
  history.replaceState(null,'',hash); 
  persist(); render({focusMessage:selectedItem()?.targetId}); showFeedback(message);
}
function completeOne(item) { return mvp.closeOne(item).catch(()=>{}); }
function openClosureDialog() { return mvp.closeMany(viewItems()).catch(()=>{}); }

function isOverview(){return !location.hash || location.hash==='#' || /^#overview(?:\/(?:analytics|history))?$/.test(location.hash);}
function overviewSection(){return location.hash.split('/')[1] || 'discussions';}
function overviewTopics(postId) {
  return [{id:'discussion',label:'Обсуждение публикации',match:/.*/,prompt:'',example:''}];
}

function overviewAnalytics(snapshot){
  const report=analyticsSnapshot(allRecords(),overviewPeriodFilters()),{series,counts,total}=report;
  const number=value=>value.toLocaleString('ru-RU');
  const fullDate=value=>value.split('-').reverse().join('.');
  const range=report.from&&report.to?fullDate(report.from)+' — '+fullDate(report.to):'Период без комментариев';
  const kinds=analyticsKinds;
  const max=Math.max(1,...series.map(day=>day.total));
  const axisStep=Math.max(1,Math.ceil(series.length/15));
  const legend='<div class="activity-legend">'+kinds.map(({key,label,color})=>'<span><i style="background:'+color+'"></i>'+esc(label)+'<b>'+number(counts[key])+'</b></span>').join('')+'</div>';
  const chart='<div class="activity-chart"><div class="activity-scale" aria-hidden="true"><span>'+number(max)+'</span><span>'+number(Math.round(max/2))+'</span><span>0</span></div><div class="overview-bars">'+series.map((day,index)=>{
    const label=report.monthly?day.day.slice(5)+'.'+day.day.slice(0,4):day.day.slice(8)+'.'+day.day.slice(5,7);
    const detail=fullDate(day.day)+' · '+kinds.map(({key,label})=>label+': '+number(day[key])).join(' · ');
    const showLabel=index%axisStep===0||index===series.length-1;
    const tooltipId='analytics-day-'+index;
    const tooltip='<span class="activity-tooltip" id="'+tooltipId+'" role="tooltip" hidden><strong>'+esc(report.monthly?day.day.slice(5)+'.'+day.day.slice(0,4):fullDate(day.day))+'</strong>'+kinds.map(({key,label,color})=>'<span class="activity-tooltip-row"><i style="background:'+color+'"></i><span>'+esc(label)+'</span><b>'+number(day[key])+'</b></span>').join('')+'<span class="activity-tooltip-total"><span>Всего</span><b>'+number(day.total)+'</b></span></span>';
    return '<div class="overview-bar activity-bar" role="img" tabindex="0" data-analytics-tooltip aria-label="'+esc(detail)+'" aria-describedby="'+tooltipId+'"><span class="bar-track"><span class="bar-stack" style="height:'+day.total/max*100+'%">'+[...kinds].reverse().map(({key,color})=>day[key]?'<span class="activity-segment" style="flex:'+day[key]+';background:'+color+'"></span>':'').join('')+'</span></span><small '+(showLabel?'':'aria-hidden="true"')+'>'+ (showLabel?label:'&nbsp;')+'</small>'+tooltip+'</div>';
  }).join('')+'</div></div>';
  const themes=snapshot.groups.flatMap(group=>group.topics.map(topic=>({...topic,count:topic.total,post:group.post}))).sort((a,b)=>b.count-a.count).slice(0,6);
  return '<section class="overview-report analytics-report" aria-label="Аналитика обсуждений"><div class="report-intro"><div><h2>Комментарии и обработка</h2><p>'+esc(range)+' · МСК</p></div><span class="report-source">Загруженные данные LikeAvto</span></div><div class="report-metrics"><div><span>За выбранный период</span><strong>'+number(total)+'</strong><small>комментариев, включая удалённые</small></div><div><span>За последние 24 часа</span><strong>'+number(report.last24)+'</strong><small>поступило комментариев</small></div><div><span>Всего необработанных</span><strong>'+number(report.allOpen)+'</strong><small>за всё время в загруженных данных</small></div></div><div class="report-grid"><section class="report-block report-dynamics"><div class="report-chart-heading"><div><h3>Текущее состояние комментариев</h3><p>'+esc(range)+' · '+(report.monthly?'по месяцам':'по дням')+' поступления</p></div><div class="report-open-total"><strong>'+number(report.open)+'</strong><span>новые · всего открытых за период</span></div></div>'+(total?chart:'<p class="overview-empty">За этот период комментариев нет.</p>')+legend+(report.undated?'<p class="activity-evidence-note">Без даты поступления: '+number(report.undated)+'. В график и выбранный период не включены.</p>':'')+'</section><section class="report-block"><h3>Обработка за выбранный период</h3><p>Текущее состояние комментариев, поступивших за период</p><div class="status-breakdown">'+kinds.map(({key,label,color})=>'<div><span>'+esc(label)+'</span><strong>'+number(counts[key])+'</strong><div><i style="width:'+(total?counts[key]/total*100:0)+'%;background:'+color+'"></i></div></div>').join('')+'</div></section><section class="report-block report-themes"><h3>О чём говорят чаще</h3><p>Повторяющиеся темы под отдельными постами</p>'+ (themes.map(t=>'<div class="report-theme"><div><strong>'+esc(t.label)+'</strong><small>'+esc(t.post.title)+'</small></div><span>'+number(t.count)+'</span></div>').join('')||'<p class="overview-empty">Повторяющихся тем пока нет.</p>')+'</section></div></section>';
}
function overviewHistory(){
  const period=overviewPeriodFilters();
  const events=allRecords().flatMap(record=>(record.state.events||[]).map(event=>({event,record}))).filter(({event})=>{const day=calendarDay(event.at);return day&&(!period.from||day>=period.from)&&(!period.to||day<=period.to);}).sort((a,b)=>(saved.overviewHistoryOrder==='oldest'?1:-1)*(Date.parse(a.event.at)-Date.parse(b.event.at)));
  const eventLabel=event=>event.type==='new'?'Новый комментарий':event.type==='restored_local'?`Восстановлен · ${labels[event.target]||'возвращён в работу'}`:event.type==='restore_undone'?'Восстановление отменено':event.type==='reopened'?'Возвращён в работу':event.type==='deleted'?'Удалён':event.type==='closed'?(event.outcome==='reply'?'Закрыт с ответом':'Закрыт без ответа'):event.type==='prepared'?'Подготовлен':event.type==='sent'?'Ответ отправлен':'Изменение обработки';
  return `<section class="overview-report" aria-label="История действий"><div class="report-intro"><div><h2>Что происходило с комментариями</h2><p>По времени действия · московское время · ${events.length} записей</p></div><span class="report-source">Локальная история</span></div><div class="overview-history">${events.map(({event,record})=>{const message=record.messages.find(m=>m.id===record.item.targetId),post=data.posts.find(p=>p.id===record.postId);return `<button class="history-entry" data-overview-comment="${esc(record.item.id)}"><span class="history-entry-icon">${icon(event.type==='deleted'?'X':event.type==='closed'?'CheckCheck':'ArrowUpRight')}</span><span class="history-entry-body"><span><strong>${esc(eventLabel(event))}</strong><time datetime="${esc(event.at)}">${esc(dateLabel(event.at))}</time></span><small>${esc(event.actor||'Автор не указан')} · ${esc(message?.author||'Комментарий')} · ${esc(post?.channel||'')}</small><p>${esc(message?.textUnavailable?'Текст комментария недоступен':message?.text||'Комментарий без текста')}</p>${event.reason?`<small class="history-reason">${esc(event.reason)}</small>`:''}</span>${icon('ChevronRight')}</button>`;}).join('')||'<p class="overview-empty">За этот период действий нет. Попробуйте выбрать «Всё время».</p>'}</div></section>`;
}
function renderOverview(){
  readingObserver?.disconnect(); composerObserver?.disconnect();
  const assistant=saved.ai?shell.querySelector('.ai'):null;
  saved.overviewInputs ||= {};saved.overviewPreviews ||= {};saved.overviewInstructions ||= {};
  const snapshot=overviewSnapshot(allRecords(),data.posts,overviewTopics,overviewPeriodFilters());
  const groups=sortDiscussions(snapshot.groups,saved.discussionOrder),topic=groups.flatMap(g=>g.topics).find(t=>t.key===saved.overviewTopic),group=groups.find(g=>g.topics.includes(topic));
  const section=overviewSection();
  const sortOrder=(section==='history'?saved.overviewHistoryOrder:saved.discussionOrder)==='oldest'?'oldest':'newest';
  const topicCount=groups.reduce((n,g)=>n+g.topics.length,0),topicOpen=groups.reduce((n,g)=>n+g.count,0);
  const periodCopy=saved.overviewPeriod==='all'?'за всё время':saved.overviewPeriod==='custom'?'за выбранный период':'за 7 дней';
  shell.className=`shell overview-shell ${navigationCollapsed()?'nav-collapsed':'nav-expanded'} ${saved.ai?'has-ai':''}`;
  if(bar)bar.hidden=true;
  shell.innerHTML=navigationHtml()+`<div class="work-surface"><main class="overview" data-overview-section="${section}" aria-label="Обзор обсуждений"><header class="overview-head"><div class="overview-title"><h1>${{discussions:'Обсуждения',analytics:'Аналитика',history:'История действий'}[section]}</h1><p>Обзор · обсуждения под вашими постами</p></div><div class="overview-period">${section!=='analytics'?`<label class="sr-only" for="overview-order">Порядок</label><select id="overview-order" aria-label="${section==='discussions'?'Порядок постов по последнему комментарию':'Порядок действий'}"><option value="newest" ${sortOrder==='newest'?'selected':''}>Сначала новые</option><option value="oldest" ${sortOrder==='oldest'?'selected':''}>Сначала старые</option></select>`:''}<button id="overview-period-button" aria-haspopup="dialog">${esc(overviewPeriodLabel())} ${icon('ChevronDown')}</button></div></header>${section==='discussions'?`<div class="overview-summary"><span class="overview-metric"><strong>${groups.length}</strong><span>постов</span></span><span class="overview-metric"><strong>${topicCount}</strong><span>повторяющихся тем</span></span><span class="overview-metric"><strong>${topicOpen}</strong><span>комментариев в работе</span><small>${periodCopy}</small></span><span class="overview-period-note">По дате комментария · МСК</span><span class="overview-period-note">Только собранные данные</span></div><div class="overview-layout ${topic?'has-topic':''}"><section class="overview-posts" aria-label="Посты и темы"><div class="overview-section-title"><strong>Все загруженные посты</strong><span>По последнему комментарию</span></div>${groups.map(g=>`<article class="overview-post"><div class="overview-post-meta">${channelBadge(g.post.channel)}<span>${esc(g.post.channel)}</span><span>${g.count} осталось · ${g.total} всего</span></div><h2>${esc(g.post.title)}</h2><p>${esc(g.post.excerpt)}</p><div class="overview-post-activity">${g.lastActivity?`Последний комментарий · <time datetime="${esc(g.lastActivity)}">${esc(dateLabel(g.lastActivity))}</time>`:'Нет комментариев за выбранный период'}${g.records.length?`<button class="text-action" data-overview-comment="${esc([...g.records].sort((a,b)=>(Date.parse(b.createdAt)||0)-(Date.parse(a.createdAt)||0))[0].item.id)}">Открыть обсуждение ${icon('ArrowUpRight')}</button>`:''}</div><div class="overview-topics">${g.topics.map(t=>`<button data-overview-topic="${esc(t.key)}" aria-pressed="${topic?.key===t.key}"><span class="topic-name">${esc(t.label)}</span><span class="topic-count"><strong>${t.open.length}</strong><small>из ${t.total}</small>${icon('ChevronRight')}</span></button>`).join('')||'<p class="overview-no-topics">Повторяющихся тем нет</p>'}</div></article>`).join('')||'<div class="overview-empty"><p>Посты ещё не загружены.</p><button id="overview-empty-period" class="text-action">Выбрать другой период</button></div>'}</section>${topic?overviewDetail(group,topic):''}</div>`:section==='analytics'?overviewAnalytics(snapshot):overviewHistory()}</main></div>`;
  shell.querySelector('.overview-head').insertAdjacentHTML('beforeend',assistantToggleHtml());
  shell.querySelector('.work-surface').insertAdjacentHTML('beforeend',`<div class="assistant-slot" ${saved.ai?'':'inert aria-hidden="true"'}>${aiHtml(null)}</div>`);
  retainAssistantNode(assistant);
  restoreTopicReading(topic);
  const report=shell.querySelector('.overview-report');
  if(report){
    report.scrollTop=overviewReportReading.get(section)||0;
    report.addEventListener('scroll',()=>{
      if(report.isConnected&&shell.querySelector('.overview-report')===report)
        overviewReportReading.set(section,report.scrollTop);
    },{passive:true});
  }
  if(shell.querySelector('.overview-posts')){
    const posts=shell.querySelector('.overview-posts');posts.scrollTop=saved.overviewScroll||0;
    posts.addEventListener('scroll',()=>{if(posts.isConnected&&shell.querySelector('.overview-posts')===posts)saved.overviewScroll=posts.scrollTop;},{passive:true});
  }
  bindNavigation();
  bindAi(null);
  if(section==='analytics')bindAnalyticsTooltips(shell.querySelector('.analytics-report'));
  shell.querySelector('#overview-order')?.addEventListener('change',event=>{saved[section==='history'?'overviewHistoryOrder':'discussionOrder']=event.target.value;saved.overviewScroll=0;overviewReportReading.set(section,0);skipNextReadingSnapshot=true;persist();render();});
  shell.querySelector('#overview-older')?.addEventListener('click',()=>{saved.overviewPeriod='all';saved.overviewPeriodExplicit=true;persist();render();});
  shell.querySelector('#overview-arrival')?.addEventListener('click',()=>{const branch=data.branches.find(b=>b.id==='branch-trip');branchState(branch).extra=true;saved.overviewArrivalAt=new Date().toISOString();itemById('item-trip-arrival').createdAt=saved.overviewArrivalAt;persist();render();announce('Добавлен учебный комментарий. Пост снова доступен, прежние указания сохранены.');});
  shell.querySelectorAll('#overview-period-button,#overview-empty-period').forEach(button=>button.addEventListener('click',()=>openListFilters({overview:true})));
  shell.querySelector('#overview-attention')?.addEventListener('click',()=>{saved.listFilters.attention=overviewPeriodFilters();navigateView('attention');});
  shell.querySelectorAll('[data-overview-topic]').forEach(button=>button.onclick=event=>{
    const key=button.dataset.overviewTopic;
    saved.overviewScroll=shell.querySelector('.overview-posts')?.scrollTop||0;saved.overviewTopic=key;persist();render();
    const heading=shell.querySelector('#topic-title');
    const destination=event.detail===0 && heading?.getClientRects().length?heading:shell.querySelector(`[data-overview-topic="${CSS.escape(key)}"]`);
    destination?.focus({preventScroll:true});
  });
  shell.querySelector('#close-topic')?.addEventListener('click',()=>{const key=saved.overviewTopic;saved.overviewTopic=null;persist();render();shell.querySelector(`[data-overview-topic="${CSS.escape(key)}"]`)?.focus();});
  shell.querySelectorAll('[data-overview-comment]').forEach(button=>button.onclick=()=>selectItem(button.dataset.overviewComment));
  shell.querySelector('#discuss-topic')?.addEventListener('click',()=>{
    const session=assistantSession();session.context=currentAssistantContext();
    if(!saved.mvpAiInput)saved.mvpAiInput='Предложи указание для темы «'+topic.label+'» под постом «'+group.post.title+'». ';
    if(!session.aiInput)session.aiInput=saved.overviewInputs[topic.key]||'';
    persist();render();setAssistantOpen(true);
  });
  shell.querySelector('#set-topic-instruction')?.addEventListener('click',()=>mvp.openInstructionEditor(group.post.id));
  shell.querySelector('#open-global-rules')?.addEventListener('click',()=>openCompanyRules(group));
  shell.querySelector('#retry-topic-instructions')?.addEventListener('click',()=>mvp.loadInstructions());
  shell.querySelector('.topic-panel')?.addEventListener('keydown',event=>{if(event.key==='Escape')shell.querySelector('#close-topic').click();});
}
function instructionRule(entry) {
  const descriptor=entry.displaySemantics;
  const title=esc(descriptor?.label||entry.title||'Указание');
  let body=`<p>${esc(entry.text)}</p>`;
  if(descriptor){
    const values=descriptor.policyType==='reply_constraint'?descriptor.constraint.values:null;
    const readable=values?`<p>${values.map(value=>`• ${esc(value)}`).join('<br>')}</p>`
      :'<p>Смешанное руководство. Исходный текст доступен ниже.</p>';
    body=`${readable}<details class="instruction-source"><summary>Исходная запись</summary><p>Название: ${esc(entry.title||'Указание')}</p><p>${esc(entry.text)}</p></details>`;
  }
  return `<details data-rule-version="${esc(entry.id)}"><summary>${title}</summary>${body}</details>`;
}
function topicInstructions(group){
  const context=mvp.instructionContext(group.post.id);
  if(context.status==='loading'||context.status==='idle')return '<section class="topic-instructions"><h3>Действующие указания</h3><p role="status">Загружаем действующие указания…</p></section>';
  if(context.status!=='ready')return '<section class="topic-instructions"><h3>Действующие указания</h3><p role="status">Не удалось проверить действующие указания.</p><button id="retry-topic-instructions" class="text-action">Повторить</button></section>';
  const rows=displayInstructions(context.post).map(instructionRule).join('');
  return `<section class="topic-instructions"><h3>Указания для этого поста · ${context.post.length}</h3>${rows||'<p>Для этого поста отдельных указаний пока нет.</p>'}<button class="text-action company-rules-link" id="open-global-rules" type="button">Общие правила аккаунта · ${context.global.length} ${icon('ArrowUpRight')}</button><p class="instruction-scope-note">Общие правила действуют для всех постов аккаунта. Указания для поста действуют во всех его темах.</p></section>`;
}
function openCompanyRules(group) {
  if(document.querySelector('#company-rules-dialog'))return;
  const context=mvp.instructionContext(group.post.id),dialog=document.createElement('dialog');
  dialog.id='company-rules-dialog';dialog.className='company-rules-dialog';
  dialog.setAttribute('aria-labelledby','company-rules-title');
  const rows=context.status==='ready'?displayInstructions(context.global).map(instructionRule).join(''):'';
  dialog.innerHTML=`<header><div><h2 id="company-rules-title">Общие правила аккаунта</h2><p>Действуют для всех постов этого аккаунта · ${context.status==='ready'?context.global.length:'статус неизвестен'}</p></div><button class="icon-button" type="button" data-close-company aria-label="Закрыть общие правила">${icon('X')}</button></header><div class="company-rules-scroll">${context.status==='ready'?(rows||'<p>Общих правил пока нет.</p>'):'<p role="status">Не удалось проверить общие правила. Повторите попытку позже.</p>'}</div>`;
  dialog.querySelector('[data-close-company]').addEventListener('click',()=>closeDialog(dialog));
  dialog.addEventListener('close',()=>{dialog.remove();shell.querySelector('#open-global-rules')?.focus({preventScroll:true});});
  document.body.append(dialog);openDialog(dialog);
}
function overviewDetail(group,topic){
  const samples=[...topic.records].sort((a,b)=>Number(b.state.view==='attention')-Number(a.state.view==='attention')).slice(0,5);
  return `<aside class="topic-panel" data-topic-key="${esc(topic.key)}" aria-label="Выбранная тема"><header class="topic-head"><button id="close-topic" class="icon-button topic-back" aria-label="Вернуться к постам и темам" title="Вернуться к постам и темам">${icon('ArrowLeft')}</button><div><p>${esc(group.post.channel)} · только этот пост</p><h2 id="topic-title" tabindex="-1">${esc(topic.label)}</h2><span class="topic-post-name">${esc(group.post.title)}</span></div></header><div class="topic-scroll">${topicInstructions(group)}<section class="topic-examples"><h3>Из обсуждения <span>${topic.open.length} осталось · ${topic.total} всего</span></h3>${samples.length?samples.map(r=>{const m=r.messages.find(m=>m.id===r.item.targetId);return `<button class="topic-comment" data-overview-comment="${esc(r.item.id)}"><span><strong>${esc(m.author)}</strong><small>${esc(labels[r.state.view])}</small></span><p>${esc(m.text)}</p><span class="topic-open">Открыть ветку ${icon('ArrowUpRight')}</span></button>`;}).join(''):'<p class="overview-empty">За выбранный период примеров нет.</p>'}</section></div><section class="topic-direction"><button id="set-topic-instruction">${icon('Plus')} Задать указание</button><button id="discuss-topic">${icon('MessagesSquare')} Обсудить новое указание</button><p>Ассистент поможет сформулировать указание. Сообщение в чате само по себе не меняет действующие правила.</p></section></aside>`;
}

