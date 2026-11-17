//! Opt-in Mac -> Windows extended display. A worker process owns an independent
//! Noise session and a native media child. Input protocol 18 is unchanged.
mod session;

use anyhow::{bail, Context, Result};
use kayiver_core::{config::Config, proto::Rect};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, Write},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
};

pub const PORT_OFFSET: u16 = 5;
pub const MAX_VIDEO_FRAME: usize = 8 * 1024 * 1024;
pub const CHUNK: usize = 32 * 1024;
const CHUNK_HEADER: usize = 25;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub phase: String,
    pub role: String,
    pub peer: String,
    pub target: Option<Rect>,
    pub target_id: Option<String>,
    pub virtual_rect: Option<Rect>,
    pub frames: u64,
    pub fps: u32,
    pub bitrate_mbps: u32,
    /// Approximate capture -> Present submission, using the session clock
    /// estimate. This is NOT a glass-to-glass or scanout measurement.
    pub latency_p95_ms: Option<f64>,
    pub clock_uncertainty_ms: Option<f64>,
    pub hardware_confirmed: bool,
    pub error: Option<String>,
}

struct Manager {
    child: Option<Child>,
    generation: u64,
    status: Status,
}
static MANAGER: OnceLock<Mutex<Manager>> = OnceLock::new();
fn manager() -> &'static Mutex<Manager> {
    MANAGER.get_or_init(|| {
        Mutex::new(Manager {
            child: None,
            generation: 0,
            status: Status {
                phase: "off".into(),
                ..Default::default()
            },
        })
    })
}

pub fn status() -> Status {
    manager().lock().unwrap().status.clone()
}

fn supported() -> bool {
    #[cfg(display_native)]
    {
        unsafe extern "C" {
            fn kayiver_display_available() -> bool;
        }
        unsafe { kayiver_display_available() }
    }
    #[cfg(not(display_native))]
    {
        false
    }
}

pub fn api_status() -> serde_json::Value {
    let cfg = Config::load_or_init().ok();
    let peers: Vec<_> = cfg
        .as_ref()
        .map(|c| {
            c.peers
                .iter()
                .map(|p| serde_json::json!({"name":p.name,"os":p.os}))
                .collect()
        })
        .unwrap_or_default();
    let monitors: Vec<_> = crate::platform::identified_monitors()
        .into_iter()
        .enumerate()
        .map(|(index, (id, rect))| serde_json::json!({"index":index,"id":id,"rect":rect}))
        .collect();
    serde_json::json!({"supported":supported(),"os":std::env::consts::OS,
        "experimental":true,"default_enabled":false,"status":status(),"peers":peers,"monitors":monitors,
        "preferences":cfg.as_ref().map(|c|c.extra_display).unwrap_or_default(),
        "port":cfg.and_then(|c|c.port.checked_add(PORT_OFFSET))})
}

/// Move only the temporary Mac surface; never persist it as a physical desk.
#[cfg(target_os = "macos")]
fn position_source(v: &serde_json::Value) -> Result<()> {
    let state = status();
    anyhow::ensure!(state.role == "source" && state.phase == "streaming", "start the Mac extended display first");
    let previous = state.virtual_rect.context("virtual display is not ready")?;
    let coordinate = |key: &str| -> Result<i32> {
        let value = v[key].as_i64().context("missing display position")?;
        anyhow::ensure!((-32768..=32767).contains(&value), "display position is out of range");
        Ok(value as i32)
    };
    let desired = Rect { x: coordinate("x")?, y: coordinate("y")?, ..previous };
    move_virtual(previous,desired)?;
    let placement=SavedPlacement {
        peer:state.peer, monitor:state.target_id.context("missing Windows monitor identity")?,
        physical:crate::platform::monitors().into_iter().filter(|r|*r!=desired).collect(),
        rect:desired,
    };
    let path=Config::path().with_file_name("extra-display-position.json");
    let tmp=path.with_extension("tmp");
    std::fs::write(&tmp,serde_json::to_vec(&placement)?)?;
    std::fs::rename(tmp,path)?;
    Ok(())
}

