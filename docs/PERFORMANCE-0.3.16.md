# Receiver-aware transfer flow — 0.3.16-dev

Protocol 18 requires both peers to update together. Protocol-17 peers are
rejected by the existing version gate; no partial mixed-version control is
supported. This is a development build pending physical acceptance.

One shared credit per connection now covers image/file chunks. The source
waits for a unique-token receiver receipt before producing another payload.
Ordinary pointer, key, layout and liveness events do not acquire that credit.
The payload bound is 8 KiB plus protocol/encryption headers. Stale/duplicate
receipts cannot release a newer token. Uncertain cancellation or receipt
timeout closes data credits until reconnect, while control messages continue.

Image receipts follow validated ordered in-memory reassembly. PNG decoding
and native clipboard application remain on the clipboard worker. Failed sends
are reported; a superseded native copy cancels the older upload between chunks.
File receipts follow disk processing on a separate async queue/blocking job.
Transfer-map locks are short; each file owns its disk-state mutex, separate
from clipboard offer/candidate state. No idle blocking thread is reserved per
peer. File acceptance is scoped to peer and local connection epoch, rejects
wrong offsets/size/EOF and invalid path names, and creates partial files with
create_new rather than truncating existing data. Disk creation is deferred
until the accepted session's first valid frame. Session cleanup releases file
state; partial bytes are preserved on disk.

Evidence:

- 58 application tests, 37 core tests and 43 two-process regressions pass.
- The real TCP/Noise acceptance test withholds the data receipt and observes
  both +100/-100 moves before acknowledgement, with one 8 KiB payload ahead.
  The previous sender-only probe allowed 32 KiB already buffered ahead.
- The stronger two-process test blocks disk work for 800 ms per chunk. Forty
  reversing movements arrive while the worker is blocked, measured at
  120.816542 ms in this run, before the first disk stall completes. The final
  file bytes match exactly. This uses simulated OS input and is not a native
  human pointer-latency measurement.
- Two-way image clipboard tests retain exact fingerprints, same-image replay
  after text and no return echo.
- Both native release builds succeed; installed Windows hash matches its
  build and Mac executable matches signed staging. Both apps report 0.3.16-dev,
  running=true and connected; Mac permission grants and certificate are intact.
- Latest Windows native injection counters remain zero. Final hardware
  movement latency, UAC/Terminal interaction and monitor sleep/wake are unproven.

Tradeoff: stop-and-wait data receipts reduce large-transfer throughput on
high-RTT links. They prevent bulk transfer from accumulating arbitrary bytes
in front of control input. Native OS scheduling and the time to transmit one
8 KiB payload remain outside a zero-latency guarantee.

Paired rollback: `/tmp/kayiver-receiver-credit-20261012/rollback-mac.app` and
`rollback-windows.exe` preserve the pre-upgrade protocol-17 installation.
Restore both together, retaining the Mac signing identity. No user clipboard
content or pairing keys are included in this evidence document.
