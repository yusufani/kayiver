# Platform notes

## macOS

- **Capture**: `CGEventTap` at the HID level on a dedicated run-loop thread.
  macOS auto-disables taps it deems slow; kayiver re-enables inside the
  callback (`kCGEventTapDisabledByTimeout`).
- **Freeze during forwarding**: `CGAssociateMouseAndMouseCursorPosition(false)`
  + `CGDisplayHideCursor` — deltas keep flowing with the cursor pinned.
- **Injection**: `CGEventPost(kCGHIDEventTap, …)`. Synthetic events carry
  explicit modifier flags (tracked in the injector — macOS does not infer
  them for posted events), click-state for double-clicks, and preserved raw
  deltas for apps that read them.
- **Permissions**: Accessibility (active tap + posting) and Input Monitoring
  (listening). Granted to whatever launches kayiver (your terminal during
  testing, the binary itself once autostarted). `kayiver doctor` reports both.
- **Coordinates**: the CG global space is already top-left-origin; no
  flipping is involved anywhere.

## Windows

- **Capture**: `WH_MOUSE_LL` + `WH_KEYBOARD_LL` on a thread with a message
  pump. Swallowing = returning 1 from the hook. Blocked mouse events still
  report the *proposed* position; deltas are computed against the parked
  cursor position. The cursor parks 300 px inside the portal edge so
  proposed positions never clamp against the desktop bounds (which would
  eat outward motion).
- **Injected-event loopback**: everything kayiver injects carries
  `LLMHF_INJECTED`/`LLKHF_INJECTED` and is passed through untouched — no
  feedback loops when a machine is host and later becomes client.
- **Injection**: `SendInput` — absolute moves normalized to the virtual
  desktop (`MOUSEEVENTF_VIRTUALDESK`), VK + scancode for keys with
  `KEYEVENTF_EXTENDEDKEY` where required (arrows, nav cluster, right-side
  modifiers, keypad enter/divide).
- **Permissions**: none. (If you ever run an elevated app, Windows UIPI will
  block injection into it unless kayiver is also elevated — known limitation.)
- **Known v0.1 limitation**: while forwarding, the parked cursor stays
  *visible* at its parking spot (hiding another process's cursor needs an
  overlay window — see ROADMAP).

## Linux (planned)

Wayland: `libei` (input emulation) + `ei`/portal APIs; X11: XTest +
XInput2 raw events. The engine/protocol layers need zero changes; it's one
new `platform/linux.rs`.

## Android (planned — client only)

Android can *receive* input without root via an **AccessibilityService**
(pointer gestures + key event injection limits apply) — this is the same
mechanism DeskDock/KDE Connect use. Plan:

- Rust core (`kayiver-core` compiles unchanged for `aarch64-linux-android`)
  behind UniFFI bindings.
- Kotlin shell: foreground service (persistent connection), Accessibility
  service (injection), `BOOT_COMPLETED` receiver (autostart), Compose
  settings UI for pairing.
- Realistic v1 scope: cursor + tap/scroll + text keys. Full modifier
  combos and low-latency games are not achievable through the
  accessibility layer; document honestly.

## iOS / iPadOS

Apple provides **no** system-wide input injection API — a device cannot be
a kayiver *client* (receive your mouse) without jailbreaking, period. What is
possible and planned instead:

- **Controller mode**: an iPad app acting as a *source* — its touchscreen
  becomes a trackpad/keyboard for the other machines (the reverse
  direction of today's host→client flow; the protocol already models input
  as generic events).
- For "iPad as extra screen", Apple's own Sidecar/Universal Control already
  cover the Mac side; kayiver doesn't compete there.

## The shared-monitor case (one monitor, two inputs)

If a monitor is cabled to two machines (like a Mac + Windows box sharing
one display), kayiver keeps your *keyboard/mouse* seamless, but the monitor's
own input source still needs switching. Most monitors expose input-source
selection over **DDC/CI**, so kayiver can flip the monitor automatically when
the cursor crosses to the machine on the other cable. This is planned as
`[monitor]` config (see ROADMAP): on focus change, the machine gaining
focus issues a DDC "input select" (VCP code 0x60) — `ddc-hi` on
Windows/Linux, `ddc-macos` on macOS. Until then: the monitor's physical
input button, or tools like Lunar/ControlMyMonitor alongside kayiver.
