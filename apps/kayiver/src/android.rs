//! Android device control, driven from the kayiver editor.
//!
//! Android forbids ordinary apps from injecting a free mouse cursor, so kayiver
//! uses `scrcpy` as the injection engine: its on-device server runs with the
//! adb `shell` uid, which *is* allowed to inject input — no root needed. kayiver
//! enumerates devices with `adb`, launches/stops `scrcpy` per device, and tracks
//! the processes so connect/disconnect lives in the same UI as everything else.
//!
//! Transport is adb's: a USB-C cable, or wireless once `adb tcpip` is armed.

#![allow(dead_code)]

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};

/// A device adb can see.
#[derive(Clone)]
pub struct Device {
    pub serial: String,
    pub model: String,
    /// "usb" or "wifi" (a `host:port` serial means it's over TCP/IP).
    pub connection: String,
    /// True while a scrcpy session for this device is alive.
    pub running: bool,
}

fn sessions() -> &'static Mutex<HashMap<String, Child>> {
    static S: OnceLock<Mutex<HashMap<String, Child>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Absolute path to a Homebrew-installed tool, else the bare name (rely on PATH).
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

/// Are the tools (scrcpy + adb) installed?
pub fn tools_ready() -> bool {
    have("scrcpy") && have("adb")
}

fn adb() -> Command {
    Command::new(bin("adb"))
}

/// Parse `adb devices -l`. A tcp/ip serial (`1.2.3.4:5555`) is a wireless link.
pub fn list_devices() -> Vec<Device> {
    if !have("adb") {
        return Vec::new();
    }
    let out = match adb().arg("devices").arg("-l").output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let running = {
        let mut s = sessions().lock().unwrap();
        reap(&mut s);
        s.keys().cloned().collect::<std::collections::HashSet<_>>()
    };
    let mut devices = Vec::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let serial = parts.next().unwrap_or("").to_string();
        let state = parts.next().unwrap_or("");
        if serial.is_empty() || state != "device" {
            continue; // skip "offline"/"unauthorized"
        }
        let model = line
            .split_whitespace()
            .find_map(|t| t.strip_prefix("model:"))
            .unwrap_or("Android")
            .replace('_', " ");
        let connection = if serial.contains(':') { "wifi" } else { "usb" }.to_string();
        let running = running.contains(&serial);
        devices.push(Device { serial, model, connection, running });
    }
    devices
}

/// Drop finished scrcpy children from the session map.
fn reap(map: &mut HashMap<String, Child>) {
    map.retain(|_, child| matches!(child.try_wait(), Ok(None)));
}

/// Launch scrcpy for `serial` (mirror + control). No-op if already running.
pub fn connect(serial: &str) -> Result<()> {
    if !tools_ready() {
        bail!("scrcpy/adb not installed (brew install scrcpy android-platform-tools)");
    }
    let mut map = sessions().lock().unwrap();
    reap(&mut map);
    if map.contains_key(serial) {
        return Ok(());
    }
    let child = Command::new(bin("scrcpy"))
        .arg("--serial")
        .arg(serial)
        .arg("--window-title")
        .arg(format!("kayıver — {serial}"))
        .arg("--stay-awake")
        // Point scrcpy at our adb so it doesn't need one on PATH.
        .env("ADB", bin("adb"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to launch scrcpy")?;
    map.insert(serial.to_string(), child);
    Ok(())
}

/// Stop the scrcpy session for `serial`.
pub fn disconnect(serial: &str) -> Result<()> {
    let mut map = sessions().lock().unwrap();
    if let Some(mut child) = map.remove(serial) {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// Arm wireless adb for a USB-connected device and connect to it over TCP/IP, so
/// the cable can be unplugged. Returns the `ip:port` now reachable.
pub fn enable_wireless(serial: &str) -> Result<String> {
    if !have("adb") {
        bail!("adb not installed");
    }
    // Device's own IP on wlan0.
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
    // Switch the device's adbd to TCP/IP.
    adb().args(["-s", serial, "tcpip", "5555"]).output().context("adb tcpip failed")?;
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let addr = format!("{ip}:5555");
    adb().args(["connect", &addr]).output().context("adb connect failed")?;
    Ok(addr)
}
