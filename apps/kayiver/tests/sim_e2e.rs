//! End-to-end regression suite on a VIRTUAL desk (`cargo test --features sim`).
//!
//! Each test spawns two real `kayiver` processes — a host and a client, each
//! with its own config dir, so each acts like its own machine — connected over
//! real TCP with the real Noise handshake. Only the OS layer is simulated
//! (virtual monitors, virtual cursor, recorded injection), driven through the
//! sim control socket.
//!
//! The scenarios are the bug classes that actually bit on the real desk:
//!   1. diagonal shared-panel entry must land at the entry height
//!   2. a primary-display switch on the client must re-derive the panel rect
//!   3. the panel vanishing locally must NOT re-anchor onto a same-size screen
//!   4. shared-panel ownership must survive a host restart
//!   5. heavy traffic + geometry churn must never desync the Noise nonce

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// base64 of 32 zero bytes — both sides share it, replacing real pairing.
const PSK: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// Mac-like host: A (main) + its copy of the shared panel B to the right.
const HOST_MONS: &str = "0,0,2560,1440;2560,0,2560,1440";
/// Windows-like client: its copy of B (primary) + a 1080p C above it.
const CLIENT_MONS: &str = "0,0,2560,1440;636,-1080,1920,1080";

struct Machine {
    child: Child,
    ctl: Option<BufReader<TcpStream>>,
    cfg_dir: PathBuf,
    log: PathBuf,
    name: &'static str,
}

