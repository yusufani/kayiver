#!/usr/bin/env python3
"""Read-only annotated-session mouse delivery observer. Never posts events.
Only motion identities are recorded by default; --positions also records cursor
coordinates. No keys or clipboard content are read.
Use with KAYIVER_NATIVE_MOTION_TRACE to inspect physical delivery independently.
"""
import argparse,ctypes as c,json,os,sys,time
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--seconds',type=float,default=30);p.add_argument('--out',required=True);p.add_argument('--positions',action='store_true');args=p.parse_args()
if sys.platform!='darwin':p.error('macOS required')
if not 1<=args.seconds<=60:p.error('seconds must be between 1 and 60')
cg=c.CDLL('/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics');cf=c.CDLL('/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation')
class P(c.Structure):_fields_=[('x',c.c_double),('y',c.c_double)]
cg.CGEventGetLocation.argtypes=[c.c_void_p];cg.CGEventGetLocation.restype=P
CB=c.CFUNCTYPE(c.c_void_p,c.c_void_p,c.c_uint32,c.c_void_p,c.c_void_p)
cg.CGEventGetIntegerValueField.argtypes=[c.c_void_p,c.c_uint32];cg.CGEventGetIntegerValueField.restype=c.c_int64
cg.CGEventGetTimestamp.argtypes=[c.c_void_p];cg.CGEventGetTimestamp.restype=c.c_uint64
cg.CGEventTapCreate.argtypes=[c.c_uint32,c.c_uint32,c.c_uint32,c.c_uint64,CB,c.c_void_p];cg.CGEventTapCreate.restype=c.c_void_p
cg.CGEventTapEnable.argtypes=[c.c_void_p,c.c_bool];cf.CFMachPortCreateRunLoopSource.argtypes=[c.c_void_p,c.c_void_p,c.c_long];cf.CFMachPortCreateRunLoopSource.restype=c.c_void_p
cf.CFRunLoopGetCurrent.restype=c.c_void_p;cf.CFRunLoopAddSource.argtypes=[c.c_void_p,c.c_void_p,c.c_void_p];cf.CFRunLoopRunInMode.argtypes=[c.c_void_p,c.c_double,c.c_bool];cf.CFRelease.argtypes=[c.c_void_p]
rows=[];dropped=0;tap=None
@CB
def observe(proxy,kind,event,user):
 global dropped
 if kind in (0xfffffffe,0xffffffff):cg.CGEventTapEnable(tap,True);return event
 if len(rows)<120000:
  row={'timestamp':cg.CGEventGetTimestamp(event),'kind':kind,'source_pid':cg.CGEventGetIntegerValueField(event,41)}
  if args.positions:
   position=cg.CGEventGetLocation(event);row['position']=[position.x,position.y]
  rows.append(row)
 else:dropped+=1
 return event
tap=cg.CGEventTapCreate(2,1,1,sum(1<<kind for kind in (5,6,7,27)),observe,None)
if not tap:raise SystemExit('read-only observer unavailable')
src=cf.CFMachPortCreateRunLoopSource(None,tap,0);mode=c.c_void_p.in_dll(cf,'kCFRunLoopDefaultMode').value
cf.CFRunLoopAddSource(cf.CFRunLoopGetCurrent(),src,mode);cg.CGEventTapEnable(tap,True)
try:
 end=time.monotonic()+args.seconds
 while time.monotonic()<end:cf.CFRunLoopRunInMode(mode,.03,False)
finally:cg.CGEventTapEnable(tap,False);cf.CFRelease(src);cf.CFRelease(tap)
fd=os.open(args.out,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
with os.fdopen(fd,'w') as f:
 for row in rows:f.write(json.dumps(row)+'\n')
print(json.dumps({'delivered_motion_reports':len(rows),'physical_source_reports':sum(x['source_pid']==0 for x in rows),'dropped':dropped,'seconds':args.seconds}))
