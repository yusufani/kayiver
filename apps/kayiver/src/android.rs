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
const MSG_SET_DISPLAY_POWER: u8 = 10;
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

/// Add a device over the network by `adb connect <ip[:port]>`. Defaults to
/// port 5555. The device must already have wireless debugging on.
pub fn add_wireless(ip: &str) -> Result<String> {
    if !have("adb") {
        bail!("adb not installed");
    }
    let addr = if ip.contains(':') { ip.to_string() } else { format!("{ip}:5555") };
    let out = adb().args(["connect", &addr]).output().context("adb connect failed")?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains("connected") || text.contains("already") {
        Ok(addr)
    } else {
        bail!("{}", text.trim())
    }
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

/// One command for the writer thread. Moves are coalescable; everything else
/// is a pre-built control message written verbatim.
enum Cmd {
    Move { buttons: u8, dx: i32, dy: i32 },
    Report(Vec<u8>),
}

/// A live scrcpy control connection. Input goes through a channel to a dedicated
/// writer thread, so a slow (e.g. wireless) socket write never blocks the input
/// router; the writer also coalesces queued moves into fewer packets.
struct TabletSink {
    tx: std::sync::mpsc::Sender<Cmd>,
    server: Child,
    serial: String,
    port: u16,
    buttons: u8,
    /// Keyboard state: modifier bitmask + up to 6 held non-modifier HID usages.
    mods: u8,
    keys: Vec<u8>,
    /// Tablet screen size in px, for the virtual cursor / edge return.
    size: (i32, i32),
}

fn uhid_input_msg(id: u16, data: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(5 + data.len());
    m.push(MSG_UHID_INPUT);
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&(data.len() as u16).to_be_bytes());
    m.extend_from_slice(data);
    m
}
fn uhid_create_msg(id: u16, name: &[u8], desc: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(desc.len() + 16);
    m.push(MSG_UHID_CREATE);
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&0u16.to_be_bytes());
    m.extend_from_slice(&0u16.to_be_bytes());
    m.push(name.len() as u8);
    m.extend_from_slice(name);
    m.extend_from_slice(&(desc.len() as u16).to_be_bytes());
    m.extend_from_slice(desc);
    m
}
fn mouse_report_msg(buttons: u8, dx: i8, dy: i8, wheel: i8, hpan: i8) -> Vec<u8> {
    uhid_input_msg(UHID_MOUSE_ID, &[buttons, dx as u8, dy as u8, wheel as u8, hpan as u8])
}
fn kbd_report_msg(mods: u8, keys: &[u8]) -> Vec<u8> {
    let mut data = [0u8; 8];
    data[0] = mods;
    for (i, k) in keys.iter().take(6).enumerate() {
        data[2 + i] = *k;
    }
    uhid_input_msg(UHID_KBD_ID, &data)
}

/// Minimum gap between UHID writes when input is flooding. A continuous stream
/// is capped to this rate (~125 Hz), with everything in the gap coalesced into
/// one cumulative report (deltas sum exactly, so position is preserved).
const WRITE_MIN_GAP: Duration = Duration::from_millis(8);

/// Idle heartbeat interval. Over wireless, Android lets the WiFi radio doze
/// between packets, so a move after any pause pays a 50-200ms wake penalty
/// instead of the ~8ms hot-radio RTT (measured). A steady no-op keeps the radio
/// awake the whole session so real moves always land hot. This — not data
/// coalescing — is what actually removes the wireless lag.
const KEEPALIVE_GAP: Duration = Duration::from_millis(6);

