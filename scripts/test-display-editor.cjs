// Regression checks against the shipped editor functions, without a browser.
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const html = fs.readFileSync('apps/kayiver/src/ui/index.html', 'utf8');
function section(start, end) { return html.slice(html.indexOf(start), html.indexOf(end, html.indexOf(start))); }
function tile(me, x, y, kind = '', hidden = false) {
  const classes = new Set(kind ? [kind] : []);
  return {me, machine:me?'mac':'windows', idx:0, x,y,w:100,h:100,
    el:{style:{display:hidden?'none':''},classList:{contains:c=>classes.has(c),add:c=>classes.add(c),remove:c=>classes.delete(c)}}};
}
const ctx = {mons:[],sharedCfg:null,candidate:null,OVERLAP_MIN:0.4,sharedBtn:{style:{},dataset:{}},t:x=>x};
vm.createContext(ctx);
vm.runInContext(section('function interArea(', "sharedBtn.addEventListener('click'"),ctx);
function overlap(a,b,expected) {
  ctx.mons=[a,b];ctx.updateOverlap();assert.equal(!!ctx.candidate,expected);
  assert.equal(ctx.sharedBtn.style.display,expected?'block':'none');
}
overlap(tile(true,0,0),tile(false,200,0),false);
overlap(tile(true,0,0),tile(false,100,0),false); // touching edges
// The hidden Windows copy of C used to produce the persistent shared button.
overlap(tile(true,0,0),tile(false,0,0,'display-presentation',true),false);
overlap(tile(true,0,0,'display-virtual'),tile(false,0,0),false);
overlap(tile(true,0,0),tile(false,0,0),true);
const anchor={...tile(true,100,200),realX:2560,realY:0,realW:2560,realH:1440,w:256,h:144};
const extra={...tile(true,100,92,'display-virtual'),realX:5120,realY:0,realW:1920,realH:1080,w:192,h:108};
let posted;
Object.assign(ctx,{mons:[anchor,extra],screenExtensionState:{role:'source',phase:'streaming'},sharedPair:()=>({a:anchor}),lang:'en',setMsg:()=>{},fetch:async(url,options)=>{posted={url,body:JSON.parse(options.body)};return {ok:true};}});
vm.runInContext(section('async function saveExtendedDisplayPosition()', "document.getElementById('save').addEventListener"),ctx);
(async()=>{await ctx.saveExtendedDisplayPosition();assert.equal(posted.url,'/api/display');assert.deepEqual(posted.body,{action:'position',x:2560,y:-1080});console.log('Passed: shared-panel overlap eligibility and B-top → C native coordinates');})().catch(e=>{console.error(e);process.exitCode=1;});
