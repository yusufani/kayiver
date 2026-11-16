#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <d3d11.h>
#include <dxgi.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mftransform.h>
#include <mferror.h>
#include <wmcodecdsp.h>
#include <codecapi.h>
#include <wtsapi32.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <io.h>

// Media Foundation is an optional Windows component (notably on N editions).
// Resolve its entry points only after opt-in, so a disabled feature cannot
// prevent the ordinary input-sharing executable from starting.
static decltype(&MFStartup) mediaStartup=nullptr;
static decltype(&MFCreateDXGIDeviceManager) mediaCreateManager=nullptr;
static decltype(&MFCreateMediaType) mediaCreateType=nullptr;
static decltype(&MFCreateMemoryBuffer) mediaCreateBuffer=nullptr;
static decltype(&MFCreateSample) mediaCreateSample=nullptr;
static bool loadMediaFoundation() {
    HMODULE module=LoadLibraryW(L"mfplat.dll");if(!module)return false;
    mediaStartup=(decltype(mediaStartup))GetProcAddress(module,"MFStartup");
    mediaCreateManager=(decltype(mediaCreateManager))GetProcAddress(module,"MFCreateDXGIDeviceManager");
    mediaCreateType=(decltype(mediaCreateType))GetProcAddress(module,"MFCreateMediaType");
    mediaCreateBuffer=(decltype(mediaCreateBuffer))GetProcAddress(module,"MFCreateMemoryBuffer");
    mediaCreateSample=(decltype(mediaCreateSample))GetProcAddress(module,"MFCreateSample");
    return mediaStartup&&mediaCreateManager&&mediaCreateType&&mediaCreateBuffer&&mediaCreateSample;
}
#define MFStartup mediaStartup
#define MFCreateDXGIDeviceManager mediaCreateManager
#define MFCreateMediaType mediaCreateType
#define MFCreateMemoryBuffer mediaCreateBuffer
#define MFCreateSample mediaCreateSample

