# Architecture

Every computer runs the same capture/router/injection engine. Pairing chooses
which computer publishes the canonical layout; it does not permanently choose
the physical mouse. The physical driver, visible cursor surface, and shared
panel owner are distinct state.

## Motion and topology

`kayiver-core::motion` is deterministic and independent of operating systems,
network transport, timers, and display labels. `Topology::compile` uses stable
monitor identities and native rectangles. Shared-panel copies alias one visible
surface. Only touching edge segments connect. Desktop bounding-box gaps remain
walls; ambiguous seams do not choose an arbitrary destination. Explicit machine
links fill unoccupied edge segments, and cannot override shared-panel geometry.

Each ordered native report is a segment. `Topology::advance` consumes it up to
the first edge, maps the intersection, and continues with the remainder. Large
reports can traverse several surfaces. Seam scale applies equally to both
components, preserving angle. The logical coordinate includes the mathematical
boundary; only the final OS position is rounded and clamped to a physical pixel.
There is no artificial landing inset or accumulated wall movement.

For example, 100px left from a point 10px inside D consumes 10px on D and 90px
on A. Immediate reversal is another report, never a vector summed with the first.

## Capture and control

`platform::navigation::Navigation` owns Local, Remote, Driven, or Recovering
control. The short navigation mutex protects the topology/location/input ledger.
Native capture is serialized with control projection changes by `motion_gate`.
No network response is awaited there. OS operations occur after releasing the
navigation mutex.

macOS supplies CGEvent motion deltas and filters events tagged by Kayiver.
Windows supplies native post-acceleration displacement; while forwarding it parks
the physical cursor inside a visible monitor so proposed positions do not clip
at the taskbar. A clipped local native endpoint cannot rebase away the logical
remainder. Receivers inject the absolute computed position, with no second
relative acceleration pass.

Capture changes suppression synchronously. On a handoff, source-side native held
keys/buttons are released or restored before another physical release can
overtake the asynchronous router. The destination receives the held-state
snapshot. Physical input on a Driven desk cannot steal control; triple Escape
requests recovery.

There is no polling cursor guard, return cooldown, or receiver-side crossing
solver. `forwarding`, `driven`, and `portals` are compatibility/status projections,
not independent geometry decisions. Android remains a peripheral adapter for
scrcpy/UHID; monitor-to-monitor transport uses the shared engine.

## Ordered transport and failures

TCP with TCP_NODELAY, Noise NNpsk0 encryption, and one writer per connection
preserve event order. Motion, release, and input contain source session,
generation, sequence, and topology revision. The transport connection identity
also fences queued events from replaced connections. Duplicate/old-generation
reports are rejected; changing topology invalidates queued source reports.

The physical driver computes the entire path; receivers validate the version and
target and apply it. A normal return does not await acknowledgement. Layout
changes publish a whole topology. A missing target, failed cursor injection,
disconnection, or rescue shortcut releases held input and restores a visible
local monitor. Session liveness remains governed by the existing watchdog.

Capture and writer queues are currently unbounded FIFO queues. Ordering is
preserved; prolonged transport congestion can build latency until liveness
recovery. Do not replace them with reversal-destroying vector sums. Bounded
backpressure and raw-input game acceptance remain separate validation work.

## Editor lifecycle

The local HTTP editor remains available when engine startup reports an error.
macOS opens its existing window on launcher re-entry, waits for HTTP readiness
before the first WebView navigation, and retains Accessory/LSUIElement behavior.
Windows keeps the SYSTEM input engine in the interactive console session and
launches its editor using the signed-in user's token. An isolated browser profile
and explicit centered window placement avoid hidden/stale normal-browser state.

The map is fit with one affine transform after shared-panel merging, independent
of which computer opens the UI. ResizeObserver refits after canvas-size changes;
zoom/pan operate on the whole arrangement and reset returns to the fit view.
