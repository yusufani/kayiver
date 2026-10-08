# Wire protocol (version 16)

Version 16 ships with 0.3.0. Both peers must be upgraded together; older protocol
versions are rejected during Hello/Welcome. Downgrades must also replace both
peers together. Pairing keys/configuration remain unchanged.

Transport is TCP with TCP_NODELAY, port 24817 by default. Frames have a u16
big-endian length followed by a postcard-serialized payload, maximum 65535 bytes.
Intro selects a known pairing identity, then Noise NNpsk0 authenticates/encrypts
all session messages. Nonces follow one ordered reader/writer per connection.

## Navigation messages

| Message | Purpose |
| --- | --- |
| MonitorIdentity | Stable monitor ID/native rectangle snapshot from each peer. |
| Navigation | Whole canonical topology, including revision, surfaces, edge seams. |
| CursorFrame | Source stamp, destination surface, absolute x/y, held HID keys/buttons. |
| ControlledInput | Source stamp plus a key, button, or wheel event. |
| CursorRelease | Ordered release at the processed movement boundary. |
| NavigationRejected | Stamp of an invalid target/layout, competing controller, or failed injection; source recovers locally. |

A stamp contains session, generation, sequence, and topology revision as u64.
Sequence is shared across movement and input. Generation changes when a driver
is reset or its topology changes. Each receiver rejects duplicates and older
generations/sequences, and the router fences messages from superseded transport
connections. Normal return has no acknowledgement round-trip dependency.

The sender solves geometry. The receiver validates its destination surface and
revision, injects absolute coordinates, and never solves an independent boundary.
Physical OS positions are rounded/clamped only at the adapter boundary. Logical
coordinates remain fractional. Held state is transferred on entry and released
on exit/disconnection. HID keyboard-page usages preserve platform key mappings;
wheel units remain the existing 1/120-notch convention.

## Other messages

Hello/Welcome, Ping/Pong, Monitors, StateSync, SharedRequest/SharedBlock, Arrange,
UseAddr, Clipboard/OpenUrl, and QuickShare offer/accept/chunk/status retain their
existing responsibilities. The arbiter publishes shared-panel ownership and the
canonical editor view. Shared clipboard remains echo guarded. File chunks remain
at most 32 KiB. Recovery Leave/Input releases are serialized in the same stream.

Legacy Enter/EnterAt, CursorLeft/Carry and SharedCross/Carry enum slots remain
reserved to avoid shifting serialization ordinals; they do not run the monitor
transition algorithm in version 16. Legacy relative MouseMove is not applied by
the monitor receiver. Absolute desktop movement is the current contract;
raw-input games require separate platform acceptance testing.