template<class T> struct Ptr {
    T *p=nullptr;
    ~Ptr(){if(p) p->Release();}
    T **put(){if(p){p->Release();p=nullptr;}return &p;}
    T *operator->(){return p;}
};
static HWND windowHandle=nullptr;
static HANDLE parentHandle=nullptr;
static RECT targetRect;
static volatile LONG stopping=0;
static bool testMode=false;
static uint64_t read64(const uint8_t *p) {uint64_t n=0;for(int i=0;i<8;i++) n=(n<<8)|p[i];return n;}
static uint32_t read32(const uint8_t *p) {return ((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3];}
static void write64(uint8_t *p,uint64_t n){for(int i=7;i>=0;i--){p[i]=n&255;n>>=8;}}
static uint64_t epochUs(){FILETIME t;GetSystemTimePreciseAsFileTime(&t);ULARGE_INTEGER n;n.LowPart=t.dwLowDateTime;n.HighPart=t.dwHighDateTime;return n.QuadPart/10-11644473600000000ULL;}
static bool readAll(void *p,DWORD size) {
    uint8_t *b=(uint8_t *)p;
    while(size && !InterlockedCompareExchange(&stopping,0,0)) {
        DWORD n=0;if(!ReadFile(GetStdHandle(STD_INPUT_HANDLE),b,size,&n,nullptr)||!n) return false;
        b+=n;size-=n;
    }
    return size==0;
}
static bool sameRect(const RECT &a,const RECT &b){return a.left==b.left&&a.top==b.top&&a.right==b.right&&a.bottom==b.bottom;}
static void fail(HRESULT hr,const char *stage) {fprintf(stderr,"display receiver: %s (0x%08lx)\n",stage,(unsigned long)hr);}
#define CHECK(call) do {HRESULT result=(call);if(FAILED(result)){fail(result,#call);return false;}}while(0)

struct Renderer {
    Ptr<ID3D11Device> device;
    Ptr<ID3D11DeviceContext> context;
    Ptr<IDXGISwapChain> swap;
    Ptr<IMFDXGIDeviceManager> manager;
    Ptr<IMFTransform> decoder;
    Ptr<ID3D11VideoDevice> videoDevice;
    Ptr<ID3D11VideoContext> videoContext;
    Ptr<ID3D11VideoProcessorEnumerator> enumerator;
    Ptr<ID3D11VideoProcessor> processor;
    Ptr<ID3D11VideoProcessorOutputView> outputView;
    bool initialized=false;
    int width,height;

    bool init(int w,int h) {
        width=w;height=h;
        if(!loadMediaFoundation()){fail(E_FAIL,"Windows Media Feature Pack is unavailable");return false;}
        CHECK(CoInitializeEx(nullptr,COINIT_MULTITHREADED));
        CHECK(MFStartup(MF_VERSION,MFSTARTUP_LITE));
        Ptr<IDXGIFactory1> factory;CHECK(CreateDXGIFactory1(__uuidof(IDXGIFactory1),(void **)factory.put()));
        Ptr<IDXGIAdapter1> adapter;
        bool found=false;
        for(UINT i=0;factory->EnumAdapters1(i,adapter.put())!=DXGI_ERROR_NOT_FOUND;i++) {
            Ptr<IDXGIOutput> output;
            for(UINT j=0;adapter->EnumOutputs(j,output.put())!=DXGI_ERROR_NOT_FOUND;j++) {
                DXGI_OUTPUT_DESC desc={};CHECK(output->GetDesc(&desc));
                if(desc.AttachedToDesktop&&sameRect(desc.DesktopCoordinates,targetRect)){found=true;break;}
            }
            if(found)break;
        }
        if(!found){fail(E_FAIL,"target monitor disappeared");return false;}
        DXGI_ADAPTER_DESC1 desc={};CHECK(adapter->GetDesc1(&desc));
        if(desc.Flags&DXGI_ADAPTER_FLAG_SOFTWARE){fail(E_FAIL,"target adapter has no hardware video support");return false;}
        const D3D_FEATURE_LEVEL levels[]={D3D_FEATURE_LEVEL_11_1,D3D_FEATURE_LEVEL_11_0};
        CHECK(D3D11CreateDevice(adapter.p,D3D_DRIVER_TYPE_UNKNOWN,nullptr,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT|D3D11_CREATE_DEVICE_VIDEO_SUPPORT,levels,2,D3D11_SDK_VERSION,device.put(),nullptr,context.put()));
        CHECK(device->QueryInterface(__uuidof(ID3D11VideoDevice),(void **)videoDevice.put()));
        CHECK(context->QueryInterface(__uuidof(ID3D11VideoContext),(void **)videoContext.put()));
        DXGI_SWAP_CHAIN_DESC sd={};sd.BufferDesc.Width=w;sd.BufferDesc.Height=h;sd.BufferDesc.Format=DXGI_FORMAT_B8G8R8A8_UNORM;
        sd.BufferDesc.RefreshRate.Numerator=60;sd.BufferDesc.RefreshRate.Denominator=1;
        sd.SampleDesc.Count=1;sd.BufferUsage=DXGI_USAGE_RENDER_TARGET_OUTPUT;sd.BufferCount=2;sd.OutputWindow=windowHandle;
        sd.Windowed=TRUE;sd.SwapEffect=DXGI_SWAP_EFFECT_DISCARD;
        CHECK(factory->CreateSwapChain(device.p,&sd,swap.put()));
        factory->MakeWindowAssociation(windowHandle,DXGI_MWA_NO_ALT_ENTER);
        Ptr<IDXGIDevice1> dxgiDevice;CHECK(device->QueryInterface(__uuidof(IDXGIDevice1),(void **)dxgiDevice.put()));
        CHECK(dxgiDevice->SetMaximumFrameLatency(1));
        UINT token=0;CHECK(MFCreateDXGIDeviceManager(&token,manager.put()));CHECK(manager->ResetDevice(device.p,token));
        CHECK(CoCreateInstance(CLSID_CMSH264DecoderMFT,nullptr,CLSCTX_INPROC_SERVER,__uuidof(IMFTransform),(void **)decoder.put()));
        CHECK(decoder->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER,(ULONG_PTR)manager.p));
        Ptr<ICodecAPI> codec;CHECK(decoder->QueryInterface(IID_ICodecAPI,(void **)codec.put()));
        // Microsoft's H.264 decoder requires VT_UI4 for this property.
        VARIANT low;VariantInit(&low);low.vt=VT_UI4;low.ulVal=1;CHECK(codec->SetValue(&CODECAPI_AVLowLatencyMode,&low));
        Ptr<IMFMediaType> input;CHECK(MFCreateMediaType(input.put()));
        CHECK(input->SetGUID(MF_MT_MAJOR_TYPE,MFMediaType_Video));CHECK(input->SetGUID(MF_MT_SUBTYPE,MFVideoFormat_H264));
        CHECK(MFSetAttributeSize(input.p,MF_MT_FRAME_SIZE,w,h));CHECK(MFSetAttributeRatio(input.p,MF_MT_FRAME_RATE,60,1));
        CHECK(input->SetUINT32(MF_MT_INTERLACE_MODE,MFVideoInterlace_Progressive));
        CHECK(decoder->SetInputType(0,input.p,0));
        if(!setOutput())return false;
        D3D11_VIDEO_PROCESSOR_CONTENT_DESC content={};content.InputFrameFormat=D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
        content.InputWidth=w;content.InputHeight=h;content.OutputWidth=w;content.OutputHeight=h;
        content.InputFrameRate.Numerator=60;content.InputFrameRate.Denominator=1;
        content.OutputFrameRate=content.InputFrameRate;content.Usage=D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;
        CHECK(videoDevice->CreateVideoProcessorEnumerator(&content,enumerator.put()));
        CHECK(videoDevice->CreateVideoProcessor(enumerator.p,0,processor.put()));
        // H.264 GPU surfaces may include padding (1080 video in 1088 rows).
        // Crop to the negotiated image before the 1:1 blit; never stretch
        // decoder padding into the desktop or soften text vertically.
        RECT imageRect={0,0,w,h};
        videoContext->VideoProcessorSetStreamSourceRect(processor.p,0,TRUE,&imageRect);
        videoContext->VideoProcessorSetStreamDestRect(processor.p,0,TRUE,&imageRect);
        videoContext->VideoProcessorSetOutputTargetRect(processor.p,TRUE,&imageRect);
        Ptr<ID3D11Texture2D> back;CHECK(swap->GetBuffer(0,__uuidof(ID3D11Texture2D),(void **)back.put()));
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC view={};view.ViewDimension=D3D11_VPOV_DIMENSION_TEXTURE2D;
        CHECK(videoDevice->CreateVideoProcessorOutputView(back.p,enumerator.p,&view,outputView.put()));
        D3D11_VIDEO_PROCESSOR_COLOR_SPACE cs={};cs.YCbCr_Matrix=1;cs.Nominal_Range=1;
        videoContext->VideoProcessorSetStreamColorSpace(processor.p,0,&cs);
        cs.RGB_Range=0;videoContext->VideoProcessorSetOutputColorSpace(processor.p,&cs);
        CHECK(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,0));CHECK(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM,0));
        initialized=true;return true;
    }
    bool setOutput() {
        Ptr<IMFMediaType> output;
        for(DWORD i=0;;i++) {
            HRESULT hr=decoder->GetOutputAvailableType(0,i,output.put());
            if(FAILED(hr)){fail(hr,"NV12 output unavailable");return false;}
            GUID format={};CHECK(output->GetGUID(MF_MT_SUBTYPE,&format));
            if(format==MFVideoFormat_NV12){CHECK(decoder->SetOutputType(0,output.p,0));return true;}
        }
    }
    bool frame(const uint8_t *bytes,DWORD size,uint64_t id,uint64_t captured) {
        Ptr<IMFMediaBuffer> buffer;CHECK(MFCreateMemoryBuffer(size,buffer.put()));
        BYTE *p=nullptr;CHECK(buffer->Lock(&p,nullptr,nullptr));memcpy(p,bytes,size);CHECK(buffer->Unlock());CHECK(buffer->SetCurrentLength(size));
        Ptr<IMFSample> sample;CHECK(MFCreateSample(sample.put()));CHECK(sample->AddBuffer(buffer.p));
        CHECK(sample->SetSampleTime((LONGLONG)(id*10000000/60)));CHECK(sample->SetSampleDuration(10000000/60));
        CHECK(decoder->ProcessInput(0,sample.p,0));
        for(int attempts=0;attempts<4;attempts++) {
            MFT_OUTPUT_DATA_BUFFER out={};DWORD flags=0;
            HRESULT hr=decoder->ProcessOutput(0,1,&out,&flags);
            if(out.pEvents)out.pEvents->Release();
            if(hr==MF_E_TRANSFORM_STREAM_CHANGE){if(!setOutput())return false;continue;}
            if(hr==MF_E_TRANSFORM_NEED_MORE_INPUT)return true;
            if(FAILED(hr)){if(out.pSample)out.pSample->Release();fail(hr,"H.264 decoding");return false;}
            Ptr<IMFSample> decoded;decoded.p=out.pSample;
            Ptr<IMFMediaBuffer> image;CHECK(decoded->GetBufferByIndex(0,image.put()));
            Ptr<IMFDXGIBuffer> dxgi;CHECK(image->QueryInterface(__uuidof(IMFDXGIBuffer),(void **)dxgi.put()));
            // A system-memory output is intentionally rejected: it would
            // silently turn the receiver into a CPU decoder.
            Ptr<ID3D11Texture2D> texture;CHECK(dxgi->GetResource(__uuidof(ID3D11Texture2D),(void **)texture.put()));
            UINT slice=0;CHECK(dxgi->GetSubresourceIndex(&slice));
            D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC iv={};iv.ViewDimension=D3D11_VPIV_DIMENSION_TEXTURE2D;iv.Texture2D.ArraySlice=slice;
            Ptr<ID3D11VideoProcessorInputView> inputView;CHECK(videoDevice->CreateVideoProcessorInputView(texture.p,enumerator.p,&iv,inputView.put()));
            D3D11_VIDEO_PROCESSOR_STREAM stream={};stream.Enable=TRUE;stream.pInputSurface=inputView.p;
            CHECK(videoContext->VideoProcessorBlt(processor.p,outputView.p,0,1,&stream));
            CHECK(swap->Present(1,0));
            if(!testMode)PostMessageW(windowHandle,WM_APP+1,0,0);
            LONGLONG timestamp=0;CHECK(decoded->GetSampleTime(&timestamp));
            uint64_t presentedId=(uint64_t)((timestamp*60+5000000)/10000000);
            uint8_t ack[16];write64(ack,presentedId);write64(ack+8,epochUs());
            if(fwrite(ack,1,sizeof(ack),stdout)!=sizeof(ack)||fflush(stdout)!=0)return false;
        }
        return true;
    }
    ~Renderer(){if(initialized){decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM,0);decoder->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH,0);}}
};

