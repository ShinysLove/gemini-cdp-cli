'use strict';
const $ = id => document.getElementById(id);
const icon = name => `<svg aria-hidden="true"><use href="#i-${name}"/></svg>`;
let state = {chats:[],active_id:'',status:'Подключение',phase:'connecting',busy:false,settings:{blur:true,font_size:15}};
let signature = '', listSignature = '', toastTimer, sending = false;
let chatHasAttachments = false;
const drafts = new Map();
const sendCommand = (action, extra={}) => window.ipc?.postMessage(JSON.stringify({action,...extra}));
const escape = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
function toast(text) { $('toast').textContent=text; $('toast').classList.add('visible'); clearTimeout(toastTimer); toastTimer=setTimeout(()=>$('toast').classList.remove('visible'),2400); }
function resizeInput(){const el=$('prompt');el.style.height='auto';el.style.height=Math.min(el.scrollHeight,170)+'px';updateSend();}
function updateSend(){const ready=state.phase==='ready';$('send').disabled=state.busy?false:sending||!ready||(!$('prompt').value.trim()&&!chatHasAttachments);$('send').classList.toggle('stopping',state.busy);$('send').innerHTML=icon(state.busy?'square':'arrow');$('send').setAttribute('aria-label',state.busy?'Прекратить ожидание':'Отправить сообщение');$('enter-hint').hidden=state.busy;$('composer-hint').textContent=state.busy?'Готовим ответ…':sending?'Отправляем…':'Текстовый диалог';}
function sidebar(){const query=$('search').value.toLocaleLowerCase();const chats=state.chats.filter(c=>c.messages.length&&c.title.toLocaleLowerCase().includes(query));const key=JSON.stringify([chats.map(c=>[c.id,c.title]),state.active_id,query]);if(key===listSignature)return;listSignature=key;$('chat-count').textContent=state.chats.filter(c=>c.messages.length).length;$('chat-list').innerHTML=chats.length?chats.map(chat=>`<button class="chat-item ${chat.id===state.active_id?'active':''}" data-chat="${escape(chat.id)}" title="${escape(chat.title)}">${icon('chat')}<span>${escape(chat.title)}</span></button>`).join(''):`<div class="empty-history">${query?'Ничего не найдено':'Ваши разговоры будут здесь.<br>Начните с первого вопроса.'}</div>`;}
window.receive = incoming => {
 if(incoming.toast){toast(incoming.toast);return;}
 sending=false;$('prompt').readOnly=false;
 const previous=state.active_id;
 const previousMessageCount=state.chats.find(c=>c.id===previous)?.messages.length||0;
 if(previous!==incoming.active_id){drafts.set(previous,$('prompt').value);$('prompt').value=drafts.get(incoming.active_id)||'';resizeInput();}
 state=incoming;
 if(incoming.sent){$('prompt').value='';drafts.delete(state.active_id);resizeInput();}
 document.body.classList.toggle('no-glass',!state.settings.blur);
 document.documentElement.style.setProperty('--chat-font',state.settings.font_size+'px');
 $('blur-toggle').setAttribute('aria-checked',String(state.settings.blur));$('font-size').value=state.settings.font_size;
 $('connection').className='connection '+(state.busy?'busy':state.phase);
 $('connection-text').textContent=state.busy?'Отвечает':state.phase==='ready'?'Готов к работе':state.phase==='error'?'Нет подключения':'Подключение';
 $('connection').title=state.status;
 const error=state.error||'';if(!window.captchaActive){$('notice').hidden=!error;$('notice').textContent=error;}
 const chat=state.chats.find(c=>c.id===state.active_id);
 $('chat-title').textContent=chat?.messages.length?chat.title:'Новый диалог';
 const messages=chat?.messages||[];$('welcome').hidden=!!messages.length;
 const key=JSON.stringify([messages,state.busy,state.active_id]);
 if(key!==signature){signature=key;const container=$('conversation');const nearBottom=container.scrollHeight-container.scrollTop-container.clientHeight<100;const last=messages.length-1;
 $('messages').innerHTML=messages.map((message,index)=>message.role==='user'?`<article class="message user ${previous===state.active_id&&index>=previousMessageCount?'message-enter':''}"><div class="user-message-content">${messageAttachmentsMarkup(message,index)}${message.text?`<div class="user-bubble">${escape(message.text)}</div>`:''}</div></article>`:`<article class="message assistant ${previous===state.active_id&&index>=previousMessageCount?'message-enter':''} ${message.interrupted?'interrupted':''}"><div class="assistant-label">${icon('star')}<span>EVIL GEMINI</span></div>${message.text?`<div class="assistant-content">${message.html}</div>`:state.busy?'<div class="thinking"><span class="thinking-dots"><i></i><i></i><i></i></span>Разбираемся в деталях</div>':'<div class="assistant-content"><p>Ответ не получен.</p></div>'}${message.text?`<div class="message-actions"><button data-copy="${index}" title="Копировать ответ" aria-label="Копировать ответ">${icon('copy')}</button>${index===last&&!state.busy?`<button data-retry title="Повторить запрос" aria-label="Повторить запрос">${icon('retry')}</button>`:''}</div>`:''}</article>`).join('');
 if(previous!==state.active_id||nearBottom||incoming.sent)requestAnimationFrame(()=>container.scrollTop=container.scrollHeight);
 }
 sidebar();updateSend();
};
$('prompt').addEventListener('input',resizeInput);
$('prompt').addEventListener('keydown',event=>{if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();$('composer').requestSubmit();}});
$('composer').addEventListener('submit',event=>{event.preventDefault();if(state.busy){sendCommand('stop');return;}const text=$('prompt').value.trim();if((text||chatHasAttachments)&&state.phase==='ready'&&!sending){sending=true;$('prompt').readOnly=true;updateSend();sendCommand('send',{text});}});
const newChat=()=>sendCommand('new');$('new-chat').onclick=newChat;$('brand').onclick=newChat;
$('search').oninput=sidebar;
$('chat-list').onclick=event=>{const item=event.target.closest('[data-chat]');if(item)sendCommand('select',{id:item.dataset.chat});};
$('messages').onclick=event=>{const preview=event.target.closest('[data-message-attachment]');if(preview){const [messageIndex,fileIndex]=preview.dataset.messageAttachment.split(':').map(Number);const file=state.chats.find(c=>c.id===state.active_id)?.messages[messageIndex]?.attachments?.[fileIndex];if(file)showAttachment(file);return;}const copy=event.target.closest('[data-copy]');if(copy){const chat=state.chats.find(c=>c.id===state.active_id);sendCommand('copy',{text:chat.messages[Number(copy.dataset.copy)].text});}if(event.target.closest('[data-retry]'))sendCommand('retry');};
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

