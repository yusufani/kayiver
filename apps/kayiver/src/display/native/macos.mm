// Experimental virtual display API is isolated here. No input hooks, audio,
// kernel extensions or changes to physical monitor modes are made by this helper.
#import <AppKit/AppKit.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <VideoToolbox/VideoToolbox.h>
#import <CoreGraphics/CoreGraphics.h>
#include <atomic>
#include <signal.h>
#include <unistd.h>
#include <time.h>

@interface KVDisplayDescriptor : NSObject
@property dispatch_queue_t queue;
@property(copy) NSString *name;
@property unsigned int maxPixelsWide, maxPixelsHigh, vendorID, productID, serialNum;
@property unsigned int serialNumber;
@property CGSize sizeInMillimeters;
@property CGPoint redPrimary, greenPrimary, bluePrimary, whitePoint;
@end
@interface KVDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)rate;
@end
@interface KVDisplaySettings : NSObject
@property unsigned int hiDPI;
@property(copy) NSArray *modes;
@end
@interface KVDisplay : NSObject
- (instancetype)initWithDescriptor:(id)descriptor;
- (BOOL)applySettings:(id)settings;
@property(readonly) unsigned int displayID;
@end

static uint64_t epochUs() {
    struct timespec t; clock_gettime(CLOCK_REALTIME, &t);
    return (uint64_t)t.tv_sec * 1000000 + t.tv_nsec / 1000;
}
static void be32(uint8_t *p, uint32_t n) { for (int i=3;i>=0;--i) {p[i]=n&255;n>>=8;} }
static void be64(uint8_t *p, uint64_t n) { for (int i=7;i>=0;--i) {p[i]=n&255;n>>=8;} }

@interface KVSource : NSObject <SCStreamOutput, SCStreamDelegate> {
@public
    VTCompressionSessionRef encoder;
    std::atomic<bool> busy;
    std::atomic<int> fps;
    uint64_t lastFrame;
}
@property KVDisplay *display;
@property SCStream *stream;
@property int width, height, bitrate;
@property BOOL stopping;
@property BOOL testMode;
@property int exitCode;
@property CGRect initialBounds;
@property dispatch_source_t signalSource, inputSource, parentWatch, testTimer;
@property dispatch_queue_t captureQueue;
- (void)stop:(NSString *)reason;
- (void)start;
- (BOOL)setupEncoder;
@end

static __strong KVSource *source;
static CFMachPortRef escapeTap=nullptr;
static void invalidEncodedFrame(KVSource *s) {
    s->busy=false;
    dispatch_async(dispatch_get_main_queue(), ^{[s stop:@"invalid hardware encoder output"];});
}
static CGEventRef escapeEvent(CGEventTapProxy proxy,CGEventType type,CGEventRef event,void *context) {
    if(type==kCGEventTapDisabledByTimeout||type==kCGEventTapDisabledByUserInput) {
        if(escapeTap&&!source.stopping)CGEventTapEnable(escapeTap,true);
        return event;
    }
    static uint64_t last=0;static unsigned count=0;
    if(type==kCGEventKeyDown&&!CGEventGetIntegerValueField(event,kCGKeyboardEventAutorepeat)) {
        uint64_t now=epochUs();
        if(CGEventGetIntegerValueField(event,kCGKeyboardEventKeycode)==53) {
            count=now-last<750000?count+1:1;last=now;
            if(count==3){count=0;dispatch_async(dispatch_get_main_queue(), ^{[source stop:@"triple Escape"];});}
        }else count=0;
    }
    return event; // observation only; never suppress or forward keys
}

