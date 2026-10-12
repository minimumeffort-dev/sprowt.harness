import AppKit

guard CommandLine.arguments.count == 2 else {
    fputs("Usage: swift GenerateAppIcon.swift /path/to/Sprowt.iconset\n", stderr)
    exit(1)
}
let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

func drawIcon(size: Int) throws -> Data {
    guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
                                      bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                                      isPlanar: false, colorSpaceName: .deviceRGB,
                                      bytesPerRow: 0, bitsPerPixel: 0),
          let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
        throw CocoaError(.fileWriteUnknown)
    }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    defer { NSGraphicsContext.restoreGraphicsState() }
    context.cgContext.scaleBy(x: CGFloat(size) / 1024, y: CGFloat(size) / 1024)
    NSColor(srgbRed: 247 / 255, green: 200 / 255, blue: 67 / 255, alpha: 1).setFill()
    NSBezierPath(roundedRect: NSRect(x: 64, y: 64, width: 896, height: 896),
                 xRadius: 196, yRadius: 196).fill()
    NSColor(srgbRed: 23 / 255, green: 23 / 255, blue: 23 / 255, alpha: 1).setStroke()
    let stem = NSBezierPath()
    stem.lineWidth = 38
    stem.lineCapStyle = .round
    stem.lineJoinStyle = .round
    stem.move(to: NSPoint(x: 386, y: 290))
    stem.line(to: NSPoint(x: 638, y: 290))
    stem.move(to: NSPoint(x: 512, y: 290))
    stem.curve(to: NSPoint(x: 530, y: 620), controlPoint1: NSPoint(x: 500, y: 420),
               controlPoint2: NSPoint(x: 490, y: 526))
    stem.stroke()
    let leaves = NSBezierPath()
    leaves.lineWidth = 38
    leaves.lineCapStyle = .round
    leaves.lineJoinStyle = .round
    leaves.move(to: NSPoint(x: 507, y: 514))
    leaves.curve(to: NSPoint(x: 324, y: 672), controlPoint1: NSPoint(x: 363, y: 480),
                 controlPoint2: NSPoint(x: 304, y: 557))
    leaves.curve(to: NSPoint(x: 507, y: 514), controlPoint1: NSPoint(x: 477, y: 695),
                 controlPoint2: NSPoint(x: 519, y: 617))
    leaves.move(to: NSPoint(x: 516, y: 581))
    leaves.curve(to: NSPoint(x: 706, y: 742), controlPoint1: NSPoint(x: 523, y: 717),
                 controlPoint2: NSPoint(x: 591, y: 756))
    leaves.curve(to: NSPoint(x: 516, y: 581), controlPoint1: NSPoint(x: 722, y: 614),
                 controlPoint2: NSPoint(x: 636, y: 558))
    leaves.stroke()
    guard let png = bitmap.representation(using: .png, properties: [:]) else {
        throw CocoaError(.fileWriteUnknown)
    }
    return png
}

for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let suffix = scale == 2 ? "@2x" : ""
        try drawIcon(size: size * scale).write(to: directory.appendingPathComponent("icon_\(size)x\(size)\(suffix).png"))
    }
}
