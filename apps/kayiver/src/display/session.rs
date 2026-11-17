use super::*;
use kayiver_core::{
    secure::{
        display_service_key, handshake_initiator, handshake_responder, SecureReader, SecureWriter,
    },
    wire::{read_frame, write_frame},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::Child as AsyncChild,
    sync::{mpsc, Semaphore},
    time::{timeout, Instant},
};

const VERSION: u16 = 1;
const REFRESH_VERSION: u16 = 2;

#[derive(Debug)]
struct PeerDisconnected;
impl std::fmt::Display for PeerDisconnected {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str("display connection interrupted") }
}

fn monitor_hz(rect: Rect) -> u32 {
    #[cfg(all(target_os="windows",display_native))]
    {
        unsafe extern "C" {fn kayiver_monitor_hz(x:i32,y:i32)->u32;}
        return unsafe {kayiver_monitor_hz(rect.x,rect.y)}.clamp(30,144);
    }
    #[allow(unreachable_code)]
    {let _=rect;60}
}
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const LIVENESS: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Request {
        version: u16,
        name: String,
    },
    Offer {
        version: u16,
        rect: Rect,
        monitor_id: String,
        #[serde(default,skip_serializing_if="Option::is_none")]
        refresh_hz: Option<u32>,
        time_us: u64,
    },
    Start {
        offset_us: i64,
        #[serde(default,skip_serializing_if="Option::is_none")]
        fps: Option<u32>,
    },
    SourceReady {
        virtual_rect: Rect,
    },
    Ping {
        sent: u64,
    },
    Pong {
        sent: u64,
    },
    Ack {
        frame: u64,
        presented_us: u64,
    },
    Stop,
}
fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
fn emit(s: &Status) {
    println!("{}", serde_json::to_string(s).unwrap());
    let _ = std::io::stdout().flush();
}
fn display_port(cfg: &Config) -> Result<u16> {
    cfg.port
        .checked_add(PORT_OFFSET)
        .context("display service port overflows")
}

