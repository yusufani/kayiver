# Testing

## The virtual desk (`--features sim`)

Every bug class that ever bit on the real desk has a regression test that
runs on a **virtual two-machine desk** — no hardware, no displays, no OS
permissions:

```sh
cargo test --features sim -p kayiver
```

Each scenario spawns two real `kayiver run` processes — a host and a client,
each with its own `KAYIVER_CONFIG_DIR`, so each behaves like its own machine —
talking over real TCP with the real Noise handshake. Only the OS layer is
swapped for a scriptable simulator ([`platform/sim.rs`](../apps/kayiver/src/platform/sim.rs)):
virtual monitors, a virtual cursor, and recorded injection, driven through a
JSON-lines control socket.

```
KAYIVER_SIM_MONITORS="0,0,2560,1440;2560,0,2560,1440"   initial displays
KAYIVER_SIM_CTL=27101                                   control port
KAYIVER_CONFIG_DIR=/tmp/simhost                         isolated config
```

Control ops (one JSON object per line): `warp` (move the "physical" cursor —
the real cursor guard reacts), `set_monitors` (unplug / re-anchor displays at
runtime), `edge` (hit an armed portal edge), `input_move`/`input_key`
(forwarded traffic), `hotkey`, `state`, `injected` (drain everything the
engine injected, with coordinates).

### Scenarios ([`tests/sim_e2e.rs`](../apps/kayiver/tests/sim_e2e.rs))

| Test | The real-desk bug it guards against |
|---|---|
| `diagonal_cross_lands_at_entry_height` | diagonal entry read as a TOP entry → cursor dumped in the peer's corner |
| `primary_display_switch_rederives_peer_rect` | Windows primary switch re-anchored every rect → crossings landed on the wrong monitor |
| `vanished_panel_never_reanchors_to_same_size_screen` | panel input switched away → size-match glued the peer's screens onto A |
| `owner_survives_host_restart` | deploy reset ownership → notice overlay covered the client's fullscreen game |
| `no_nonce_desync_under_load_and_geometry_churn` | timer racing a half-read frame → Noise nonce desync → `decrypt error` loop |

The suite runs in seconds and the scenarios are independent (unique ports and
config dirs), so they parallelize under plain `cargo test`.

### Adding a scenario

1. Reproduce the bug's *shape* in sim terms: which monitors, who owns the
   panel, what motion or geometry change.
2. Use the helpers in `sim_e2e.rs` (`desk`, `give_panel_to_client`,
   `cross_diagonally`, `wait_until`) — a scenario is typically ~20 lines.
3. Assert on what the OTHER machine observed (`injected`) or on persisted
   config — never on internal logs alone.

## Unit tests

```sh
cargo test            # geometry/logic unit tests (fast, no processes)
```

`entry_on_rect` (the crossing-point solver) and friends live next to the code
in `platform/mod.rs`.
