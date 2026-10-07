// A loaded, filtered queue is never a provider total. Only canonical current
// coverage can establish a completed pass; mutable queues are not snapshots.
export function queueCoverageState(sync={},mode='open') {
  const row=mode==='open'?sync.openCoverage:sync.scan?.closed;
  if(!row||typeof row!=='object'||mode==='open'&&row.scope!=='all-open')return 'unknown';
  if(row.invalidatedAt||sync.scan?.invalidatedAt)return 'incomplete';
  const accounting=row.accounting,count=value=>Number.isSafeInteger(value)&&value>=0;
  const counted=accounting?.version===1
    &&['trackedUnique','importedUnique','unresolvedUnique','unverifiedPages'].every(key=>count(accounting[key]))
    &&typeof accounting.overflow==='boolean'
    &&accounting.importedUnique+accounting.unresolvedUnique===accounting.trackedUnique;
  if(row.done===true&&row.traversalComplete===true&&row.contextComplete===true&&row.coverageComplete===true
    &&counted&&accounting.unresolvedUnique===0&&accounting.unverifiedPages===0&&!accounting.overflow
    &&!(row.unknownDates>0))return 'complete';
  if(row.coverageComplete===false||row.done===false||row.traversalComplete===false||row.contextComplete===false)return 'incomplete';
  return 'unknown';
}

export function queueCoveragePresentation(sync={},view='attention') {
  const mode=['closed','deleted'].includes(view)?'closed':'open',state=queueCoverageState(sync,mode);
  const retrying=sync.status==='error'||sync.background?.state==='backoff';
  const note=state==='complete'
    ?mode==='open'?'Сверка очереди завершена.':'Сверка истории за период загрузки завершена.'
    :retrying?'Связь временно недоступна. Полнота списка пока не подтверждена.'
    :state==='incomplete'?mode==='open'?'Сверка очереди продолжается.':'История загружается.'
    :mode==='open'?'Полнота очереди пока не подтверждена.':'Полнота истории пока не подтверждена.';
  return {state,note,countTitle:'Количество среди загруженных комментариев с учётом условий списка.'};
}

export function loadedQueueCountLabel(count,filtered=false) {
  return `${filtered?'Найдено среди загруженных':'Загружено'}: ${count}`;
}
