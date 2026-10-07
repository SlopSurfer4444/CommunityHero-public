// Synthetic stdio lifecycle fixture. No provider, OS-secret or network calls.
import {serveProviderSession} from '../provider-session.mjs';
let retiring=false,active=0,closed=0;
const runs=[];
try {
  await serveProviderSession({account:'likeavto',limits:{retirementAckMs:250},session:{
    get retiring(){return retiring;},
    async run(request){
      active++;runs.push(request.itemId);
      try{
        if(request.delayMs)await new Promise(resolve=>setTimeout(resolve,request.delayMs));
        if(request.fail){retiring=true;throw Object.assign(new Error('synthetic read failure'),{code:'TRANSPORT_ERROR'});}
        return {itemId:request.itemId,settled:true};
      }finally{active--;}
    },
    async close(){
      closed++;process.stderr.write(JSON.stringify({kind:'fixture-close',active,closed,runs})+'\n');
      if(active!==0)throw new Error('FIXTURE_CLOSED_ACTIVE');
    },
  }});
}catch(error){
  process.stdout.write(JSON.stringify({type:'fatal',error:{code:error.code??'FIXTURE_FAILED'}})+'\n');
  process.exitCode=1;
}