impl Machine {
    fn spawn(
        name: &'static str,
        cfg_toml: &str,
        monitors: &str,
        ctl_port: u16,
        scenario: &str,
    ) -> Machine {
        let cfg_dir = std::env::temp_dir().join(format!("kayiver-sim-{scenario}-{name}"));
        let _ = std::fs::remove_dir_all(&cfg_dir);
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), cfg_toml).unwrap();
        Machine::spawn_in(name, cfg_dir, monitors, ctl_port)
    }

    /// Start (or restart) a machine on an EXISTING config dir — the deploy /
    /// crash-recovery flow, where persisted state must carry over.
    fn spawn_in(name: &'static str, cfg_dir: PathBuf, monitors: &str, ctl_port: u16) -> Machine {
        let log = cfg_dir.join("kayiver.log");
        let _ = std::fs::remove_file(&log); // fresh log per process lifetime
        let child = Command::new(env!("CARGO_BIN_EXE_kayiver"))
            .args(["run", "--no-gui"])
            .env("KAYIVER_CONFIG_DIR", &cfg_dir)
            .env("KAYIVER_SIM_CTL", ctl_port.to_string())
            .env("KAYIVER_SIM_MONITORS", monitors)
            .env("KAYIVER_LOGFILE", &log)
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn kayiver sim process");
        let mut m = Machine { child, ctl: None, cfg_dir, log, name };
        m.connect_ctl(ctl_port);
        m
    }

    fn connect_ctl(&mut self, port: u16) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    self.ctl = Some(BufReader::new(s));
                    return;
                }
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => panic!("{}: sim ctl port {port} never came up: {e}", self.name),
            }
        }
    }

    fn ctl(&mut self, cmd: serde_json::Value) -> serde_json::Value {
        let r = self.ctl.as_mut().expect("ctl connected");
        writeln!(r.get_mut(), "{cmd}").expect("ctl write");
        let mut line = String::new();
        r.read_line(&mut line).expect("ctl read");
        serde_json::from_str(&line).expect("ctl reply json")
    }

    fn state(&mut self) -> serde_json::Value {
        self.ctl(serde_json::json!({ "op": "state" }))
    }

    fn injected(&mut self) -> Vec<serde_json::Value> {
        self.ctl(serde_json::json!({ "op": "injected" }))["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(self.cfg_dir.join("config.toml")).unwrap_or_default()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_until(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

fn host_cfg(port: u16) -> String {
    format!(
        r#"name = "simhost"
mode = "host"
port = {port}
edge_dwell_ms = 0

[[peers]]
name = "simwin"
psk = "{PSK}"
os = "windows"

[[peers.screens]]
x = 0
y = 0
w = 2560
h = 1440

[[peers.screens]]
x = 636
y = -1080
w = 1920
h = 1080

[[layout.links]]
from = "simhost"
edge = "right"
to = "simwin"

[shared_monitor]
local_index = 2
peer = "simwin"
peer_index = 0
hotkey = true

[shared_monitor.local_rect]
x = 2560
y = 0
w = 2560
h = 1440

[shared_monitor.peer_rect]
x = 0
y = 0
w = 2560
h = 1440
"#
    )
}

fn client_cfg(port: u16) -> String {
    format!(
        r#"name = "simwin"
mode = "client"
port = {port}

[[peers]]
name = "simhost"
psk = "{PSK}"
addr = "127.0.0.1:{port}"
"#
    )
}

/// Spawn a connected host+client pair for one scenario. `base` must be unique
/// per scenario so parallel tests never share a port.
fn desk(scenario: &'static str, base: u16) -> (Machine, Machine) {
    let port = base;
    let mut host = Machine::spawn("host", &host_cfg(port), HOST_MONS, base + 1, scenario);
    let client = Machine::spawn("client", &client_cfg(port), CLIENT_MONS, base + 2, scenario);
    // (The right edge is legitimately a WALL on this desk — the panel fills
    // it and C is above, not beyond — so probe the session, not the portals.)
    wait_until("host sees the client", Duration::from_secs(15), || {
        host.log_text().contains("client connected: simwin")
    });
    (host, client)
}

/// Give the panel to the client (hotkey toggle) and wait for the local block.
fn give_panel_to_client(host: &mut Machine) {
    assert!(host.ctl(serde_json::json!({ "op": "hotkey" }))["ok"].as_bool().unwrap());
    wait_until("host blocks its panel copy", Duration::from_secs(5), || {
        !host.state()["blocked"].is_null()
    });
}

/// Drive the host's virtual cursor from A diagonally into the blocked panel
/// (|dy| > |dx| on the entering step — the motion that used to misread the
/// entry edge) and return the client's resulting warp (x, y).
fn cross_diagonally(host: &mut Machine, client: &mut Machine) -> (i64, i64) {
    client.injected(); // drain anything stale
    for (x, y) in [(2300, 700), (2480, 700), (2550, 700), (2565, 760)] {
        host.ctl(serde_json::json!({ "op": "warp", "x": x, "y": y }));
        std::thread::sleep(Duration::from_millis(40)); // guard polls every 8ms
    }
    let mut landing = None;
    wait_until("client receives the EnterAt warp", Duration::from_secs(10), || {
        let evs = client.injected();
        landing = evs
            .iter()
            .find(|e| e["kind"] == "mouse_to" && e["dx"] == 0 && e["dy"] == 0)
            .map(|e| (e["x"].as_i64().unwrap(), e["y"].as_i64().unwrap()));
        landing.is_some()
    });
    landing.unwrap()
}

// ------------------------------------------------------------ scenarios ----

/// Bug class #1: a diagonal entry (|dy| > |dx|) used to be read as a TOP-edge
/// entry and dumped the cursor in the peer's top-left corner. The entry point
/// of prev→cur crosses B's left edge at y = 740; the peer must be warped to
/// its panel's left inset at that exact height.
#[test]
fn diagonal_cross_lands_at_entry_height() {
    let (mut host, mut client) = desk("cross", 27200);
    give_panel_to_client(&mut host);
    let (x, y) = cross_diagonally(&mut host, &mut client);
    assert_eq!(x, 2, "must land just inside the panel's LEFT edge, got x={x}");
    assert!((735..=745).contains(&y), "must land at the entry height (~740), got y={y}");
    assert!(
        host.state()["forwarding"].as_bool().unwrap(),
        "host must be forwarding after the handover"
    );
}

/// Bug class #2: switching the client's primary display re-anchors every rect
/// (B moved to (-638,1080)); the host's peer_rect must follow within seconds
/// and the next crossing must land inside the panel's NEW location.
#[test]
fn primary_display_switch_rederives_peer_rect() {
    let (mut host, mut client) = desk("primary", 27210);
    let r = client.ctl(serde_json::json!({
        "op": "set_monitors",
        "monitors": [[-638, 1080, 2560, 1440], [0, 0, 1920, 1080]],
    }));
    assert!(r["ok"].as_bool().unwrap());
    wait_until("host re-derives peer_rect to the new panel position", Duration::from_secs(10), || {
        let cfg = host.config_text();
        cfg.contains("x = -638") && cfg.contains("y = 1080")
    });
    give_panel_to_client(&mut host);
    let (x, y) = cross_diagonally(&mut host, &mut client);
    assert_eq!(x, -636, "left inset of the MOVED panel, got x={x}");
    assert!((1815..=1825).contains(&y), "entry height inside the moved panel (~1820), got y={y}");
}

/// Bug class #3: when the panel disappears from the host's own display list
/// (its input switched away for a moment), the re-derivation must NOT re-anchor
/// onto A just because A has the same resolution — that glued the peer's
/// screens onto the wrong monitor ("A suddenly crosses to C").
#[test]
fn vanished_panel_never_reanchors_to_same_size_screen() {
    let (mut host, mut client) = desk("vanish", 27220);
    // Panel gone locally; only A (same 2560x1440!) remains.
    assert!(host.ctl(serde_json::json!({
        "op": "set_monitors", "monitors": [[0, 0, 2560, 1440]],
    }))["ok"]
        .as_bool()
        .unwrap());
    // Nudge the client's geometry so the host runs its re-derivation path.
    assert!(client.ctl(serde_json::json!({
        "op": "set_monitors",
        "monitors": [[0, 0, 2560, 1440], [640, -1080, 1920, 1080]],
    }))["ok"]
        .as_bool()
        .unwrap());
    wait_until("host processed the client geometry update", Duration::from_secs(10), || {
        host.log_text().contains("geometry update")
    });
    let cfg = host.config_text();
    assert!(
        cfg.contains("x = 2560"),
        "local_rect must still point at the (absent) panel, not re-anchor onto A:\n{cfg}"
    );
}

/// Bug class #4: shared-panel ownership must survive a host restart — a
/// deploy used to silently claim the panel back and cover the client's screen
/// (and its fullscreen game) with the notice overlay.
#[test]
fn owner_survives_host_restart() {
    let (mut host, _client) = desk("owner", 27230);
    give_panel_to_client(&mut host);
    wait_until("owner persisted", Duration::from_secs(5), || {
        host.config_text().contains(r#"last_owner = "simwin""#)
    });
    // Kill and restart the host on the SAME config dir — the deploy flow.
    let dir = host.cfg_dir.clone();
    drop(host);
    let mut host = Machine::spawn_in("host", dir, HOST_MONS, 27231);
    wait_until("client reconnects to restarted host", Duration::from_secs(20), || {
        host.log_text().contains("client connected: simwin")
    });
    wait_until("restored owner blocks the host's panel copy", Duration::from_secs(5), || {
        !host.state()["blocked"].is_null()
    });
    assert!(host.config_text().contains(r#"last_owner = "simwin""#));
}

/// Bug class #5: heavy input traffic while the client's geometry watcher ticks
/// used to cancel a frame read mid-bytes and desync the Noise nonce ("decrypt
/// error" disconnect loop). Hammer the session and churn geometry; the session
/// must hold with zero decrypt errors.
#[test]
fn no_nonce_desync_under_load_and_geometry_churn() {
    let (mut host, mut client) = desk("load", 27240);
    give_panel_to_client(&mut host);
    let (_, _) = cross_diagonally(&mut host, &mut client);

    let start = Instant::now();
    let mut flip = false;
    let mut next_churn = Instant::now();
    while start.elapsed() < Duration::from_secs(6) {
        let r = host.ctl(serde_json::json!({ "op": "input_move", "dx": 3, "dy": 1 }));
        assert!(r["ok"].as_bool().unwrap(), "forwarding dropped mid-stream: {r}");
        if Instant::now() >= next_churn {
            next_churn = Instant::now() + Duration::from_secs(1);
            flip = !flip;
            let c = if flip { 638 } else { 636 };
            client.ctl(serde_json::json!({
                "op": "set_monitors",
                "monitors": [[0, 0, 2560, 1440], [c, -1080, 1920, 1080]],
            }));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let host_log = host.log_text();
    let client_log = client.log_text();
    assert!(!client_log.contains("decrypt error"), "client hit decrypt errors:\n{client_log}");
    assert!(!host_log.contains("client disconnected"), "session dropped under load:\n{host_log}");
    let moves = client.injected().iter().filter(|e| e["kind"] == "mouse_to").count();
    assert!(moves > 200, "client should have received a steady stream of motion, got {moves}");
}
