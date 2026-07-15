//! Seamless Android control, driven from kayiver's own capture — the
//! InputShare model, not a mirror window.
//!
//! Android forbids ordinary apps from injecting a free cursor, so kayiver reuses
//! scrcpy's on-device server as the injection engine: pushed over adb, it runs
//! with the `shell` uid and exposes a control socket. We speak that control
//! protocol directly (no scrcpy window) and register a **UHID virtual mouse**,
//! which makes Android draw a real system pointer on the tablet's own screen.
//! kayiver's captured mouse deltas/buttons/scroll become UHID reports, so moving
//! the Mac's mouse moves a cursor on the tablet. Transport is adb's: USB or
//! wireless. No root, nothing installed on the device (the server is pushed each
//! session and removed on exit).

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

const SERVER_VERSION: &str = "4.1";
const FORWARD_PORT_BASE: u16 = 27600;
const UHID_MOUSE_ID: u16 = 1;
const UHID_KBD_ID: u16 = 2;

// scrcpy control message types (v4.1).
const MSG_UHID_CREATE: u8 = 12;
const MSG_UHID_INPUT: u8 = 13;
const MSG_UHID_DESTROY: u8 = 14;

/// Standard relative-mouse HID report descriptor (5-button + wheel + AC pan),
/// taken verbatim from scrcpy so Android accepts it and shows a pointer.
const MOUSE_REPORT_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x05,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03,
    0x81, 0x06, 0x05, 0x0C, 0x0A, 0x38, 0x02, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81,
    0x06, 0xC0, 0xC0,
];

/// Keyboard HID report descriptor, verbatim from scrcpy (includes the LED
/// output report so Android accepts it). 8-byte input report
/// [modifiers, reserved, key1..key6], key usages 0..0x65.
const KBD_REPORT_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x75, 0x08, 0x95, 0x01, 0x81, 0x01, 0x05, 0x08, 0x19, 0x01,
    0x29, 0x05, 0x75, 0x01, 0x95, 0x05, 0x91, 0x02, 0x75, 0x03, 0x95, 0x01, 0x91, 0x01, 0x05, 0x07,
    0x19, 0x00, 0x29, 0x65, 0x15, 0x00, 0x25, 0x65, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00, 0xC0,
];

// ------------------------------------------------------------- devices ----

/// A device adb can see.
#[derive(Clone)]
pub struct Device {
    pub serial: String,
    pub model: String,
    /// "usb" or "wifi" (a `host:port` serial means it's over TCP/IP).
    pub connection: String,
}

fn bin(name: &str) -> String {
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let p = format!("{dir}/{name}");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    name.to_string()
}

fn have(name: &str) -> bool {
    ["/opt/homebrew/bin", "/usr/local/bin"]
        .iter()
        .any(|d| std::path::Path::new(&format!("{d}/{name}")).exists())
}

fn server_jar() -> Option<String> {
    // Homebrew keeps it under share/scrcpy; the Cellar path carries the version.
    let candidates = [
        "/opt/homebrew/share/scrcpy/scrcpy-server".to_string(),
        "/usr/local/share/scrcpy/scrcpy-server".to_string(),
        format!("/opt/homebrew/Cellar/scrcpy/{SERVER_VERSION}/share/scrcpy/scrcpy-server"),
    ];
    candidates.into_iter().find(|p| std::path::Path::new(p).exists())
}

/// Are the tools (scrcpy server + adb) present?
pub fn tools_ready() -> bool {
    have("adb") && server_jar().is_some()
}

fn adb() -> Command {
    let mut c = Command::new(bin("adb"));
    c.stdin(Stdio::null());
    c
}

/// Parse `adb devices -l`.
pub fn list_devices() -> Vec<Device> {
    if !have("adb") {
        return Vec::new();
    }
    let Ok(out) = adb().args(["devices", "-l"]).output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut devices = Vec::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let serial = parts.next().unwrap_or("").to_string();
        if serial.is_empty() || parts.next() != Some("device") {
            continue;
        }
        let model = line
            .split_whitespace()
            .find_map(|t| t.strip_prefix("model:"))
            .unwrap_or("Android")
            .replace('_', " ");
        let connection = if serial.contains(':') { "wifi" } else { "usb" }.to_string();
        let d = Device { serial, model, connection };
        // The same physical tablet can appear over both USB and Wi-Fi; keep one
        // per model, preferring the USB link.
        if let Some(slot) = devices.iter_mut().find(|e: &&mut Device| e.model == d.model) {
            if slot.connection != "usb" && d.connection == "usb" {
                *slot = d;
            }
        } else {
            devices.push(d);
        }
    }
    devices
}

