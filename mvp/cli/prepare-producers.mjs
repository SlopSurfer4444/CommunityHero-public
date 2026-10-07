// Local observers own separate, already-persisted child identities. This module
// never writes checkpoints/journals, retries admission or cancels a server job.
export function createPrepareProducers({maxProducers=1,signal}={}) {
  if(!Number.isSafeInteger(maxProducers)||maxProducers<0)
    throw new TypeError('maxProducers must be a nonnegative safe integer');
  if(signal!==undefined&&!(signal instanceof AbortSignal))
    throw new TypeError('signal must be an AbortSignal');
  const owned=new Map(),windows=new Set(),waiters=new Set();
  const notify=()=>{for(const resolve of waiters)resolve();waiters.clear();};
  let closed=false;
  const release=entry=>{
    // Concurrent joins cannot release a later owner of the same identity.
    if(owned.get(entry.id)===entry){owned.delete(entry.id);windows.delete(entry.windowId);notify();}
  };
  return {
    start({id,windowId,run}) {
      if(typeof id!=='string'||!id||typeof windowId!=='string'||!windowId||typeof run!=='function')
        throw new TypeError('A persisted child id, windowId and run function are required');
      if(closed||signal?.aborted||owned.has(id)||windows.has(windowId)||owned.size>=maxProducers)return false;
      const controller=new AbortController();
      const stop=()=>controller.abort(signal.reason);
      signal?.addEventListener('abort',stop,{once:true});
      const entry={id,windowId,controller};
      // Register ownership before the factory runs, including synchronous
      // failures. Rejections are values from the moment the promise is created.
      owned.set(id,entry);windows.add(windowId);
      entry.promise=Promise.resolve().then(()=>{
        controller.signal.throwIfAborted();
        return run(controller.signal);
      }).then(checkpoint=>({id,checkpoint}),error=>({id,error}))
        .then(result=>{entry.result=result;notify();return result;})
        .finally(()=>signal?.removeEventListener('abort',stop));
      return true;
    },
    has(id) { return owned.has(id); },
    get size() { return owned.size; },
    async waitReady() {
      while(owned.size&&!Array.from(owned.values()).some(entry=>entry.result))
        await new Promise(resolve=>waiters.add(resolve));
    },
    async joinReady({wait=false}={}) {
      // The queue remains the only consumer. Wait for one observed child, not
      // the whole cohort; an unfinished sibling keeps its original owner.
      // An acknowledged first child can register siblings after this wait
      // starts. A race over the initial promise list would miss their results.
      while(wait&&owned.size&&!Array.from(owned.values()).some(entry=>entry.result))
        await new Promise(resolve=>waiters.add(resolve));
      const entries=Array.from(owned.values()).filter(entry=>entry.result);
      const results=await Promise.all(entries.map(entry=>entry.promise));
      for(const entry of entries)release(entry);
      return results;
    },
    async join(id) {
      const entry=owned.get(id);
      if(!entry)return undefined;
      const result=await entry.promise;release(entry);return result;
    },
    async joinAll({stop=false}={}) {
      if(stop)closed=true;
      const entries=[...owned.values()];
      if(stop)for(const entry of entries)entry.controller.abort();
      const results=await Promise.all(entries.map(entry=>entry.promise));
      for(const entry of entries)release(entry);
      return results;
    },
  };
}