#[cfg(target_os="macos")]
#[derive(Serialize,Deserialize)]
struct SavedPlacement {peer:String,monitor:String,physical:Vec<Rect>,rect:Rect}

#[cfg(target_os="macos")]
fn restore_virtual(peer:&str,monitor:&str,physical:&[Rect],current:Rect)->Option<Rect>{
    let path=Config::path().with_file_name("extra-display-position.json");
    let placement:SavedPlacement=serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if placement.peer!=peer || placement.monitor!=monitor || placement.physical!=physical ||
       placement.rect.w!=current.w || placement.rect.h!=current.h {return None;}
    move_virtual(current,placement.rect).ok()?;
    Some(placement.rect)
}

#[cfg(target_os="macos")]
fn move_virtual(previous:Rect,desired:Rect)->Result<()> {
    let mut screens = crate::platform::monitors();
    let index = screens.iter().position(|r| *r == previous).context("display position changed; refresh and retry")?;
    for (i, other) in screens.iter().enumerate() {
        if i == index { continue; }
        let overlaps = (desired.x as i64) < other.x as i64 + other.w as i64
            && (other.x as i64) < desired.x as i64 + desired.w as i64
            && (desired.y as i64) < other.y as i64 + other.h as i64
            && (other.y as i64) < desired.y as i64 + desired.h as i64;
        anyhow::ensure!(!overlaps, "place the extra display beside an edge, without overlapping another Mac screen");
    }
    if previous != desired {
        screens[index] = desired;
        anyhow::ensure!(crate::platform::apply_arrangement(&screens)?, "could not apply the display position");
    }
    Ok(())
}

pub fn action(body: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(body)?;
    let action = v["action"].as_str().context("missing display action")?;
    if action == "preferences" {
        anyhow::ensure!(cfg!(target_os="macos"),"change display preferences on the Mac");
        let preferences:kayiver_core::config::DisplayPreferences=serde_json::from_value(v["preferences"].clone()).context("invalid display preferences")?;
        let mut cfg=Config::load_or_init()?;cfg.extra_display=preferences;cfg.save()?;
        return Ok(());
    }
    if action == "position" {
        #[cfg(target_os = "macos")]
        return position_source(&v);
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("position the extra display from the Mac");
    }
    if action == "stop" {
        stop();
        return Ok(());
    }
    anyhow::ensure!(supported(), "extended display is unavailable in this build");
    let cfg = Config::load_or_init()?;
    let peer = v["peer"].as_str().context("choose a paired device")?;
    let p = cfg.peer(peer).context("device is not paired")?;
    let mut args = vec![action.to_string(), peer.to_string()];
    match action {
        "source" => {
            anyhow::ensure!(
                cfg!(target_os = "macos"),
                "only a Mac can provide the extended desktop"
            );
            anyhow::ensure!(
                compatible_peer("source", p.os.as_deref()),
                "choose a paired Windows device"
            );
        }
        "receive" => {
            anyhow::ensure!(
                cfg!(target_os = "windows"),
                "only Windows can receive this display"
            );
            anyhow::ensure!(
                compatible_peer("receive", p.os.as_deref()),
                "choose a paired Mac"
            );
            let id = v["monitor_id"]
                .as_str()
                .context("choose an identified local monitor")?;
            let (_, rect) = crate::platform::identified_monitors()
                .into_iter()
                .find(|(key, _)| key.as_deref() == Some(id))
                .context("monitor disconnected; refresh and choose again")?;
            validate_size(rect.w, rect.h)?;
            anyhow::ensure!(
                crate::platform::monitors()
                    .iter()
                    .filter(|r| **r == rect)
                    .count()
                    == 1,
                "mirrored or ambiguous target monitor; extend it in Windows settings first"
            );
            // A physical panel already cabled to both peers has two identities;
            // require a dedicated target to avoid streaming back onto itself.
            anyhow::ensure!(
                cfg.shared_monitor.local_rect != Some(rect),
                "the shared physical panel cannot also be an extended display target"
            );
            args.push(id.to_string());
            // Windows dials the Mac using the known working cable address.
            if let Some(addr) = crate::ui::peer_remote_addr(peer) {
                args.push(addr);
            }
        }
        _ => bail!("unknown display action"),
    }
    let mut m = manager().lock().unwrap();
    if let Some(child) = m.child.as_mut() {
        anyhow::ensure!(
            child.try_wait()?.is_some(),
            "stop the current display session first"
        );
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("screen-worker")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Login-task/GUI launches may have no valid standard-error handle.
        // Give the worker three real pipe handles instead of inheriting one.
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().context("could not start display worker")?;
    let output = child.stdout.take().unwrap();
    let errors = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(errors)
            .lines()
            .map_while(Result::ok)
        {
            tracing::warn!(target:"kayiver::display", "{line}");
        }
    });
    m.generation += 1;
    let generation = m.generation;
    m.status = Status {
        phase: "starting".into(),
        role: action.into(),
        peer: peer.into(),
        ..Default::default()
    };
    m.child = Some(child);
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            let Ok(next) = serde_json::from_str::<Status>(&line) else {
                continue;
            };
            let mut m = manager().lock().unwrap();
            if m.generation != generation {
                return;
            }
            let changed = m.status.phase != next.phase
                || m.status.target != next.target
                || m.status.virtual_rect != next.virtual_rect;
            m.status = next;
            drop(m);
            if changed {
                let _ = crate::ui::send_cmd(crate::ui::UiCmd::DisplayChanged);
            }
        }
        let mut m = manager().lock().unwrap();
        if m.generation != generation {
            return;
        }
        let exit = m.child.as_mut().and_then(|c| c.wait().ok());
        m.child = None;
        if !matches!(m.status.phase.as_str(), "off" | "error") {
            m.status.phase = "error".into();
            m.status.error = Some(format!("display worker exited ({exit:?})"));
        }
        drop(m);
        let _ = crate::ui::send_cmd(crate::ui::UiCmd::DisplayChanged);
    });
    Ok(())
}

