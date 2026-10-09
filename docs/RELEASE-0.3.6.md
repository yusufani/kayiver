# Kayıver 0.3.6

Local screen-edge crossing now starts from the preceding native cursor sample, rather than preserving a stale model position when the OS clips the cursor. Acceleration or repositioning could leave the real cursor at C’s upper edge while the model remained inside C, requiring hidden extra travel before entering Windows.

Native references are cleared during remote control and layout replacement. Local return seeds the reference from the fractional landing position, preserving rapid round trips. Empty local edges remain passive. Status diagnostics now expose the actual native cursor alongside logical navigation position.

Regression tests cover a clipped edge with mismatched native/model travel and C-to-Windows reentry after repositioning. Physical acceptance on both devices is still required; simulated results do not establish native acceleration behavior.

Protocol 16; update both devices together. Stable macOS signing is preserved. To roll back, restore both matching previous packages together.
