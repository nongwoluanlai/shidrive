import assert from 'node:assert/strict';
import { Window } from 'happy-dom';
const window = new Window({ url: 'https://regression.invalid' });
for (const k of ['window','document','navigator','Node','NodeFilter','Element','HTMLElement','HTMLDivElement','HTMLTextAreaElement','HTMLInputElement','HTMLSelectElement','HTMLMediaElement','SVGElement','DocumentFragment','Text','Comment','Event','MouseEvent','KeyboardEvent','MutationObserver','ResizeObserver','FileReader','getComputedStyle','localStorage','requestAnimationFrame','cancelAnimationFrame']) {
  if (k === 'window') globalThis.window = window;
  else if (window[k] !== undefined) Object.defineProperty(globalThis, k, { value: typeof window[k] === 'function' && ['getComputedStyle','requestAnimationFrame','cancelAnimationFrame'].includes(k) ? window[k].bind(window) : window[k], configurable: true, writable: true });
}
const calls = [], handlers = {}, store = new Map();
globalThis.__listeners = new Map();
globalThis.__api = (method, args) => {
  calls.push({method, args: JSON.parse(JSON.stringify(args))});
  if (handlers[method]) return handlers[method](...args);
  if (method === 'chatStoreGet') return Promise.resolve(store.get(args[0]) ?? '');
  if (method === 'chatStoreSet') { store.set(args[0], args[1]); return Promise.resolve(); }
  if (method === 'chatStoreDelete') { store.delete(args[0]); return Promise.resolve(); }
  if (method === 'bindingGet') return Promise.resolve({session_id:'old-' + args[0], title:'Title-' + args[0]});
  if (method === 'acpSessionBind' || method === 'bindingsAll') return Promise.resolve([]);
  return Promise.resolve(null);
};
globalThis.__confirm = () => Promise.resolve(true);
const {mount, unmount, flushSync, tick} = await import('svelte');
const state = await import('./compiled/state.svelte.js');
const {app, loadChatLocal, pushLocal, setChatRows, applySessionUpdate, adoptBinding, finishTurn, clearChat, clearDraftImages} = state;
const loadComponent = async name => (await import(`./compiled/components/${name}.svelte.js`)).default;
const ChatView = await loadComponent('ChatView');
const {wireEvents} = await import('./compiled/events.js');
await wireEvents();
const emit = (name, payload) => { assert.ok(globalThis.__listeners.has(name), name); globalThis.__listeners.get(name)({payload}); };
const wait = ms => new Promise(r => setTimeout(r, ms));
const settle = async () => { await tick(); await wait(8); flushSync(); };
const deferred = () => { let resolve, reject; const promise = new Promise((a,b)=>{resolve=a;reject=b;}); return {promise,resolve,reject}; };
const root = document.createElement('div'); document.body.append(root);
app.contexts = [{id:'A',project_id:'P',name:'A'}, {id:'B',project_id:'P',name:'B'}];
app.contextId='A'; app.agent='codex'; app.agents=[{id:'codex',name:'Codex'},{id:'deepseek',name:'DeepSeek'}];
const which = process.argv[2];
const oldRows = text => [{id:'i1',kind:'user',text}, {id:'i1',kind:'assistant',text:'answer'}];
const snapshot = (sessionId, items) => JSON.stringify({version:2,sessionId,items});
const seed = () => { for (const id of ['A','B']) { adoptBinding(`${id}:${app.agent}`, 'old-'+id); app.chat[`${id}:${app.agent}`]=[{id:'old-'+id,kind:'user',text:id+' history'}]; } };
let component;
try {
  if (which === 'snapshot-ids') {
    adoptBinding('A:codex','old-A'); store.set('A:codex',JSON.stringify(oldRows('old user')));
    app.chat['A:codex'] = await loadChatLocal('A:codex');
    pushLocal('A:codex',{kind:'user',text:'new user after restart'});
    const ids=app.chat['A:codex'].map(x=>x.id);
    assert.equal(new Set(ids).size,3); assert.equal(ids.length,3);
    component=mount(ChatView,{target:root}); await settle();
    assert.match(root.textContent,/new user after restart/);
    await wait(450); assert.equal(JSON.parse(store.get('A:codex')).version,2);
  } else if (which === 'draft-effect') {
    seed(); component=mount(ChatView,{target:root}); await settle(); calls.length=0;
    component.audit().setStick(false);
    const ta=root.querySelector('.input-row textarea');
    for(const text of ['a','ab','abc']) { ta.value=text; ta.dispatchEvent(new Event('input',{bubbles:true})); await settle(); }
    assert.equal(calls.filter(c=>['chatStoreGet','bindingGet','acpSessionBind'].includes(c.method)).length,0);
    assert.equal(component.audit().getState().stickToBottom,false);
    assert.equal(app.drafts['A:codex'],'abc');
  } else if (which === 'new-session-race' || which === 'resume-session-race') {
    app.agent='deepseek'; seed();
    const request=deferred(); handlers.acpSessionNew=()=>request.promise;
    component=mount(ChatView,{target:root}); await settle();
    const fresh=which==='new-session-race';
    const p=component.audit()[fresh ? 'newSession' : 'resumeSession'](); await settle();
    assert.equal(calls.find(c=>c.method==='acpSessionNew').args[0].id,'A');
    if(fresh) assert.equal(calls.find(c=>c.method==='acpSessionNew').args[2],true);
    app.contextId='B'; await settle(); request.resolve('new-session-for-A'); await p; await settle();
    assert.equal(app.chat['B:deepseek'][0].text,'B history');
    assert.equal(app.bindingSession['B:deepseek'],'old-B');
    assert.equal(app.bindingSession['A:deepseek'],'new-session-for-A');
  } else if (which === 'failed-new-keeps-history') {
    seed(); handlers.acpSessionNew=()=>Promise.reject('temporary failure');
    component=mount(ChatView,{target:root}); await settle(); await component.audit().newSession();
    assert.equal(app.chat['A:codex'][0].text,'A history'); assert.equal(app.bindingSession['A:codex'],'old-A');
    assert.equal(calls.filter(c=>c.method==='bindingUnbind').length,0); assert.equal(app.sessionBusy['A:codex'],false);
  } else if (which === 'empty-rebind') {
    adoptBinding('A:codex','old-A'); store.set('A:codex',snapshot('old-A',oldRows('OLD SESSION')));
    pushLocal('A:codex',{kind:'user',text:'queued write'}); setChatRows('A:codex',[]);
    await wait(450); assert.equal(await loadChatLocal('A:codex'),null); assert.equal(store.has('A:codex'),false);
  } else if (which === 'snapshot-sid-and-write-order') {
    adoptBinding('A:codex','new'); store.set('A:codex',snapshot('old',oldRows('stale')));
    assert.equal(await loadChatLocal('A:codex'),null);
    const slow=deferred(); handlers.chatStoreSet=async (key,json)=>{ await slow.promise; store.set(key,json); };
    pushLocal('A:codex',{kind:'user',text:'slow write'}); await wait(450);
    clearChat('A:codex'); slow.resolve(); await settle(); assert.equal(store.has('A:codex'),false);
  } else if (which === 'loading-owner-and-stale-read') {
    seed(); const reads=[deferred(),deferred()]; let count=0;
    handlers.bindingGet=id=>id==='A' ? reads[count++].promise : Promise.resolve({session_id:'old-B'});
    component=mount(ChatView,{target:root}); await settle();
    app.contextId='B'; await settle(); app.contextId='A'; await settle();
    reads[0].resolve({session_id:'stale-A'}); await settle(); assert.equal(app.chatLoading['A:codex'],true);
    reads[1].resolve({session_id:'old-A'}); await settle(); assert.equal(app.chatLoading['A:codex'],false);
    assert.equal(app.bindingSession['A:codex'],'old-A'); assert.equal(app.chat['A:codex'][0].text,'A history');
  } else if (which === 'permission-double-submit' || which === 'elicitation-double-submit') {
    const perm=which.startsWith('permission'); const queue=perm?'permissions':'elicitations';
    const method=perm?'acpRespondPermission':'acpRespondElicitation'; const name=perm?'PermissionDialog':'ElicitationDialog';
    app[queue]=['r1','r2'].map(requestId=>({requestId,agentType:'codex',params:{options:[],message:'Question'}}));
    let response=deferred(); handlers[method]=()=>response.promise;
    component=mount(await loadComponent(name),{target:root}); await settle();
    const p=component.audit().respond(perm?'allow':'accept'); await component.audit().respond(perm?'allow':'accept');
    assert.equal(calls.filter(c=>c.method===method).length,1);
    response.resolve(); await p; await settle(); assert.deepEqual(app[queue].map(r=>r.requestId),['r2']);
    handlers[method]=()=>Promise.reject('temporary IPC failure'); await component.audit().respond(perm?'allow':'accept');
    assert.deepEqual(app[queue].map(r=>r.requestId),['r2']);
    response=deferred(); handlers[method]=()=>response.promise; const stale=component.audit().respond(perm?'allow':'accept');
    emit('acp://requests-cancelled',{requestIds:['r2']}); app[queue].push({requestId:'r3',agentType:'codex',params:{options:[]}});
    response.resolve(); await stale; await settle(); assert.deepEqual(app[queue].map(r=>r.requestId),['r3']);
    handlers[method]=()=>Promise.reject('请求已失效'); await component.audit().respond(perm?'allow':'accept');
    assert.equal(app[queue].length,0);
  } else if (which === 'session-event-routing') {
    seed(); const k='A:codex';
    const route={agentType:'codex',contextId:'A',sessionId:'old-A',turnId:'t1',connectionId:'c1',source:'chat'};
    app.promptRequests[k]='t1'; emit('acp://binding-status',{...route,status:'running'});
    const chunk=(r,text)=>emit('acp://update',{...r,update:{sessionUpdate:'agent_message_chunk',content:{type:'text',text}}});
    chunk({...route,sessionId:'workflow-temp'},'WRONG SID'); chunk({...route,source:'workflow'},'WORKFLOW');
    chunk({...route,turnId:'old'},'OLD TURN'); chunk({...route,connectionId:'old'},'OLD CONNECTION');
    chunk(route,'accepted'); assert.equal(app.chat[k].at(-1).text,'accepted'); assert.equal(app.chat[k].length,2);
    emit('acp://binding-status',{...route,turnId:'old',status:'completed'}); assert.equal(app.streaming[k],true);
    emit('acp://binding-status',{...route,status:'completed'}); emit('acp://binding-status',{...route,status:'completed'});
    assert.equal(calls.filter(c=>c.method==='systemAfterAction').length,1);
    app.promptRequests[k]='t2'; app.streaming[k]=true; finishTurn('codex','A','t1'); assert.equal(app.streaming[k],true);
  } else if (which === 'pending-config-is-per-context') {
    handlers.bindingGet=()=>Promise.resolve(null);
    component=mount(ChatView,{target:root}); await settle();
    component.audit().setCfg('model','m2'); component.audit().setCfg('sandbox','chosen');
    assert.deepEqual(app.pendingCfg['A:codex'],{model:'m2',sandbox:'chosen'});
    assert.equal(app.pendingCfg['B:codex'],undefined);
  } else if (which === 'invoke-result-before-final-events') {
    seed(); const result=deferred(); handlers.acpPrompt=()=>result.promise;
    component=mount(ChatView,{target:root}); await settle();
    component.audit().addImage({data:'AA',mime:'image/png',preview:'data:image/png;base64,AA'}); await settle();
    const p=component.audit().send(); await settle();
    const route={contextId:'A',agentType:'codex',sessionId:'old-A',turnId:app.promptRequests['A:codex'],connectionId:'conn',source:'chat'};
    emit('acp://binding-status',{...route,status:'running'}); result.resolve({stopReason:'end_turn'}); await p;
    emit('acp://update',{...route,update:{sessionUpdate:'agent_message_chunk',content:{type:'text',text:'final chunk'}}});
    emit('acp://binding-status',{...route,status:'completed'});
    assert.equal(app.chat['A:codex'].at(-1).text,'final chunk'); assert.equal(app.streaming['A:codex'],false);
  } else if (which === 'stale-prompt-finally') {
    seed(); const result=deferred(); handlers.acpPrompt=()=>result.promise;
    component=mount(ChatView,{target:root}); await settle();
    component.audit().addImage({data:'AA',mime:'image/png',preview:'data:image/png;base64,AA'});
    const p=component.audit().send(); await settle(); const old=app.promptRequests['A:codex'];
    assert.ok(old); app.promptRequests['A:codex']='newer'; result.resolve({stopReason:'end_turn'}); await p;
    assert.equal(app.streaming['A:codex'],true); assert.equal(app.promptRequests['A:codex'],'newer');
  } else if (which === 'ready-caps-and-first-message') {
    adoptBinding('A:codex',null); app.promptRequests['A:codex']='r';
    app.pendingMessage['A:codex']=pushLocal('A:codex',{kind:'user',text:'first prompt'});
    app.sessionInfo['A:codex']={sessionId:'other',response:{models:{currentModelId:'wrong'},modes:{currentModeId:'wrong'}}};
    app.agentCaps.codex={models:{currentModelId:'from-B',availableModels:[{modelId:'m1',name:'One'}]},configOptions:[{id:'model',currentValue:'from-B'}]};
    app.pendingCfg['A:codex']={model:'A-choice',sandbox:'A-mode'}; app.pendingCfg['B:codex']={model:'B-choice'};
    emit('acp://session-ready',{contextId:'A',agentType:'codex',connectionId:'c1',sessionId:'new-A',response:{}});
    assert.equal(app.chat['A:codex'][0].text,'first prompt');
    assert.equal(app.sessionInfo['A:codex'].response.models.currentModelId,'');
    assert.equal(app.sessionInfo['A:codex'].response.modes,undefined);
    assert.deepEqual(calls.filter(c=>c.method==='acpSetConfigOption').map(c=>c.args[3]),['A-choice','A-mode']);
    assert.deepEqual(app.pendingCfg['B:codex'],{model:'B-choice'});
    emit('acp://update',{contextId:'A',agentType:'codex',sessionId:'new-A',connectionId:'old',source:'chat',update:{sessionUpdate:'config_option_update',configOptions:[{id:'bad'}]}});
    assert.notEqual(app.sessionInfo['A:codex'].response.configOptions[0].id,'bad');
  } else if (which === 'content-only-tool') {
    adoptBinding('A:codex','s1');
    const content=[{type:'content',content:{type:'text',text:'ONLY TOOL RESULT'}},{type:'diff',path:'a.ts',oldText:'old',newText:'new'}];
    applySessionUpdate('codex','A','s1',{sessionUpdate:'tool_call',toolCallId:'t1',title:'Read',status:'completed',content});
    const tool=app.chat['A:codex'][0].tools[0]; assert.deepEqual(tool.content,content);
    component=mount(await loadComponent('MessageItem'),{target:root,props:{item:app.chat['A:codex'][0]}}); await settle();
    const detail=component.audit().toolDetail(tool); assert.match(detail,/ONLY TOOL RESULT/); assert.match(detail,/a.ts/);
    root.querySelector('.tools-group').click(); await settle();
    root.querySelector('.thead').click(); await settle();
    assert.match(root.textContent,/ONLY TOOL RESULT/);
    applySessionUpdate('codex','A','s1',{sessionUpdate:'tool_call_update',toolCallId:'t1',content:[{type:'content',content:{type:'text',text:'x'.repeat(200000)}}]});
    assert.ok(JSON.stringify(app.chat['A:codex']).length<25000);
  } else if (which === 'image-cross-context' || which === 'delayed-image-reader') {
    seed(); component=mount(ChatView,{target:root}); await settle();
    if(which==='image-cross-context') {
      component.audit().addImage({data:'YXNk',mime:'image/png',preview:'data:image/png;base64,YXNk'});
      app.contextId='B'; await settle(); await component.audit().send();
      assert.equal(calls.filter(c=>c.method==='acpPrompt').length,0);
      app.contextId='A'; await settle(); await component.audit().send();
      assert.equal(calls.find(c=>c.method==='acpPrompt').args[0].id,'A'); assert.equal(app.draftImages['A:codex'].length,0);
    } else {
      const readers=[]; globalThis.FileReader=class { constructor(){readers.push(this);} readAsDataURL(){} };
      const paste=()=>component.audit().onPaste({clipboardData:{files:[{type:'image/png'}]},preventDefault(){}});
      paste(); app.contextId='B'; await settle(); readers[0].result='data:image/png;base64,QQ=='; readers[0].onload(); await settle();
      assert.equal(component.audit().getState().pendingImages.length,0); assert.equal(app.draftImages['A:codex'].length,1);
      app.contextId='A'; await settle(); paste(); clearDraftImages('A:codex'); readers[1].result='data:image/png;base64,QQ=='; readers[1].onload();
      assert.equal(app.draftImages['A:codex'].length,0);
    }
  } else if (which === 'unbind-clears-snapshot') {
    seed(); store.set('A:codex',snapshot('old-A',oldRows('stale')));
    component=mount(await loadComponent('SessionManagerModal'),{target:root}); await settle();
    await component.audit().unbind({context_id:'A',agent_type:'codex',context_name:'A'}); await settle();
    assert.equal(app.bindingSession['A:codex'],null); assert.equal(app.chat['A:codex'].length,0); assert.equal(store.has('A:codex'),false);
  } else if (which === 'history-bind-race') {
    seed(); app.historyBind='codex'; const pending=deferred(); handlers.acpSessionBind=()=>pending.promise;
    component=mount(await loadComponent('HistoryBindModal'),{target:root}); await settle();
    const p=component.audit().bind({session_id:'historical',title:'History'}); await settle(); app.contextId='B'; await settle();
    pending.resolve([{role:'assistant',content:'A imported'}]); await p; await settle();
    assert.equal(app.chat['A:codex'][0].text,'A imported'); assert.equal(app.chat['B:codex'][0].text,'B history');
    assert.equal(app.bindingSession['B:codex'],'old-B');
  } else if (which === 'default-full-access-unchanged') {
    seed(); app.sessionInfo['A:codex']={agentType:'codex',contextId:'A',sessionId:'old-A',response:{configOptions:[
      {id:'sandbox',name:'Sandbox',type:'select',currentValue:'read-only',options:[{value:'read-only',name:'Read only'},{value:'danger-full-access',name:'Full'}]},
      {id:'approval',name:'Approval',type:'select',currentValue:'on-request',options:[{value:'on-request',name:'Ask'},{value:'never',name:'Never'}]}
    ]}};
    component=mount(ChatView,{target:root}); await settle();
    assert.deepEqual(calls.filter(c=>c.method==='acpSetConfigOption').map(c=>c.args.slice(2,4)),[['sandbox','danger-full-access'],['approval','never']]);
    assert.equal(app.cfgPref['codex:sandbox'],'danger-full-access'); assert.equal(app.cfgPref['codex:approval'],'never');
  } else { throw new Error('unknown case '+which); }
  if(component) await unmount(component);
  console.log('PASS',which); process.exit(0);
} catch (e) { console.error('FAIL',which,e); process.exit(1); }