/// Own the socket on its own thread. Fires real input immediately, coalesces
/// bursts, and (on wireless) emits an idle no-op heartbeat to keep the radio hot.
fn spawn_writer(mut stream: TcpStream, rx: std::sync::mpsc::Receiver<Cmd>, wifi: bool) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Instant;
    std::thread::Builder::new()
        .name("kayiver-tablet-w".into())
        .spawn(move || {
            // Seed so the very first move sends with zero added delay.
            let mut last_send = Instant::now()
                .checked_sub(WRITE_MIN_GAP)
                .unwrap_or_else(Instant::now);
            // Held-button state, so an idle heartbeat never drops a drag.
            let mut last_buttons = 0u8;
            loop {
                let first = match rx.recv_timeout(KEEPALIVE_GAP) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => {
                        // Idle: on wireless, poke the radio so it stays out of
                        // doze; a zero-delta report is a true no-op for Android.
                        if wifi {
                            if stream
                                .write_all(&mouse_report_msg(last_buttons, 0, 0, 0, 0))
                                .is_err()
                            {
                                return;
                            }
                            last_send = Instant::now();
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                let mut batch = vec![first];
                // If we just sent, hold briefly and gather more input to coalesce
                // rather than firing another tiny packet into a backed-up link.
                let since = last_send.elapsed();
                if since < WRITE_MIN_GAP {
                    let deadline = Instant::now() + (WRITE_MIN_GAP - since);
                    loop {
                        let now = Instant::now();
                        if now >= deadline || batch.len() >= 512 {
                            break;
                        }
                        match rx.recv_timeout(deadline - now) {
                            Ok(c) => batch.push(c),
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                }
                // Sweep up anything else already queued.
                while let Ok(c) = rx.try_recv() {
                    batch.push(c);
                }
                last_send = Instant::now();
                let mut i = 0;
                while i < batch.len() {
                    match &batch[i] {
                        Cmd::Move { .. } => {
                            let (mut buttons, mut dx, mut dy) = (0u8, 0i32, 0i32);
                            while let Some(Cmd::Move { buttons: b, dx: x, dy: y }) = batch.get(i) {
                                buttons = *b;
                                dx += *x;
                                dy += *y;
                                i += 1;
                            }
                            last_buttons = buttons;
                            loop {
                                let sx = clamp(dx);
                                let sy = clamp(dy);
                                if stream.write_all(&mouse_report_msg(buttons, sx, sy, 0, 0)).is_err() {
                                    return;
                                }
                                dx -= sx as i32;
                                dy -= sy as i32;
                                if dx == 0 && dy == 0 {
                                    break;
                                }
                            }
                        }
                        Cmd::Report(v) => {
                            if stream.write_all(v).is_err() {
                                return;
                            }
                            i += 1;
                        }
                    }
                }
            }
        })
        .ok();
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
            "stay_awake=true",
            "power_on=true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to launch scrcpy server")?;

    // adb accepts the forward port before the server has bound its socket, so a
    // premature connect gets an immediate EOF. scrcpy sends a readiness "dummy"
    // byte first (tunnel_forward); retry connect+read until we get it, then
    // drain the 64-byte device name that follows.
    // Over wireless adb the readiness byte can take well over a second to arrive,
    // and reconnecting on every timeout churns the server's single accept slot.
    // So: retry only the *connect* (server may not have bound yet), then hold one
    // connection open with a generous read window for the dummy + 64-byte name.
    let mut stream = None;
    for _ in 0..80 {
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            s.set_read_timeout(Some(Duration::from_secs(4))).ok();
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

    let size = query_size(serial);
    let _ = scid;
    // Drain device->client control messages (UHID LED output, clipboard, …) so
    // the socket buffer can't fill and stall the server. Ends on disconnect.
    if let Ok(mut rd) = stream.try_clone() {
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while matches!(rd.read(&mut buf), Ok(n) if n > 0) {}
        });
    }
    // Writer thread owns the socket; input is queued to it. A `host:port` serial
    // means the transport is TCP/IP (wireless) — enable the radio heartbeat.
    let wifi = serial.contains(':');
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_writer(stream, rx, wifi);
    // Register the UHID mouse (real pointer) and keyboard.
    let _ = tx.send(Cmd::Report(uhid_create_msg(UHID_MOUSE_ID, b"kayiver mouse", MOUSE_REPORT_DESC)));
    let _ = tx.send(Cmd::Report(uhid_create_msg(UHID_KBD_ID, b"kayiver keyboard", KBD_REPORT_DESC)));
    let s = TabletSink {
        tx, server, serial: serial.to_string(), port,
        buttons: 0, mods: 0, keys: Vec::new(), size,
    };
    *sink().lock().unwrap() = Some(s);
    Ok(())
}

/// Tear down the session (kills the server, removes the forward).
pub fn disconnect() {
    stop_locked(&mut sink().lock().unwrap());
}

fn stop_locked(guard: &mut Option<TabletSink>) {
    if let Some(mut s) = guard.take() {
        drop(std::mem::replace(&mut s.tx, std::sync::mpsc::channel().0)); // end the writer
        let _ = s.server.kill();
        let _ = s.server.wait();
        let _ = adb().args(["-s", &s.serial, "forward", "--remove", &format!("tcp:{}", s.port)]).output();
    }
}

fn clamp(v: i32) -> i8 {
    v.clamp(-127, 127) as i8
}

/// Wake the tablet's screen when taking control. The scrcpy control socket
/// can't wake a slept physical screen with video disabled, so use adb's `input`
/// path (KEYCODE_WAKEUP), which does. Fire-and-forget on its own thread.
pub fn wake() {
    let Some(serial) = connected_serial() else { return };
    std::thread::spawn(move || {
        let _ = adb().args(["-s", &serial, "shell", "input", "keyevent", "224"]).output();
    });
}

/// Forward a relative move (coalesced + chunked by the writer thread).
pub fn mouse_move(dx: i32, dy: i32) {
    let guard = sink().lock().unwrap();
    let Some(s) = guard.as_ref() else { return };
    let _ = s.tx.send(Cmd::Move { buttons: s.buttons, dx, dy });
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
    let _ = s.tx.send(Cmd::Report(mouse_report_msg(s.buttons, 0, 0, 0, 0)));
}

/// Wheel: positive `dy` scrolls up, positive `dx` pans right.
pub fn mouse_scroll(dx: i32, dy: i32) {
    let guard = sink().lock().unwrap();
    let Some(s) = guard.as_ref() else { return };
    let _ = s.tx.send(Cmd::Report(mouse_report_msg(s.buttons, 0, 0, clamp(dy), clamp(dx))));
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
    let _ = s.tx.send(Cmd::Report(kbd_report_msg(s.mods, &s.keys)));
}
