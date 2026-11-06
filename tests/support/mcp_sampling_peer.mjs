// Independent MCP peer: counts actual native model operations and loses one reply ACK.
import http from 'node:http';
import {createInterface} from 'node:readline';
const mode = process.argv[2];
const sample = {jsonrpc:'2.0', id:'sample', method:'sampling/createMessage', params:{
  messages:[{role:'user',content:[{type:'text',text:'Question'},{type:'image',data:'TQ==',mimeType:'image/png'},{type:'audio',data:'TQ==',mimeType:'audio/wav'}]}],
  maxTokens:10, modelPreferences:{hints:[{name:'advisory'}],costPriority:0.1}, systemPrompt:'host policy is authoritative',
  includeContext:'thisServer', tools:[{name:'server-owned-tool',inputSchema:{type:'object'}}], toolChoice:{mode:'required'},
}};
let outer, modelOperations=0, callbackReplies=0, saved;
let modelResult, rootNotify;
const modelValue=(final=false)=>({sample:{role:'assistant',model:'actual-native-model',content:final?{type:'text',text:'Final answer'}:[{type:'tool_use',id:'next',name:'server-owned-tool',input:{}}],stopReason:final?'endTurn':'toolUse'}});
const state = {stream:null};
function handle(r, result, notify, ack) {
  if(r.method==='initialize') return result({protocolVersion:'2025-11-25',serverInfo:{name:'peer',version:'1'},capabilities:{tools:{}}});
  if(r.method==='notifications/initialized') return ack();
  if(r.method==='notifications/cancelled') return ack();
  if(r.method==='tools/call' && r.params.name==='outer') {outer=result;rootNotify=notify;notify(sample);return;}
  if(r.method==='tools/call' && r.params.name==='host-model') {
    if(++modelOperations>2)process.exit(41);
    const a=r.params.arguments;
    if(a.model!=='actual-native-model' || a.context.text!=='bounded context' || a.context.source!=='host-authorized' || a.sampling.modelPreferences.hints[0].name!=='advisory' || a.sampling.messages[0].content[1].type!=='image' || a.sampling.messages[0].content[2].type!=='audio')process.exit(46);
    if(modelOperations===2) {
      const history=a.sampling.messages;
      if(a.sampling.toolChoice.mode!=='none' || history.at(-1).content[0].toolUseId!=='next')process.exit(47);
      return result(modelValue(true));
    }
    if(mode==='stdio')return result(modelValue());
    if(mode==='ambiguous') {
      notify({jsonrpc:'2.0',id:'ambiguous-child',method:'sampling/createMessage',params:{messages:[],maxTokens:1}});return;
    }
    modelResult=result;
    notify({jsonrpc:'2.0',id:'native-child',method:'sampling/createMessage',params:{messages:[],maxTokens:1}});return;
  }
  if(!r.method && r.id==='native-child') {
    if(r.result?.content?.type!=='audio')process.exit(45);
    ack();modelResult(modelValue());return;
  }
  if(!r.method && r.id==='sample') {
    if(r.error || r.result.model!=='actual-native-model' || r.result.content[0].name!=='server-owned-tool')process.exit(42);
    const value=JSON.stringify(r); if(saved && saved!==value)process.exit(43); saved=value;
    callbackReplies++;
    if(mode!=='stdio' && callbackReplies===1) {ack(true);return;}
    ack();
    rootNotify({...sample,id:'followup',params:{...sample.params,toolChoice:{mode:'none'},messages:[...sample.params.messages,
      {role:'assistant',content:r.result.content}, {role:'user',content:[{type:'tool_result',toolUseId:'next',content:[{type:'text',text:'server performed its tool'}]}]}
    ]}});return;
  }
  if(!r.method && r.id==='followup') {
    if(r.error || r.result.content?.text!=='Final answer')process.exit(48);
    ack();outer({content:[],modelOperations,callbackReplies});return;
  }
  process.exit(44); // Server-supplied tool definitions must never be auto-dispatched.
}
if(mode==='stdio' || mode==='ambiguous') {
  const send=v=>process.stdout.write(JSON.stringify(v)+'\n');
  for await(const line of createInterface({input:process.stdin})) {const r=JSON.parse(line);handle(r, value=>send({jsonrpc:'2.0',id:r.id,result:value}),send,()=>{});}
} else {
  const server=http.createServer(async(req,res)=>{
    if(req.method==='DELETE'){state.stream?.end();res.writeHead(204).end();return;}
    if(req.method==='GET') {res.writeHead(200,{'content-type':'text/event-stream'});res.flushHeaders();state.stream=res;return;}
    let body='';for await(const chunk of req)body+=chunk;
    const r=JSON.parse(body);
    if(r.method==='initialize')res.setHeader('mcp-session-id','sampling');
    let streaming=false;
    const notify=value=>{
      if(!streaming){res.writeHead(200,{'content-type':'text/event-stream'});res.flushHeaders();streaming=true;}
      res.write(`data: ${JSON.stringify(value)}\n\n`);
    };
    const result=value=>{
      if(streaming){notify({jsonrpc:'2.0',id:r.id,result:value});res.end();}
      else res.writeHead(200,{'content-type':'application/json'}).end(JSON.stringify({jsonrpc:'2.0',id:r.id,result:value}));
    };
    handle(r,result,notify,lost=>lost?res.destroy():res.writeHead(202).end());
  });
  server.listen(0,'127.0.0.1',()=>console.log(server.address().port));
}
