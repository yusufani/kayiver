# Architecture

## Components

```
kayiver/
├── crates/kayiver-core     platform-agnostic library (fully unit-testable)
│   ├── proto             wire messages, HID-based input model
│   ├── layout            virtual screen arrangement, edge/"portal" math
│   ├── config            config.toml load/save
│   ├── pairing           SPAKE2 PIN pairing -> per-peer PSK
│   ├── secure            Noise NNpsk0 encrypted transport
│   ├── discovery         mDNS advertise/browse (_kayiver._tcp)
│   └── wire              u16-length-prefixed framing
└── apps/kayiver            the binary
    ├── engine/host       input router (the machine with the kb/mouse)
    ├── engine/client     input applier (screen-only machines)
    ├── engine/pairing    `kayiver pair` / `kayiver join` CLI flows
    ├── platform/{macos,windows,stub}   capture + injection backends
    ├── keymap            HID usage <-> native virtual-key tables
    └── autostart         LaunchAgent / registry Run key
```

Roles are static (set during pairing): the **host** owns the physical
keyboard/mouse; **clients** receive input. One host, N clients; clients can
be chained in the layout (`mac ⇄ win ⇄ tablet`) — routing always goes
through the host.

## Threads (host)

```
┌────────────────────┐  UnboundedSender   ┌─────────────────────────────┐
│ OS capture thread   │ ─────────────────> │ tokio runtime               │
│ (CGEventTap runloop │   Captured::*      │  ├─ router (focus machine)  │
│  or LL-hook pump)   │ <─ AtomicBool ──── │  ├─ accept loop             │
└────────────────────┘   forwarding flag   │  └─ per-client reader+writer│
                                           └─────────────────────────────┘
```

The capture thread is the *only* latency-critical path. It never allocates
on the hot path, never blocks on the network, and communicates with the
router via an unbounded channel (send = one atomic push).

## The crossing state machine

**Local mode.** All input passes through to the local OS untouched. On every
mouse move the capture callback checks the cursor against *armed* portal
edges (an edge is armed only while the machine behind it has a live
session — the cursor can never vanish into an offline screen).

**Edge hit.** The capture callback itself flips the `forwarding` atomic and
freezes the local cursor, *synchronously inside the OS callback*. This is
the core design decision: by the time the router (async land) even hears
about the crossing, the very next input event is already being swallowed.
There is no window where events double-apply on both machines, and no
round-trip anywhere on the decision path.

- macOS: `CGAssociateMouseAndMouseCursorPosition(false)` + hide cursor —
  deltas keep flowing to the tap with the cursor pinned (the FPS-game grab).
- Windows: events are swallowed by the LL hook (cursor freezes), parked a
  safe inset from the edge so proposed positions never clamp against the
  desktop bounds.

**Forwarding mode.** Every event is translated to the wire model (HID key
usages, relative mouse deltas, 1/120-notch wheel units) and sent to the
focused client. The router coalesces bursts of queued mouse moves so a
network hiccup can never build a backlog of stale motion. The client owns
its cursor position: it accumulates deltas, clamps to its own desktop
bounds, and injects. Different resolutions/DPI need no negotiation.

**Return.** When the client's cursor pushes through one of *its* portal
edges, it sends `CursorLeft{edge, ratio}` and stops applying input
immediately (in-flight events are dropped, so no post-handoff twitch). The
host maps the edge through the layout: back home (warp local cursor to the
mapped point, un-freeze), or on to another client (send it `Enter`).

**Failure paths, all leading home:**

| Event | Handling |
|---|---|
| Focused client disconnects | Router exits forwarding, cursor returns |
| Client wedged / network dead | Session watchdog (15 s) kills it → same as above |
| User panic | Triple-`Esc` within 900 ms, handled in the capture thread itself — works even if the router or network is stuck |
| Key/button held during any transition | Both sides send/perform releases for everything held (host tracks the set it forwarded; client tracks the set it injected) |
| Re-trigger on arrival | Warp lands inset from the edge + 300 ms portal cooldown |

## Latency design

Budget for cursor motion, host edge → client screen (LAN):

| Stage | Cost |
|---|---|
| OS callback → channel push | < 10 µs (one atomic, no lock on hot path) |
| Router → Noise encrypt → TCP write | ~20 µs (ChaCha20 on 8-byte payload) |
| LAN RTT (one direction) | 100–500 µs wired, 1–3 ms Wi-Fi |
| Client decrypt → `SendInput`/`CGEventPost` | < 50 µs |

TCP with `TCP_NODELAY` is deliberate: on a LAN the RTT dwarfs everything
else and UDP would buy nothing measurable while costing ordering,
loss-handling and NAT/VPN behavior. Every message is tiny (a mouse move is
~6 bytes + 16-byte AEAD tag + 2-byte frame header), so there is no
fragmentation and no head-of-line blocking in practice. If a future
measurement disagrees, the transport is isolated in `kayiver-core::secure`
and can grow a UDP path without touching the engines.

Mouse motion is sent **relative** (raw deltas), which is also what makes
crossing feel native: OS pointer acceleration is applied exactly once, on
the receiving side's absolute-position accumulation, and multi-monitor
setups *within* one machine keep working natively since the local OS handles
its own internal edges with zero kayiver involvement.

## Connection lifecycle

```
client                                   host
  │ TCP connect (static addr, else mDNS)   │
  │ ── Intro::Session{name} (plaintext) ─> │  look up PSK for name
  │ <───── Noise NNpsk0 handshake ───────> │  (2 messages)
  │ ── Hello{version, screen} ───────────> │
  │ <───────── Welcome{portal_edges} ───── │  session live, portal armed
  │ <── Ping ── every 5 s ──── Pong ─────> │  watchdogs both sides
```

Clients reconnect forever with 1→5 s backoff — sleep/wake, DHCP changes,
VPN toggles and host restarts all heal without user action.