$('chat-attach').onclick=()=>sendCommand('choose_attachments');
$('chat-attachments').onclick=event=>$('attachment-list').onclick(event);

window.challengeNotice=(text,active)=>{window.captchaActive=active;$('notice').hidden=false;$('notice').textContent=text;if(!active)toast(text);};

// Clipboard data is read only in response to a paste in a composer.
function pasteMedia(event) {
 const files=Array.from(event.clipboardData?.items||[]).filter(item=>item.kind==='file').map(item=>item.getAsFile()).filter(Boolean);
 if(!files.length)return;
 if(!event.clipboardData.getData('text/plain'))event.preventDefault();
 for(const file of files){
  if(!['image/png','image/jpeg','image/webp','image/gif','image/bmp'].includes(file.type)){toast('Этот формат медиа не поддерживается. Вставьте PNG, JPEG, WebP, GIF или BMP.');continue;}
  if(!file.size||file.size>20*1024*1024){toast('Изображение должно быть размером от 1 байта до 20 МБ.');continue;}
  const reader=new FileReader();
  reader.onload=()=>{const data=String(reader.result||'');const comma=data.indexOf(',');if(comma<0){toast('Не удалось прочитать изображение из буфера.');return;}sendCommand('attach_clipboard',{id:file.type,text:data.slice(comma+1)});};
  reader.onerror=()=>toast('Не удалось прочитать изображение из буфера.');
  reader.readAsDataURL(file);
 }
}
$('prompt').addEventListener('paste',pasteMedia);
$('coding-goal').addEventListener('paste',pasteMedia);

const surfaceAnimations=new WeakMap();
function animateSurface(element,show=true,direction=1,onHidden=()=>{}) {
 const previous=surfaceAnimations.get(element);if(previous)previous.cancel();
 surfaceAnimations.delete(element);
 if(show)element.hidden=false;
 const reduced=typeof matchMedia==='function'&&matchMedia('(prefers-reduced-motion: reduce)').matches;
 if(reduced||typeof element.animate!=='function'){if(!show){element.hidden=true;onHidden();}return;}
 const offset=direction*10;
 const frames=[{opacity:0,transform:'translateX('+offset+'px)'},{opacity:1,transform:'translateX(0)'}];
 const animation=element.animate(show?frames:frames.slice().reverse(),{duration:show?220:160,easing:'cubic-bezier(.2,.7,.2,1)'});
 surfaceAnimations.set(element,animation);
 animation.onfinish=()=>{if(surfaceAnimations.get(element)!==animation)return;surfaceAnimations.delete(element);if(!show){element.hidden=true;onHidden();}};
}

function attachmentMarkup(file){
 const image=typeof file.image==='string'&&/^data:image\/(png|jpeg|webp|gif|bmp);base64,/.test(file.image)?file.image:null;
 return '<span class="attachment-card '+(image?'has-image':'')+'"><button type="button" class="attachment-preview-button" data-preview="'+escape(file.id)+'" title="'+escape(file.name)+'">'+(image?'<img src="'+escape(image)+'" alt="'+escape(file.name)+'">':'<span class="attachment-file-icon" aria-hidden="true">▤</span>')+'<span class="attachment-name">'+escape(file.name)+'</span></button><button type="button" class="attachment-remove" data-remove="'+escape(file.id)+'" aria-label="Убрать вложение">×</button></span>';
}

function safeAttachmentImage(file){return typeof file.image==='string'&&/^data:image\/(png|jpeg|webp|gif|bmp);base64,/.test(file.image)?file.image:null;}
function messageAttachmentsMarkup(message,index){
 if(!message.attachments?.length)return '';
 return '<div class="message-attachments">'+message.attachments.map((file,fileIndex)=>{const image=safeAttachmentImage(file);return '<button type="button" class="message-attachment '+(image?'has-image':'')+'" data-message-attachment="'+index+':'+fileIndex+'" title="'+escape(file.name)+'" aria-label="Открыть вложение '+escape(file.name)+'">'+(image?'<img src="'+escape(image)+'" alt="'+escape(file.name)+'">':'<span class="attachment-file-icon" aria-hidden="true">▤</span><span>'+escape(file.name)+'</span>')+'</button>';}).join('')+'</div>';
}
function showAttachment(file){
 $('attachment-title').textContent=file.name;$('attachment-preview').replaceChildren();
 const image=safeAttachmentImage(file);if(image){const element=document.createElement('img');element.src=image;element.alt=file.name;$('attachment-preview').append(element);}
 const details=document.createElement('p');details.textContent=file.preview||file.mime||'Предпросмотр недоступен';$('attachment-preview').append(details);$('attachment-dialog').showModal();
}
