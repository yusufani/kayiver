// A temporary, synthetic window on Kayiver's virtual display. This creates its
// own test content; it never moves or reads another application's windows.
// swiftc scripts/display-motion-probe.swift -o /tmp/KayiverDisplayProbe
import AppKit
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
guard let screen = NSScreen.screens.first(where: { $0.localizedName.contains("Kayiver") }) else {
    fputs("Kayiver virtual display is not active\n", stderr); exit(1)
}
final class Pattern: NSView {
    var phase: CGFloat = 0
    override func draw(_ rect: NSRect) {
        NSColor(calibratedWhite: 0.08, alpha: 1).setFill(); bounds.fill()
        let text = "Kayiver · Mac extended display\n1920 × 1080 · native text and motion check\nAa Bb Cc 0123456789  {} [] ()  // scrolling and dragging"
        text.draw(at: NSPoint(x: 30, y: bounds.height-100), withAttributes: [
            .font: NSFont.monospacedSystemFont(ofSize: 18, weight: .regular), .foregroundColor: NSColor.white])
        for row in 0..<16 {
            let sample = "let display = mac.desktop.extend(to: windows.monitor)   // line \(row)"
            sample.draw(at: NSPoint(x: 30, y: 100+CGFloat(row)*22), withAttributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 12, weight: .regular),
                .foregroundColor: row%2 == 0 ? NSColor.systemCyan : NSColor.systemPink])
        }
        NSColor.systemCyan.setFill()
        NSRect(x: phase.truncatingRemainder(dividingBy: bounds.width-150), y: 30, width: 150, height: 35).fill()
    }
}
let frame=screen.visibleFrame.insetBy(dx: 20, dy: 30)
let window=NSWindow(contentRect: frame, styleMask: [.titled], backing: .buffered, defer: false, screen: screen)
window.collectionBehavior=[.canJoinAllSpaces,.fullScreenAuxiliary]
window.level = .floating
window.setFrame(frame, display: true)
window.title="Kayiver temporary display check"
let pattern=Pattern(frame: NSRect(origin: .zero, size: frame.size));window.contentView=pattern
window.orderFrontRegardless()
print("probe target=\(screen.localizedName) frame=\(window.frame) actual=\(window.screen?.localizedName ?? "none") visible=\(window.isVisible) activeSpace=\(window.isOnActiveSpace)")
guard window.screen == screen else { fputs("Probe did not reach the virtual display\n",stderr);exit(2) }
let started=Date()
let timer=Timer.scheduledTimer(withTimeInterval: 1.0/60, repeats: true) { _ in
    pattern.phase += 8;pattern.needsDisplay=true
    if Date().timeIntervalSince(started)>15 {app.terminate(nil)}
}
app.run()