pub fn stop() {
    let mut m = manager().lock().unwrap();
    if let Some(child) = m.child.as_mut() {
        if let Some(input) = child.stdin.as_mut() {
            let _ = input.write_all(&[0]);
            let _ = input.flush();
        }
        m.status.phase = "stopping".into();
    }
}

/// The Windows surface is a presentation endpoint, never an input target.
pub fn presentation() -> Option<(String, Rect)> {
    let s = status();
    if !matches!(s.phase.as_str(), "streaming" | "connecting" | "stopping") {
        return None;
    }
    let target = s.target?;
    let machine = if s.role == "source" {
        s.peer
    } else {
        Config::load_or_init().ok()?.name
    };
    Some((machine, target))
}

pub fn virtual_screen_active() -> bool {
    let s = status();
    s.role == "source" && matches!(s.phase.as_str(), "connecting" | "streaming" | "stopping")
}

fn compatible_peer(role: &str, os: Option<&str>) -> bool {
    // Welcome in input protocol 18 has no OS field, so a client's paired
    // host may legitimately have no cached platform. The authenticated
    // display handshake establishes capability instead of that cache.
    os.is_none_or(|os| os == if role == "source" { "windows" } else { "macos" })
}

pub fn exclude_presentation(
    desks: &mut [kayiver_core::motion::Desk],
    endpoint: Option<(String, Rect)>,
) {
    if let Some((machine, rect)) = endpoint {
        for desk in desks {
            if desk.machine == machine {
                desk.monitors.retain(|(_, r)| *r != rect);
            }
        }
    }
}

fn requested_fps(rate:kayiver_core::config::DisplayFrameRate)->u32 {
    use kayiver_core::config::DisplayFrameRate::*;
    match rate { Adaptive | Fps60=>60, Fps30=>30, Fps120=>120, Fps144=>144 }
}