static DWORD WINAPI consume(LPVOID param) {
    Renderer r;int *size=(int *)param;
    if(!r.init(size[0],size[1])){PostMessageW(windowHandle,WM_CLOSE,0,0);return 1;}
    puts("{\"ready\":true,\"hardware_device\":true}");fflush(stdout);
    for(;;) {
        uint8_t head[20];if(!readAll(head,sizeof(head)))break;
        uint32_t n=read32(head);if(n==0||n>8*1024*1024)break;
        uint8_t *data=(uint8_t *)malloc(n);if(!data)break;
        bool ok=readAll(data,n)&&r.frame(data,n,read64(head+12),read64(head+4));free(data);
        if(!ok){PostMessageW(windowHandle,WM_CLOSE,0,0);return 1;}
    }
    PostMessageW(windowHandle,WM_CLOSE,0,0);return 0;
}
static LRESULT CALLBACK wndproc(HWND hwnd,UINT msg,WPARAM wp,LPARAM lp) {
    switch(msg) {
        case WM_APP+1: if(!IsWindowVisible(hwnd)){ShowWindow(hwnd,SW_SHOW);SetForegroundWindow(hwnd);SetFocus(hwnd);}return 0;
        case WM_COMMAND: if(LOWORD(wp)==100)PostMessageW(hwnd,WM_CLOSE,0,0);return 0;
        case WM_KEYDOWN: if(wp==VK_ESCAPE)PostMessageW(hwnd,WM_CLOSE,0,0);return 0;
        case WM_TIMER: if(parentHandle&&WaitForSingleObject(parentHandle,0)==WAIT_OBJECT_0)PostMessageW(hwnd,WM_CLOSE,0,0);return 0;
        case WM_DISPLAYCHANGE: PostMessageW(hwnd,WM_CLOSE,0,0);return 0;
        case WM_WTSSESSION_CHANGE: if(wp==WTS_SESSION_LOCK||wp==WTS_SESSION_LOGOFF)PostMessageW(hwnd,WM_CLOSE,0,0);return 0;
        case WM_POWERBROADCAST: if(wp==PBT_APMSUSPEND)PostMessageW(hwnd,WM_CLOSE,0,0);return TRUE;
        case WM_SETCURSOR: if((HWND)wp==hwnd&&LOWORD(lp)==HTCLIENT){SetCursor(nullptr);return TRUE;}break;
        case WM_CLOSE: InterlockedExchange(&stopping,1);DestroyWindow(hwnd);return 0;
        case WM_DESTROY: PostQuitMessage(0);return 0;
        case WM_ERASEBKGND: return 1;
    }
    return DefWindowProcW(hwnd,msg,wp,lp);
}

