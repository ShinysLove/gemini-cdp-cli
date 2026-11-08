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
