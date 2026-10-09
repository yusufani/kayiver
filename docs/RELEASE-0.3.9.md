> WITHDRAWN 2026-10-09: physical mouse crossing regressed after deployment.
> The prerelease is draft again; both devices were rolled back to 0.3.8.
> The synthetic checks described below did not establish physical acceptance.
> See MOUSE-REGRESSION-2026-10-09.md for the confirmed source-motion defect.

# Kayıver 0.3.9

Mac-to-Windows motion is now consumed at the macOS session event tap, after source acceleration and before local application delivery. Returning a rewritten HID event was insufficient: it could reach local applications and overwrite cursor parking. The session capture keeps mouse association enabled, passes local return reports through, and consumes all remote reports, including zero-delta and programmatic movement.

Cursor hiding now enables background cursor control on Kayiver's own WindowServer connection. This avoids depending on Kayiver being the foreground application and preserves balanced hide/show counts. Optional private symbols are resolved dynamically; unsupported systems log a limitation while the session event gate continues to consume remote movement. Future macOS changes can affect this background visibility API.

Validation: 105 automated tests, plus a native annotated-session observer. The observer confirms local and return reports arrive, remote reports do not arrive, the source stays parked and hidden, visibility returns, and a queued one-pixel return report advances one pixel. Marker-tagged synthetic motion exercises the actual native delivery pipeline; it does not measure physical mouse acceleration feel.

The native check is opt-in and moves the real cursor on both paired devices. Run only when the devices are idle, with coordinates for the installed layout. For the tested C-to-upper-Windows seam:

```sh
python3 scripts/verify-mac-capture.py --start 3450 120 --exit 3450 -500 --exit-delta 0 -620 --return-delta 0 800
```

Protocol 16 is unchanged. Both packages use version 0.3.9; stable macOS signing is retained. Rollback restores both matching 0.3.8 packages together. This remains a prerelease pending physical acceptance.