static void encoded(void *context, void *frameContext, OSStatus status,
                    VTEncodeInfoFlags flags, CMSampleBufferRef sample) {
    @autoreleasepool {
    KVSource *s = (__bridge KVSource *)context;
    if (status || !sample || !CMSampleBufferDataIsReady(sample)) {
        s->busy = false;
        dispatch_async(dispatch_get_main_queue(), ^{[s stop:@"hardware encoding failed"];});
        return;
    }
    const uint8_t start[] = {0,0,0,1};
    NSMutableData *data = [NSMutableData data];
    CFArrayRef attachments = CMSampleBufferGetSampleAttachmentsArray(sample, false);
    bool key = !attachments || CFArrayGetCount(attachments)==0 || !CFDictionaryContainsKey((CFDictionaryRef)CFArrayGetValueAtIndex(attachments,0), kCMSampleAttachmentKey_NotSync);
    if (key) {
        CMFormatDescriptionRef format = CMSampleBufferGetFormatDescription(sample);
        for (size_t i=0;i<2;i++) {
            const uint8_t *p = nullptr; size_t n = 0;
            if (CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format,i,&p,&n,nullptr,nullptr)==noErr) {
                [data appendBytes:start length:4]; [data appendBytes:p length:n];
            }
        }
    }
    CMBlockBufferRef block = CMSampleBufferGetDataBuffer(sample);
    size_t size = CMBlockBufferGetDataLength(block);
    if(size==0||size>8*1024*1024){invalidEncodedFrame(s);return;}
    NSMutableData *avcc = [NSMutableData dataWithLength:size];
    if (CMBlockBufferCopyDataBytes(block,0,size,avcc.mutableBytes)!=noErr) {invalidEncodedFrame(s);return;}
    const uint8_t *p = (const uint8_t *)avcc.bytes;
    size_t pos=0;
    while (pos+4<=size) {
        uint32_t n=((uint32_t)p[pos]<<24)|((uint32_t)p[pos+1]<<16)|((uint32_t)p[pos+2]<<8)|p[pos+3];
        pos+=4;
        if (!n || n>size-pos) {invalidEncodedFrame(s);return;}
        [data appendBytes:start length:4]; [data appendBytes:p+pos length:n]; pos+=n;
    }
    if (data.length>8*1024*1024 || pos!=size) {invalidEncodedFrame(s);return;}
    uint8_t header[12]; be32(header,(uint32_t)data.length); be64(header+4,(uint64_t)(uintptr_t)frameContext);
    // At most one encode is in flight. If the parent is slow, this pipe
    // blocks this callback and new raw frames are skipped BEFORE encoding.
    bool ok = fwrite(header,1,sizeof(header),stdout)==sizeof(header)
        && fwrite(data.bytes,1,data.length,stdout)==data.length && fflush(stdout)==0;
    // A pipe's byte capacity can otherwise queue dozens of small frames.
    // Keep busy until the parent explicitly acknowledges this access unit.
    if (!ok) {s->busy=false;dispatch_async(dispatch_get_main_queue(), ^{[s stop:@"parent disconnected"];});}
    }
}