fn display_profile(rect:Rect,preferences:kayiver_core::config::DisplayPreferences)->(u32,u32) {
    use kayiver_core::config::DisplayQuality;
    let large=rect.w as i64*rect.h as i64>1920*1080;
    let fps=requested_fps(preferences.frame_rate);
    let bitrate=match (preferences.quality,large) {
        (DisplayQuality::Economy,false)=>12,(DisplayQuality::Economy,true)=>20,
        (DisplayQuality::Balanced,false)=>20,(DisplayQuality::Balanced,true)=>35,
        (DisplayQuality::High,false)=>30,(DisplayQuality::High,true)=>45,
        (DisplayQuality::Ultra,false)=>60,(DisplayQuality::Ultra,true)=>90,
        (DisplayQuality::Studio,false)=>100,(DisplayQuality::Studio,true)=>150,
    };
    (fps,bitrate)
}

pub fn validate_size(w: i32, h: i32) -> Result<()> {
    anyhow::ensure!((640..=2560).contains(&w) && (480..=2560).contains(&h) && w%2==0 && h%2==0 && (w as i64)*(h as i64)<=2560*1440,
        "experimental display supports even native dimensions from 640×480 up to 1440p (including portrait)");
    Ok(())
}

pub fn native(args: Vec<String>) -> Result<()> {
    #[cfg(display_native)]
    {
        unsafe extern "C" {
            fn kayiver_display_native(argc: i32, argv: *const *const std::ffi::c_char) -> i32;
        }
        let args: Vec<_> = args
            .into_iter()
            .map(std::ffi::CString::new)
            .collect::<std::result::Result<_, _>>()?;
        let pointers: Vec<_> = args.iter().map(|s| s.as_ptr()).collect();
        let code = unsafe { kayiver_display_native(pointers.len() as i32, pointers.as_ptr()) };
        anyhow::ensure!(code == 0, "native display helper exited with code {code}");
        Ok(())
    }
    #[cfg(not(display_native))]
    {
        let _ = args;
        bail!("native extended display unavailable on this target")
    }
}

pub fn worker(args: Vec<String>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(session::run(args))
}

/// A companion must not outlive the explicit launcher that owns its consent.
pub fn parent_lifetime() -> Result<impl std::future::Future<Output = ()>> {
    let parent = session::ParentWatch::new()?;
    Ok(async move { parent.wait().await })
}

fn chunk(frame: u64, captured: u64, total: usize, offset: usize, data: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(CHUNK_HEADER + data.len());
    packet.push(0x10);
    packet.extend_from_slice(&frame.to_be_bytes());
    packet.extend_from_slice(&captured.to_be_bytes());
    packet.extend_from_slice(&(total as u32).to_be_bytes());
    packet.extend_from_slice(&(offset as u32).to_be_bytes());
    packet.extend_from_slice(data);
    packet
}

