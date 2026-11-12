// Independent receiver-task peer: no SDK or shared validators/state machinery.
import {createInterface} from 'node:readline';
import {createServer} from 'node:http';
const mode=process.argv[1]??'stdio', scenario=process.argv[2]??'complete', method=process.argv[3]??'sampling/createMessage';
const related='io.modelcontextprotocol/related-task';
let emit, original, finish, taskId, acceptanceCount=0, started=false, replayed=false, resultSeen=false, cancelSeen=false;
let callback;
let resultReplies=0;
let getSeen=false, listSeen=false, getAfterSeen=false, released=false;
const cancelling=scenario==='cancel' || scenario==='saturated';
const assert=(yes,message)=>{if(!yes)throw new Error(message);};
function releaseWork(){
  if(cancelling || released || !getSeen || !listSeen || (scenario==='wait-cancel' && !getAfterSeen))return;
  released=true;
  emit({jsonrpc:'2.0',method:'notifications/message',params:{level:'info',data:'receiver-controls-observed'}});
}
function begin(){
  if(started || !emit || !original)return;
  started=true;
  callback={jsonrpc:'2.0',id:'create-work',method,params:method==='sampling/createMessage'
    ? {messages:[{role:'user',content:{type:'text',text:'model work'}}],maxTokens:16,task:{ttl:1000}}
    : {message:'Continue?',requestedSchema:{type:'object',properties:{}},task:{ttl:1000}}};
  emit(callback);
}
function done(){
  if(!resultSeen || (cancelling && !cancelSeen))return;
  finish({jsonrpc:'2.0',id:original.id,result:{content:[],acceptances:acceptanceCount,resultSeen,cancelSeen}});
}
function control(id,method,params){emit({jsonrpc:'2.0',id,method,params});}
function response(r){
  if(r.id==='create-work'){
    assert(r.result?.task?.status==='working','initial result is task acceptance, never model output');
    const id=r.result.task.taskId;
    assert(typeof id==='string' && id.length>=32,'receiver generated opaque ID');
    if(taskId)assert(taskId===id,'acceptance replay must keep the task ID');
    taskId=id; acceptanceCount++;
    if(acceptanceCount===1){
      // Same request redelivery while its handler runs must not dispatch twice.
      if(scenario!=='saturated'){
        setTimeout(()=>emit(callback),5);
        control('get','tasks/get',{taskId,_meta:{[related]:{taskId:'irrelevant'}}});
        control('list','tasks/list',{});
      }
      control('result','tasks/result',{taskId});
      if(scenario==='saturated')control('result2','tasks/result',{taskId});
      if(cancelling)setTimeout(()=>control('cancel','tasks/cancel',{taskId}),20);
      if(scenario==='wait-cancel')setTimeout(()=>emit({jsonrpc:'2.0',method:'notifications/cancelled',params:{requestId:'result'}}),20);
    }
  }else if(r.id==='get' || r.id==='get-after'){
    assert(['working','input_required'].includes(r.result?.status),'get progresses before the model finishes');
    if(r.id==='get')getSeen=true;else getAfterSeen=true;
    releaseWork();
  }else if(r.id==='list'){
    assert(r.result?.tasks.some(t=>t.taskId===taskId),'retrievable task is listed');
    listSeen=true;releaseWork();
  }else if(r.id==='result' || r.id==='result2'){
    if(scenario==='wait-cancel' && r.id==='result'){
      assert(r.error?.code===-32800,'cancelled result RPC must stop waiting');
      control('get-after','tasks/get',{taskId});
      control('result2','tasks/result',{taskId});return;
    }
    if(cancelling)assert(r.error?.code===-32800,'cancelled task result is terminal error');
    else if(method==='sampling/createMessage')assert(r.result?.model==='live-model','saved sampling result');
    else assert(r.result?.action==='decline','saved elicitation result');
    assert((r.result??r.error)?._meta?.[related]?.taskId===taskId,'result association');
    resultReplies++;resultSeen=resultReplies>=(scenario==='saturated'?2:1);done();
  }else if(r.id==='cancel'){
    assert(r.result?.status==='cancelled','cancel is terminal before its response');
    if(!replayed){replayed=true;control('cancel','tasks/cancel',{taskId});}
    else {cancelSeen=true;control('cancel-again','tasks/cancel',{taskId});}
  }else if(r.id==='cancel-again'){
    assert(r.error?.code===-32602,'new cancellation of terminal task is invalid');done();
  }else throw new Error(`unexpected response ${JSON.stringify(r)}`);
}
async function receive(r,send,disconnect){
  if(!r.method){
    if(mode==='http-json' && scenario==='lost-ack' && r.id==='create-work' && acceptanceCount===0){
      taskId=r.result?.task?.taskId;assert(taskId,'lost ACK already observed acceptance');disconnect();return;
    }
    response(r);return;
  }
  if(r.method==='initialize'){
    const tasks=r.params.capabilities.tasks;
    assert(tasks?.requests?.sampling?.createMessage && tasks?.requests?.elicitation?.create && tasks.list && tasks.cancel,'derived task capabilities');
    send({jsonrpc:'2.0',id:r.id,result:{protocolVersion:'2025-11-25',serverInfo:{name:'receiver-peer',version:'1'},capabilities:{tools:{},logging:{}}}});
  }else if(r.method==='notifications/initialized'){}
  else if(r.method==='tools/call'){original=r;finish=send;begin();}
  else if(r.method==='notifications/cancelled'){}
  else throw new Error(`unexpected method ${r.method}`);
}
if(mode==='stdio'){
  const send=r=>process.stdout.write(JSON.stringify(r)+'\n');emit=send;
  for await(const line of createInterface({input:process.stdin}))await receive(JSON.parse(line),send,()=>{});
}else{
  let lost=false;
  const server=createServer(async(req,res)=>{
    if(req.method==='GET'){
      if(mode==='http-sse'){res.writeHead(405).end();return;}
      res.writeHead(200,{'Content-Type':'text/event-stream'});res.flushHeaders();
      emit=r=>res.write(`data: ${JSON.stringify(r)}\n\n`);begin();return;
    }
    if(req.method==='DELETE'){res.writeHead(200).end();return;}
    let body='';for await(const chunk of req)body+=chunk;
    const r=JSON.parse(body);
    if(!r.method){
      // Simulate lost HTTP acceptance exactly once, without altering cached bytes.
      if(scenario==='lost-ack' && r.id==='create-work' && !lost){lost=true;taskId=r.result?.task?.taskId;req.socket.destroy();return;}
      response(r);res.writeHead(202).end();return;
    }
    const send=value=>{
      if(mode==='http-sse'){if(!res.headersSent)res.writeHead(200,{'Content-Type':'text/event-stream'});res.end(`data: ${JSON.stringify(value)}\n\n`);}
      else {res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify(value));}
    };
    if(mode==='http-sse' && r.method==='tools/call'){
      res.writeHead(200,{'Content-Type':'text/event-stream'});res.flushHeaders();emit=value=>res.write(`data: ${JSON.stringify(value)}\n\n`);
    }
    await receive(r,send,()=>req.socket.destroy());
    if(r.id===undefined && !res.headersSent)res.writeHead(202).end();
  });
  server.listen(0,'127.0.0.1',()=>process.stdout.write(`${server.address().port}\n`));
}