extern "C" bool kayiver_display_available() {
    wchar_t path[MAX_PATH];UINT n=GetSystemDirectoryW(path,MAX_PATH);
    if(!n||n+12>=MAX_PATH)return false;
    wcscat(path,L"\\mfplat.dll");return GetFileAttributesW(path)!=INVALID_FILE_ATTRIBUTES;
}

extern "C" int kayiver_display_native(int argc,const char **argv) {
    if(argc!=6||(strcmp(argv[0],"sink")!=0&&strcmp(argv[0],"test-sink")!=0))return 2;
    testMode=strcmp(argv[0],"test-sink")==0;
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    int x=atoi(argv[1]),y=atoi(argv[2]),w=atoi(argv[3]),h=atoi(argv[4]);
    if(w<640||h<480||w>2560||h>2560||w%2||h%2||w*h>2560*1440)return 2;
    targetRect={x,y,x+w,y+h};
    HMONITOR monitor=MonitorFromRect(&targetRect,MONITOR_DEFAULTTONULL);MONITORINFO mi={sizeof(mi)};
    if(!monitor||!GetMonitorInfoW(monitor,&mi)||!sameRect(mi.rcMonitor,targetRect))return 3;
    parentHandle=OpenProcess(SYNCHRONIZE,FALSE,(DWORD)strtoul(argv[5],nullptr,10));
    if(!parentHandle)return 4;
    _setmode(_fileno(stdin),_O_BINARY);_setmode(_fileno(stdout),_O_BINARY);
    WNDCLASSW cls={};cls.lpfnWndProc=wndproc;cls.hInstance=GetModuleHandleW(nullptr);cls.lpszClassName=L"KayiverExtendedDisplay";cls.hbrBackground=(HBRUSH)GetStockObject(BLACK_BRUSH);
    if(!RegisterClassW(&cls))return 5;
    windowHandle=CreateWindowExW(WS_EX_TOPMOST,cls.lpszClassName,L"Kayiver — Mac extended display",WS_POPUP|WS_CLIPCHILDREN,x,y,w,h,nullptr,nullptr,cls.hInstance,nullptr);
    if(!windowHandle)return 6;
    CreateWindowW(L"BUTTON",L"Stop display (Esc)",WS_CHILD|WS_VISIBLE|BS_PUSHBUTTON,w-180,12,168,36,windowHandle,(HMENU)100,cls.hInstance,nullptr);
    WTSRegisterSessionNotification(windowHandle,NOTIFY_FOR_THIS_SESSION);SetTimer(windowHandle,1,1000,nullptr);
    int size[]={w,h};HANDLE thread=CreateThread(nullptr,0,consume,size,0,nullptr);
    if(!thread){DestroyWindow(windowHandle);CloseHandle(parentHandle);return 7;}
    MSG msg;
    while(GetMessageW(&msg,nullptr,0,0)>0){
        if(msg.message==WM_KEYDOWN&&msg.wParam==VK_ESCAPE){PostMessageW(windowHandle,WM_CLOSE,0,0);continue;}
        TranslateMessage(&msg);DispatchMessageW(&msg);
    }
    InterlockedExchange(&stopping,1);CancelSynchronousIo(thread);WaitForSingleObject(thread,2000);
    DWORD exitCode=0;GetExitCodeThread(thread,&exitCode);
    CloseHandle(thread);CloseHandle(parentHandle);return exitCode==STILL_ACTIVE?1:(int)exitCode;
}
