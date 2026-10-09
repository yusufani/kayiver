# Kayıver 0.3.5

The Mac source cursor is now explicitly contained while controlling Windows. Native cursor association is best-effort, so a background menu-bar process must not assume the source cursor stays still. Capture remembers a source anchor at departure and corrects native drift only during remote control. Logical remote movement uses the original report unchanged.

Returning locally releases the anchor immediately; ordinary local motion and unconnected edges remain untouched. No asynchronous source mouse-position events are reintroduced.

Regression coverage simulates 100 leaking native reports, verifies the source stays parked while Windows receives every pixel, then verifies immediate local continuation. Native adapter behavior on the physical devices remains to be confirmed.

Protocol 16. Use matching 0.3.5 packages on both devices. Stable macOS signing is preserved; rollback both devices together to matching previous packages.
