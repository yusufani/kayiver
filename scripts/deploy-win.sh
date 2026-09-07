#!/usr/bin/env bash
# Cross-build kayiver for Windows, copy it over SSH, and restart it IN THE
# CONSOLE SESSION via the scheduled task (never Start-Process over SSH: that
# lands in session 0, where Windows shows one fake 1024x768 display).
#
#   ./scripts/deploy-win.sh            # build + deploy
#   SKIP_BUILD=1 ./scripts/deploy-win.sh
#
# Needs sshd running on the Windows box (Start-Service sshd there if the
# probe below fails) and the mingw linker from brew.
set -euo pipefail
cd "$(dirname "$0")/.."

TOOLCHAIN_BIN="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
export PATH="$TOOLCHAIN_BIN:$PATH"
EXE=target/x86_64-pc-windows-gnu/release/kayiver.exe
KEY="$HOME/.ssh/drift_win_ed25519"
USER_="yusuf"
REMOTE_EXE='C:/Users/yusuf/AppData/Local/kayiver/kayiver.exe'
HOSTS=("${WIN_HOST:-}" 192.168.0.18 10.99.0.2 192.168.0.13)

if [ -z "${SKIP_BUILD:-}" ]; then
  echo "==> building windows release"
  cargo build --release -p kayiver --target x86_64-pc-windows-gnu
fi

HOST=""
for h in "${HOSTS[@]}"; do
  [ -n "$h" ] || continue
  if nc -z -w2 "$h" 22 2>/dev/null; then HOST="$h"; break; fi
done
if [ -z "$HOST" ]; then
  echo "!! no Windows sshd reachable on ${HOSTS[*]} — on the Windows box run (admin PowerShell):" >&2
  echo "     Start-Service sshd" >&2
  exit 1
fi
SSH=(ssh -i "$KEY" -o ConnectTimeout=5 -o BatchMode=yes "$USER_@$HOST")
echo "==> deploying to $HOST"

echo "==> stopping kayiver.exe"
"${SSH[@]}" 'taskkill /IM kayiver.exe /F' >/dev/null 2>&1 || true
# Wait for it to actually go: the successor's single-instance guard only
# retries for ~6s, so a predecessor still holding the port makes the new one
# exit with "another kayiver instance is already running" and nothing runs.
for _ in $(seq 1 20); do
  if ! "${SSH[@]}" 'tasklist /FI "IMAGENAME eq kayiver.exe" /NH' 2>/dev/null | grep -qi kayiver; then break; fi
  sleep 1
done

echo "==> copying $EXE"
scp -i "$KEY" -o ConnectTimeout=5 "$EXE" "$USER_@$HOST:$REMOTE_EXE"

echo "==> restarting via scheduled task (console session)"
"${SSH[@]}" 'schtasks /Run /TN kayiver'
sleep 4
"${SSH[@]}" 'powershell -NoProfile -Command "Get-Process kayiver | Select Id,SessionId | Format-Table -HideTableHeaders"'
echo "==> done (SessionId must be 1 above; 0 means it landed in the SSH session)"
