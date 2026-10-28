// Renders the Pascoda app icon at every size an .icns needs. No dependencies beyond macOS.
// Usage: swift make-icon.swift <iconset-dir> <master.png> [classic]
// The default icon has a run triangle as the bowl of the P; "classic" is the plain P.
import AppKit

func render(_ px: Int, run: Bool) -> Data {
    let s = CGFloat(px)
    let ctx = CGContext(data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.scaleBy(x: s / 1024, y: s / 1024)
    // macOS icon grid: 824pt rounded square centred in 1024.
    let body = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shape = CGPath(roundedRect: body, cornerWidth: 185, cornerHeight: 185, transform: nil)
    // Drop shadow.
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: CGColor(gray: 0, alpha: 0.35))
    ctx.addPath(shape); ctx.setFillColor(CGColor(red: 0.1, green: 0.15, blue: 0.4, alpha: 1)); ctx.fillPath()
    ctx.restoreGState()
    // Gradient body (top light, bottom deep).
    ctx.saveGState()
    ctx.addPath(shape); ctx.clip()
    let cs = CGColorSpace(name: CGColorSpace.sRGB)!
    let g = CGGradient(colorsSpace: cs, colors: [
        CGColor(red: 0.36, green: 0.52, blue: 0.98, alpha: 1),
        CGColor(red: 0.20, green: 0.22, blue: 0.70, alpha: 1)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(g, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])
    // Soft top highlight.
    let h = CGGradient(colorsSpace: cs, colors: [
        CGColor(gray: 1, alpha: 0.22), CGColor(gray: 1, alpha: 0)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(h, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 560), options: [])
    ctx.restoreGState()
    // Inner hairline.
    ctx.addPath(shape); ctx.setStrokeColor(CGColor(gray: 1, alpha: 0.18)); ctx.setLineWidth(3); ctx.strokePath()

    // Glyph: a "P" built like a classical pillar: capital slab, stem, base slab, bowl.
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 18, color: CGColor(gray: 0, alpha: 0.30))
    ctx.beginTransparencyLayer(auxiliaryInfo: nil)
    ctx.setFillColor(CGColor(gray: 1, alpha: 1))
    let stemX: CGFloat = 340, stemW: CGFloat = 130
    ctx.addPath(CGPath(roundedRect: CGRect(x: stemX, y: 262, width: stemW, height: 500), cornerWidth: 20, cornerHeight: 20, transform: nil))
    ctx.addPath(CGPath(roundedRect: CGRect(x: stemX - 50, y: 262, width: stemW + 100, height: 56), cornerWidth: 22, cornerHeight: 22, transform: nil))
    ctx.addPath(CGPath(roundedRect: CGRect(x: stemX - 50, y: 706, width: stemW + 100, height: 56), cornerWidth: 22, cornerHeight: 22, transform: nil))
    ctx.fillPath()
    do {
        // Bowl: thick half-ring on the right of the stem.
        let cy: CGFloat = 590, r: CGFloat = 116, lw: CGFloat = 112
        let bowl = CGMutablePath()
        bowl.move(to: CGPoint(x: stemX + 50, y: cy + r))
        bowl.addLine(to: CGPoint(x: 560, y: cy + r))
        bowl.addArc(center: CGPoint(x: 560, y: cy), radius: r, startAngle: .pi / 2, endAngle: -.pi / 2, clockwise: true)
        bowl.addLine(to: CGPoint(x: stemX + 50, y: cy - r))
        ctx.addPath(bowl.copy(strokingWithWidth: lw, lineCap: .butt, lineJoin: .round, miterLimit: 10))
        ctx.fillPath()
    }
    ctx.endTransparencyLayer()
    ctx.restoreGState()
    if run {
        // The D-shaped counter of the P becomes a green "run" triangle.
        let cy: CGFloat = 590
        let d = CGMutablePath()
        d.addRect(CGRect(x: stemX + stemW - 2, y: cy - 62, width: 94, height: 124))
        d.addEllipse(in: CGRect(x: 560 - 62, y: cy - 62, width: 124, height: 124))
        ctx.setFillColor(CGColor(gray: 1, alpha: 1))
        ctx.addPath(d); ctx.fillPath()
        let tri = CGMutablePath()
        tri.move(to: CGPoint(x: 486, y: 644))
        tri.addLine(to: CGPoint(x: 486, y: 536))
        tri.addLine(to: CGPoint(x: 606, y: 590))
        tri.closeSubpath()
        ctx.setFillColor(CGColor(red: 0.18, green: 0.78, blue: 0.38, alpha: 1))
        ctx.addPath(tri); ctx.fillPath()
        ctx.addPath(tri.copy(strokingWithWidth: 22, lineCap: .round, lineJoin: .round, miterLimit: 10))
        ctx.fillPath()
    }
    let rep = NSBitmapImageRep(cgImage: ctx.makeImage()!)
    return rep.representation(using: .png, properties: [:])!
}

let args = CommandLine.arguments
let dir = args[1]
let run = args.count < 4 || args[3] != "classic"
try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
for base in [16, 32, 128, 256, 512] {
    try render(base, run: run).write(to: URL(fileURLWithPath: "\(dir)/icon_\(base)x\(base).png"))
    try render(base * 2, run: run).write(to: URL(fileURLWithPath: "\(dir)/icon_\(base)x\(base)@2x.png"))
}
try render(1024, run: run).write(to: URL(fileURLWithPath: args[2]))
