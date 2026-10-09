#!/usr/bin/env bash
# Apple bundle versions are numeric; Cargo prerelease/build labels stay in the
# binary's version/API. Shared by local packaging, deployment and release CI.
set -euo pipefail
version="${1:?Cargo package version required}"
bundle_version="${version%%[-+]*}"
if [[ ! "$bundle_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo 'invalid numeric macOS bundle version' >&2
  exit 1
fi
printf '%s\n' "$bundle_version"