#[derive(Default)]
struct Assembly {
    id: u64,
    captured: u64,
    total: usize,
    data: Vec<u8>,
    last_complete: u64,
}
impl Assembly {
    fn push(&mut self, packet: &[u8]) -> Result<Option<(u64, u64, Vec<u8>)>> {
        anyhow::ensure!(
            packet.len() > CHUNK_HEADER
                && packet.len() <= CHUNK_HEADER + CHUNK
                && packet[0] == 0x10,
            "invalid video chunk"
        );
        let id = u64::from_be_bytes(packet[1..9].try_into()?);
        let captured = u64::from_be_bytes(packet[9..17].try_into()?);
        let total = u32::from_be_bytes(packet[17..21].try_into()?) as usize;
        let offset = u32::from_be_bytes(packet[21..25].try_into()?) as usize;
        anyhow::ensure!(
            total > 0
                && total <= MAX_VIDEO_FRAME
                && offset <= total
                && packet.len() - CHUNK_HEADER <= total - offset,
            "invalid video frame bounds"
        );
        if offset == 0 {
            anyhow::ensure!(
                self.data.is_empty() && self.last_complete.checked_add(1) == Some(id),
                "incomplete or replayed video frame"
            );
            self.id = id;
            self.captured = captured;
            self.total = total;
        }
        anyhow::ensure!(
            id == self.id
                && captured == self.captured
                && total == self.total
                && offset == self.data.len(),
            "out-of-order video chunks"
        );
        self.data.extend_from_slice(&packet[CHUNK_HEADER..]);
        if self.data.len() == self.total {
            self.last_complete = id;
            Ok(Some((id, captured, std::mem::take(&mut self.data))))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn picture_preferences_control_fps_and_native_resolution_budgets() {
        use kayiver_core::config::{DisplayPreferences,DisplayFrameRate,DisplayQuality};
        let full=Rect{x:0,y:0,w:1920,h:1080};let large=Rect{w:2560,h:1440,..full};
        assert_eq!(display_profile(full,DisplayPreferences::default()),(60,20));
        assert_eq!(display_profile(large,DisplayPreferences{frame_rate:DisplayFrameRate::Fps30,quality:DisplayQuality::High}),(30,45));
        assert_eq!(display_profile(large,DisplayPreferences{frame_rate:DisplayFrameRate::Fps144,quality:DisplayQuality::Studio}),(144,150));
        assert_eq!(display_profile(full,DisplayPreferences{frame_rate:DisplayFrameRate::Fps120,quality:DisplayQuality::Ultra}),(120,60));
        assert_eq!(display_profile(full,DisplayPreferences{frame_rate:DisplayFrameRate::Fps60,quality:DisplayQuality::Economy}),(60,12));
    }
    use super::*;
    #[test]
    fn reassembly_bounds_sequence_and_replay() {
        let mut a = Assembly::default();
        let data = vec![7; CHUNK + 3];
        let first = chunk(1, 99, data.len(), 0, &data[..CHUNK]);
        assert!(a.push(&first).unwrap().is_none());
        assert!(a.push(&first).is_err());
        let complete = a
            .push(&chunk(1, 99, data.len(), CHUNK, &data[CHUNK..]))
            .unwrap()
            .unwrap();
        assert_eq!(complete, (1, 99, data));
        assert!(a.push(&first).is_err());
        assert!(Assembly::default()
            .push(&chunk(1, 0, MAX_VIDEO_FRAME + 1, 0, &[0]))
            .is_err());
        assert!(Assembly::default()
            .push(&chunk(1, 0, 1, 0, &[1, 2]))
            .is_err());
        assert!(Assembly::default().push(&chunk(2, 0, 1, 0, &[0])).is_err());
        assert!(Assembly::default().push(&[0x10; 24]).is_err());
    }
    #[test]
    fn profiles_keep_native_dimensions_and_limit_work() {
        for (w, h) in [(1920, 1080), (2560, 1440), (1080, 1920), (1440, 2560)] {
            assert!(validate_size(w, h).is_ok());
        }
        for (w, h) in [(3840, 2160), (1921, 1080), (0, 1080), (i32::MAX, i32::MAX)] {
            assert!(validate_size(w, h).is_err());
        }
    }
    #[test]
    fn paired_hosts_without_cached_platform_can_negotiate_display() {
        assert!(compatible_peer("receive", None));
        assert!(compatible_peer("source", None));
        assert!(compatible_peer("receive", Some("macos")));
        assert!(compatible_peer("source", Some("windows")));
        assert!(!compatible_peer("receive", Some("windows")));
        assert!(!compatible_peer("source", Some("linux")));
    }
    #[test]
    fn presentation_is_not_a_second_input_target() {
        use kayiver_core::motion::{Desk, Point, Topology};
        let native = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let virtual_rect = Rect { x: 1920, ..native };
        let mut desks = vec![
            Desk {
                machine: "arbitrary-mac".into(),
                monitors: vec![
                    ("physical".into(), native),
                    ("virtual".into(), virtual_rect),
                ],
            },
            Desk {
                machine: "arbitrary-win".into(),
                monitors: vec![("presentation".into(), native)],
            },
        ];
        exclude_presentation(&mut desks, Some(("arbitrary-win".into(), native)));
        let t = Topology::compile(1, &desks, None, &[]);
        let at = t
            .locate("arbitrary-mac", Point { x: 1930., y: 10. })
            .unwrap();
        assert_eq!(t.surface(&at.surface).unwrap().machine, "arbitrary-mac");
        assert!(t
            .locate("arbitrary-win", Point { x: 10., y: 10. })
            .is_none());
        assert_eq!(desks[0].monitors.len(), 2);
    }
}