@implementation KVSource
- (void)stop:(NSString *)reason {
    if (self.stopping) return;
    self.stopping = YES;
    NSSet *normal=[NSSet setWithArray:@[@"session stopped",@"parent closed session",@"parent exited",@"triple Escape",@"desktop locked",@"computer sleeping",@"synthetic encoder test complete"]];
    self.exitCode=[normal containsObject:reason]?0:1;
    fprintf(stderr,"display source: %s\n",reason.UTF8String);
    if (self.stream) [self.stream stopCaptureWithCompletionHandler:^(NSError *error) {}];
    if(self.captureQueue)dispatch_sync(self.captureQueue, ^{});
    if (encoder) { VTCompressionSessionInvalidate(encoder); CFRelease(encoder); encoder=nullptr; }
    // Never leave the pointer on the disappearing virtual surface.
    if(!self.testMode&&self.display) {
        CGEventRef event=CGEventCreate(nullptr);
        if(event) {
            CGPoint cursor=CGEventGetLocation(event);CFRelease(event);
            if(CGRectContainsPoint(CGDisplayBounds(self.display.displayID),cursor)) {
                CGRect visible=CGDisplayBounds(CGMainDisplayID());
                CGWarpMouseCursorPosition(CGPointMake(CGRectGetMidX(visible),CGRectGetMidY(visible)));
            }
        }
    }
    self.stream=nil; self.display=nil;
    [NSApp stop:nil];
    // Wake NSApplication's event loop even when no windows exist.
    [NSApp postEvent:[NSEvent otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0 timestamp:0 windowNumber:0 context:nil subtype:0 data1:0 data2:0] atStart:NO];
}
- (BOOL)setupEncoder {
    NSDictionary *spec=@{(__bridge NSString *)kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder:@YES,
        (__bridge NSString *)kVTVideoEncoderSpecification_EnableLowLatencyRateControl:@YES};
    OSStatus result=VTCompressionSessionCreate(kCFAllocatorDefault,self.width,self.height,kCMVideoCodecType_H264,
        (__bridge CFDictionaryRef)spec,nullptr,nullptr,encoded,(__bridge void *)self,&encoder);
    if(result!=noErr) {return NO;}
    NSDictionary *properties=@{
        (__bridge NSString *)kVTCompressionPropertyKey_RealTime:@YES,
        (__bridge NSString *)kVTCompressionPropertyKey_AllowFrameReordering:@NO,
        (__bridge NSString *)kVTCompressionPropertyKey_ExpectedFrameRate:@60,
        (__bridge NSString *)kVTCompressionPropertyKey_AverageBitRate:@(self.bitrate),
        (__bridge NSString *)kVTCompressionPropertyKey_MaxKeyFrameInterval:@120,
        (__bridge NSString *)kVTCompressionPropertyKey_ProfileLevel:(__bridge NSString *)kVTProfileLevel_H264_High_AutoLevel,
        (__bridge NSString *)kVTCompressionPropertyKey_ColorPrimaries:(__bridge NSString *)kCVImageBufferColorPrimaries_ITU_R_709_2,
        (__bridge NSString *)kVTCompressionPropertyKey_TransferFunction:(__bridge NSString *)kCVImageBufferTransferFunction_ITU_R_709_2,
        (__bridge NSString *)kVTCompressionPropertyKey_YCbCrMatrix:(__bridge NSString *)kCVImageBufferYCbCrMatrix_ITU_R_709_2,
    };
    if(VTSessionSetProperties(encoder,(__bridge CFDictionaryRef)properties)!=noErr
        || VTCompressionSessionPrepareToEncodeFrames(encoder)!=noErr) {return NO;}
    return YES;
}
- (void)start {
    if(self.testMode) {
        if(![self setupEncoder]) {[self stop:@"hardware encoder self-test failed"];return;}
        printf("{\"ready\":true,\"hardware\":true,\"synthetic\":true,\"x\":0,\"y\":0}\n");fflush(stdout);
        self.testTimer=dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER,0,0,dispatch_get_main_queue());
        dispatch_source_set_timer(self.testTimer,dispatch_time(DISPATCH_TIME_NOW,0),NSEC_PER_SEC/60,0);
        __block int frame=0;
        dispatch_source_set_event_handler(self.testTimer, ^{
            if(self->busy.load())return;
            if(frame>=180){[self stop:@"synthetic encoder test complete"];return;}
            CVPixelBufferRef image=nullptr;
            NSDictionary *attrs=@{(__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey:@{}};
            if(CVPixelBufferCreate(kCFAllocatorDefault,self.width,self.height,kCVPixelFormatType_32BGRA,(__bridge CFDictionaryRef)attrs,&image)!=kCVReturnSuccess){[self stop:@"synthetic frame allocation failed"];return;}
            CVPixelBufferLockBaseAddress(image,0);
            uint8_t *pixels=(uint8_t *)CVPixelBufferGetBaseAddress(image);size_t stride=CVPixelBufferGetBytesPerRow(image);
            for(int y=0;y<self.height;y++)for(int x=0;x<self.width;x++) {
                uint8_t *p=pixels+y*stride+x*4;
                bool moving=x>frame*4 && x<frame*4+128;
                p[0]=moving?255:(x/64%2?48:220);p[1]=moving?96:(y/64%2?32:200);p[2]=moving?16:128;p[3]=255;
            }
            CVPixelBufferUnlockBaseAddress(image,0);self->busy=true;
            OSStatus result=VTCompressionSessionEncodeFrame(self->encoder,image,CMTimeMake(frame,60),CMTimeMake(1,60),nullptr,(void *)(uintptr_t)epochUs(),nullptr);
            CVPixelBufferRelease(image);frame++;
            if(result!=noErr){self->busy=false;[self stop:@"synthetic encode failed"];}
        });dispatch_resume(self.testTimer);return;
    }
    Class descriptorClass=NSClassFromString(@"CGVirtualDisplayDescriptor");
    Class displayClass=NSClassFromString(@"CGVirtualDisplay");
    Class modeClass=NSClassFromString(@"CGVirtualDisplayMode");
    Class settingsClass=NSClassFromString(@"CGVirtualDisplaySettings");
    if (!descriptorClass || !displayClass || !modeClass || !settingsClass) {[self stop:@"virtual displays unavailable on this macOS"];return;}
    if (!CGPreflightScreenCaptureAccess()) {
        CGRequestScreenCaptureAccess();
        [self stop:@"Screen Recording permission required; enable Kayiver in System Settings, then retry"];return;
    }
    @try {
        KVDisplayDescriptor *d=[[descriptorClass alloc] init];
        d.queue=dispatch_get_main_queue(); d.name=@"Kayiver Mac Extended Display";
        d.maxPixelsWide=self.width; d.maxPixelsHigh=self.height;
        d.vendorID=0x4b56; d.productID=1; d.serialNum=1;
        if([d respondsToSelector:@selector(setSerialNumber:)]) d.serialNumber=1;
        d.sizeInMillimeters=CGSizeMake(self.width*25.4/96.0,self.height*25.4/96.0);
        d.redPrimary=CGPointMake(.64,.33); d.greenPrimary=CGPointMake(.30,.60);
        d.bluePrimary=CGPointMake(.15,.06); d.whitePoint=CGPointMake(.3127,.3290);
        self.display=[[displayClass alloc] initWithDescriptor:d];
        KVDisplaySettings *settings=[[settingsClass alloc] init];
        settings.hiDPI=0;
        settings.modes=@[[[modeClass alloc] initWithWidth:self.width height:self.height refreshRate:60]];
        if (!self.display || ![self.display applySettings:settings]) {[self stop:@"virtual display creation failed"];return;}
    } @catch (NSException *error) {[self stop:@"unsupported virtual display interface"];return;}
    // Place the added screen beside the current desktop, without moving or
    // changing modes on any existing display.
    uint32_t ids[32],count=0; int right=0, top=0;
    if (CGGetOnlineDisplayList(32,ids,&count)==kCGErrorSuccess) {
        top=(int)CGDisplayBounds(CGMainDisplayID()).origin.y;
        for(uint32_t i=0;i<count;i++) if(ids[i]!=self.display.displayID)
            right=MAX(right,(int)CGRectGetMaxX(CGDisplayBounds(ids[i])));
    }
    CGDisplayConfigRef cfg=nullptr;
    if (CGBeginDisplayConfiguration(&cfg)!=kCGErrorSuccess) {[self stop:@"display placement failed"];return;}
    CGError error=CGConfigureDisplayOrigin(cfg,self.display.displayID,right,top);
    if(error==kCGErrorSuccess) error=CGCompleteDisplayConfiguration(cfg,kCGConfigureForSession);
    else CGCancelDisplayConfiguration(cfg);
    if(error!=kCGErrorSuccess) {[self stop:@"display placement rejected"];return;}

    if(![self setupEncoder]) {[self stop:@"hardware low latency encoder unavailable"];return;}
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,300*NSEC_PER_MSEC),dispatch_get_main_queue(),^{
        [SCShareableContent getShareableContentExcludingDesktopWindows:NO onScreenWindowsOnly:NO completionHandler:^(SCShareableContent *content,NSError *err) {
            dispatch_async(dispatch_get_main_queue(), ^{
                if(self.stopping) return;
                SCDisplay *selected=nil;
                for(SCDisplay *d in content.displays) if(d.displayID==self.display.displayID) selected=d;
                if(!selected) {[self stop:@"new virtual display is not capturable"];return;}
                SCContentFilter *filter=[[SCContentFilter alloc] initWithDisplay:selected excludingWindows:@[]];
                SCStreamConfiguration *config=[SCStreamConfiguration new];
                config.width=self.width; config.height=self.height; config.queueDepth=3;
                config.minimumFrameInterval=CMTimeMake(1,60); config.showsCursor=YES;
                if([config respondsToSelector:@selector(setCapturesAudio:)])config.capturesAudio=NO;
                config.pixelFormat=kCVPixelFormatType_32BGRA;
                config.colorSpaceName=kCGColorSpaceSRGB;
                self.stream=[[SCStream alloc] initWithFilter:filter configuration:config delegate:self];
                NSError *outputError=nil;
                self.captureQueue=dispatch_queue_create("app.kayiver.display.capture",DISPATCH_QUEUE_SERIAL);
                if(![self.stream addStreamOutput:self type:SCStreamOutputTypeScreen sampleHandlerQueue:self.captureQueue error:&outputError]) {
                    [self stop:@"screen output setup failed"];return;
                }
                // READY precedes all binary frames on stdout.
                CGRect r=CGDisplayBounds(self.display.displayID);
                self.initialBounds=r;
                printf("{\"ready\":true,\"x\":%d,\"y\":%d,\"w\":%d,\"h\":%d,\"display_id\":%u,\"hardware\":true}\n",(int)r.origin.x,(int)r.origin.y,self.width,self.height,self.display.displayID);
                fflush(stdout);
                [self.stream startCaptureWithCompletionHandler:^(NSError *e) {
                    if(e) dispatch_async(dispatch_get_main_queue(), ^{[self stop:e.localizedDescription];});
                }];
            });
        }];
    });
}
- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    dispatch_async(dispatch_get_main_queue(), ^{[self stop:error.localizedDescription];});
}
- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sample ofType:(SCStreamOutputType)type {
    if(type!=SCStreamOutputTypeScreen || self.stopping || !CMSampleBufferIsValid(sample)) return;
    CFArrayRef attachments=CMSampleBufferGetSampleAttachmentsArray(sample,false);
    if(!attachments || CFArrayGetCount(attachments)==0) return;
    NSDictionary *info=(__bridge NSDictionary *)CFArrayGetValueAtIndex(attachments,0);
    if([info[SCStreamFrameInfoStatus] integerValue]!=SCFrameStatusComplete) return;
    uint64_t now=epochUs();
    if(now-lastFrame<(uint64_t)(900000/fps.load()) || busy.exchange(true)) return;
    lastFrame=now;
    CVImageBufferRef image=CMSampleBufferGetImageBuffer(sample);
    CMTime pts=CMSampleBufferGetPresentationTimeStamp(sample);
    double age=CMTimeGetSeconds(CMTimeSubtract(CMClockGetTime(CMClockGetHostTimeClock()),pts));
    uint64_t captured=now-(age>=0 && age<2 ? (uint64_t)(age*1000000):0);
    OSStatus result=VTCompressionSessionEncodeFrame(encoder,image,pts,CMTimeMake(1,fps.load()),nullptr,(void *)(uintptr_t)captured,nullptr);
    if(result!=noErr) {busy=false;dispatch_async(dispatch_get_main_queue(), ^{[self stop:@"encoding stopped"];});}
}
@end

