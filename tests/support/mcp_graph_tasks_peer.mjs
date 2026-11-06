// Required graph tool task, with a related callback after its create RPC ended.
import {createServer} from 'node:http';
const mode=process.argv[1]??'json';
const related='io.modelcontextprotocol/related-task';
const sendError=message=>{throw new Error(message);};
const task={taskId:'graph-task',status:'working',createdAt:new Date().toISOString(),lastUpdatedAt:new Date().toISOString(),ttl:60000,pollInterval:1};
let creates=0,emit,resultReply,waiting=false;
function sample(){
  if(!waiting || !emit)return;
  waiting=false;
  emit({jsonrpc:'2.0',id:'sample',method:'sampling/createMessage',params:{messages:[],maxTokens:10,_meta:{[related]:{taskId:task.taskId}}}});
}
const server=createServer(async(req,res)=>{
  if(req.method==='GET'){
    if(mode==='sse'){res.writeHead(405).end();return;}
    res.writeHead(200,{'Content-Type':'text/event-stream'});res.flushHeaders();
    emit=v=>res.write(`data: ${JSON.stringify(v)}\n\n`);sample();return;
  }
  if(req.method==='DELETE'){res.writeHead(200).end();return;}
  let body='';for await(const chunk of req)body+=chunk;
  const r=JSON.parse(body);
  const send=result=>{
    const value={jsonrpc:'2.0',id:r.id,result};
    if(mode==='sse'){
      if(!res.headersSent)res.writeHead(200,{'Content-Type':'text/event-stream','Mcp-Session-Id':'graph-tasks'});
      res.end(`data: ${JSON.stringify(value)}\n\n`);
    }else{res.writeHead(200,{'Content-Type':'application/json','Mcp-Session-Id':'graph-tasks'});res.end(JSON.stringify(value));}
  };
  if(r.method==='initialize')send({protocolVersion:'2025-11-25',serverInfo:{name:'peer',version:'1'},capabilities:{tools:{},tasks:{requests:{tools:{call:{}}},cancel:{}}}});
  else if(r.method==='notifications/initialized' || r.method==='notifications/cancelled')res.writeHead(202).end();
  else if(r.method==='tools/list')send({tools:[{name:'tool',inputSchema:{type:'object'},execution:{taskSupport:'required'}}]});
   else if(r.method==='tools/call' && r.params.name==='host-model')send({sample:{role:'assistant',model:'fixture',content:{type:'text',text:'Answer'}}});
   else if(r.method==='tools/call'){
    if(!r.params.task || ++creates!==1 || JSON.stringify(r.params.arguments)!=='{}')sendError('frozen task creation must happen once');
    send({task});
  }else if(r.method==='tasks/get')send({...task,status:'input_required'});
  else if(r.method==='tasks/result'){
    resultReply=send;waiting=true;
    if(mode==='sse'){
      res.writeHead(200,{'Content-Type':'text/event-stream'});res.flushHeaders();
      emit=v=>res.write(`data: ${JSON.stringify(v)}\n\n`);
    }
    sample();
  }else if(r.id==='sample'){
    if((r.result??r.error)?._meta?.[related]?.taskId!==task.taskId)sendError('callback response association');
    resultReply({content:[],structuredContent:{sampled:!r.error},_meta:{[related]:{taskId:task.taskId}}});
    res.writeHead(202).end();
  }else sendError(`unexpected graph task message ${JSON.stringify(r)}`);
});
server.listen(0,'127.0.0.1',()=>process.stdout.write(`${server.address().port}\n`));
