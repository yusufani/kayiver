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

/// The client as real pairing leaves it: with its OWN copy of the shared
/// panel config (local = its panel copy, peer = the host's), and a stale,
/// restart-restored belief that the panel is currently its.
fn client_cfg_shared(port: u16) -> String {
    format!(
        r#"{}
[[layout.links]]
from = "simwin"
edge = "left"
to = "simhost"

[shared_monitor]
local_index = 0
peer = "simhost"
peer_index = 2
hotkey = true
last_owner = "simwin"

[shared_monitor.local_rect]
x = 0
y = 0
w = 2560
h = 1440

[shared_monitor.peer_rect]
x = 2560
y = 0
w = 2560
h = 1440
"#,
        client_cfg(port)
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
    // The host pushes its editor view on connect; the client's /api/state
    // must mirror it (machines by name, links, shared panel) — that is what
    // keeps the two editors in sync.
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

    // Mac-modifier remap: the sim host build runs on macOS and the peer's
    // config says os = "windows". Defaults: ⌘ (0xE3) → Ctrl (224),
    // ⌥ (0xE2) → Win (227), ⌃ (0xE0) → Ctrl (224). Press+release both.
    client.injected();
    for (send, want) in [(0xE3u16, 224i64), (0xE2u16, 227i64), (0xE0u16, 224i64)] {
        for pressed in [true, false] {
            let r = host.ctl(serde_json::json!({ "op": "input_key", "key": send, "pressed": pressed }));
            assert!(r["ok"].as_bool().unwrap(), "key inject failed: {r}");
        }
        let mut got = Vec::new();
        wait_until("client receives the remapped key", Duration::from_secs(5), || {
            got.extend(client.injected());
            got.iter().filter(|e| e["kind"] == "key").count() >= 2
        });
        for e in got.iter().filter(|e| e["kind"] == "key") {
            assert_eq!(e["key"].as_i64().unwrap(), want, "HID {send:#x} must remap to {want}");
        }
    }
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

/// The headline of the symmetric engine: the machine that used to be a
/// pure "client" can now take control of the other one. It arms a portal edge
/// of its own (adopting the link from the peer's Welcome, since pairing never
/// wrote one on that side), grabs the cursor on an edge hit, and the peer
/// becomes the one being driven.
#[test]
fn client_can_take_control_of_the_host() {
    let (mut host, mut client) = desk("clientdrive", 27260);
    wait_until("client arms a portal edge of its own", Duration::from_secs(15), || {
        client.state()["portals"].as_array().is_some_and(|a| !a.is_empty())
    });
    host.injected(); // drain anything stale

    assert!(client.ctl(serde_json::json!({ "op": "edge", "edge": "left", "ratio": 0.5 }))["ok"]
        .as_bool()
        .unwrap());
    wait_until("client takes control", Duration::from_secs(5), || {
        client.state()["forwarding"].as_bool().unwrap_or(false)
    });
    // The host is now the driven side: it injects what the client sends.
    wait_until("host injects the client's input", Duration::from_secs(10), || {
        client.ctl(serde_json::json!({ "op": "input_move", "dx": 12, "dy": 0 }));
        !host.injected().is_empty()
    });
    // While driven, the host's own edges are disarmed — a physical nudge there
    // must not start a second, competing crossing.
    assert!(
        host.state()["portals"].as_array().unwrap().is_empty(),
        "driven side must disarm its portal edges"
    );
}

/// Bug class #6: every route for switching the shared panel — hotkey, tray,
/// editor button, `kayiver monitor` — used to work only on the machine running
/// the router. On the other machine the hotkey reached no hook at all and the
/// editor button 400'd into an empty `catch`, so the panel silently refused to
/// switch. The client now captures locally (portals stay empty, so it can
/// never grab the cursor) and asks the router over the wire.
#[test]
fn client_can_switch_the_shared_panel() {
    let (mut host, mut client) = desk("clientflip", 27250);
    // Panel starts with the host; press the hotkey on the CLIENT.
    assert!(client.ctl(serde_json::json!({ "op": "hotkey" }))["ok"].as_bool().unwrap());
    wait_until("host hands the panel over and blocks its own copy", Duration::from_secs(10), || {
        !host.state()["blocked"].is_null()
    });
    assert!(host.config_text().contains(r#"last_owner = "simwin""#));

    // And back again, so this isn't a one-way latch.
    assert!(client.ctl(serde_json::json!({ "op": "hotkey" }))["ok"].as_bool().unwrap());
    wait_until("host takes the panel back", Duration::from_secs(10), || {
        host.state()["blocked"].is_null()
    });
}

/// Bug class #7: reclaiming the panel while still forwarding to the peer must
/// pull input home, not just flip the display-side bookkeeping.
///
/// The real desk hit this by crossing HOST→panel (forwarding starts), then
/// pressing the hotkey again before ever leaving through a portal — exactly
/// what happens when the panel is flipped back with the peer's cursor still
/// resting on it. `set_shared_owner` used to only touch `blocked` and the
/// persisted owner, leaving `forwarding` (and focus) untouched: the panel
/// visibly showed the host again while every keystroke and mouse move kept
/// going to the peer, with no portal left to cross back through since the
/// host was never actually "away". That reads as the desk being stuck on the
/// peer's side.
#[test]
fn reclaiming_panel_mid_forward_pulls_input_home() {
    let (mut host, mut client) = desk("reclaim", 27310);
    give_panel_to_client(&mut host);
    cross_diagonally(&mut host, &mut client);
    assert!(host.state()["forwarding"].as_bool().unwrap(), "handover must have started forwarding");

    // Flip the panel back to the host WITHOUT the peer ever reporting a
    // portal exit (CursorLeft) — the hotkey/editor/physical-switch path.
    assert!(host.ctl(serde_json::json!({ "op": "hotkey" }))["ok"].as_bool().unwrap());
    wait_until("host stops forwarding once it reclaims the panel", Duration::from_secs(5), || {
        !host.state()["forwarding"].as_bool().unwrap()
    });
    wait_until("host unblocks its own panel copy", Duration::from_secs(5), || {
        host.state()["blocked"].is_null()
    });

    // The client's cursor was left sitting on ITS copy of the panel — which
    // the reclaim just blocked. Its guard must NOT read that as a fresh
    // "moved onto the shared panel" and hand control straight back to us:
    // that put the host into "driven by the client" with nobody at the
    // client's desk, and while driven the host's own guard is a no-op, so
    // the host could never cross onto the panel again (the real-desk "stuck
    // on the Mac" symptom).
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        !host.state()["driven"].as_bool().unwrap(),
        "host must not end up driven by a client nobody is sitting at:\n{}",
        host.log_text()
    );
    assert!(!host.log_text().contains("is driving this desk"), "spurious EnterAt after reclaim");
    // Its cursor must end up on a monitor that is actually showing it — not
    // on the hidden panel, and not skipped out to x=-1 off the desktop (the
    // panel's left edge is also that desk's portal edge).
    wait_until("client parks its cursor on a visible monitor", Duration::from_secs(5), || {
        let st = client.state();
        let c = st["cursor"].as_array().unwrap();
        let (x, y) = (c[0].as_i64().unwrap(), c[1].as_i64().unwrap());
        let inside = |r: &serde_json::Value| {
            let r = r.as_array().unwrap();
            let (rx, ry, rw, rh) =
                (r[0].as_i64().unwrap(), r[1].as_i64().unwrap(), r[2].as_i64().unwrap(), r[3].as_i64().unwrap());
            x >= rx && x < rx + rw && y >= ry && y < ry + rh
        };
        !inside(&st["blocked"]) && st["monitors"].as_array().unwrap().iter().any(inside)
    });

    // And the desk is fully usable again: hand the panel over and cross a
    // second time. (Move the host cursor back onto A first — the reclaim
    // warped it onto the panel, and a cursor already resting there when the
    // panel flips is deliberately carried over at once.)
    host.ctl(serde_json::json!({ "op": "warp", "x": 1000, "y": 700 }));
    std::thread::sleep(Duration::from_millis(50));
    give_panel_to_client(&mut host);
    cross_diagonally(&mut host, &mut client);
    assert!(host.state()["forwarding"].as_bool().unwrap(), "second handover must forward again");
}

/// Bug class #10: (re)connecting must never hand control to a desk nobody is
/// sitting at, and the two desks must agree on who the panel shows.
///
/// The real desk hit this at EVERY restart: the client's cursor was left
/// resting on its copy of the panel; the host re-sent its block on connect;
/// the client's guard read "cursor inside a freshly blocked rect" as a real
/// entry and took control of the host — whose own guard is a no-op while
/// driven, so the host could never cross onto the panel again. On top, both
/// sides re-asserted their own restart-restored owner on connect, so the two
/// desks blocked opposite panels.
#[test]
fn reconnect_never_hands_control_to_an_empty_desk() {
    let scenario = "reconnect";
    let base = 27320;
    let mut host = Machine::spawn("host", &host_cfg(base), HOST_MONS, base + 1, scenario);
    let mut client = Machine::spawn("client", &client_cfg_shared(base), CLIENT_MONS, base + 2, scenario);
    // (The sim cursor starts at (100,100): on the client that is ON its panel copy.)
    let settle = |host: &mut Machine, client: &mut Machine, when: &str| {
        wait_until(&format!("client blocks its panel copy ({when})"), Duration::from_secs(10), || {
            !client.state()["blocked"].is_null()
        });
        std::thread::sleep(Duration::from_millis(700));
        assert!(!host.state()["driven"].as_bool().unwrap(), "{when}: host driven by an empty desk:\n{}", host.log_text());
        assert!(!host.log_text().contains("is driving this desk"), "{when}: spurious handover");
        assert!(host.state()["blocked"].is_null(), "{when}: client's stale owner blocked the host's panel");
        assert!(!client.state()["forwarding"].as_bool().unwrap(), "{when}: client forwarding to nobody");
        let st = client.state();
        let c = st["cursor"].as_array().unwrap();
        let (x, y) = (c[0].as_i64().unwrap(), c[1].as_i64().unwrap());
        assert!(!(x < 2560 && y >= 0), "{when}: client cursor still on the hidden panel at ({x},{y})");
    };
    settle(&mut host, &mut client, "first connect");
    // The client adopted the arbiter's owner, and persisted it.
    wait_until("client persists the adopted owner", Duration::from_secs(5), || {
        client.config_text().contains(r#"last_owner = "simhost""#)
    });

    // Put the client's cursor back on its panel copy and restart the host —
    // the deploy / crash flow, where the host re-sends its block on reconnect.
    client.ctl(serde_json::json!({ "op": "warp", "x": 100, "y": 100 }));
    let dir = host.cfg_dir.clone();
    drop(host);
    let mut host = Machine::spawn_in("host", dir, HOST_MONS, base + 1);
    wait_until("client reconnects to restarted host", Duration::from_secs(20), || {
        host.log_text().contains("client connected: simwin")
    });
    settle(&mut host, &mut client, "after host restart");

    // And the desk works: hand the panel over and cross.
    host.ctl(serde_json::json!({ "op": "warp", "x": 1000, "y": 700 }));
    std::thread::sleep(Duration::from_millis(50));
    give_panel_to_client(&mut host);
    cross_diagonally(&mut host, &mut client);
    assert!(host.state()["forwarding"].as_bool().unwrap());
}

/// Bug class #12: the shared panel's edge that leads to a peer monitor
/// BEYOND it must arm itself from geometry, not wait for a layout link. The
/// link that puts C above the panel is drawn on the PEER's side (C-bottom
/// touches the panel), so it only arms the PEER's edge; this desk's top edge
/// stayed unarmed, the hook never fired, and the cursor could not leave the
/// panel upward toward C at all — the "B shared / Mac active, still can't get
/// to Windows C" bug. Here the host has NO top link (only right), yet must
/// cross up onto C.
#[test]
fn shared_panel_arms_the_edge_to_a_beyond_monitor_without_a_link() {
    let (mut host, mut client) = desk("beyondarm", 27340);
    // Panel starts with the host, so its own copy is not blocked and the
    // cursor can sit on it. C is at (636,-1080) on the client — above the
    // panel copy — so the host's TOP edge should now be armed.
    wait_until("host arms its top edge from panel geometry", Duration::from_secs(10), || {
        host.state()["portals"].as_array().unwrap().iter().any(|e| e == "Top")
    });
    // The host has no top LINK — only the right one from host_cfg.
    assert!(!host.config_text().contains(r#"edge = "top""#), "test premise: no top link exists");

    client.injected(); // drain
    host.ctl(serde_json::json!({ "op": "warp", "x": 3800, "y": 40 }));
    let r = host.ctl(serde_json::json!({ "op": "edge", "edge": "top", "ratio": 3800.0 / 5120.0 }));
    assert!(r["ok"].as_bool().unwrap_or(false), "top edge must be armed and cross: {r}");

    let mut landing = None;
    wait_until("client is driven onto C above the panel", Duration::from_secs(10), || {
        let evs = client.injected();
        landing = evs.iter().find(|e| e["kind"] == "mouse_to" && e["dx"] == 0 && e["dy"] == 0)
            .map(|e| e["y"].as_i64().unwrap());
        landing.is_some()
    });
    assert!(landing.unwrap() < 0, "must land on C (negative y, above the panel), got y={}", landing.unwrap());
    assert!(host.state()["forwarding"].as_bool().unwrap(), "host must be forwarding to the client");
}

/// Bug class #11: the editor's desk arrangement must reach the peer and
/// stick. The real desk: Windows keeps parking C BESIDE its copy of the panel
/// after every KVM switch, while C physically sits ABOVE it — so the panel's
/// top edge became a wall and the right edge led to a monitor that isn't
/// there. Dragging C above in the editor never persisted anywhere (positions
/// are derived from the peer's real geometry on every load). Now: Save pushes
/// the arrangement, the peer applies it to its OS and remembers it, and
/// re-applies it whenever the OS forgets — including across a restart that
/// starts out "beside".
#[test]
fn desk_arrangement_reaches_the_peer_and_survives_it_forgetting() {
    let (mut host, mut client) = desk("arrange", 27330);
    let beside = serde_json::json!([[0, 0, 2560, 1440], [2560, 0, 1920, 1080]]);
    let above = serde_json::json!([[0, 0, 2560, 1440], [636, -1080, 1920, 1080]]);
    let mons_of = |m: &mut Machine| m.state()["monitors"].clone();

    // The client's OS has C beside the panel.
    assert!(client.ctl(serde_json::json!({ "op": "set_monitors", "monitors": beside }))["ok"].as_bool().unwrap());
    wait_until("host learns the 'beside' geometry", Duration::from_secs(10), || {
        host.log_text().contains("simwin: geometry update")
    });

    // Editor Save on the host: C above.
    let r = host.ctl(serde_json::json!({ "op": "arrange", "machine": "simwin", "monitors": above }));
    assert!(r["ok"].as_bool().unwrap(), "{r}");
    wait_until("client's OS arrangement follows the editor", Duration::from_secs(10), || mons_of(&mut client) == above);
    wait_until("client remembers it", Duration::from_secs(5), || client.config_text().contains("[[arrangement]]"));
    wait_until("host sees C above", Duration::from_secs(10), || {
        host.log_text().contains("[Rect { x: 0, y: 0, w: 2560, h: 1440 }, Rect { x: 636, y: -1080, w: 1920, h: 1080 }]")
    });

    // The OS forgets (KVM switch): the client puts it back on its own.
    assert!(client.ctl(serde_json::json!({ "op": "set_monitors", "monitors": beside }))["ok"].as_bool().unwrap());
    wait_until("client re-applies after the OS forgot", Duration::from_secs(10), || mons_of(&mut client) == above);

    // And across a restart whose OS starts out "beside".
    let dir = client.cfg_dir.clone();
    drop(client);
    let mut client = Machine::spawn_in("client", dir, "0,0,2560,1440;2560,0,1920,1080", 27332);
    wait_until("restarted client applies the remembered arrangement", Duration::from_secs(10), || {
        mons_of(&mut client) == above
    });
}

/// Bug class #8: landing on a peer monitor BEYOND the shared panel must keep
/// a real margin off the panel's edge, not one that evaporates when the panel
/// and the peer's copy of it are different sizes.
///
/// The real desk hit this with a 2560-wide panel over a 1920-wide Windows
/// copy (scale 4/3): the landing point came out one peer pixel off the
/// boundary — enough on paper, but thin enough that the handover "crossed"
/// and then immediately bounced back, because the margin had been computed
/// as EDGE_INSET pixels on THIS side and only divided down to peer pixels
/// afterwards. This scenario uses a starker, differently-shaped desk — a 4x
/// scale between the panel and its peer copy — where that division rounds
/// the margin to exactly zero: the old landing sat AT the boundary instead of
/// past it.
#[test]
fn beyond_panel_landing_keeps_native_margin_across_scale() {
    let port = 27290;
    let host_toml = format!(
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
w = 1200
h = 400

[[peers.screens]]
x = 1200
y = 0
w = 1200
h = 400

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
x = 1000
y = 0
w = 4800
h = 1600

[shared_monitor.peer_rect]
x = 0
y = 0
w = 1200
h = 400
"#
    );
    let client_toml = format!(
        r#"name = "simwin"
mode = "client"
port = {port}

[[peers]]
name = "simhost"
psk = "{PSK}"
addr = "127.0.0.1:{port}"
"#
    );

    // Host: a small main screen A + a panel B four times the size of its
    // peer copy in each axis. Client: B's real-size copy + C right beside it.
    let mut host = Machine::spawn("host", &host_toml, "0,0,1000,1600;1000,0,4800,1600", port + 1, "scale");
    let mut client = Machine::spawn("client", &client_toml, "0,0,1200,400;1200,0,1200,400", port + 2, "scale");
    wait_until("host sees the client", Duration::from_secs(15), || {
        host.log_text().contains("client connected: simwin")
    });
    client.injected(); // drain
    host.ctl(serde_json::json!({ "op": "warp", "x": 5750, "y": 300 }));
    std::thread::sleep(Duration::from_millis(40));

    // Hit the desktop's right edge at y = 300 (ratio 300/1600), straight past
    // the panel onto C — same trigger the real capture layer sends when the
    // cursor is pushed off the physical screen edge.
    let r = host.ctl(serde_json::json!({ "op": "edge", "edge": "right", "ratio": 300.0 / 1600.0 }));
    assert!(r["ok"].as_bool().unwrap_or(false), "edge must be armed: {r}");

    let mut landing = None;
    wait_until("client receives the EnterAt warp", Duration::from_secs(10), || {
        let evs = client.injected();
        landing = evs
            .iter()
            .find(|e| e["kind"] == "mouse_to" && e["dx"] == 0 && e["dy"] == 0)
            .map(|e| (e["x"].as_i64().unwrap(), e["y"].as_i64().unwrap()));
        landing.is_some()
    });
    let (x, y) = landing.unwrap();
    // C starts at x=1200 on the peer; the landing must clear it by a full,
    // UNSCALED EDGE_INSET — not the ~0px that dividing the inset by a 4x
    // scale leaves. The old code landed exactly AT x=1200.
    assert!(x >= 1202, "must clear C's left edge by a real margin, got x={x} (was landing AT the boundary)");
    assert!((70..=80).contains(&y), "must keep the entry height (~75), got y={y}");
    assert!(
        host.log_text().contains("shared geometry: right edge -> peer monitor"),
        "must have crossed via shared geometry, not a wall or a stale link:\n{}",
        host.log_text()
    );
}

/// Bug class #9: landing on a peer monitor beyond the panel must NOT flip the
/// panel's owner. The physical panel is still showing this side; kayiver
/// cannot switch a monitor's input, only follow it. An earlier fix flipped
/// the owner "so the peer's blocked copy next to the cursor cannot bounce
/// the handover" — and desynced kayiver from reality on the real desk: the
/// panel kept showing the Mac while kayiver believed Windows had it, so
/// every later move onto the panel vanished into Windows and came straight
/// back. Physically, dipping from C back onto the panel IS a seam crossing
/// home, and that is exactly what must happen.
#[test]
fn beyond_panel_crossing_keeps_the_owner_and_dips_home_through_the_seam() {
    let port = 27300;
    let host_toml = format!(
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
w = 1200
h = 400

[[peers.screens]]
x = 1200
y = 0
w = 1200
h = 400

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
x = 1000
y = 0
w = 4800
h = 1600

[shared_monitor.peer_rect]
x = 0
y = 0
w = 1200
h = 400
"#
    );
    let client_toml = format!(
        r#"name = "simwin"
mode = "client"
port = {port}

[[peers]]
name = "simhost"
psk = "{PSK}"
addr = "127.0.0.1:{port}"
"#
    );

    let mut host = Machine::spawn("host", &host_toml, "0,0,1000,1600;1000,0,4800,1600", port + 1, "noboun");
    let mut client = Machine::spawn("client", &client_toml, "0,0,1200,400;1200,0,1200,400", port + 2, "noboun");
    wait_until("host sees the client", Duration::from_secs(15), || {
        host.log_text().contains("client connected: simwin")
    });
    // The panel starts owned by the host — never handed to the client — so
    // the client's copy of it is blocked when the crossing happens.
    wait_until("client's panel copy is blocked", Duration::from_secs(5), || {
        !client.state()["blocked"].is_null()
    });

    host.ctl(serde_json::json!({ "op": "warp", "x": 5750, "y": 300 }));
    std::thread::sleep(Duration::from_millis(40));
    let r = host.ctl(serde_json::json!({ "op": "edge", "edge": "right", "ratio": 300.0 / 1600.0 }));
    assert!(r["ok"].as_bool().unwrap_or(false), "edge must be armed: {r}");

    wait_until("client receives the EnterAt warp", Duration::from_secs(10), || {
        client.injected().iter().any(|e| e["kind"] == "mouse_to" && e["dx"] == 0 && e["dy"] == 0)
    });

    // Ownership stays where the monitor's input physically is: with the host.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!client.state()["blocked"].is_null(), "client's panel copy must stay blocked (owner unchanged)");
    assert!(!host.config_text().contains(r#"last_owner = "simwin""#), "owner must not follow the cursor past the panel");
    assert!(host.state()["forwarding"].as_bool().unwrap(), "host must still be forwarding to C");

    // Dip back toward the panel by more than any pixel margin. On the peer
    // that is a move from C onto its (blocked) copy of the panel — which is
    // showing the HOST — so control comes home onto the host's panel. A real
    // seam, not a bounce: the cursor lands where the user is looking.
    let r = host.ctl(serde_json::json!({ "op": "input_move", "dx": -120, "dy": 0 }));
    assert!(r["ok"].as_bool().unwrap(), "host must still be forwarding: {r}");
    wait_until("dip onto the panel brings control home", Duration::from_secs(5), || {
        !host.state()["forwarding"].as_bool().unwrap()
    });
    assert!(host.log_text().contains("onto shared panel"), "must come home through the panel seam:\n{}", host.log_text());
    assert!(!client.state()["blocked"].is_null(), "still the host's panel afterwards");
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

/// The plain crossing, and the one a user actually performs: push STRAIGHT
/// right off A's main screen onto the panel while the panel is showing B.
///
/// On any desk where the panel fills the desktop edge facing the peer, that
/// edge is a legitimate wall (`shared_edge_is_wall`) — so this seam is the ONLY
/// route from A to B, on every such desk, whatever the monitor sizes are. If it
/// needs a diagonal or a particular entry height to fire, the desk has no
/// usable A→B crossing at all and the user is left going the long way round.
#[test]
fn straight_push_onto_the_panel_hands_over() {
    let (mut host, mut client) = desk("straight", 27270);
    give_panel_to_client(&mut host);
    client.injected(); // drain

    // Dead horizontal: dy = 0 the whole way, crossing x = 2560.
    for (x, y) in [(2300, 700), (2450, 700), (2550, 700), (2600, 700)] {
        host.ctl(serde_json::json!({ "op": "warp", "x": x, "y": y }));
        std::thread::sleep(Duration::from_millis(40));
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
    let (x, y) = landing.unwrap();
    assert_eq!(x, 2, "must land just inside the panel's LEFT edge, got x={x}");
    assert!((695..=705).contains(&y), "must keep the entry height (~700), got y={y}");
    assert!(
        host.state()["forwarding"].as_bool().unwrap(),
        "host must be forwarding after the handover"
    );
}

/// Bug class #6: **the crossing rule must not be this desk's numbers.**
///
/// Every other scenario here runs on one shape — panel on the RIGHT, two
/// 2560x1440 screens, peer's copy at the origin — which is exactly the shape
/// that would let a hardcoded constant pass for a rule. So this one is
/// deliberately nothing like it and shares no number with it:
///
/// - the panel is on the **left**, not the right,
/// - it is a 3440x1440 ultrawide, and the main screen is 1920x1080,
/// - the host's desktop origin is negative,
/// - the peer's copy sits at a different origin with a 1280x1024 beside it.
///
/// Everything the handover needs — which edge is a wall, where the seam is,
/// where the cursor lands on the far side — has to fall out of the geometry.
/// If any of it were tuned to the other desk, this test is what says so.
#[test]
fn crossing_is_derived_from_geometry_not_from_one_desk() {
    let port = 27280;
    let host_toml = format!(
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
w = 3440
h = 1440

[[peers.screens]]
x = 3440
y = -200
w = 1280
h = 1024

[[layout.links]]
from = "simhost"
edge = "left"
to = "simwin"

[shared_monitor]
local_index = 1
peer = "simwin"
peer_index = 0
hotkey = true

[shared_monitor.local_rect]
x = -3440
y = 0
w = 3440
h = 1440

[shared_monitor.peer_rect]
x = 0
y = 0
w = 3440
h = 1440
"#
    );
    let client_toml = format!(
        r#"name = "simwin"
mode = "client"
port = {port}

[[peers]]
name = "simhost"
psk = "{PSK}"
addr = "127.0.0.1:{port}"
"#
    );

    // Panel LEFT of a smaller main screen; peer's copy elsewhere entirely.
    let mut host = Machine::spawn("host", &host_toml, "-3440,0,3440,1440;0,0,1920,1080", port + 1, "geom");
    let mut client = Machine::spawn("client", &client_toml, "0,0,3440,1440;3440,-200,1280,1024", port + 2, "geom");
    wait_until("host sees the client", Duration::from_secs(15), || {
        host.log_text().contains("client connected: simwin")
    });

    give_panel_to_client(&mut host);
    client.injected();

    // Push LEFT off the 1920x1080 main screen, across x = 0, onto the panel.
    for (x, y) in [(300, 600), (150, 600), (30, 600), (-40, 600)] {
        host.ctl(serde_json::json!({ "op": "warp", "x": x, "y": y }));
        std::thread::sleep(Duration::from_millis(40));
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
    let (x, y) = landing.unwrap();
    // Entered through the panel's RIGHT side, so it must land just inside it —
    // near 3440, nowhere near the other desk's 2560.
    assert!((3430..3440).contains(&x), "must land just inside the panel's RIGHT edge, got x={x}");
    assert!((595..=605).contains(&y), "must keep the entry height (~600), got y={y}");
    assert!(
        host.state()["forwarding"].as_bool().unwrap(),
        "host must be forwarding after the handover"
    );
}
