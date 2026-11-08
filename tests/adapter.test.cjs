const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

// Minimal DOM fixture for the observed Google layout: several response blocks
// belong to one numeric data-container-id, with actions at the end of the turn.
class Element {
  constructor(tag, children=[], attributes={}) {
    this.nodeType=1; this.tagName=tag.toUpperCase(); this.childNodes=children;
    this.attributes=attributes; this.parentElement=null;
    for(const child of children) child.parentElement=this;
  }
  get innerText(){return this.childNodes.map(child=>child.innerText ?? child.textContent).join('');}
  get textContent(){return this.innerText;}
  get children(){return this.childNodes.filter(child=>child.nodeType===1);}
  getAttribute(name){return this.attributes[name] ?? null;}
  hasAttribute(name){return name in this.attributes;}
  getClientRects(){return [1];}
  matches(selector){return selector==='.n6owBd'&&this.attributes.class==='n6owBd';}
  contains(element){return this===element||this.children.some(child=>child.contains(element));}
  closest(){for(let p=this;p;p=p.parentElement)if(p.hasAttribute('data-container-id'))return p;return null;}
  querySelector(selector){return selector.includes('Gd7Hsc')&&this.complete?new Element('div'):null;}
  querySelectorAll(){return [];}
}
const text=value=>({nodeType:3,textContent:value});
const block=value=>new Element('div',[text(value)],{class:'n6owBd','data-complete':'true'});
function fixture(complete, codeOnly=false, refusal=false, staleRefusal=false) {
  const old=block('Old reply'); const first=block('First paragraph.');
  const second=block('Second paragraph.');
  // Google places rich blocks beside prose, outside the .n6owBd paragraphs.
  const code=new Element('div',[new Element('pre',[text('fn main() {}')])],{class:'rich-code'});
  const before=new Element('div',[old],{'data-container-id':'7'});before.complete=true;
  const current=new Element('div',codeOnly?[code]:[first,second,code],{'data-container-id':'8'});current.complete=complete;
  const errorRoot=new Element('div',[],{'data-container-id':staleRefusal?'6':'9'});
  const fallback=new Element('div',[text('Не удается дать ответ на этот поисковый запрос. Попробуйте спросить что-нибудь другое.')],{'data-subtree':'aimfl'});errorRoot.childNodes=[fallback];fallback.parentElement=errorRoot;
  const input=new Element('textarea');
  const config={input_selector:'inputs',response_selector:'responses',stop_selector:'stop',complete_selector:'actions'};
  const globals={URL,location:new URL('https://www.google.com/search?udm=50'),
    Node:{TEXT_NODE:3,ELEMENT_NODE:1},getComputedStyle:()=>({visibility:'visible'}),
    document:{body:{innerText:''},querySelector:()=>null,querySelectorAll:selector=>selector==='[data-container-id]'?(staleRefusal?[errorRoot,before,current]:[before,current,errorRoot]):selector==='[data-subtree="aimfl"]'&&(refusal||staleRefusal)?[fallback]:selector==='inputs'?[input]:selector==='responses'?(codeOnly?[old,code]:[old,first,second]):[]}};
  const adapter=vm.runInNewContext('('+fs.readFileSync('src/ai_mode.js','utf8')+')',globals);
  return adapter(config,'snapshot','');
}
test('combines all paragraphs and code from the latest turn',()=>{
  const response=fixture(true);
  assert.equal(response.response_count,2);
  assert.ok(response.text.includes('First paragraph.'));
  assert.ok(response.text.includes('Second paragraph.'));
  assert.ok(response.text.includes('```\nfn main() {}\n```'));
  assert.ok(!response.text.includes('Old reply'));
  assert.equal(response.complete,true); assert.equal(response.busy,false);
});
test('completed paragraphs do not mean the whole turn has finished',()=>{
  const response=fixture(false);
  assert.equal(response.complete,false);assert.equal(response.busy,true);
});
test('a response containing only a code block still completes',()=>{const response=fixture(true,true);assert.equal(response.response_count,2);assert.equal(response.complete,true);assert.ok(response.text.includes('fn main() {}'));assert.ok(!response.text.includes('First paragraph'));});

