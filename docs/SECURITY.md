# Security

Input events are keystrokes — i.e. passwords — so kayiver treats the network
as hostile even though it only runs on your LAN/VPN.

## Pairing (trust bootstrap)

`kayiver pair` / `kayiver join` run **SPAKE2** (Ed25519 group) over the 6-digit
PIN, followed by direction-tagged key confirmation
(`SHA256(key ‖ "kayiver-pair-confirm-{display,input}")`).

Why a 6-digit PIN is enough here: SPAKE2 is a PAKE — the PIN never travels
on the wire and a transcript gives an attacker **zero** offline information.
The only attack is an *online* guess, each costing one full exchange, and
`kayiver pair` accepts exactly one attempt per displayed code (a failed
attempt burns the code). A man-in-the-middle without the PIN fails key
confirmation and the user sees the pairing fail.

The result is a per-peer 32-byte PSK (`SHA256(key ‖ "kayiver-session-psk-v1")`),
stored in `config.toml` on both sides.

## Sessions

Every session runs **Noise `NNpsk0_25519_ChaChaPoly_BLAKE2s`**:

- **Mutual authentication**: both sides must hold the pairing PSK; the
  handshake fails otherwise. Unknown machines can't connect, and clients
  can't be lured to a fake host.
- **Forward secrecy**: fresh ephemeral X25519 keys every session (`ee`),
  so a leaked PSK does not decrypt recorded past traffic.
- **AEAD everything**: after the 2-message handshake, every frame is
  ChaCha20-Poly1305 with per-direction counters — no replay, no tampering,
  no injected keystrokes.

## What is deliberately plaintext

- The `Intro::Session { name }` frame (host needs it to select the PSK
  before the handshake). An eavesdropper learns machine names and that
  kayiver is in use — never input data.
- mDNS advertisements (inherently public on the LAN).

## Residual risks & choices

| Risk | Position |
|---|---|
| PSK at rest in `config.toml` (mode 600 dir, user-owned) | Same trust level as your SSH keys; OS keychain storage is on the roadmap. |
| Active MITM present *during pairing itself* who also delays/rewrites frames after key confirmation | Can only tamper with the exchanged `name`/`port` metadata (DoS-level nuisance), never learn or influence the key. |
| DoS (connect floods, bogus intros) | Unauthenticated work per connection is one frame parse + a failing handshake. |
| Compromised peer machine | Out of scope: a machine you pair *is* trusted to receive your keystrokes while focused. Unpair by deleting its peer entry. |

Report vulnerabilities via a GitHub security advisory (private), not a
public issue.
