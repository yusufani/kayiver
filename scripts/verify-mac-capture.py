#!/usr/bin/env python3
"""Opt-in native capture test. Moves the cursor on the running paired devices.
Only marker-tagged synthetic motion is observed; no keys or clipboard are read.
Requires an idle local source and a known connected seam.
This tests synthetic event delivery only; it cannot validate physical mouse
acceleration or establish crossing acceptance. 0.3.9 was withdrawn.
"""
import argparse,sys,math
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--start',type=float,nargs=2,required=True)
parser.add_argument('--exit',type=float,nargs=2,required=True)
parser.add_argument('--exit-delta',type=int,nargs=2,required=True)
parser.add_argument('--return-delta',type=int,nargs=2,required=True)
parser.add_argument('--roundtrips',type=int,default=1)
args=parser.parse_args()
if not 1<=args.roundtrips<=1000:parser.error('roundtrips must be between 1 and 1000')
if sys.platform!='darwin':parser.error('requires macOS')
if not all(math.isfinite(x) for x in args.start+args.exit):parser.error('coordinates must be finite')
import ctypes as c,threading,time,json,urllib.request
cg=c.CDLL('/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics');cf=c.CDLL('/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation')
class P(c.Structure):_fields_=[('x',c.c_double),('y',c.c_double)]
CB=c.CFUNCTYPE(c.c_void_p,c.c_void_p,c.c_uint32,c.c_void_p,c.c_void_p)
cg.CGEventGetIntegerValueField.argtypes=[c.c_void_p,c.c_uint32];cg.CGEventGetIntegerValueField.restype=c.c_int64
cg.CGEventTapCreate.argtypes=[c.c_uint32,c.c_uint32,c.c_uint32,c.c_uint64,CB,c.c_void_p];cg.CGEventTapCreate.restype=c.c_void_p
cg.CGEventTapEnable.argtypes=[c.c_void_p,c.c_bool]
cf.CFMachPortCreateRunLoopSource.argtypes=[c.c_void_p,c.c_void_p,c.c_long];cf.CFMachPortCreateRunLoopSource.restype=c.c_void_p
cf.CFRunLoopGetCurrent.restype=c.c_void_p;cf.CFRunLoopAddSource.argtypes=[c.c_void_p,c.c_void_p,c.c_void_p];cf.CFRunLoopRunInMode.argtypes=[c.c_void_p,c.c_double,c.c_bool]
cf.CFRelease.argtypes=[c.c_void_p];cg.CGEventCreateMouseEvent.argtypes=[c.c_void_p,c.c_uint32,P,c.c_uint32];cg.CGEventCreateMouseEvent.restype=c.c_void_p
cg.CGEventSetDoubleValueField.argtypes=[c.c_void_p,c.c_uint32,c.c_double]
cg.CGEventSetIntegerValueField.argtypes=[c.c_void_p,c.c_uint32,c.c_int64];cg.CGEventPost.argtypes=[c.c_uint32,c.c_void_p]
cg.CGCursorIsVisible.restype=c.c_bool
seen=[];ready=threading.Event();stop=threading.Event();failure=[]
@CB
def observe(proxy,kind,event,user):
 tag=cg.CGEventGetIntegerValueField(event,42)
 if 0x54455300<=tag<0x54455400:seen.append(tag)
 return event
def monitor():
 tap=cg.CGEventTapCreate(2,1,1,1<<5,observe,None)
 if not tap:failure.append('annotated observer permission unavailable');ready.set();return
 src=cf.CFMachPortCreateRunLoopSource(None,tap,0);rl=cf.CFRunLoopGetCurrent();mode=c.c_void_p.in_dll(cf,'kCFRunLoopDefaultMode').value
 cf.CFRunLoopAddSource(rl,src,mode);cg.CGEventTapEnable(tap,True);ready.set()
 while not stop.is_set():cf.CFRunLoopRunInMode(mode,.03,False)
 cg.CGEventTapEnable(tap,False);cf.CFRelease(src);cf.CFRelease(tap)
t=threading.Thread(target=monitor);t.start();assert ready.wait(2),"observer did not start";assert not failure,failure

def state():
 with urllib.request.urlopen('http://127.0.0.1:24818/api/status',timeout=3) as r:return json.load(r)['capture']
def move(x,y,dx,dy,tag,unaccelerated_y=None):
 e=cg.CGEventCreateMouseEvent(None,5,P(x,y),0)
 for field,v in [(42,tag),(4,dx),(5,dy)]:cg.CGEventSetIntegerValueField(e,field,v)
 if unaccelerated_y is not None:cg.CGEventSetDoubleValueField(e,171,unaccelerated_y)
 cg.CGEventPost(0,e);cf.CFRelease(e);time.sleep(.015);return state()
def verify_roundtrip():
 initial=state();assert not initial["forwarding"] and not initial["driven"],"requires local control on an idle source"
 seen.clear()
 local=0x54455301;remote=0x54455302;remote2=0x54455303;back=0x54455304;queued=0x54455305
 start=state()['navigation']['location'];point=(start or {}).get('point',{'x':args.start[0],'y':args.start[1]})
 s=move(*args.start,round(args.start[0]-point['x']),round(args.start[1]-point['y']),local)
 s=move(*args.exit,*args.exit_delta,remote);assert s['forwarding'],'seam did not cross'
 park=s['native_cursor'];hidden=not cg.CGCursorIsVisible();s=move(park[0]+12.5,park[1]+20.25,12,20,remote2)
 assert s['forwarding'],'concurrent physical input returned control during this fixture'
 assert s['native_cursor']==park,('native drift',s['native_cursor'],park,s['navigation'])
 zero=0x54455306;physical_zero=0x54455307
 s=move(park[0]+.25,park[1]-.25,0,0,zero)
 assert s['forwarding'] and s['native_cursor']==park,'zero-input native reposition escaped containment'
 s=move(park[0]+.25,park[1]-.25,0,0,physical_zero,-1.0)
 assert s['forwarding'] and s['native_cursor']==park,'quantized physical zero escaped containment'
 s=move(park[0]+args.return_delta[0],park[1]+args.return_delta[1],*args.return_delta,back);assert not s['forwarding'],'return failed'
 point=s['navigation']['location']['point'];s=move(park[0]+args.return_delta[0]+1,park[1]+args.return_delta[1],1,0,queued)
 assert abs(s['navigation']['location']['point']['x']-point['x']-1)<.01,'return queue gained parking distance'
 result=({'delivered_tags':seen,'remote_tags_leaked':[x for x in seen if x in [remote,remote2,zero,physical_zero]],'local_delivered':local in seen,'return_delivered':back in seen,'native_park':park,'cursor_hidden_while_remote':hidden,'cursor_visible_after_return':cg.CGCursorIsVisible()})
 assert local in seen and back in seen,'observer did not see local controls'
 assert all(x not in seen for x in [remote,remote2,zero,physical_zero]),'remote movement reached Mac applications'
 # CGCursorIsVisible is an advisory query, not physical visibility proof.
 return result
try:
 for iteration in range(args.roundtrips):result=verify_roundtrip()
 print(json.dumps({'roundtrips':args.roundtrips,**result}))
finally:stop.set();t.join(2)
