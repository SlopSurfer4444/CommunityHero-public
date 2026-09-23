// Presentation of the server's strict media gate; never infer proof from media text.
export function mediaPreparationHold(item={}) {
  const current=Object.hasOwn(item,'mediaReadiness');
  const marker=current?item.mediaReadiness:item.preparationMediaWait;
  if(!current&&marker==null)return null;
  const valid=current&&marker?.schemaVersion===2&&marker.required===true;
  if(valid&&marker.status==='ready')return null;
  const waiting=(valid||!current)&&marker?.status==='media_wait';
  return {status:waiting?'media_wait':'media_unavailable',
    label:waiting?'Получаем контекст видео':'Контекст видео пока недоступен',
    detail:waiting?'Получаем контекст видео. Черновик сохранён; действия доступны после проверки.':'Контекст видео пока недоступен. Черновик сохранён; действия ждут завершения анализа.'};
}

export function replyReadiness(item,state={}) {
  const hold=mediaPreparationHold(item);
  if(hold)return {disabled:true,reason:hold.label};
  if(state._staleGenerated&&!state.manualEdited)return {disabled:true,reason:'Сначала проверьте сохранённый ответ'};
  if(!String(state.draft||'').trim())return {disabled:true,reason:'Сначала напишите ответ'};
  return {disabled:false,reason:''};
}

export function assertMediaReady(item) {
  const hold=mediaPreparationHold(item);
  if(hold)throw new Error(hold.detail);
}

export function reviewMediaHold(proposals=[],items=[]) {
  const byId=new Map(items.map(item=>[item.id,item]));
  for(const proposal of proposals){
    const item=byId.get(proposal.itemId);
    if(!item)return {label:'Комментарий недоступен',detail:'Обновите список перед подтверждением.'};
    const hold=mediaPreparationHold(item);if(hold)return hold;
  }
  return null;
}

// Refresh these controls even while a focused draft prevents a full repaint.
export function updateMediaActionControls(root,item,state) {
  const hold=mediaPreparationHold(item),readiness=replyReadiness(item,state);
  const send=root.querySelector('.composer .send-button');
  if(send){send.disabled=readiness.disabled;send.title=readiness.reason||'Проверить ответ перед отправкой';}
  const close=root.querySelector('#close-comment');if(close)close.disabled=!!hold;
  const status=root.querySelector('#draft-status');
  if(status&&hold){status.dataset.mediaHold='true';status.textContent=hold.label;status.title=hold.detail;}
  else if(status?.dataset.mediaHold){delete status.dataset.mediaHold;status.textContent=state._staleGenerated&&!state.manualEdited?'Нужна перепроверка':'Черновик · не отправлен';status.title=status.textContent;}
}
