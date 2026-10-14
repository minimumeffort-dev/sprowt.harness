import AppKit
import CoreText
import SwiftUI

enum Theme {
    // One warm-paper identity in both appearances; every surface resolves
    // through these dynamic colors so the system appearance just works.
    static let paper = Color(light: 0xfafaf7, dark: 0x1b1b18)
    static let sidebar = Color(light: 0xf0f0e9, dark: 0x23231f)
    static let panel = Color(light: 0xf6f6f1, dark: 0x282820)
    static let ink = Color(light: 0x171717, dark: 0xf2f0e9)
    static let muted = Color(light: 0x62625c, dark: 0xaba79b)
    static let line = Color(light: 0xdeded6, dark: 0x3c3c34)
    static let selected = Color(light: 0xe5e5dc, dark: 0x35352c)
    static let gold = Color(light: 0xf7c843, dark: 0xf7c843)
    static let green = Color(light: 0x477759, dark: 0x86ba97)
    static let blue = Color(light: 0x39739f, dark: 0x7aabcf)
    static let red = Color(light: 0xa13e35, dark: 0xda8d80)
    static let inset: CGFloat = 24
    static let radius: CGFloat = 7

    static var nsText: NSColor {
        NSColor(name: nil) { $0.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 0.93, alpha: 1) : NSColor(white: 0.09, alpha: 1) }
    }
    static var nsCodeBackground: NSColor {
        NSColor(name: nil) { $0.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            ? NSColor(white: 0.22, alpha: 1) : NSColor(white: 0.93, alpha: 1) }
    }

    static func font(_ size: CGFloat = 13, weight: Font.Weight = .regular) -> Font {
        let style: Font.TextStyle = size >= 20 ? .title2 : size <= 11 ? .caption : .body
        return .custom("Manrope", size: size, relativeTo: style).weight(weight)
    }

    static var composerFont: NSFont {
        let size = NSFont.preferredFont(forTextStyle: .body).pointSize + 1
        return NSFont(name: "Manrope", size: size) ?? .systemFont(ofSize: size)
    }

    @discardableResult static func registerFonts() -> Bool {
        guard let url = resourceBundle()?.url(forResource: "Manrope", withExtension: "ttf", subdirectory: "Manrope") else { return false }
        return CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
    }

    static func resourceBundle(in main: Bundle = .main) -> Bundle? {
        if main.bundleURL.pathExtension == "app" {
            return main.url(forResource: "SprowtDesktop_SprowtDesktop", withExtension: "bundle").flatMap(Bundle.init(url:))
        }
        return .module
    }
}

extension View {
    func symbolBaseline(textSize: CGFloat) -> some View {
        let capHeight = (NSFont(name: "Manrope", size: textSize) ?? NSFont.systemFont(ofSize: textSize)).capHeight
        return alignmentGuide(.firstTextBaseline) { dimensions in
            dimensions[VerticalAlignment.center] + capHeight / 2
        }
    }
}

private extension Color {
    init(hex: UInt32) { self.init(NSColor(hex: hex)) }
    init(light: UInt32, dark: UInt32) {
        self.init(NSColor(name: nil) {
            $0.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? NSColor(hex: dark) : NSColor(hex: light)
        })
    }
}

private extension NSColor {
    convenience init(hex: UInt32) {
        self.init(srgbRed: Double((hex >> 16) & 255) / 255,
                  green: Double((hex >> 8) & 255) / 255, blue: Double(hex & 255) / 255, alpha: 1)
    }
}

struct WorkspaceButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var enabled
    var selected = false
    var prominent = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.font(12, weight: selected || prominent ? .semibold : .medium))
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, 9).padding(.vertical, 7)
            .background(prominent ? Theme.gold : selected || configuration.isPressed ? Theme.selected : .clear,
                        in: RoundedRectangle(cornerRadius: 5))
            .contentShape(RoundedRectangle(cornerRadius: 5))
            .opacity(enabled ? 1 : 0.4)
    }
}

extension WorkflowPhase {
    var color: Color {
        switch self {
        case .ready, .published: Theme.green
        case .needsYou: Theme.gold
        case .planning, .building, .checking, .recovering, .reviewing, .git: Theme.blue
        case .new, .paused, .closed: Theme.muted
        }
    }
}

struct GlyphButton: View {
    let symbol: String
    let label: String
    var selected = false
    let action: () -> Void
    var body: some View {
        Button(action: action) { Image(systemName: symbol).font(.system(size: 13, weight: .regular)).frame(width: 15, height: 15) }
            .buttonStyle(WorkspaceButtonStyle(selected: selected))
            .accessibilityLabel(label).help(label)
    }
}

struct WorkspaceDivider: View {
    var body: some View { Rectangle().fill(Theme.line).frame(height: 1) }
}

struct SectionHeading: View {
    let title: String
    var detail = ""
    var body: some View {
        HStack {
            Text(title).font(Theme.font(11, weight: .semibold))
            Spacer()
            if !detail.isEmpty { Text(detail).font(Theme.font(11)) }
        }.foregroundStyle(Theme.muted)
    }
}