pub async fn run(args: Vec<String>) -> Result<()> {
    anyhow::ensure!(args.len() >= 2, "screen-worker needs role and paired peer");
    let mut state = Status {
        phase: "starting".into(),
        role: args[0].clone(),
        peer: args[1].clone(),
        ..Default::default()
    };
    let cfg = Config::load_or_init()?;
    let peer = cfg.peer(&state.peer).context("peer is no longer paired")?;
    let key = display_service_key(&peer.psk_bytes()?);
    // Tokio stdin uses an uncancellable runtime blocking task. A worker that
    // finishes remotely must exit even while the editor keeps its pipe open.
    // This isolated process owns a detached reader instead; process exit ends
    // it without holding runtime shutdown or the editor's child reaper open.
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("display-stop".into())
        .spawn(move || {
            use std::io::Read;
            let _ = std::io::stdin().read(&mut [0u8; 1]);
            let _ = stop_tx.send(());
        })?;
    let parent = ParentWatch::new()?;
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let outcome = {
        let work = async {
            loop {
                if *cancel_rx.borrow() {break Ok(());}
                let outcome = match state.role.as_str() {
                "source" => source(&cfg, &args, &key, &mut state, cancel_rx.clone()).await,
                "receive" => loop {
                    match receive(&cfg, &args, &key, &mut state, cancel_rx.clone()).await {
                        Ok(true) if !*cancel_rx.borrow()=>continue,
                        Ok(_)=>break Ok(()),
                        Err(error)=>break Err(error),
                    }
                },
                _ => bail!("unknown display worker role"),
                };
                match outcome {
                    Err(error) if error.is::<PeerDisconnected>() && !*cancel_rx.borrow() => {
                        if state.role=="source" {break Err(error);}
                        let old=std::mem::take(&mut state);
                        state=Status { role:old.role,peer:old.peer,phase:"connecting".into(),..Default::default() };
                        emit(&state);
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    other => break other,
                }
            }
        };
        tokio::pin!(work);
        let stopped = async {
            tokio::select! {_=stop_rx=>{},_=parent.wait()=>{}}
        };
        tokio::select! {
            result=&mut work=>result,
            _=stopped=>{
                let _=cancel_tx.send(true);
                // Let a healthy media channel deliver Stop before dropping it.
                // A stuck startup/driver still has a bounded cleanup path.
                let _=timeout(Duration::from_secs(3),&mut work).await;
                Ok(())
            },
        }
    };
    // Route selection and the hardware presenter can still be settling on a
    // cold app launch. The manager bounds startup retries per explicit start.
    if state.role=="source" && !state.hardware_confirmed && !*cancel_rx.borrow() &&
        outcome.as_ref().is_err_and(|error|error.is::<tokio::time::error::Elapsed>()) {
        emit(&Status {phase:"retrying".into(),role:state.role,peer:state.peer,
            error:Some("The display did not become ready in time".into()),..Default::default()});
        return Ok(());
    }
    if state.role=="source" && outcome.as_ref().is_err_and(|error|error.is::<PeerDisconnected>()) && !*cancel_rx.borrow() {
        emit(&Status {phase:"reconnecting".into(),role:state.role,peer:state.peer,..Default::default()});
        return Ok(());
    }
    state.virtual_rect = None;
    state.phase = if outcome.is_err() { "error" } else { "off" }.into();
    state.error = outcome.as_ref().err().map(|e| format!("{e:#}"));
    emit(&state);
    outcome
}

pub(super) struct ParentWatch {
    #[cfg(target_os = "macos")]
    pid: libc::pid_t,
    #[cfg(target_os = "windows")]
    handle: windows::Win32::Foundation::HANDLE,
}
impl ParentWatch {
    pub(super) fn new() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                pid: unsafe { libc::getppid() },
            })
        }
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::{
                Foundation::CloseHandle,
                System::{
                    Diagnostics::ToolHelp::*,
                    Threading::{OpenProcess, PROCESS_SYNCHRONIZE},
                },
            };
            unsafe {
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?;
                let mut entry = PROCESSENTRY32W {
                    dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                    ..Default::default()
                };
                let mut parent = 0;
                if Process32FirstW(snapshot, &mut entry).is_ok() {
                    loop {
                        if entry.th32ProcessID == std::process::id() {
                            parent = entry.th32ParentProcessID;
                            break;
                        }
                        if Process32NextW(snapshot, &mut entry).is_err() {
                            break;
                        }
                    }
                }
                let _ = CloseHandle(snapshot);
                Ok(Self {
                    handle: OpenProcess(PROCESS_SYNCHRONIZE, false, parent)?,
                })
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Ok(Self {})
        }
    }
    pub(super) async fn wait(&self) {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            #[cfg(target_os = "macos")]
            if unsafe { libc::getppid() } != self.pid {
                return;
            }
            #[cfg(target_os = "windows")]
            if unsafe { windows::Win32::System::Threading::WaitForSingleObject(self.handle, 0) }
                == windows::Win32::Foundation::WAIT_OBJECT_0
            {
                return;
            }
        }
    }
}
#[cfg(target_os = "windows")]
impl Drop for ParentWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