extern "C" bool kayiver_display_available() {
    NSOperatingSystemVersion minimum={12,3,0};
    if([[NSProcessInfo processInfo] isOperatingSystemAtLeastVersion:minimum]) {
        return NSClassFromString(@"CGVirtualDisplay") && NSClassFromString(@"CGVirtualDisplayDescriptor") && NSClassFromString(@"SCStream");
    }
    return false;
}

extern "C" int kayiver_display_native(int argc,const char **argv) {
    @autoreleasepool {
        if(argc==1 && strcmp(argv[0],"probe")==0) {
            bool available=kayiver_display_available();
            printf("{\"supported\":%s,\"screen_recording\":%s,\"experimental\":true}\n",available?"true":"false",CGPreflightScreenCaptureAccess()?"true":"false");
            return 0;
        }
        if(argc!=5 || (strcmp(argv[0],"source")!=0 && strcmp(argv[0],"test-source")!=0)) return 2;
        if(!kayiver_display_available()){fprintf(stderr,"display source: macOS 12.3+ and virtual display support required\n");return 3;}
        int width=atoi(argv[1]),height=atoi(argv[2]),fps=atoi(argv[3]),bitrate=atoi(argv[4]);
        if(width<640 || height<480 || width>2560 || height>2560 || width%2 || height%2 || width*height>2560*1440 || fps!=60 || bitrate<1000000 || bitrate>45000000) return 2;
        signal(SIGPIPE,SIG_IGN); signal(SIGTERM,SIG_IGN);
        [NSApplication sharedApplication]; [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
        source=[KVSource new]; source.width=width; source.height=height; source.bitrate=bitrate; source->fps=60; source->busy=false; source->lastFrame=0;
        source.testMode=strcmp(argv[0],"test-source")==0;
        if(!source.testMode&&CGPreflightListenEventAccess()) {
            escapeTap=CGEventTapCreate(kCGSessionEventTap,kCGHeadInsertEventTap,kCGEventTapOptionListenOnly,CGEventMaskBit(kCGEventKeyDown),escapeEvent,nullptr);
            if(escapeTap) {
                CFRunLoopSourceRef loop=CFMachPortCreateRunLoopSource(kCFAllocatorDefault,escapeTap,0);
                CFRunLoopAddSource(CFRunLoopGetMain(),loop,kCFRunLoopCommonModes);CFRelease(loop);
            }
        }
        source.signalSource=dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL,SIGTERM,0,dispatch_get_main_queue());
        dispatch_source_set_event_handler(source.signalSource, ^{[source stop:@"session stopped"];}); dispatch_resume(source.signalSource);
        source.inputSource=dispatch_source_create(DISPATCH_SOURCE_TYPE_READ,STDIN_FILENO,0,dispatch_get_main_queue());
        dispatch_source_set_event_handler(source.inputSource, ^{
            uint8_t bytes[32]; ssize_t n=read(STDIN_FILENO,bytes,sizeof(bytes));
            if(n<=0) {[source stop:@"parent closed session"];return;}
            for(ssize_t i=0;i<n;i++) {
                if(bytes[i]==0) {[source stop:@"session stopped"];return;}
                if(bytes[i]==1) source->busy=false;
                if(bytes[i]==30 || bytes[i]==60) {
                    source->fps=bytes[i];
                    int rate=bytes[i]==30 ? source.bitrate*3/4:source.bitrate;
                    VTSessionSetProperty(source->encoder,kVTCompressionPropertyKey_AverageBitRate,(__bridge CFNumberRef)@(rate));
                }
            }
        }); dispatch_resume(source.inputSource);
        pid_t parent=getppid();
        source.parentWatch=dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER,0,0,dispatch_get_main_queue());
        dispatch_source_set_timer(source.parentWatch,dispatch_time(DISPATCH_TIME_NOW,NSEC_PER_SEC),NSEC_PER_SEC,0);
        dispatch_source_set_event_handler(source.parentWatch, ^{
            if(getppid()!=parent) [source stop:@"parent exited"];
            if(!source.testMode) {
                if(source.display&&!CGRectIsEmpty(source.initialBounds)&&!CGRectEqualToRect(CGDisplayBounds(source.display.displayID),source.initialBounds)) {
                    [source stop:@"virtual display geometry changed; restart the display session"];
                }
                CFDictionaryRef session=CGSessionCopyCurrentDictionary();
                if(session) {
                    if(CFDictionaryGetValue(session,CFSTR("CGSSessionScreenIsLocked"))==kCFBooleanTrue) [source stop:@"desktop locked"];
                    CFRelease(session);
                }
            }
        }); dispatch_resume(source.parentWatch);
        [[NSDistributedNotificationCenter defaultCenter] addObserverForName:@"com.apple.screenIsLocked" object:nil queue:[NSOperationQueue mainQueue] usingBlock:^(NSNotification *n){if(!source.testMode)[source stop:@"desktop locked"]; }];
        [[[NSWorkspace sharedWorkspace] notificationCenter] addObserverForName:NSWorkspaceSessionDidResignActiveNotification object:nil queue:[NSOperationQueue mainQueue] usingBlock:^(NSNotification *n){[source stop:@"desktop locked"]; }];
        [[[NSWorkspace sharedWorkspace] notificationCenter] addObserverForName:NSWorkspaceWillSleepNotification object:nil queue:[NSOperationQueue mainQueue] usingBlock:^(NSNotification *n){[source stop:@"computer sleeping"]; }];
        dispatch_async(dispatch_get_main_queue(), ^{[source start];});
        [NSApp run];
        if(escapeTap){CFMachPortInvalidate(escapeTap);CFRelease(escapeTap);escapeTap=nullptr;}
        int result=source.exitCode;source=nil;
        return result;
    }
}
