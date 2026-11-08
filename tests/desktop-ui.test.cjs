const {test}=require('node:test');const assert=require('node:assert/strict');const fs=require('node:fs');const vm=require('node:vm');
function boot(){const html=fs.readFileSync(require.resolve('../ui/index.html'),'utf8');const elements=new Map();class Element{constructor(id){this.id=id;this.style={setProperty(){}};this.dataset={};this.value='';this.innerHTML='';this.textContent='';this.hidden=false;this.disabled=false;this.scrollHeight=60;this.scrollTop=0;this.clientHeight=60;this.classList={toggle(){},add(){},remove(){},contains(){return true;}};}addEventListener(name,handler){(this.listeners??={})[name]=handler;}setAttribute(name,value){this[name]=value;}querySelector(){return new Element('child');}querySelectorAll(){return [];}replaceChildren(){this.innerHTML='';this.children=[];}append(child){(this.children??=[]).push(child);}focus(){}scrollIntoView(){}showModal(){this.open=true;}close(){this.open=false;}}
for(const match of html.matchAll(/\bid="([^"]+)"/g))elements.set(match[1],new Element(match[1]));const commands=[];const document={body:new Element('body'),documentElement:new Element('root'),getElementById(id){assert.ok(elements.has(id),'Missing actual HTML element '+id);return elements.get(id);},querySelectorAll(){return [];},querySelector(){return null;},addEventListener(){},createElement(){return new Element('created');}};const context=vm.createContext({document,window:{ipc:{postMessage(raw){commands.push(JSON.parse(raw));}}},CSS:{escape:x=>x},localStorage:{getItem(){return null;},setItem(){}},getComputedStyle(){return {getPropertyValue(){return '248px';}}},requestAnimationFrame(fn){fn();},setTimeout(){},clearTimeout(){},console});for(const file of ['app.js','workbench-core.js','agents.js'])vm.runInContext(fs.readFileSync(require.resolve('../ui/'+file),'utf8'),context,{filename:file});return{context,elements,commands};}
const run={id:'run-test',goal:'Fix <script>alert(1)</script>',workspace:'C:/public',configured:3,active:0,waiting_capacity:0,state:'failed',agents:[{id:'agent-test',task:{id:'analyze',title:'Analysis',dependencies:[]},state:'failed',progress:'',error:'Invalid model reply: done=true requires actions=[]',steps:2,attempt:1,output:null}],summary:'No changes',changes:[]};
test('real embedded UI boots against its actual HTML and lazily requests browser connection',()=>{const {commands}=boot();assert.deepEqual(commands.map(c=>c.action),['init_history','agent_init']);});
test('archived failed run shows diagnostics and does not offer source application',()=>{const {context,elements}=boot();context.window.agentReceive({type:'archive',data:run,live:false});assert.equal(elements.get('apply-agent-changes').disabled,true);assert.equal(elements.get('retry-stages').hidden,true);assert.match(elements.get('agent-cards').innerHTML,/Противоречивые команды не выполнялись/);assert.equal(elements.get('task-request').textContent,run.goal);});
test('model output and terminal output remain escaped data in the real renderer',()=>{const {context,elements}=boot();context.window.agentReceive({type:'run',data:run});vm.runInContext("selectedAgent='agent-test';inspectorTab='logs'",context);context.window.agentReceive({type:'activity',data:{agent_id:'agent-test',role:'summary',content:'<img src=x onerror=alert(1)>'}});assert.ok(!elements.get('activity-log').innerHTML.includes('<img'));assert.match(elements.get('activity-log').innerHTML,/&lt;img/);});
test('real file viewer renders line changes with syntax highlighting and escapes code',()=>{const {context,elements}=boot();const changed={...run,state:'succeeded',agents:[],changes:[{path:'main.rs',before:'fn main() {}',after:'fn main() { println!("<script>"); }',binary:false}]};context.window.agentReceive({type:'archive',data:changed,live:false});const diff=elements.get('diff-view').innerHTML;assert.match(diff,/diff-line remove/);assert.match(diff,/diff-line add/);assert.match(diff,/syn-keyword/);assert.match(diff,/&lt;script&gt;/);assert.ok(!diff.includes('<script>'));assert.equal(elements.get('apply-agent-changes').disabled,false);});

test('mode tabs preserve drafts and access changes request host-side policy',()=>{const {context,elements,commands}=boot();elements.get('prompt').value='chat draft';elements.get('coding-goal').value='work draft';elements.get('mode-chat').onclick();assert.equal(elements.get('mode-chat')['aria-selected'],'true');elements.get('mode-work').onclick();assert.equal(elements.get('mode-work')['aria-selected'],'true');assert.equal(elements.get('prompt').value,'chat draft');assert.equal(elements.get('coding-goal').value,'work draft');elements.get('access-mode').value='full_access';elements.get('access-mode').onchange();assert.deepEqual(commands.at(-1),{action:'set_access',text:'full_access'});});
test('token estimate includes current draft and actual recorded task character counts',()=>{const {context,elements}=boot();const measured={...run,agents:run.agents.map(a=>({...a,input_chars:200,output_chars:100}))};context.window.agentReceive({type:'run',data:measured});assert.match(elements.get('work-token-count').textContent,/75/);context.window.agentReceive({type:'access',data:'ask_for_approval'});assert.match(elements.get('access-hint').textContent,/подтверждением/);});

test('chat attachment button opens picker and selected files are visible in chat',()=>{const {elements,commands,context}=boot();elements.get('chat-attach').onclick();assert.equal(commands.at(-1).action,'choose_attachments');context.window.agentReceive({type:'attachments',data:[{id:'photo',name:'photo <1>.png',size:100}]});assert.match(elements.get('chat-attachments').innerHTML,/photo &lt;1&gt;.png/);context.window.agentReceive({type:'attachments',data:[]});assert.equal(elements.get('chat-attachments').innerHTML,'');});

test('CAPTCHA notice persists until completion and then releases the notice',()=>{const {elements,context}=boot();context.window.challengeNotice('Пройдите CAPTCHA',true);assert.equal(elements.get('notice').hidden,false);assert.equal(context.window.captchaActive,true);context.window.challengeNotice('Проверка завершена',false);assert.equal(context.window.captchaActive,false);assert.equal(elements.get('notice').textContent,'Проверка завершена');});

test('Ctrl+V attaches clipboard images in both composers and preserves ordinary text paste',()=>{
 const {context,elements,commands}=boot();
 context.FileReader=class { readAsDataURL(file){this.result='data:'+file.type+';base64,aW1hZ2U=';this.onload();} };
 for(const id of ['prompt','coding-goal']){
  let prevented=false;
  elements.get(id).listeners.paste({clipboardData:{items:[{kind:'file',getAsFile:()=>({type:'image/png',size:5})}],getData:()=>''},preventDefault(){prevented=true;}});
  assert.equal(prevented,true);assert.deepEqual(commands.at(-1),{action:'attach_clipboard',id:'image/png',text:'aW1hZ2U='});
 }
 const count=commands.length;
 elements.get('prompt').listeners.paste({clipboardData:{items:[{kind:'string'}],getData:()=>'plain text'},preventDefault(){assert.fail('text paste should remain native');}});
 assert.equal(commands.length,count);
});
test('clipboard rejects unsupported and oversized media and keeps accompanying text',()=>{
 const {context,elements,commands}=boot();
 context.FileReader=class {readAsDataURL(){this.result='data:image/png;base64,eA==';this.onload();}};
 const paste=(file,text='')=>elements.get('prompt').listeners.paste({clipboardData:{items:[{kind:'file',getAsFile:()=>file}],getData:()=>text},preventDefault(){if(text)assert.fail('mixed clipboard text should be preserved');}});
 const count=commands.length;
 paste({type:'video/mp4',size:1});paste({type:'image/png',size:21*1024*1024});
 assert.equal(commands.length,count);
 paste({type:'image/png',size:1},'caption');assert.equal(commands.at(-1).action,'attach_clipboard');
});

test('panel motion defers hiding and reopening cancels a pending close',()=>{
 const {context,elements}=boot();const panel=elements.get('inspector');const animations=[];
 panel.hidden=true;panel.animate=(frames,options)=>{const animation={frames,options,cancel(){this.cancelled=true;}};animations.push(animation);return animation;};
 vm.runInContext('openInspector()',context);assert.equal(panel.hidden,false);assert.equal(animations[0].options.duration,220);
 vm.runInContext('closeInspector()',context);assert.equal(panel.hidden,false);assert.equal(animations[0].cancelled,true);
 const closing=animations[1];vm.runInContext('openInspector()',context);assert.equal(closing.cancelled,true);
 closing.onfinish();assert.equal(panel.hidden,false);
 vm.runInContext('closeInspector()',context);animations.at(-1).onfinish();assert.equal(panel.hidden,true);
});
test('reduced motion applies panel visibility immediately without animations',()=>{
 const {context,elements}=boot();context.matchMedia=()=>({matches:true});const panel=elements.get('inspector');panel.animate=()=>assert.fail('reduced motion must skip animation');
 vm.runInContext('openInspector();closeInspector()',context);assert.equal(panel.hidden,true);
});
test('new messages animate once and streamed text updates do not replay entrance',()=>{
 const {context,elements}=boot();const incoming={chats:[{id:'chat',title:'Test',messages:[]}],active_id:'chat',phase:'ready',busy:false,settings:{blur:true,font_size:15}};
 context.window.receive(incoming);
 const withReply={...incoming,busy:true,chats:[{...incoming.chats[0],messages:[{role:'assistant',text:'Hello',html:'<p>Hello</p>'}]}]};
 context.window.receive(withReply);assert.match(elements.get('messages').innerHTML,/message-enter/);
 const update={...withReply,chats:[{...withReply.chats[0],messages:[{role:'assistant',text:'Hello world',html:'<p>Hello world</p>'}]}]};
 context.window.receive(update);assert.ok(!elements.get('messages').innerHTML.includes('message-enter'));assert.match(elements.get('messages').innerHTML,/Hello world/);
});

test('attachment previews render inside both composers with removable image thumbnails',()=>{
 const {context,elements,commands}=boot();const html=fs.readFileSync('ui/index.html','utf8');
 assert.match(html,/<form id="composer"[^>]*><div id="chat-attachments"/);
 assert.match(html,/<div class="run-input"><div id="attachment-list"/);
 context.window.agentReceive({type:'attachments',data:[{id:'photo',name:'photo.png',size:50,image:'data:image/png;base64,eA=='}]});
 for(const id of ['chat-attachments','attachment-list']){assert.match(elements.get(id).innerHTML,/<img src="data:image\/png;base64,eA=="/);assert.match(elements.get(id).innerHTML,/type="button"/);}
 elements.get('chat-attachments').onclick({target:{closest(selector){return selector==='[data-remove]'?{dataset:{remove:'photo'}}:null;}}});assert.equal(commands.at(-1).action,'remove_attachment');
});
test('attachment preview cannot inject remote URLs or markup',()=>{
 const {context,elements}=boot();context.window.agentReceive({type:'attachments',data:[{id:'x',name:'<script>',size:1,image:'https://example.org/tracker'}]});
 assert.ok(!elements.get('chat-attachments').innerHTML.includes('<img'));assert.match(elements.get('chat-attachments').innerHTML,/&lt;script&gt;/);
});

test('sent image stays with its message after draft attachments clear and opens in preview',()=>{
 const {context,elements}=boot();
 const file={id:'sent-photo',name:'pet.png',mime:'image/png',image:'data:image/png;base64,eA=='};
 const incoming={chats:[{id:'chat',title:'Photo',messages:[{role:'user',text:'Зацени',attachments:[file]}]}],active_id:'chat',phase:'ready',busy:false,settings:{blur:true,font_size:15}};
 context.window.receive(incoming);context.window.agentReceive({type:'attachments',data:[]});
 assert.match(elements.get('messages').innerHTML,/<img src="data:image\/png;base64,eA=="/);assert.match(elements.get('messages').innerHTML,/Зацени/);assert.equal(elements.get('chat-attachments').innerHTML,'');
 elements.get('messages').onclick({target:{closest(selector){return selector==='[data-message-attachment]'?{dataset:{messageAttachment:'0:0'}}:null;}}});
 assert.equal(elements.get('attachment-dialog').open,true);assert.equal(elements.get('attachment-title').textContent,'pet.png');assert.equal(elements.get('attachment-preview').children[0].src,file.image);
 // Reload the renderer's state, as when reopening saved history.
 context.window.receive(JSON.parse(JSON.stringify(incoming)));assert.match(elements.get('messages').innerHTML,/pet.png/);
});
test('old text-only messages and unsafe or missing image previews render safely',()=>{
 const {context,elements}=boot();context.window.receive({chats:[{id:'chat',title:'Old',messages:[{role:'user',text:'Old message'},{role:'user',text:'New',attachments:[{name:'<script>',image:'javascript:alert(1)'}]}]}],active_id:'chat',phase:'ready',busy:false,settings:{blur:true,font_size:15}});
 const markup=elements.get('messages').innerHTML;assert.match(markup,/Old message/);assert.match(markup,/&lt;script&gt;/);assert.ok(!markup.includes('<img'));assert.ok(!markup.includes('javascript:'));
});

test('apply flow opens an explicit confirmation, sends one decision and shows success',()=>{
 const {context,elements,commands}=boot();const completed={...run,state:'succeeded',agents:[],changes:[{path:'main.py',before:null,after:'print("Hello")',binary:false}]};
 context.window.agentReceive({type:'archive',data:completed,live:false});assert.equal(elements.get('apply-agent-changes').disabled,false);
 elements.get('apply-agent-changes').onclick();elements.get('apply-agent-changes').onclick();assert.equal(commands.filter(c=>c.action==='agent_apply').length,1);
 const request={id:'approval-apply',agent_id:'integration',description:'Применить файл?',cwd:'C:/project',action:{tool:'apply_changes',run_id:completed.id,changes:completed.changes}};
 context.window.agentReceive({type:'approval',data:request});assert.equal(elements.get('apply-dialog').open,true);assert.match(elements.get('apply-diff').textContent,/main.py/);assert.equal(elements.get('apply-workspace').textContent,'C:/project');assert.ok(!elements.get('approval-list').innerHTML.includes('approval-apply'));
 elements.get('apply-confirm').onclick();elements.get('apply-confirm').onclick();assert.equal(commands.filter(c=>c.action==='approval').length,1);assert.deepEqual(commands.at(-1),{action:'approval',id:'approval-apply',allowed:true});
 context.window.agentReceive({type:'apply_status',data:{run_id:completed.id,state:'applied',message:'Изменения применены к проекту'}});
 assert.equal(elements.get('apply-agent-changes').disabled,true);assert.equal(elements.get('apply-agent-changes').textContent,'Изменения применены');
});
test('canceling or failing application leaves changes available for another attempt',()=>{
 const {context,elements,commands}=boot();const completed={...run,state:'succeeded',changes:[{path:'x',before:null,after:'x'}]};context.window.agentReceive({type:'archive',data:completed,live:false});elements.get('apply-agent-changes').onclick();
 context.window.agentReceive({type:'approval',data:{id:'cancel-me',agent_id:'integration',description:'Apply',cwd:'C:/project',action:{tool:'apply_changes',run_id:completed.id,changes:completed.changes}}});
 elements.get('apply-dialog').listeners.cancel({preventDefault(){}});assert.deepEqual(commands.at(-1),{action:'approval',id:'cancel-me',allowed:false});
 context.window.agentReceive({type:'apply_status',data:{run_id:completed.id,state:'failed',message:'Отменено'}});assert.equal(elements.get('apply-agent-changes').disabled,false);
 context.window.agentReceive({type:'archive',data:{...completed,applied:true},live:false});assert.equal(elements.get('apply-agent-changes').disabled,true);
});
