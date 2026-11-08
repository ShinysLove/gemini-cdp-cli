'use strict';
// Pure presentation helpers: never execute model text or command output.
const WorkbenchCore = (() => {
 const labels={queued:'Ожидает зависимости',waiting_capacity:'Ожидает ресурсов',running:'В работе',waiting_approval:'Нужно подтверждение',rate_limited:'Пауза сервиса',paused:'Пауза сервиса',succeeded:'Готово',failed:'Ошибка этапа',cancelled:'Отменён',blocked:'Зависимость не завершена'};
 const terminal=new Set(['succeeded','failed','cancelled','blocked','paused']);
 function diagnosis(error){const text=String(error||'');if(text.includes('Unverified completion'))return 'Модель заявила о завершении без успешного вызова инструментов. Результат не засчитан; повторите этап.';if(text.includes('done=true'))return 'Модель одновременно завершила этап и запросила инструменты. Противоречивые команды не выполнялись. Можно повторить этап.';if(text.includes('Invalid model reply')||text.includes('structured JSON'))return 'Ответ модели не соответствует протоколу инструментов. Откройте действия этапа для просмотра ответа и повторите незавершённые этапы.';if(text.includes('dependency failed'))return 'Ожидает исправления предшествующего этапа. Это не отдельная ошибка.';if(text.includes('timed out')||text.includes('Истекло время ожидания'))return 'Истекло время ожидания. Успешные этапы сохранены; проверьте состояние Google перед повтором.';return text;}
 function diffLines(before,after){const a=before==null?[]:before.split('\n'),b=after==null?[]:after.split('\n');let prefix=0;while(prefix<a.length&&prefix<b.length&&a[prefix]===b[prefix])prefix++;let suffix=0;while(suffix<a.length-prefix&&suffix<b.length-prefix&&a[a.length-1-suffix]===b[b.length-1-suffix])suffix++;
 const result=a.slice(0,prefix).map(text=>({kind:'context',text}));const aa=a.slice(prefix,a.length-suffix),bb=b.slice(prefix,b.length-suffix);
 if(aa.length*bb.length<=160000){const table=Array.from({length:aa.length+1},()=>new Uint16Array(bb.length+1));for(let i=aa.length-1;i>=0;i--)for(let j=bb.length-1;j>=0;j--)table[i][j]=aa[i]===bb[j]?1+table[i+1][j+1]:Math.max(table[i+1][j],table[i][j+1]);let i=0,j=0;while(i<aa.length||j<bb.length){if(i<aa.length&&j<bb.length&&aa[i]===bb[j]){result.push({kind:'context',text:aa[i++]});j++;}else if(i<aa.length&&(j===bb.length||table[i+1][j]>=table[i][j+1]))result.push({kind:'remove',text:aa[i++]});else result.push({kind:'add',text:bb[j++]});}}
 else {result.push(...aa.map(text=>({kind:'remove',text})),...bb.map(text=>({kind:'add',text})));}
 result.push(...a.slice(a.length-suffix).map(text=>({kind:'context',text})));let old=0,next=0;return result.map(line=>({...line,old:line.kind==='add'?'':++old,next:line.kind==='remove'?'':++next}));
 }
 function levels(agents){const byId=new Map(agents.map(a=>[a.task.id,a]));const memo=new Map();function depth(id,seen=new Set()){if(memo.has(id))return memo.get(id);if(seen.has(id))return 0;seen.add(id);const deps=byId.get(id)?.task.dependencies||[];const n=deps.length?1+Math.max(...deps.map(d=>depth(d,new Set(seen)))):0;memo.set(id,n);return n;}return agents.map(a=>depth(a.task.id));}
 function characters(value){let n=0;for(const _ of String(value||''))n++;return n;}
 function tokens(chars){return Math.ceil(Math.max(0,Number(chars)||0)/4);}
 return {labels,terminal,diagnosis,diffLines,levels,characters,tokens};
})();
if(typeof module!=='undefined')module.exports=WorkbenchCore;