/// Arm wireless adb on a USB device and connect over TCP/IP; returns `ip:port`.
pub fn enable_wireless(serial: &str) -> Result<String> {
    if !have("adb") {
        bail!("adb not installed");
    }
    let ip_out = adb()
        .args(["-s", serial, "shell", "ip", "-f", "inet", "addr", "show", "wlan0"])
        .output()
        .context("adb shell ip failed")?;
    let ip_text = String::from_utf8_lossy(&ip_out.stdout);
    let ip = ip_text
        .split_whitespace()
        .skip_while(|t| *t != "inet")
        .nth(1)
        .and_then(|cidr| cidr.split('/').next())
        .context("could not find the tablet's Wi-Fi IP (is it on Wi-Fi?)")?
        .to_string();
    adb().args(["-s", serial, "tcpip", "5555"]).output().context("adb tcpip failed")?;
    std::thread::sleep(Duration::from_millis(1200));
    let addr = format!("{ip}:5555");
    adb().args(["connect", &addr]).output().context("adb connect failed")?;
    Ok(addr)
}

// ----------------------------------------------------- the control sink ----

/// A live scrcpy control connection with a registered UHID mouse.
struct TabletSink {
    stream: TcpStream,
    server: Child,
    serial: String,
    scid: String,
    port: u16,
    buttons: u8,
    /// Keyboard state: modifier bitmask + up to 6 held non-modifier HID usages.
    mods: u8,
    keys: Vec<u8>,
    /// Tablet screen size in px, for the virtual cursor / edge return.
    size: (i32, i32),
}

/// The tablet's screen size in px (for edge-return math), if connected.
pub fn size() -> Option<(i32, i32)> {
    sink().lock().unwrap().as_ref().map(|s| s.size)
}

/// Serial of the first visible device (for auto-connect on edge crossing).
pub fn first_serial() -> Option<String> {
    list_devices().into_iter().next().map(|d| d.serial)
}

/// Make sure a control session is up (connect the first device if not). Cheap
/// when already connected. Returns whether a session is live.
pub fn ensure_connected() -> bool {
    if is_connected() {
        return true;
    }
    if let Some(serial) = first_serial() {
        let _ = connect(&serial);
    }
    is_connected()
}

fn query_size(serial: &str) -> (i32, i32) {
    let out = adb().args(["-s", serial, "shell", "wm", "size"]).output();
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout);
        // Prefer "Override size", else "Physical size".
        for key in ["Override size:", "Physical size:"] {
            if let Some(line) = text.lines().find(|l| l.contains(key)) {
                if let Some(dims) = line.split(':').nth(1) {
                    let dims = dims.trim();
                    if let Some((w, h)) = dims.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse()) {
                            return (w, h);
                        }
                    }
                }
            }
        }
    }
    (2560, 1600)
}

fn sink() -> &'static Mutex<Option<TabletSink>> {
    static S: OnceLock<Mutex<Option<TabletSink>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

fn next_scid() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("{:08x}", (t ^ (n.wrapping_mul(0x9E37_79B1))) & 0x7fff_ffff)
}

/// True while a tablet control session is live.
pub fn is_connected() -> bool {
    sink().lock().unwrap().is_some()
}

/// The serial of the connected tablet, if any.
pub fn connected_serial() -> Option<String> {
    sink().lock().unwrap().as_ref().map(|s| s.serial.clone())
}

