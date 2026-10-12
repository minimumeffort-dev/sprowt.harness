import AppKit
import CoreText
import SwiftUI

enum Theme {
    static let paper = Color(hex: 0xfafaf7)
    static let sidebar = Color(hex: 0xf0f0e9)
    static let panel = Color(hex: 0xf6f6f1)
    static let ink = Color(hex: 0x171717)
    static let muted = Color(hex: 0x62625c)
    static let line = Color(hex: 0xdeded6)
    static let selected = Color(hex: 0xe5e5dc)
    static let gold = Color(hex: 0xf7c843)
    static let green = Color(hex: 0x477759)
    static let red = Color(hex: 0xa13e35)
    static let inset: CGFloat = 24
    static let radius: CGFloat = 7

    static func font(_ size: CGFloat = 13, weight: Font.Weight = .regular) -> Font {
        .custom("Manrope", size: size).weight(weight)
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
    init(hex: UInt32) {
        self.init(.sRGB, red: Double((hex >> 16) & 255) / 255,
                  green: Double((hex >> 8) & 255) / 255, blue: Double(hex & 255) / 255)
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
        case .planning, .building, .checking, .recovering, .reviewing, .git, .needsYou: Theme.gold
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
