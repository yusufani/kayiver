# Desktop prerelease publication

`release.yml` builds both platforms, runs the regression suite, scans the exact
Windows artifact, and creates a draft only when requested. For example:

```sh
gh workflow run release.yml --ref main -f publish=true
```

Development versions ending in `-dev` retain the unsigned Windows channel.
Stable versions additionally require trusted Authenticode signing. Both paths
require the scanner's protection, freshness, exclusion and exact-hash checks.

If build jobs succeeded but only the scan environment failed, reuse those
immutable artifacts instead of rebuilding a different executable:

```sh
gh workflow run publish-verified-build.yml --ref main \
  -f build_run_id=BUILD_RUN_ID -f publish=true
```

The promotion workflow accepts only completed main-branch release runs from
this repository with both build jobs successful. It derives the version from
the build's source commit, records that commit/run in the verification report,
refuses to replace a published release or a tag pointing at another commit,
and scans the original Windows bytes without executing them. It produces the
`verified-release-bundle` artifact with read-only repository permissions. An
authorized operator creates the draft from those files, setting `--target` to
the report's build commit. The release tag must point to that build commit,
not a later verification-only workflow commit.

`prepare-defender-ci.ps1` is restricted to an explicitly identified ephemeral
GitHub-hosted scan job. It removes the hosted image's build-performance
exclusions, enables realtime/behavior protection when needed, and tries the
supported MMPC and Microsoft Update sources. It does not change cloud reporting
or sample consent. A failed update never waives the independent verifier's
24-hour freshness rule. Do not run this preparation script on a user desktop.

Download the verified bundle and create a draft with the existing authorized
GitHub session; do not add a broad publisher PAT to the scan workflow. Inspect
every draft asset before publication: Windows hash must
match its successful report; checksums must match all files; Mac signatures,
architecture and bundle version must be verified. Publish the draft with its
security limitations stated, including unsigned Windows and self-signed,
non-notarized macOS packages. An antivirus finding blocks the affected package:
do not allowlist, disguise or repeatedly rebuild it to seek acceptance.
