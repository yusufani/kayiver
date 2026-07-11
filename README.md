# drift

**Share one keyboard and mouse across your machines — seamlessly.**

drift is a lightweight, open-source software KVM. Slide your cursor off the
edge of one machine's screen and it appears on the next, exactly like moving
between two monitors of the same computer. Keyboard input follows the cursor.

- **Native feel, no lag** — a single ~4 MB Rust binary per machine, raw OS
  input APIs (CGEventTap / low-level hooks), relative mouse deltas over
  TCP+`TCP_NODELAY` on your LAN. No Electron, no runtime, no daemon zoo.
- **No cursor lock-ups** — the machine that owns the physical mouse flips
  into forwarding mode *inside the OS input callback*, so not one event
  leaks or double-applies during a crossing. A triple-tap of `Esc` always
  yanks the cursor home, even if the remote machine hangs.
- **VPN-proof** — connections try a static address before mDNS discovery,
  so a corporate VPN that blocks multicast doesn't break anything.
- **Secure by default** — one-time PIN pairing (SPAKE2), then every session
  is end-to-end encrypted (Noise `NNpsk0`, ChaCha20-Poly1305).
- **Autostart** — `drift autostart enable` and it's just *there* after boot.

| Platform | Give input (host) | Receive input (client) |
|----------|:-:|:-:|
| macOS    | ✅ | ✅ |
| Windows 10/11 | ✅ | ✅ |
| Linux    | 🚧 planned | 🚧 planned |
| Android  | — | 🚧 planned (see [docs/PLATFORMS.md](docs/PLATFORMS.md)) |
| iOS/iPadOS | 🚧 controller only | ❌ OS restriction (see [docs/PLATFORMS.md](docs/PLATFORMS.md)) |

## Quick start

Build (Rust 1.85+):

```sh
cargo build --release          # -> target/release/drift
```

Cross-compile a Windows binary from macOS/Linux (no Rust needed on the
Windows box): install mingw-w64 (`brew install mingw-w64`), then

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu   # -> drift.exe
```

**1. Pair** (once). On the machine that has the keyboard/mouse:

```sh
drift pair
# shows a 6-digit PIN and this machine's IP
```

On the other machine:

```sh
drift join <host-ip>
# type the PIN
```

**2. Run** both sides:

```sh
drift run
```

**3. Arrange your screens** (drag & drop):

```sh
drift ui
```

opens the visual layout editor in your browser — drag the machines to match
your desk; touching edges become crossings. Saving applies **live** to a
running host, no restart needed. (Prefer a file? `drift config-path` works
too.)

**4. Make it permanent:**

```sh
drift autostart enable
```

Now push your cursor against the edge between the machines. That's it.

macOS will ask for **Accessibility** and **Input Monitoring** permissions on
first run (System Settings → Privacy & Security). `drift doctor` shows what's
missing.

## Layout

Pairing creates a default layout (new machine to the right of the host).
`drift ui` is the comfortable way to change it; under the hood it writes:

```toml
[[layout.links]]
from = "mac-studio"     # when mac-studio's cursor exits its...
edge = "right"          # ...right edge...
to = "win-desktop"      # ...it enters win-desktop (from the left)
```

Links are bidirectional; positions map proportionally between different
resolutions. Any edge (`left`/`right`/`top`/`bottom`) works, and you can
chain machines: `mac ⇄ win ⇄ tablet`.

## On a VPN / multicast-blocked network?

Pairing stores the host's address, and clients always try it before mDNS:

```toml
[[peers]]
name = "mac-studio"
addr = "10.8.0.3:24817"   # update if the host IP changes
```

Only TCP port **24817** (configurable) between the machines is required.

## Troubleshooting

| Symptom | Fix |
|---|---|
| Cursor won't cross | `drift doctor` on both sides: are they connected? Portal edges only arm when the peer is online. |
| Cursor stuck on remote machine | Triple-tap `Esc` — input snaps back to the host. |
| "CGEventTapCreate failed" on macOS | Grant Accessibility + Input Monitoring to your terminal (or the drift binary), then restart drift. |
| Not discovered over Wi-Fi | Multicast may be filtered; set `addr` on the client's peer entry (see above). |
| Keys stuck after crossing | Shouldn't happen (both sides release held keys on every focus change) — file a bug with `RUST_LOG=debug` output. |

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — components, threads, the crossing state machine, latency design
- [docs/PROTOCOL.md](docs/PROTOCOL.md) — wire protocol specification
- [docs/SECURITY.md](docs/SECURITY.md) — threat model, pairing & session crypto
- [docs/PLATFORMS.md](docs/PLATFORMS.md) — per-OS implementation notes, Android/iOS plans, shared-monitor (DDC/CI) story
- [docs/ROADMAP.md](docs/ROADMAP.md) — what's next

## License

MIT — see [LICENSE](LICENSE).
