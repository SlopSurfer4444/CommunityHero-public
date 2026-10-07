// One-time, asserted adaptation of the archived iteration-04 source.
import {readFile,writeFile} from 'node:fs/promises';
const path=new URL('../workshop/app.js',import.meta.url);
let s=await readFile(path,'utf8');
function replace(from,to){if(!s.includes(from))throw Error('Missing seam: '+from.slice(0,100));s=s.replace(from,to);}
function section(start,end,body){const a=s.indexOf(start),b=s.indexOf(end,a);if(a<0||b<0)throw Error('Missing section '+start);s=s.slice(0,a)+body+'\n'+s.slice(b);}
replace("import {isOpen", "import {createMvpConnection} from './mvp-connection.js';\nimport {isOpen");
replace("const liveReadMode = new URLSearchParams(location.search).get('source') === 'angry';", "const liveReadMode = false;\nlet mvp;");
replace("const storageKey = liveReadMode ? 'communityhero-angry-likeavto-read-v1' : 'communityhero-space-workshop-20260908-v4';", "const storageKey = 'communityhero-mvp-original-workshop-v1';");
replace("if (!liveReadMode && new URLSearchParams(location.search).get('workshop') === '1')", "if (false)");
replace("{period:'week'}; }", "{period:'all',dateField:'created'}; }");
replace("Не удалось сохранить учебные правки в браузере.", "Не удалось сохранить локальные правки в браузере.");
replace("const label = destination === 'reply' ? 'Отправить ответ' : 'Отправить ассистенту';", "const label = destination === 'reply' ? 'Проверить ответ перед отправкой' : 'Отправить ассистенту';");
replace("${label} — публичная отправка не подключена", "${label} — сначала напишите ответ");
replace("${sendButton('reply',true)}", "${sendButton('reply',!state.draft.trim())}");
section('function aiHtml(item) {','function threadControlsHtml(', 'function aiHtml(item) { return mvp.aiHtml(); }');
section('function bindAi(item) {','function requestPrototypeRestore(', 'function bindAi(item) { mvp.bindAi(); }');
replace('function requestPrototypeRestore(item,', 'function requestPrototypeRestore(item,');
replace("function applyPrototypeRestore(item,destination,text=destination==='auto'?'Реши сама':`Верни в «${labels[destination]}»`) {", "function applyPrototypeRestore(item,destination,text='') {\n  return announce('Восстановление в Angry.Space пока не подключено.');");
section('function completeOne(item) {','function isOverview()', `function completeOne(item) { return mvp.closeOne(item); }
function openClosureDialog() { return mvp.closeMany(viewItems()); }
`);
section('function overviewTopics(postId){','function overviewAnalytics(', `function overviewTopics(postId) {
  return [{id:'discussion',label:'Обсуждение публикации',match:/.*/,prompt:'',example:''}];
}
`);
const exerciseStart=s.indexOf('<details class="overview-exercise">');
const exerciseEnd=s.indexOf('</details>',exerciseStart);
if(exerciseStart<0||exerciseEnd<0)throw Error('Missing exercise');
s=s.slice(0,exerciseStart)+'<span class="overview-period-note">LikeAvto · загруженная выборка</span>'+s.slice(exerciseEnd+10);
section('function completionHtml(item) {','function navigateView(', `function completionHtml(item) {
  const state=stateFor(item);
  return \`<section class="composer" aria-label="Результат обработки"><div class="input-surface editor-surface completion"><div class="completion-copy"><div class="completion-head"><strong>\${icon('CheckCheck')} Закрыто в Angry.Space</strong><span class="completion-meta">Статус получен от источника</span></div>\${state.draft ? \`<details><summary>Сохранённый черновик · не отправлен</summary><p class="preserved-draft">\${esc(state.draft)}</p></details>\` : ''}</div><div class="composer-actions completion-actions"><button class="text-action" data-mvp-history>История действий</button></div></div></section>\`;
}
`);
replace("shell.querySelector('#reopen-comment')?.addEventListener('click', () => {", "shell.querySelector('#reopen-comment')?.addEventListener('click', () => {\n    return announce('Возврат закрытого комментария пока не подключён.');");
replace("const draft = shell.querySelector('#draft'), assistantInput = shell.querySelector('#ai-input');", `const draft = shell.querySelector('#draft'), assistantInput = shell.querySelector('#ai-input');
  shell.querySelector('.composer .send-button')?.addEventListener('click',()=>mvp.prepareReply(item));
  draft?.addEventListener('blur',()=>mvp.saveDraft(item));`);
replace("state.manualEdited = true; persist();", "state.manualEdited = true; persist();\n    const send=shell.querySelector('.composer .send-button');if(send)send.disabled=!state.draft.trim();");
replace("'Сохранено в макете'", "'Локальные правки · сохраняются при выходе из поля'");
replace("announce('Версия черновика восстановлена.');", "announce('Версия черновика восстановлена.'); mvp.saveDraft(item);");
replace("function bindNavigation() {", "function bindNavigation() {\n  mvp.bindExtras();");
replace("'В учебных сценах сейчас никого не ждём.'", "'Сейчас никого не ждём.'");
section('if (!liveReadMode) try {','function recordFor(', `try {
  saved.items ||= {}; saved.branches ||= {}; saved.navOpen ??= true;
  saved.filter ||= 'all'; saved.search ||= ''; saved.view ||= 'attention';
  saved.listFilters ||= {}; saved.overviewPeriod ||= 'all';
  for(const view of Object.keys(labels))saved.listFilters[view] ||= {period:'all',dateField:'created'};
  mvp=createMvpConnection({getSaved:()=>saved,getData:()=>data,stateFor,render,announce,
    rememberReading,setAssistantOpen,currentAssistantContext,assistantSession,selectedItem,
    icon,esc,persist,filtersFor});
  [data,icons]=await Promise.all([mvp.load(),fetch('/icons.json').then(r=>{if(!r.ok)throw Error('Icons unavailable');return r.json();})]);
  mvp.hydrate();
  const hashId=decodeURIComponent(location.hash.replace(/^#item\\//,''));
  if(itemById(hashId))saved.selected=hashId;
  if(!itemById(saved.selected))saved.selected=data.items.find(i=>stateFor(i).view===saved.view)?.id||null;
  const route=location.hash.match(/^#view\\/(\\w+)$/)?.[1];if(labels[route])saved.view=route;
  persist();render({focusMessage:selectedItem()?.targetId});
  brandNode.addEventListener('click',event=>{event.preventDefault();location.hash='overview';});
  window.addEventListener('hashchange',()=>{
    if(isOverview()){rememberReading();render();return;}
    const id=decodeURIComponent(location.hash.replace(/^#item\\//,''));
    if(itemById(id))selectItem(id);
    else {const view=location.hash.match(/^#view\\/(\\w+)$/)?.[1];if(labels[view])navigateView(view,false);}
  });
  window.addEventListener('pagehide',()=>{rememberReading();persist();});
} catch(error) {
  shell.innerHTML='<p class="empty" role="alert">Не удалось загрузить рабочее место LikeAvto. Обновите страницу. '+esc(error.message)+'</p>';
  console.error(error);
}

`);
// No synthetic fallback, even if someone navigates to the old ?source=angry route.
s=s.slice(0,s.indexOf('// The Angry.Space view has its own read-only state'));
await writeFile(path,s);
