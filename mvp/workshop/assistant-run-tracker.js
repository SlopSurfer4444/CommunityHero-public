// A browser session may show old assistant messages, but only the current
// submitted run may move the operator's screen.
export function createAssistantRunTracker() {
  let active = null;
  let sequence = 0;
  return {
    begin({conversationId, operatorId, navigationRevision, screenKey}) {
      active = {token: ++sequence, conversationId, operatorId, navigationRevision, screenKey, jobId: null, navigated: false};
      return active.token;
    },
    bind(token, jobId) {
      if (active?.token === token && typeof jobId === 'string' && jobId) active.jobId = jobId;
    },
    clear() {active = null;},
    current() {return active;},
    navigation({conversationId, operatorId, navigationRevision, screenKey, job, message}) {
      const run = active;
      if (!run || run.navigated || !run.jobId || run.conversationId !== conversationId
        || run.operatorId !== operatorId || run.navigationRevision !== navigationRevision
        || run.screenKey !== screenKey || job?.id !== run.jobId || job?.kind !== 'assistant'
        || job?.refId !== conversationId || job?.operatorId !== operatorId
        || job?.status !== 'completed' || message?.role !== 'assistant'
        || message?.prepareRunId !== run.jobId) return null;
      const target = message.navigation;
      if (!target || target.kind === 'queue' && !['attention', 'prepared', 'waiting', 'closed'].includes(target.workflow)
        || target.kind === 'comment' && (typeof target.itemId !== 'string' || !target.itemId)
        || !['queue', 'comment'].includes(target.kind)) return null;
      run.navigated = true;
      return target;
    }
  };
}