struct Native {
    child: AsyncChild,
    source_rect: Option<Rect>,
    physical: Vec<Rect>,
}
impl Native {
    fn spawn(args: &[String]) -> Result<Self> {
        let physical = crate::platform::monitors();
        let mut cmd = tokio::process::Command::new(std::env::current_exe()?);
        cmd.arg("screen-native")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x08000000);
        Ok(Self {
            child: cmd.spawn()?,
            source_rect: None,
            physical,
        })
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        if let Some(rect) = self.source_rect {
            // The existing best-effort Accessibility adapter does not activate
            // apps and leaves non-standard/fullscreen windows to macOS.
            crate::platform::rescue_windows_off(rect);
            let cursor = crate::platform::cursor_pos();
            if kayiver_core::layout::point_in(rect, cursor.0, cursor.1) {
                if let Some(r) = self.physical.first() {
                    crate::platform::warp_cursor_settled(r.x + r.w / 2, r.y + r.h / 2);
                }
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            for _ in 0..40 {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = self.child.start_kill();
    }
}

/// A reader is owned by exactly one task. Select/timeout never cancel and
/// resume a partial Noise frame; aborting always discards the whole session.
struct Network {
    tx: mpsc::Sender<Outbound>,
    rx: mpsc::Receiver<Result<Vec<u8>>>,
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
    heartbeat: tokio::task::JoinHandle<()>,
}
struct Outbound {
    bytes: Vec<u8>,
    flushed: Option<tokio::sync::oneshot::Sender<bool>>,
}
impl Drop for Network {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
        self.heartbeat.abort();
    }
}
impl Network {
    fn new(mut reader: SecureReader, mut writer: SecureWriter) -> Self {
        let (tx, mut out) = mpsc::channel::<Outbound>(4);
        let (incoming, rx) = mpsc::channel(2);
        let errors = incoming.clone();
        let write = tokio::spawn(async move {
            while let Some(packet) = out.recv().await {
                match timeout(IO_TIMEOUT, writer.send_payload(&packet.bytes)).await {
                    Ok(Ok(())) => {
                        if let Some(done) = packet.flushed {
                            let _ = done.send(true);
                        }
                    }
                    _ => {
                        if let Some(done) = packet.flushed {
                            let _ = done.send(false);
                        }
                        let _=errors.send(Err(anyhow::anyhow!("display network stalled; session stopped to avoid delayed playback"))).await;
                        break;
                    }
                }
            }
        });
        let read = tokio::spawn(async move {
            loop {
                let result = match timeout(LIVENESS, reader.recv_payload()).await {
                    Ok(Ok(data)) => Ok(data.to_vec()),
                    Ok(Err(e)) => Err(e.into()),
                    Err(_) => Err(anyhow::anyhow!("display peer stopped responding")),
                };
                let failed = result.is_err();
                if incoming.send(result).await.is_err() || failed {
                    break;
                }
            }
        });
        let ping = tx.clone();
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let packet = serde_json::to_vec(&Control::Ping { sent: now_us() }).unwrap();
                if !matches!(
                    timeout(
                        IO_TIMEOUT,
                        ping.send(Outbound {
                            bytes: packet,
                            flushed: None
                        })
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        });
        Self {
            tx,
            rx,
            reader: read,
            writer: write,
            heartbeat,
        }
    }
    async fn control(&self, c: Control) -> Result<()> {
        send_control(&self.tx, c).await
    }
    async fn finish(&self) {
        let (done, received) = tokio::sync::oneshot::channel();
        if matches!(
            timeout(
                IO_TIMEOUT,
                self.tx.send(Outbound {
                    bytes: serde_json::to_vec(&Control::Stop).unwrap(),
                    flushed: Some(done)
                })
            )
            .await,
            Ok(Ok(()))
        ) {
            let _ = timeout(IO_TIMEOUT, received).await;
        }
    }
    async fn next(&mut self) -> Result<Vec<u8>> {
        self.rx.recv().await.context(PeerDisconnected)?.context(PeerDisconnected)
    }
}
async fn send_control(tx: &mpsc::Sender<Outbound>, c: Control) -> Result<()> {
    timeout(
        IO_TIMEOUT,
        tx.send(Outbound {
            bytes: serde_json::to_vec(&c)?,
            flushed: None,
        }),
    )
    .await
    .context("display control queue stalled")?
    .context("display writer stopped")?;
    Ok(())
}

fn source_addresses(cfg: &Config, args: &[String]) -> Result<Vec<std::net::SocketAddr>> {
    let active_route = crate::ui::peer_remote_addr(&args[1]);
    source_addresses_for_route(cfg,args,active_route.as_deref())
}
fn source_addresses_for_route(cfg:&Config,args:&[String],active_route:Option<&str>) -> Result<Vec<std::net::SocketAddr>> {
    let peer = cfg.peer(&args[1]).context("peer no longer paired")?;
    let mut candidates = Vec::new();
    // A live authenticated input connection is stronger evidence than a
    // saved Wi-Fi address or an earlier manager snapshot. Do not race its
    // refused media port against Wi-Fi while the Mac source is starting.
    // With input sharing off/unavailable, saved screen-only routes still work.
    if let Some(route) = active_route.and_then(|s|s.parse::<std::net::SocketAddr>().ok()) {
        let mut route=route;route.set_port(display_port(cfg)?);
        return Ok(vec![route]);
    }
    for address in args
        .get(3)
        .filter(|s| !s.starts_with("--"))
        .into_iter()
        .chain(peer.addr.iter())
        .chain(peer.last_good.iter())
        .chain(peer.addrs.iter())
    {
        if let Ok(mut address) = address.parse::<std::net::SocketAddr>() {
            address.set_port(display_port(cfg)?);
            if !candidates.contains(&address) {
                candidates.push(address);
            }
        }
    }
    anyhow::ensure!(
        !candidates.is_empty(),
        "no saved Mac address; connect Kayiver to this paired Mac once first"
    );
    Ok(candidates)
}

async fn accept_receiver(
    cfg: &Config,
    args: &[String],
    key: &[u8; 32],
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<Option<(SecureReader, SecureWriter)>> {
    let listener = TcpListener::bind(("0.0.0.0", display_port(cfg)?))
        .await
        .context("cannot open Mac display service")?;
    loop {
        let (mut socket, _) =
            tokio::select! {result=listener.accept()=>result?,_=cancel.changed()=>return Ok(None)};
        socket.set_nodelay(true)?;
        let auth = async {
            let identity = read_frame(&mut socket).await?;
            anyhow::ensure!(
                identity == args[1].as_bytes(),
                "unexpected display receiver identity"
            );
            Ok::<_, anyhow::Error>(handshake_responder(socket, key).await?)
        };
        if let Ok(Ok(session)) = timeout(Duration::from_secs(3), auth).await {
            return Ok(Some(session));
        }
    }
}

async fn source(
    cfg: &Config,
    args: &[String],
    key: &[u8; 32],
    state: &mut Status,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    anyhow::ensure!(
        cfg!(target_os = "macos") && cfg!(display_native),
        "source requires the native Mac build"
    );
    state.phase = "connecting".into();
    emit(state);
    // Windows dials the explicitly enabled Mac service. No new inbound port
    // or firewall exception is needed on the Windows receiver.
    let Some((mut reader, mut writer)) = accept_receiver(cfg, args, key, &mut cancel).await? else {
        return Ok(());
    };
    let preferences=Config::load_or_init()?.extra_display;
    let requested_fps=requested_fps(preferences.frame_rate);
    let requested_version=if requested_fps>60 {REFRESH_VERSION}else{VERSION};
    let started = now_us();
    writer
        .send_payload(&serde_json::to_vec(&Control::Request {
            version: requested_version,
            name: cfg.name.clone(),
        })?)
        .await?;
    let offer = timeout(Duration::from_secs(5), reader.recv_payload()).await?
        .context("Windows receiver disconnected during setup; high refresh rates require an updated receiver")?;
    let Control::Offer {
        version,
        rect,
        monitor_id,
        refresh_hz,
        time_us,
    } = serde_json::from_slice(offer)?
    else {
        bail!("invalid display offer")
    };
    anyhow::ensure!(version == requested_version, "incompatible display service version");
    validate_size(rect.w, rect.h)?;
    let ended = now_us();
    let midpoint = started + (ended - started) / 2;
    let offset = time_us as i64 - midpoint as i64;
    state.clock_uncertainty_ms = Some((ended - started) as f64 / 2000.0);
    let (preferred_fps,preferred_bitrate)=display_profile(rect,preferences);
    let preferred_fps=preferred_fps.min(refresh_hz.unwrap_or(60).clamp(30,144));
    writer
        .send_payload(&serde_json::to_vec(&Control::Start { offset_us: offset, fps:refresh_hz.map(|_|preferred_fps) })?)
        .await?;
    state.phase = "connecting".into();
    state.target = Some(rect);
    state.target_id = Some(monitor_id.clone());
    state.fps = preferred_fps;
    state.bitrate_mbps = preferred_bitrate;
    let base_bitrate_mbps = state.bitrate_mbps;
    emit(state);
    let mut network = Network::new(reader, writer);
    let synthetic = args.iter().any(|s| s == "--synthetic-test");
    let mut native = Native::spawn(&[
        if synthetic { "test-source" } else { "source" }.into(),
        rect.w.to_string(),
        rect.h.to_string(),
        state.fps.to_string(),
        (state.bitrate_mbps * 1_000_000).to_string(),
    ])?;
    let stdout = native.child.stdout.take().unwrap();
    let mut frames = BufReader::new(stdout);
    let mut ready = String::new();
    timeout(Duration::from_secs(15), frames.read_line(&mut ready)).await.context("Mac capture startup timed out")??;
    let ready: serde_json::Value = serde_json::from_str(&ready)
        .context("Mac capture could not start; check Screen Recording permission")?;
    anyhow::ensure!(ready["ready"] == true, "Mac capture did not become ready");
    let virtual_rect = Rect {
        x: ready["x"].as_i64().context("missing display x")? as i32,
        y: ready["y"].as_i64().context("missing display y")? as i32,
        w: rect.w,
        h: rect.h,
    };
    let source_id = if synthetic { None } else {
        crate::platform::identified_monitors().into_iter().find(|(_,r)| *r == virtual_rect).and_then(|(id,_)|id)
    };
    anyhow::ensure!(synthetic || source_id.is_some(), "virtual display identity unavailable");
    #[cfg(target_os="macos")]
    let virtual_rect=if synthetic {virtual_rect} else {
        restore_virtual(&state.peer,&monitor_id,&native.physical,virtual_rect).unwrap_or(virtual_rect)
    };
    if !synthetic {
        native.source_rect = Some(virtual_rect);
        state.virtual_rect = Some(virtual_rect);
    }
    emit(state);
    network
        .control(Control::SourceReady { virtual_rect })
        .await?;
    let native_input = Arc::new(tokio::sync::Mutex::new(native.child.stdin.take().unwrap()));
    let frame_ack = native_input.clone();
    let permits = Arc::new(Semaphore::new(2));
    let pending = Arc::new(Mutex::new(HashMap::<u64, u64>::new()));
    let count = Arc::new(AtomicU64::new(0));
    let p = permits.clone();
    let times = pending.clone();
    let counter = count.clone();
    let tx = network.tx.clone();
    // The producer is a future in this scope, so cancellation drops its pipe
    // before Native's graceful cleanup (no detached media tasks or orphans).
    let producer = async move {
        let mut id = 0u64;
        loop {
            let permit = timeout(Duration::from_secs(3), p.acquire())
                .await
                .context("receiver stopped presenting frames")??;
            // Release the native producer only after a network/presentation
            // slot exists, so even a single prefetched frame cannot age in
            // its pipe while both network slots are occupied.
            if id > 0 {
                frame_ack.lock().await.write_u8(1).await?;
            }
            let size = frames.read_u32().await? as usize;
            anyhow::ensure!(
                size > 0 && size <= MAX_VIDEO_FRAME,
                "invalid frame from Mac encoder"
            );
            let captured = frames.read_u64().await?;
            let mut data = vec![0; size];
            frames.read_exact(&mut data).await?;
            id += 1;
            times.lock().unwrap().insert(id, captured);
            permit.forget();
            let send = async {
                for (part, bytes) in data.chunks(CHUNK).enumerate() {
                    tx.send(Outbound {
                        bytes: chunk(id, captured, size, part * CHUNK, bytes),
                        flushed: None,
                    })
                    .await
                    .context("display writer stopped")?;
                }
                Ok::<_, anyhow::Error>(())
            };
            timeout(Duration::from_millis(200), send)
                .await
                .context("video backlog exceeded 200 ms")??;
            counter.store(id, Ordering::Relaxed);
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    };
    tokio::pin!(producer);
    let mut latencies = VecDeque::<f64>::new();
    let mut last_report = Instant::now();
    let mut last_adapt = Instant::now();
    let mut stats_tick = tokio::time::interval(Duration::from_secs(1));
    let mut finishing = false;
    loop {
        if finishing && pending.lock().unwrap().is_empty() {
            state.frames = count.load(Ordering::Relaxed);
            emit(state);
            network.finish().await;
            return Ok(());
        }
        tokio::select! {
            _=cancel.changed()=>{network.finish().await;return Ok(());}
            result=&mut producer, if !finishing=>{
                if timeout(Duration::from_secs(2),native.child.wait()).await??.success() {
                    if synthetic {finishing=true;continue;}
                    network.finish().await;return Ok(());
                }
                return result;
            }
            _=stats_tick.tick()=>{
                if let Some(id) = source_id.as_ref() {
                    let (_,current) = crate::platform::identified_monitors().into_iter()
                        .find(|(key,_)| key.as_ref()==Some(id)).context("virtual display disconnected")?;
                    anyhow::ensure!(current.w==rect.w && current.h==rect.h,"virtual display resolution changed");
                    if state.virtual_rect != Some(current) {
                        native.source_rect=Some(current);
                        state.virtual_rect=Some(current);
                        network.control(Control::SourceReady {virtual_rect:current}).await?;
                        emit(state);
                    }
                }
                if last_report.elapsed()>=Duration::from_secs(1) {state.frames=count.load(Ordering::Relaxed);emit(state);last_report=Instant::now();}
            }
            packet=network.next()=>{
                let control:Control=serde_json::from_slice(&packet?)?;
                match control {
                    Control::Ping {sent}=>network.control(Control::Pong {sent}).await?,
                    Control::Pong {..}=>{},
                    Control::Stop=>return Ok(()),
                    Control::Ack {frame,presented_us}=>{
                        let captured=pending.lock().unwrap().remove(&frame).context("unexpected or duplicate presentation acknowledgment")?;
                        permits.add_permits(1);
                        let elapsed=presented_us as i64-captured as i64-offset;
                        anyhow::ensure!((-100_000..=2_000_000).contains(&elapsed),"display timing became invalid; clock or playback stalled");
                        latencies.push_back(elapsed.max(0) as f64/1000.0);if latencies.len()>120{latencies.pop_front();}
                        let mut sorted:Vec<_>=latencies.iter().copied().collect();sorted.sort_by(f64::total_cmp);
                        state.latency_p95_ms=sorted.get((sorted.len()*95/100).min(sorted.len()-1)).copied();
                        state.phase="streaming".into();state.hardware_confirmed=true;
                        if preferences.frame_rate==kayiver_core::config::DisplayFrameRate::Adaptive && last_adapt.elapsed()>Duration::from_secs(3) {
                            let desired=if state.latency_p95_ms.unwrap_or(0.)>80. {30}else if state.latency_p95_ms.unwrap_or(0.)<45. {60}else{state.fps};
                            if desired!=state.fps {native_input.lock().await.write_u8(desired as u8).await?;state.fps=desired;state.bitrate_mbps=if desired==30{base_bitrate_mbps*3/4}else{base_bitrate_mbps};}
                            last_adapt=Instant::now();
                        }
                    }
                    _=>bail!("unexpected display control message"),
                }
            }
        }
    }
}

async fn receive(
    cfg: &Config,
    args: &[String],
    key: &[u8; 32],
    state: &mut Status,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<bool> {
    let synthetic = args.iter().any(|s| s == "--synthetic-test");
    anyhow::ensure!(
        cfg!(target_os = "windows") && cfg!(display_native),
        "receiver requires the native Windows build"
    );
    let id = args.get(2).context("missing monitor identity")?;
    let (_, rect) = crate::platform::identified_monitors()
        .into_iter()
        .find(|(key, _)| key.as_ref() == Some(id))
        .context("selected monitor disconnected")?;
    validate_size(rect.w, rect.h)?;
    anyhow::ensure!(
        state.target.is_none_or(|old| old == rect),
        "monitor changed; accept a new display session locally"
    );
    anyhow::ensure!(
        synthetic || cfg.shared_monitor.local_rect != Some(rect),
        "shared physical panel is not an eligible target"
    );
    state.phase = "listening".into();
    state.virtual_rect = None;
    state.frames = 0;
    state.hardware_confirmed = false;
    state.target = Some(rect);
    state.target_id = Some(id.clone());
    state.fps = 60;
    emit(state);
    // The selected Mac must also explicitly start its service. Windows is
    // always the TCP/Noise initiator; media direction remains Mac -> Windows.
    let (mut reader, mut writer,version) = loop {
        // A KVM connection may become ready after the receiver was launched.
        // Refresh its optional route hint while waiting, rather than pinning
        // an old Wi-Fi address for the entire accepted application session.
        let addresses = source_addresses(cfg, args)?;
        let mut authenticated = None;
        for address in &addresses {
            let dial = async {
                let mut socket = TcpStream::connect(address).await?;
                socket.set_nodelay(true)?;
                write_frame(&mut socket, cfg.name.as_bytes()).await?;
                let (mut reader, writer) = handshake_initiator(socket, key).await?;
                let request: Control = serde_json::from_slice(reader.recv_payload().await?)?;
                let Control::Request{version,name}=request else {bail!("invalid Mac display request")};
                anyhow::ensure!(matches!(version,VERSION|REFRESH_VERSION) && name==args[1],"incompatible Mac display request");
                Ok::<_, anyhow::Error>((reader, writer,version))
            };
            let attempt = tokio::select! {result=timeout(Duration::from_secs(2),dial)=>result,_=cancel.changed()=>return Ok(false)};
            if let Ok(Ok(session)) = attempt {
                eprintln!("display transport: connected through {}",address);
                authenticated = Some(session);
                break;
            }
        }
        if let Some(session) = authenticated {
            break session;
        }
        let current = crate::platform::identified_monitors();
        anyhow::ensure!(
            current
                .iter()
                .any(|(key, r)| key.as_ref() == Some(id) && *r == rect),
            "target monitor changed while waiting"
        );
        tokio::select! {_=tokio::time::sleep(Duration::from_millis(500))=>{},_=cancel.changed()=>return Ok(false)};
    };
    let current = crate::platform::identified_monitors();
    anyhow::ensure!(
        current
            .iter()
            .any(|(key, r)| key.as_ref() == Some(id) && *r == rect),
        "target monitor changed while waiting"
    );
    writer
        .send_payload(&serde_json::to_vec(&Control::Offer {
            version,
            refresh_hz:(version==REFRESH_VERSION).then(||monitor_hz(rect)),
            rect,
            monitor_id: id.clone(),
            time_us: now_us(),
        })?)
        .await?;
    let start: Control =
        serde_json::from_slice(timeout(Duration::from_secs(5), reader.recv_payload()).await??)?;
    let Control::Start { fps,.. } = start else {
        bail!("invalid display start")
    };
    state.fps=fps.unwrap_or(60);
    anyhow::ensure!((30..=144).contains(&state.fps),"invalid negotiated frame rate");
    state.phase = "connecting".into();
    emit(state);
    let mut native = Native::spawn(&[
        if synthetic { "test-sink" } else { "sink" }.into(),
        rect.x.to_string(),
        rect.y.to_string(),
        rect.w.to_string(),
        rect.h.to_string(),
        std::process::id().to_string(),
        format!("fps={}",state.fps),
    ])?;
    let mut acknowledgments = BufReader::new(native.child.stdout.take().unwrap());
    let mut ready = String::new();
    timeout(
        Duration::from_secs(10),
        acknowledgments.read_line(&mut ready),
    )
    .await??;
    anyhow::ensure!(
        serde_json::from_str::<serde_json::Value>(&ready)
            .ok()
            .is_some_and(|v| v["ready"] == true),
        "Windows hardware decoder setup failed on the selected monitor"
    );
    let mut input = native.child.stdin.take().unwrap();
    let mut network = Network::new(reader, writer);
    let ack_tx = network.tx.clone();
    let count = Arc::new(AtomicU64::new(0));
    let counter = count.clone();
    let acks = async move {
        loop {
            let frame = acknowledgments.read_u64().await?;
            let presented_us = acknowledgments.read_u64().await?;
            send_control(
                &ack_tx,
                Control::Ack {
                    frame,
                    presented_us,
                },
            )
            .await?;
            counter.store(frame, Ordering::Relaxed);
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    };
    tokio::pin!(acks);
    let mut assembly = Assembly::default();
    let mut report = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _=cancel.changed()=>{network.finish().await;return Ok(false);}
            _result=&mut acks=>{
                let exited=timeout(Duration::from_secs(2),native.child.wait()).await??;
                anyhow::ensure!(exited.success(),"Windows hardware decoder stopped unexpectedly");
                network.finish().await;return Ok(false);
            }
            _=report.tick()=>{
                if let Some(exit)=native.child.try_wait()? {
                    anyhow::ensure!(exit.success(),"Windows hardware decoder stopped unexpectedly");
                    network.finish().await;return Ok(false);
                }
                let current=crate::platform::identified_monitors();
                anyhow::ensure!(current.iter().any(|(key,r)|key.as_ref()==Some(id)&&*r==rect),"target monitor disconnected or changed");
                state.frames=count.load(Ordering::Relaxed);
                if state.frames>0 {state.phase="streaming".into();state.hardware_confirmed=true;}
                emit(state);
            }
            packet=network.next()=>{
                let packet=packet?;
                if packet.first()==Some(&0x10) {
                    if let Some((id,captured,data))=assembly.push(&packet)? {
                        let write=async {
                            input.write_u32(data.len() as u32).await?;input.write_u64(captured).await?;input.write_u64(id).await?;
                            input.write_all(&data).await?;input.flush().await?;Ok::<_,anyhow::Error>(())
                        };
                        timeout(Duration::from_millis(200),write).await.context("Windows decoder stalled")??;
                    }
                } else {
                    match serde_json::from_slice::<Control>(&packet)? {
                        Control::SourceReady {virtual_rect}=>{
                            validate_size(virtual_rect.w,virtual_rect.h)?;
                            anyhow::ensure!(virtual_rect.w==rect.w&&virtual_rect.h==rect.h,"source resolution changed");
                            state.virtual_rect=Some(virtual_rect);emit(state);
                        }
                        Control::Ping {sent}=>network.control(Control::Pong {sent}).await?,
                        Control::Pong {..}=>{},
                        Control::Stop=>{state.frames=count.load(Ordering::Relaxed);emit(state);return Ok(true);}
                        _=>bail!("unexpected display packet"),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_display_control_stays_compatible() {
        let start=Control::Start {offset_us:42,fps:None};
        let json=serde_json::to_value(start).unwrap();
        assert!(json.get("fps").is_none());
        let old=serde_json::json!({"type":"offer","version":1,"rect":{"x":0,"y":0,"w":1920,"h":1080},"monitor_id":"panel","time_us":5});
        assert!(matches!(serde_json::from_value::<Control>(old),Ok(Control::Offer{refresh_hz:None,..})));
        let new=Control::Start{offset_us:42,fps:Some(120)};
        assert_eq!(serde_json::to_value(new).unwrap()["fps"],120);
    }

    #[test]
    fn live_cable_route_beats_stale_wifi_but_screen_only_retains_fallbacks() {
        let mut cfg=Config::default();
        cfg.peers.push(serde_json::from_value(serde_json::json!({"name":"mac","psk":"test","addr":"192.168.0.22:24817","addrs":["10.99.0.1:24817"]})).unwrap());
        let args=vec!["receive".into(),"mac".into(),"monitor".into(),"192.168.0.22:24817".into()];
        let cable=source_addresses_for_route(&cfg,&args,Some("10.99.0.1:24817")).unwrap();
        assert_eq!(cable,vec!["10.99.0.1:24822".parse::<std::net::SocketAddr>().unwrap()]);
        let standalone=source_addresses_for_route(&cfg,&args,None).unwrap();
        assert_eq!(standalone,vec!["192.168.0.22:24822".parse::<std::net::SocketAddr>().unwrap(),"10.99.0.1:24822".parse().unwrap()]);
        assert_eq!(source_addresses_for_route(&cfg,&args,Some("invalid")).unwrap(),standalone);
    }
    #[tokio::test]
    async fn encrypted_media_finishes_with_a_flushed_stop() {
        timeout(Duration::from_secs(5), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let key = display_service_key(&[3; 32]);
            let (left, right) = tokio::join!(
                async {
                    handshake_initiator(TcpStream::connect(addr).await.unwrap(), &key)
                        .await
                        .unwrap()
                },
                async {
                    let (socket, _) = listener.accept().await.unwrap();
                    handshake_responder(socket, &key).await.unwrap()
                }
            );
            let sender = Network::new(left.0, left.1);
            let mut receiver = Network::new(right.0, right.1);
            let bytes = vec![0x42; CHUNK + 19];
            for (index, part) in bytes.chunks(CHUNK).enumerate() {
                sender
                    .tx
                    .send(Outbound {
                        bytes: chunk(1, 77, bytes.len(), index * CHUNK, part),
                        flushed: None,
                    })
                    .await
                    .unwrap();
            }
            sender.finish().await;
            let mut assembly = Assembly::default();
            let mut complete = None;
            loop {
                let packet = receiver.next().await.unwrap();
                if packet[0] == 0x10 {
                    complete = assembly.push(&packet).unwrap().or(complete);
                } else {
                    match serde_json::from_slice::<Control>(&packet).unwrap() {
                        Control::Stop => break,
                        Control::Ping { sent } => {
                            receiver.control(Control::Pong { sent }).await.unwrap()
                        }
                        Control::Pong { .. } => {}
                        _ => panic!("unexpected control"),
                    }
                }
            }
            assert_eq!(complete, Some((1, 77, bytes)));
        })
        .await
        .unwrap();
    }
}