test('completed Google refusal is an actionable error, not an empty response timeout',()=>{assert.match(fixture(true,false,true).error,/SERVICE_REFUSAL/);assert.equal(fixture(true,false,false,true).error,'');});

test('CAPTCHA is detected from challenge URL and visible embedded widget',()=>{
  for (const embedded of [false,true]) {
    const element=new Element('iframe');
    const globals={URL,location:new URL(embedded?'https://www.google.com/search?udm=50':'https://www.google.com/sorry/index'),getComputedStyle:()=>({visibility:'visible'}),Node:{TEXT_NODE:3,ELEMENT_NODE:1},document:{body:{innerText:''},querySelector:()=>null,querySelectorAll:selector=>embedded&&selector.includes('recaptcha')?[element]:[]}};
    const adapter=vm.runInNewContext('('+fs.readFileSync('src/ai_mode.js','utf8')+')',globals);
    const snapshot=adapter({input_selector:'inputs',response_selector:'responses',stop_selector:'stop',complete_selector:'actions'},'snapshot','');
    assert.equal(snapshot.captcha,true);assert.match(snapshot.error,/CAPTCHA/);
  }
});
test('upload action opens the native attachment control when input is lazy',()=>{
  let clicked=false;const button=new Element('button',[],{'aria-label':'Прикрепить фото'});button.click=()=>clicked=true;
  const adapter=vm.runInNewContext('('+fs.readFileSync('src/ai_mode.js','utf8')+')',{URL,location:new URL('https://www.google.com/search?udm=50'),getComputedStyle:()=>({visibility:'visible'}),document:{querySelector:()=>null,querySelectorAll:selector=>selector==='button, [role="button"]'?[button]:[]}});
  assert.equal(adapter({input_selector:'inputs'},'open_upload',''),true);assert.equal(clicked,true);
});

function uploadFixture({images=[],busy=false,label='',ready=true}={}){
 const scope={innerText:label,querySelectorAll(selector){if(selector==='img')return images.map(src=>({complete:true,naturalWidth:90,currentSrc:src,getClientRects:()=>[1]}));if(selector.includes('aria-busy'))return busy?[{getClientRects:()=>[1]}]:[];return [];}};
 const input={disabled:false,getClientRects:()=>[1],closest:()=>scope};
 const adapter=vm.runInNewContext('('+fs.readFileSync('src/ai_mode.js','utf8')+')',{URL,location:new URL('https://www.google.com/search?udm=50'),getComputedStyle:()=>({visibility:'visible'}),document:{querySelectorAll:selector=>selector==='inputs'&&ready?[input]:[],querySelector:()=>null}});
 return (op,request)=>adapter({input_selector:'inputs'},op,request?JSON.stringify(request):'');
}
test('upload accepts new loaded composer thumbnails without visible filenames',()=>{
 const adapter=uploadFixture({images:['blob:old','blob:new']});const request={names:['photo.png'],before:{images:['blob:old']},image_count:1};
 assert.equal(adapter('upload_confirm',request),true);assert.deepEqual(Array.from(adapter('upload_state').images),['blob:old','blob:new']);
});
test('upload rejects stale thumbnails, partial batches, loading indicators and a missing composer',()=>{
 const request={names:['photo.png'],before:{images:['blob:old']},image_count:1};
 assert.equal(uploadFixture({images:['blob:old']})('upload_confirm',request),false);
 assert.equal(uploadFixture({images:['blob:new'],busy:true})('upload_confirm',request),false);
 assert.equal(uploadFixture({ready:false})('upload_confirm',request),false);
 assert.equal(uploadFixture({images:['blob:new']})('upload_confirm',{...request,names:['one.png','two.png'],image_count:2}),false);
});
test('named document attachments still confirm without thumbnails',()=>{
 assert.equal(uploadFixture({label:'report.pdf'})('upload_confirm',{names:['report.pdf'],before:{images:[]},image_count:0}),true);
});
