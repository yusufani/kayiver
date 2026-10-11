# Kayiver: device-based layout editor

This is a product-wide contract, not a description of one user's desk.
No hostname, monitor letter, vendor, resolution, scale factor or arrangement
may be hardcoded as a requirement. A/B/C/D are labels, never device identities.

## Device visuals and unknown hardware

Use the Precision/Gallery direction: near-front-facing device bodies, subtle
material depth, a readable screen map, and blue or amber selection accents.
Laptop keyboards and monitor stands are decoration. The screen rectangle alone
determines adjacency, drag coordinates and cursor projection.

Every positive screen aspect ratio must render, including portrait, ultrawide,
mixed-DPI and brands/models unknown to Kayiver. Built-in flags select a laptop
body; a manual presentation override remains available. Unknown models use an
original, bundled generic device body and wallpaper, without an error or a
network lookup. Never mislabel generic artwork as an exact manufacturer model.
Missing vendor/model data must not disable input sharing or layout editing.
Exact model artwork is an optional enhancement requiring reliable matching and
redistribution rights, not a dependency of the product.

## Layout interaction

Drag screens freely. Optional snapping applies only near an actual neighboring
edge; it must never pull a distant screen back to the nearest neighbor.
Unconnected edges remain walls. Device feet, keyboards, shadows and bezel
thickness cannot create a crossing. Keep existing shared-screen creation,
ownership, keyboard recovery, live cursor, device addition, network selection,
permissions and settings accessible. Do not invent brightness/color-profile
controls that the platform backend cannot provide.

Viewport zoom and pan are presentation transforms, not changes to the desk.
Unsaved edits survive viewport resizing; a changed physical display snapshot
invalidates stale drafts rather than applying them to a different display.
Saving positions is explicit. Client views remain read-only where the current
control architecture requires editing on the host. A rejected platform
arrangement must be reported, never disguised as a successful save.

## Desktop notifications

Quick Share belongs to a native desktop popup, not the layout editor.
When a notification arrives, show a circular Kayiver mark at the lower-right
of the chosen display's usable work area, with the action bubble above it.
Both disappear on dismissal/expiry; no permanent desktop mascot or button.
Keep URL-open and file-accept actions explicit. Do not steal keyboard focus.
Preserve file progress, failure and completion states and hover-paused expiry.
Account for taskbars, DPI, small screens, negative display origins and long text.

## Release gates

Test unknown hardware, very wide and portrait ratios, missing built-in flags,
arbitrary hostnames, disconnected peers, monitor reorder/loss, mixed scales,
free gaps, immediate reversal, shared aliases, zoom/pan/resize and both themes.
Native popup acceptance requires both platforms, including focus and taskbar
checks. A browser preview or simulation is not native desktop acceptance.
Do not install/restart live apps as part of a design preview. Preserve signing
and the working input engine; desktop deployments remain a separate operation.

## Initial implementation scope

Generic material-based device rendering requires no model catalog. Model-name
discovery and optional exact-model artwork are future enhancements. The initial
editor uses existing physical rectangles and built-in flags and does not
change the mouse motion protocol. Generated concept images are references,
not screenshots of shipped features.

### Platform limits

Free dragging is available in the editor, but the current runtime uses native
desktop coordinates. Quartz can adjust requested origins to remove physical
gaps. The new macOS apply path verifies the result and reports rejection;
it does not persist an impossible request for repeated application. Arbitrary
virtual positions independent of OS arrangement require a separate runtime
geometry extension and are not claimed by this design change. Model catalog
lookup and automatic exact-model artwork are likewise not implemented.
