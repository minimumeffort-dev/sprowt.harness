import AppKit
import SwiftUI

enum Theme {
    static let paper = Color(light: 0xfafbf8, dark: 0x181c1a)
    static let sidebar = Color(light: 0xf0f3ee, dark: 0x212723)
    static let rail = Color(light: 0xe9eee7, dark: 0x1c221e)
    static let panel = Color(light: 0xeef3ed, dark: 0x252f28)
    static let ink = Color(light: 0x202923, dark: 0xedf1eb)
    static let muted = Color(light: 0x5b685e, dark: 0xaab6ad)
    static let line = Color(light: 0xd9dfd7, dark: 0x39433c)
    static let control = Color(light: 0x7d8b80, dark: 0x728477)
    static let selected = Color(light: 0xdeeadf, dark: 0x304338)
    static let accent = Color(light: 0x296746, dark: 0xa2d5b4)
    static let onAccent = Color(light: 0xffffff, dark: 0x19291f)
    static let green = accent
    static let blue = Color(light: 0x30668d, dark: 0x9bc5e2)
    static let amber = Color(light: 0x825b12, dark: 0xebc477)
    static let red = Color(light: 0xa13730, dark: 0xefa39d)
    static func statusColor(_ status: String) -> Color {
        switch status.lowercased() {
        case "check failed", "final checks failed", "worker stopped", "blocked", "failed": red
        case "needs your answer", "needs network access": amber
        case "done", "complete": green
        case "working", "checking", "starting", "resolving conflicts", "connecting worker",
             "starting repair", "fixing failed check", "restoring environment", "stopping",
             "routing", "connecting", "running": blue
        default: muted
        }
    }

    static let inset: CGFloat = 24
    static let radius: CGFloat = 7

    static let nsText = NSColor(ink)
    static let nsMuted = NSColor(muted)
    static let nsAccent = NSColor(accent)
    static let nsSelected = NSColor(selected)
    static let codeBackground = Color(light: 0xe9eee6, dark: 0x242d27)
    static let nsCodeBackground = NSColor(codeBackground)

    static func font(_ size: CGFloat = 14, weight: Font.Weight = .regular) -> Font {
        .system(size: scaledSize(size), weight: weight)
    }

    static func scaledSize(_ size: CGFloat) -> CGFloat {
        let style: NSFont.TextStyle = size >= 18 ? .title2 : size <= 11 ? .caption1 : .body
        let standard: CGFloat = size >= 18 ? 17 : size <= 11 ? 10 : 13
        return size + NSFont.preferredFont(forTextStyle: style).pointSize - standard
    }

    static var composerFont: NSFont { .systemFont(ofSize: scaledSize(14)) }

}

extension View {
    func symbolBaseline(textSize: CGFloat) -> some View {
        let capHeight = NSFont.systemFont(ofSize: Theme.scaledSize(textSize)).capHeight
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
            .foregroundStyle(prominent ? Theme.onAccent : Theme.ink)
            .padding(.horizontal, 9).padding(.vertical, 7)
            .background(prominent ? Theme.accent : selected || configuration.isPressed ? Theme.selected : .clear,
                        in: RoundedRectangle(cornerRadius: 5))
            .contentShape(RoundedRectangle(cornerRadius: 5))
            .opacity(enabled ? 1 : 0.4)
    }
}

extension WorkflowPhase {
    var color: Color {
        switch self {
        case .ready, .published: Theme.green
        case .needsYou: Theme.amber
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
            Text(title).font(Theme.font(14, weight: .semibold)).foregroundStyle(Theme.ink)
            Spacer()
            if !detail.isEmpty { Text(detail).font(Theme.font(12)).foregroundStyle(Theme.muted) }
        }
    }
}
