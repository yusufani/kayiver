# Kayıver 0.3.4

Returning to the source computer no longer posts a second, asynchronous mouse-position event. That redundant event could arrive after fresh physical motion and pull the pointer back. Handoff input state now updates button coordinates without moving the pointer; the synchronous navigation placement remains the sole source-side placement.

macOS synthetic input sources explicitly allow physical events during both the post-event interval and synthetic dragging, with a zero suppression interval. This covers held keys and buttons as well as ordinary pointer returns.

A regression test rejects the previous duplicate-position behavior and checks immediate continuation after returning from the upper Windows screen. Existing crossing, drag, queue, empty-edge and reconnection tests remain enabled. Physical acceleration acceptance is not yet confirmed.

Protocol 16. Install matching 0.3.4 packages on both devices; stable macOS signing is preserved. Restore both matching previous packages together to roll back.
