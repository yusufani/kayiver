# Input optimization verification — 0.3.15-dev

This is a development build, not a completed physical acceptance release.
Protocol remains 17; encrypted frame format, motion units and navigation
sequence rules are unchanged. Monitor-specific settings are not hardcoded.

Implemented:

- Preserve the Windows injection thread's desktop binding until the actual
  input desktop changes. UAC remains handled by the windowless worker.
- Typed, bounded ordered commands reuse completion storage instead of
  allocating a command closure and reply channel for every event.
- Reuse encrypted session buffers and write frame prefix/ciphertext together.
- Send absolute Windows pointer events for Terminal visibility without fake
  Shift/F24 wake keys. Power-manager wake requests remain enabled.
- Read native cursor position once. Exact SetCursorPos remains a fallback
  when absolute input does not land at the requested point.
- Expose payload-free application-time and fallback counters in local status.

Evidence from this run:

- 37 core tests, 50 application tests and 42 two-process transition tests pass.
- Three Windows dispatcher tests and three isolated desktop/coordinate tests
  pass. The benchmark is separate from the regular tests.
- On the development Windows machine, five-trial median for 10,000 worker
  round-trips: 15,017 us reused completion versus 39,635 us legacy reply
  channels. This measures IPC only; it is not end-to-end pointer latency.
- Legacy transport interoperability: 1,000 mixed bulk/motion messages retain
  correct nonce order and stable buffer capacity after warming.
- Both installed apps report 0.3.15-dev; Mac signing identity and permission
  grants were retained. Both peers reconnect on the existing Ethernet path.
- Before the final native fast path, live Windows counters recorded 1,648
  commands, zero failures, mean 846.98 us, maximum 14,759 us, and 44 calls
  over 2 ms. These data do not establish elimination of latency spikes.
- The final fast path has been cross-checked and release-built for Windows,
  then installed. Its live command counters are currently zero; its actual
  application latency and fallback rate are not yet proven.

Remaining acceptance:

- Gather representative movement samples for the final native path, separating
  desktop tracking, native API time, worker wait and network latency.
- Verify fast bidirectional crossings, immediate reversals, Terminal cursor
  visibility and real UAC consent-dialog interaction after all optimizations.
- Verify monitor sleep/wake behavior without fabricated key/motion pulses.
- Exercise motion concurrently with image/file clipboard traffic and inspect
  queued frames before changing scheduling or prioritizing bulk transfer.

Rollback artifacts: the paired pre-optimization apps are preserved in the
restricted temporary directory `/tmp/kayiver-optimized-input-20261012`.
Windows also retains `kayiver.pre-fastpath.exe` beside its installed binary.
Restore the pair together for a full rollback, and preserve the Mac signing
identity. No clipboard content or pairing secrets are included in these
verification notes or status counters.

## Subsequent bulk-production improvement

Prepared and verified after the installed native fast path: a shared per-peer
file-frame budget prevents file producers from filling the unbounded control
FIFO, 8 KiB outbound data chunks shorten each encrypted frame, cancelled
producers release their slot, and disconnected/truncated file streams report
failure. Tests confirm correct input reversal ordering while the file producer
is blocked and exact/empty file transfer semantics. 55 application tests and
42 two-process transition tests pass for this stage; Windows cross-check passes.

This stage has not been installed yet. Application queue backpressure does
not bound bytes already accepted by TCP or synchronous inbound disk writes.
Further slow-receiver testing is required before making an end-to-end latency
claim or adding receiver credits/a separate data path.

The explicit encrypted-socket slow-reader probe demonstrated 32,768 bytes
already buffered ahead of newly queued +100/-100 motion events even with the
one-frame application file budget. This is a verified limitation, not a failed
nonce/order test: both movement events retained their order. The probe pauses
the receiver until four chunks have reached the socket, then queues motion and
resumes reading. Further data flow must account for receiver consumption or
separate bulk traffic; application send completion is insufficient proof.
