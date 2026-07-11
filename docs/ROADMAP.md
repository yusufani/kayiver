# Roadmap

## v0.1 — now
- [x] macOS ⇄ Windows keyboard/mouse sharing, edge crossing, PIN pairing,
      encrypted sessions, mDNS + static-addr discovery, reconnect,
      autostart, panic escape, `drift doctor`

## v0.2 — polish the two-desktop experience
- [ ] Clipboard sync (text first; images later) — follows the same
      host⇄client channel, size-capped
- [ ] Hide the parked cursor on Windows while forwarding (transparent
      overlay window)
- [ ] Pixel-precise / momentum scrolling passthrough (macOS trackpads)
- [ ] React to display reconfiguration without restart (hotplug monitors)
- [ ] Keyboard hotkey to switch focus without touching the mouse
- [ ] OS keychain storage for PSKs
- [ ] `drift status` (IPC to the running instance) + `drift pair` while running
- [ ] Prebuilt binaries (GitHub releases, signed/notarized) + CI

## v0.3 — more platforms & the shared monitor
- [ ] Linux backend (Wayland `libei`, X11 XTest)
- [ ] **DDC/CI monitor input switching**: flip a shared monitor's input
      source automatically when focus crosses to the machine on its other
      cable (`[monitor]` config mapping machines → VCP 0x60 values)
- [ ] Android client (AccessibilityService + UniFFI; see PLATFORMS.md)
- [ ] Any-machine-as-host (role negotiation instead of static roles)

## v0.4+
- [ ] iPad controller mode (touchscreen as trackpad for the mesh)
- [ ] File drag-and-drop between machines
- [ ] Optional UDP transport for >5 ms-RTT networks (measure first)
- [ ] Tray/menu-bar UI (keep the core dependency-free; UI stays optional)

Contributions welcome — the platform backends are intentionally isolated
behind a small trait-like surface (`apps/drift/src/platform/mod.rs`), and
everything else is portable Rust with unit tests.
