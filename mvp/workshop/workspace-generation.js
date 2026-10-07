const pattern=/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/u;
export const workspaceGenerationHeader='x-communityhero-workspace-generation';
export function canonicalWorkspaceGeneration(value){
  if(value===null||typeof value==='string'&&pattern.test(value))return value;
  const error=new Error('Рабочее место изменилось. Обновите страницу для новой очереди.');
  error.code='WORKSPACE_GENERATION_MISMATCH';error.status=409;throw error;
}
export function createWorkspaceGenerationFence(initial){
  let generation=initial===undefined?undefined:canonicalWorkspaceGeneration(initial);
  const pin=value=>{
    const next=canonicalWorkspaceGeneration(value);
    if(generation!==undefined&&generation!==next)canonicalWorkspaceGeneration(undefined);
    generation=next;return generation;
  };
  return {
    get generation(){return generation;},
    headers(){return generation?{[workspaceGenerationHeader]:generation}:{};},
    observe(value,response,{initial=false}={}){
      const header=response?.headers?.get?.(workspaceGenerationHeader);
      if(header!=null)pin(header);
      if(Object.hasOwn(value||{},'storageGeneration'))pin(value.storageGeneration);
      else if(initial&&header==null)pin(null);
      return generation;
    }
  };
}
