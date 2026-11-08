'use strict';
const $ = id => document.getElementById(id);
const icon = name => `<svg aria-hidden="true"><use href="#i-${name}"/></svg>`;
let state = {chats:[],active_id:'',status:'Подключение',phase:'connecting',busy:false,settings:{blur:true,font_size:15}};
let signature = '', listSignature = '', toastTimer, sending = false;
const drafts = new Map();
const sendCommand = (action, extra={}) => window.ipc?.postMessage(JSON.stringify({action,...extra}));
const escape = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
function toast(text) { $('toast').textContent=text; $('toast').classList.add('visible'); clearTimeout(toastTimer); toastTimer=setTimeout(()=>$('toast').classList.remove('visible'),2400); }
function resizeInput(){const el=$('prompt');el.style.height='auto';el.style.height=Math.min(el.scrollHeight,170)+'px';updateSend();}
function updateSend(){const ready=state.phase==='ready';$('send').disabled=state.busy?false:sending||!ready||!$('prompt').value.trim();$('send').classList.toggle('stopping',state.busy);$('send').innerHTML=icon(state.busy?'square':'arrow');$('send').setAttribute('aria-label',state.busy?'Прекратить ожидание':'Отправить сообщение');$('enter-hint').hidden=state.busy;$('composer-hint').textContent=state.busy?'Готовим ответ…':sending?'Отправляем…':'Текстовый диалог';}
function sidebar(){const query=$('search').value.toLocaleLowerCase();const chats=state.chats.filter(c=>c.messages.length&&c.title.toLocaleLowerCase().includes(query));const key=JSON.stringify([chats.map(c=>[c.id,c.title]),state.active_id,query]);if(key===listSignature)return;listSignature=key;$('chat-count').textContent=state.chats.filter(c=>c.messages.length).length;$('chat-list').innerHTML=chats.length?chats.map(chat=>`<button class="chat-item ${chat.id===state.active_id?'active':''}" data-chat="${escape(chat.id)}" title="${escape(chat.title)}">${icon('chat')}<span>${escape(chat.title)}</span></button>`).join(''):`<div class="empty-history">${query?'Ничего не найдено':'Ваши разговоры будут здесь.<br>Начните с первого вопроса.'}</div>`;}
window.receive = incoming => {
 if(incoming.toast){toast(incoming.toast);return;}
 sending=false;$('prompt').readOnly=false;
 const previous=state.active_id;
 if(previous!==incoming.active_id){drafts.set(previous,$('prompt').value);$('prompt').value=drafts.get(incoming.active_id)||'';resizeInput();}
 state=incoming;
 if(incoming.sent){$('prompt').value='';drafts.delete(state.active_id);resizeInput();}
 document.body.classList.toggle('no-glass',!state.settings.blur);
 document.documentElement.style.setProperty('--chat-font',state.settings.font_size+'px');
 $('blur-toggle').setAttribute('aria-checked',String(state.settings.blur));$('font-size').value=state.settings.font_size;
 $('connection').className='connection '+(state.busy?'busy':state.phase);
 $('connection-text').textContent=state.busy?'Отвечает':state.phase==='ready'?'Готов к работе':state.phase==='error'?'Нет подключения':'Подключение';
 $('connection').title=state.status;
 const error=state.error||'';$('notice').hidden=!error;$('notice').textContent=error;
 const chat=state.chats.find(c=>c.id===state.active_id);
 $('chat-title').textContent=chat?.messages.length?chat.title:'Новый диалог';
 const messages=chat?.messages||[];$('welcome').hidden=!!messages.length;
 const key=JSON.stringify([messages,state.busy,state.active_id]);
 if(key!==signature){signature=key;const container=$('conversation');const nearBottom=container.scrollHeight-container.scrollTop-container.clientHeight<100;const last=messages.length-1;
 $('messages').innerHTML=messages.map((message,index)=>message.role==='user'?`<article class="message user"><div class="user-bubble">${escape(message.text)}</div></article>`:`<article class="message assistant ${message.interrupted?'interrupted':''}"><div class="assistant-label">${icon('star')}<span>EVIL GEMINI</span></div>${message.text?`<div class="assistant-content">${message.html}</div>`:state.busy?'<div class="thinking"><span class="thinking-dots"><i></i><i></i><i></i></span>Разбираемся в деталях</div>':'<div class="assistant-content"><p>Ответ не получен.</p></div>'}${message.text?`<div class="message-actions"><button data-copy="${index}" title="Копировать ответ" aria-label="Копировать ответ">${icon('copy')}</button>${index===last&&!state.busy?`<button data-retry title="Повторить запрос" aria-label="Повторить запрос">${icon('retry')}</button>`:''}</div>`:''}</article>`).join('');
 if(previous!==state.active_id||nearBottom||incoming.sent)requestAnimationFrame(()=>container.scrollTop=container.scrollHeight);
 }
 sidebar();updateSend();
};
$('prompt').addEventListener('input',resizeInput);
$('prompt').addEventListener('keydown',event=>{if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();$('composer').requestSubmit();}});
$('composer').addEventListener('submit',event=>{event.preventDefault();if(state.busy){sendCommand('stop');return;}const text=$('prompt').value.trim();if(text&&state.phase==='ready'&&!sending){sending=true;$('prompt').readOnly=true;updateSend();sendCommand('send',{text});}});
const newChat=()=>sendCommand('new');$('new-chat').onclick=newChat;$('brand').onclick=newChat;
$('search').oninput=sidebar;
$('chat-list').onclick=event=>{const item=event.target.closest('[data-chat]');if(item)sendCommand('select',{id:item.dataset.chat});};
$('messages').onclick=event=>{const copy=event.target.closest('[data-copy]');if(copy){const chat=state.chats.find(c=>c.id===state.active_id);sendCommand('copy',{text:chat.messages[Number(copy.dataset.copy)].text});}if(event.target.closest('[data-retry]'))sendCommand('retry');};
document.addEventListener('click',event=>{const link=event.target.closest('a');if(link){event.preventDefault();sendCommand('open_link',{text:link.getAttribute('href')||''});}});
document.querySelectorAll('[data-prompt]').forEach(button=>button.onclick=()=>{$('prompt').value=button.dataset.prompt;resizeInput();$('prompt').focus();});
$('toggle-sidebar').onclick=()=>{document.body.classList.toggle('sidebar-hidden');$('toggle-sidebar').setAttribute('aria-label',document.body.classList.contains('sidebar-hidden')?'Показать боковую панель':'Скрыть боковую панель');};
$('open-settings').onclick=()=>$('settings-dialog').showModal();$('close-settings').onclick=()=>$('settings-dialog').close();
$('settings-dialog').onclick=event=>{if(event.target===$('settings-dialog')){const r=event.target.getBoundingClientRect();if(event.clientX<r.left||event.clientX>r.right||event.clientY<r.top||event.clientY>r.bottom)event.target.close();}};
$('blur-toggle').onclick=()=>sendCommand('settings',{blur:!state.settings.blur,font_size:state.settings.font_size});
$('font-size').onchange=()=>sendCommand('settings',{blur:state.settings.blur,font_size:Number($('font-size').value)});
$('open-google').onclick=()=>sendCommand('show_browser');
document.querySelectorAll('[data-window]').forEach(button=>button.onclick=()=>sendCommand(button.dataset.window));
$('drag-space').onmousedown=event=>{if(event.button===0)sendCommand('drag');};$('drag-space').ondblclick=()=>sendCommand('maximize');
document.addEventListener('keydown',event=>{if(event.ctrlKey&&event.key.toLowerCase()==='n'){event.preventDefault();newChat();}if(event.ctrlKey&&event.key.toLowerCase()==='k'){event.preventDefault();$('search').focus();}if(event.key==='Escape'&&state.busy&&!$('settings-dialog').open)sendCommand('stop');});
resizeInput();sendCommand('init_history');