/// Start a scrcpy control session for `serial` and register the UHID mouse.
/// Replaces any existing session.
pub fn connect(serial: &str) -> Result<()> {
    stop_locked(&mut sink().lock().unwrap());
    let jar = server_jar().context("scrcpy-server not found (brew install scrcpy)")?;
    // Push the server (idempotent; overwrites).
    let push = adb()
        .args(["-s", serial, "push", &jar, "/data/local/tmp/scrcpy-server.jar"])
        .output()
        .context("adb push (scrcpy-server) failed")?;
    if !push.status.success() {
        bail!("adb push failed: {}", String::from_utf8_lossy(&push.stderr));
    }

    let scid = next_scid();
    let port = FORWARD_PORT_BASE;
    // Forward a local TCP port to the server's abstract control socket.
    let _ = adb().args(["-s", serial, "forward", "--remove", &format!("tcp:{port}")]).output();
    let fwd = adb()
        .args(["-s", serial, "forward", &format!("tcp:{port}"), &format!("localabstract:scrcpy_{scid}")])
        .output()
        .context("adb forward failed")?;
    if !fwd.status.success() {
        bail!("adb forward failed: {}", String::from_utf8_lossy(&fwd.stderr));
    }

    // Launch the server: control only, no video/audio, forward tunnel.
    let server = adb()
        .args([
            "-s", serial, "shell",
            "CLASSPATH=/data/local/tmp/scrcpy-server.jar",
            "app_process", "/", "com.genymobile.scrcpy.Server", SERVER_VERSION,
            &format!("scid={scid}"),
            "log_level=warn",
            "video=false",
            "audio=false",
            "control=true",
            "tunnel_forward=true",
            "cleanup=true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to launch scrcpy server")?;

    // adb accepts the forward port before the server has bound its socket, so a
    // premature connect gets an immediate EOF. scrcpy sends a readiness "dummy"
    // byte first (tunnel_forward); retry connect+read until we get it, then
    // drain the 64-byte device name that follows.
    let mut stream = None;
    for _ in 0..60 {
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            s.set_read_timeout(Some(Duration::from_millis(300))).ok();
            let mut dummy = [0u8; 1];
            if s.read_exact(&mut dummy).is_ok() {
                let mut name = [0u8; 64];
                let _ = s.read_exact(&mut name); // device name; unused
                s.set_read_timeout(None).ok();
                s.set_nodelay(true).ok();
                stream = Some(s);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    let mut stream = match stream {
        Some(s) => s,
        None => {
            let mut server = server;
            let _ = server.kill();
            bail!("no handshake from scrcpy server (is USB debugging authorized on the tablet?)");
        }
    };

    // Register the UHID mouse (real pointer) and keyboard.
    let size = query_size(serial);
    let mut s = TabletSink {
        stream, server, serial: serial.to_string(), scid, port,
        buttons: 0, mods: 0, keys: Vec::new(), size,
    };
    s.uhid_create(UHID_MOUSE_ID, b"kayiver mouse", MOUSE_REPORT_DESC)?;
    s.uhid_create(UHID_KBD_ID, b"kayiver keyboard", KBD_REPORT_DESC)?;
    // Drain device->client control messages (UHID LED output, clipboard, …) so
    // the socket buffer can't fill and stall the server. Ends on disconnect.
    if let Ok(mut rd) = s.stream.try_clone() {
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match rd.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
    }
    *sink().lock().unwrap() = Some(s);
    Ok(())
}

/// Tear down the session (kills the server, removes the forward).
pub fn disconnect() {
    stop_locked(&mut sink().lock().unwrap());
}

fn stop_locked(guard: &mut Option<TabletSink>) {
    if let Some(mut s) = guard.take() {
        let _ = s.uhid_destroy();
        let _ = s.server.kill();
        let _ = s.server.wait();
        let _ = adb().args(["-s", &s.serial, "forward", "--remove", &format!("tcp:{}", s.port)]).output();
    }
}

impl TabletSink {
    fn uhid_create(&mut self, id: u16, name: &[u8], desc: &[u8]) -> Result<()> {
        let mut msg = Vec::with_capacity(desc.len() + 16);
        msg.push(MSG_UHID_CREATE);
        msg.extend_from_slice(&id.to_be_bytes());
        msg.extend_from_slice(&0u16.to_be_bytes()); // vendor id
        msg.extend_from_slice(&0u16.to_be_bytes()); // product id
        msg.push(name.len() as u8); // write_string_tiny: 1-byte length
        msg.extend_from_slice(name);
        msg.extend_from_slice(&(desc.len() as u16).to_be_bytes());
        msg.extend_from_slice(desc);
        self.stream.write_all(&msg).context("uhid create failed")?;
        Ok(())
    }

    fn uhid_destroy(&mut self) -> Result<()> {
        for id in [UHID_MOUSE_ID, UHID_KBD_ID] {
            let mut msg = vec![MSG_UHID_DESTROY];
            msg.extend_from_slice(&id.to_be_bytes());
            let _ = self.stream.write_all(&msg);
        }
        Ok(())
    }

    fn uhid_input(&mut self, id: u16, data: &[u8]) -> Result<()> {
        let mut msg = vec![MSG_UHID_INPUT];
        msg.extend_from_slice(&id.to_be_bytes());
        msg.extend_from_slice(&(data.len() as u16).to_be_bytes());
        msg.extend_from_slice(data);
        self.stream.write_all(&msg)?;
        Ok(())
    }

    /// Send one 5-byte mouse report [buttons, dx, dy, wheel, hpan].
    fn report(&mut self, dx: i8, dy: i8, wheel: i8, hpan: i8) -> Result<()> {
        let data = [self.buttons, dx as u8, dy as u8, wheel as u8, hpan as u8];
        self.uhid_input(UHID_MOUSE_ID, &data)
    }

    /// Send the 8-byte keyboard report from the current mods + held keys.
    fn kbd_report(&mut self) -> Result<()> {
        let mut data = [0u8; 8];
        data[0] = self.mods;
        for (i, k) in self.keys.iter().take(6).enumerate() {
            data[2 + i] = *k;
        }
        self.uhid_input(UHID_KBD_ID, &data)
    }
}

fn clamp(v: i32) -> i8 {
    v.clamp(-127, 127) as i8
}

/// Forward a relative move; large deltas are split into ≤127 steps.
pub fn mouse_move(mut dx: i32, mut dy: i32) {
    let mut guard = sink().lock().unwrap();
    let Some(s) = guard.as_mut() else { return };
    loop {
        let sx = clamp(dx);
        let sy = clamp(dy);
        if s.report(sx, sy, 0, 0).is_err() {
            drop(guard);
            disconnect();
            return;
        }
        dx -= sx as i32;
        dy -= sy as i32;
        if dx == 0 && dy == 0 {
            break;
        }
    }
}

/// Button index: 0 left, 1 right, 2 middle, 3 X1, 4 X2.
pub fn mouse_button(index: u8, pressed: bool) {
    if index > 4 {
        return;
    }
    let mut guard = sink().lock().unwrap();
    let Some(s) = guard.as_mut() else { return };
    if pressed {
        s.buttons |= 1 << index;
    } else {
        s.buttons &= !(1 << index);
    }
    let _ = s.report(0, 0, 0, 0);
}

/// Wheel: positive `dy` scrolls up, positive `dx` pans right.
pub fn mouse_scroll(dx: i32, dy: i32) {
    let mut guard = sink().lock().unwrap();
    let Some(s) = guard.as_mut() else { return };
    let _ = s.report(0, 0, clamp(dy), clamp(dx));
}

/// Forward a key by HID usage code (`hid`). Modifiers (0xE0..=0xE7) set the
/// modifier byte; other keys go into the 6-key rollover array.
pub fn key(hid: u16, pressed: bool) {
    if hid == 0 || hid > 0xFF {
        return;
    }
    let hid = hid as u8;
    let mut guard = sink().lock().unwrap();
    let Some(s) = guard.as_mut() else { return };
    if (0xE0..=0xE7).contains(&hid) {
        let bit = 1u8 << (hid - 0xE0);
        if pressed {
            s.mods |= bit;
        } else {
            s.mods &= !bit;
        }
    } else if pressed {
        if !s.keys.contains(&hid) {
            if s.keys.len() >= 6 {
                s.keys.remove(0);
            }
            s.keys.push(hid);
        }
    } else {
        s.keys.retain(|k| *k != hid);
    }
    let _ = s.kbd_report();
}
