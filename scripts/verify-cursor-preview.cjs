const assert=require('node:assert/strict');
const machines=[{name:'mac',me:true},{name:'win',me:false}]; const tabPos=null;
const mac={machine:'mac',realX:2560,realY:0,realW:2560,realH:1440,x:500,y:100,w:256,h:144,el:{style:{display:''}}};
const shadow={machine:'win',realX:-163,realY:1080,realW:2560,realH:1440,x:0,y:0,w:256,h:144,el:{style:{display:'none'}}};
const upper={machine:'win',realX:0,realY:0,realW:1920,realH:1080,x:500,y:0,w:192,h:108,el:{style:{display:''}}};
let mons=[mac,shadow,upper];function sharedPair(){return{a:mac,b:shadow};}
const fs=require('node:fs'); const path=require('node:path');
const html=fs.readFileSync(path.join(__dirname,'../apps/kayiver/src/ui/index.html'),'utf8');
const begin=html.indexOf('function mapCursor(c) {'); const end=html.indexOf('async function pollCursor()',begin);
assert.ok(begin>=0 && end>begin,'cursor projection function missing');
const mapCursor=new Function('machines','mons','sharedPair','tabPos','TAB_W','TAB_H',html.slice(begin,end)+'; return mapCursor;')(machines,mons,sharedPair,tabPos,200,100);

assert.deepEqual(mapCursor({machine:'win',x:1117,y:1800}),{x:628,y:172});
assert.deepEqual(mapCursor({machine:'win',focus:'mac',x:727,y:580}),{x:572.7,y:58});
assert.deepEqual(mapCursor({machine:'mac',x:3840,y:720}),{x:628,y:172});
assert.equal(mapCursor({machine:'win',x:727,y:580,visible:false}),null);
mons.push({...upper});assert.equal(mapCursor({machine:'win',x:727,y:580}),null);
console.log('Remote, receiver, hidden shared alias and ambiguous cursor projections verified');
