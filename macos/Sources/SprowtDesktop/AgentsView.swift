import AppKit
import SwiftUI

struct AgentsView: View {
    let agents: AgentsEvidence
    @Binding var selection: WorkspaceSelection
    var selectedWorker: Int64?

    var body: some View {
        VStack(alignment: .leading, spacing: 27) {
            ForEach(agents.workers.filter { selectedWorker == nil || $0.id == selectedWorker }) { agent in
                worker(agent)
            }
            if !agents.timings.isEmpty { timings }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func worker(_ agent: AgentEvidence) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 9) {
                HStack {
                    Text(agent.taskNumber.map { "Task \($0)" } ?? agent.worker.role.capitalized)
                        .font(Theme.font(12, weight: .medium)).foregroundStyle(Theme.muted)
                    Spacer(minLength: 12)
                    Text(agent.state).font(Theme.font(11, weight: .medium))
                        .foregroundStyle(statusColor(agent))
                        .padding(.horizontal, 7).padding(.vertical, 3)
                        .background(agent.tone == "working" ? Theme.preparing : statusColor(agent).opacity(0.08), in: RoundedRectangle(cornerRadius: 4))
                }
                Text(agent.title).font(Theme.font(18, weight: .semibold))
                    .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                if let failure = agent.failure {
                    Label(failure.summary, systemImage: "exclamationmark.circle")
                        .foregroundStyle(Theme.red).font(Theme.font(13))
                        .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                } else if !agent.activityPreview.isEmpty {
                    Label(agent.activityPreview, systemImage: agent.tone == "complete" ? "checkmark.circle" : "arrow.triangle.2.circlepath")
                        .foregroundStyle(Theme.muted).font(Theme.font(13))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }.padding(.horizontal, 22).padding(.top, 20).padding(.bottom, 18)
                .frame(maxWidth: .infinity, alignment: .leading)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.accent).frame(width: 3) }
            WorkspaceDivider()
            ribbon(agent)
            if !agent.output.isEmpty && !selection.agentDetails.contains("activity-hidden:\(agent.id)") {
                AgentConsole(output: agent.output)
            }
        }.background(Theme.paper)
            .clipShape(RoundedRectangle(cornerRadius: 9))
            .overlay { RoundedRectangle(cornerRadius: 9).strokeBorder(Theme.line, lineWidth: 1) }
    }

    private func ribbon(_ agent: AgentEvidence) -> some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 20) {
                workerFields(agent).fixedSize(horizontal: true, vertical: false)
                Spacer(minLength: 0)
                activityToggle(agent).fixedSize()
            }
            VStack(alignment: .leading, spacing: 12) {
                workerFields(agent)
                HStack { Spacer(minLength: 0); activityToggle(agent) }
            }
        }.padding(.horizontal, 22).padding(.vertical, 12)
            .frame(maxWidth: .infinity, alignment: .leading).background(Theme.sidebar)
    }

    private func workerFields(_ agent: AgentEvidence) -> some View {
        HStack(alignment: .top, spacing: 14) {
            workerField("Worker", value: agent.workerLabel)
            Rectangle().fill(Theme.line).frame(width: 1, height: 31)
            workerField("Model", value: agent.model, monospaced: true)
            Rectangle().fill(Theme.line).frame(width: 1, height: 31)
            workerField("Reasoning", value: agent.reasoning)
        }.textSelection(.enabled)
    }

    private func workerField(_ label: String, value: String, monospaced: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label).font(Theme.font(11)).foregroundStyle(Theme.muted)
            Text(value).font(monospaced ? .system(size: Theme.scaledSize(12), design: .monospaced) : Theme.font(12))
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder private func activityToggle(_ agent: AgentEvidence) -> some View {
        if !agent.output.isEmpty {
            let key = "activity-hidden:\(agent.id)"
            let hidden = selection.agentDetails.contains(key)
            Button { toggle(key) } label: {
                HStack(spacing: 6) {
                    Text(hidden ? "Show activity" : "Hide activity")
                    Image(systemName: hidden ? "chevron.down" : "chevron.up").font(.system(size: 9, weight: .medium))
                }.font(Theme.font(11)).foregroundStyle(Theme.muted)
            }.buttonStyle(.plain)
                .accessibilityLabel(hidden ? "Show activity" : "Hide activity")
                .accessibilityValue(hidden ? "Collapsed" : "Expanded")
        }
    }

    private var timings: some View {
        let expanded = selection.agentDetails.contains("all-timings")
        let durationFont = NSFont.monospacedDigitSystemFont(ofSize: Theme.scaledSize(12), weight: .medium)
        let durationWidth = max(55, ceil(agents.timings.map {
            ($0.duration as NSString).size(withAttributes: [.font: durationFont]).width
        }.max() ?? 0))
        return VStack(alignment: .leading, spacing: 13) {
            HStack {
                Label("Setup timings", systemImage: "clock")
                    .font(Theme.font(13, weight: .semibold))
                Spacer(minLength: 12)
                if agents.timings.count > 3 {
                    Button(expanded ? "Show longest 3" : "Show all \(agents.timings.count)") { toggle("all-timings") }
                        .buttonStyle(.plain).font(Theme.font(11)).foregroundStyle(Theme.accent)
                        .accessibilityValue(expanded ? "Expanded" : "Collapsed")
                }
            }
            VStack(spacing: 12) {
                ForEach(Array(agents.timings.prefix(expanded ? agents.timings.count : 3).enumerated()), id: \.element.label) { index, timing in
                    timingRow(timing, strongest: index == 0, durationWidth: durationWidth)
                }
            }
            Text("Recorded operation durations · may overlap")
                .font(Theme.font(11)).foregroundStyle(Theme.muted)
        }
    }

    private func timingRow(_ timing: TimingEvidence, strongest: Bool, durationWidth: CGFloat) -> some View {
        TimingColumns(durationWidth: durationWidth) {
            Text(timing.label).font(Theme.font(12))
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    RoundedRectangle(cornerRadius: 2).fill(Theme.timingTrack)
                    RoundedRectangle(cornerRadius: 2).fill(strongest ? Theme.accent : Theme.timingBar)
                        .frame(width: geometry.size.width * timing.relativeDuration)
                }
            }.frame(maxWidth: .infinity).frame(height: 5).accessibilityHidden(true)
            Text(timing.duration).font(Theme.font(12, weight: .medium)).monospacedDigit().fixedSize()
        }.accessibilityElement(children: .combine)
    }

    private func statusColor(_ agent: AgentEvidence) -> Color {
        switch agent.tone {
        case "attention": Theme.amber
        case "working": Theme.blue
        case "complete": Theme.accent
        default: Theme.muted
        }
    }

    private func toggle(_ key: String) {
        if selection.agentDetails.contains(key) { selection.agentDetails.remove(key) }
        else { selection.agentDetails.insert(key) }
    }
}

// Keep source selectable and copyable while drawing line numbers outside the text.
struct AgentConsole: NSViewRepresentable {
    let output: String

    func makeNSView(context: Context) -> AgentConsoleScrollView {
        let scroll = AgentConsoleScrollView()
        scroll.outputView.load(output)
        return scroll
    }

    func updateNSView(_ scroll: AgentConsoleScrollView, context: Context) {
        scroll.outputView.load(output)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: AgentConsoleScrollView, context: Context) -> CGSize? {
        nsView.fittingSize(width: proposal.width ?? 780)
    }
}

final class AgentConsoleScrollView: NSScrollView {
    let outputView = AgentConsoleTextView()

    init() {
        super.init(frame: .zero)
        borderType = .noBorder
        hasVerticalScroller = true
        autohidesScrollers = true
        scrollerStyle = .overlay
        backgroundColor = NSColor(Theme.terminalBackground)
        documentView = outputView
        setAccessibilityLabel("Raw worker activity")
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func fittingSize(width: CGFloat) -> CGSize {
        NSSize(width: width, height: min(outputView.measuredHeight(width: width), 320))
    }

    override func layout() {
        super.layout()
        let width = contentView.bounds.width
        guard width > 0 else { return }
        let size = NSSize(width: width, height: outputView.measuredHeight(width: width))
        if outputView.frame.size != size { outputView.setFrameSize(size) }
    }

}

final class AgentConsoleTextView: NSTextView {
    private let documentStorage: NSTextStorage
    private let measurementLayout = NSLayoutManager()
    private let measurementContainer = NSTextContainer(size: .zero)
    private var source = ""
    private var lineNumbers: [Int: Int] = [:]
    private var numberAttributes: [NSAttributedString.Key: Any] = [:]

    init() {
        documentStorage = NSTextStorage()
        let layout = NSLayoutManager()
        documentStorage.addLayoutManager(layout)
        let container = NSTextContainer(size: NSSize(width: 780, height: CGFloat.greatestFiniteMagnitude))
        layout.addTextContainer(container)
        super.init(frame: NSRect(x: 0, y: 0, width: 780, height: 50), textContainer: container)
        documentStorage.addLayoutManager(measurementLayout)
        measurementLayout.addTextContainer(measurementContainer)
        measurementContainer.lineFragmentPadding = 0
        isEditable = false
        isSelectable = true
        isRichText = false
        drawsBackground = false
        usesAdaptiveColorMappingForDarkAppearance = false
        textContainerInset = NSSize(width: 21, height: 17)
        container.lineFragmentPadding = 0
        container.widthTracksTextView = true
        isVerticallyResizable = true
        isHorizontallyResizable = false
        // The viewport owns document sizing; speculative SwiftUI measurements stay separate.
        autoresizingMask = []
        minSize = .zero
        maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        selectedTextAttributes = [.backgroundColor: NSColor(Theme.selected), .foregroundColor: NSColor(Theme.terminalInk)]
        setAccessibilityLabel("Raw worker activity")
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func load(_ output: String) {
        let size = Theme.scaledSize(12)
        guard source != output || font?.pointSize != size else { return }
        source = output
        let font = NSFont.monospacedSystemFont(ofSize: size, weight: .regular)
        self.font = font
        let value = output as NSString
        lineNumbers = [:]
        var offset = 0
        while offset < value.length {
            lineNumbers[offset] = lineNumbers.count + 1
            value.getLineStart(nil, end: &offset, contentsEnd: nil, for: NSRange(location: offset, length: 0))
        }
        if output.isEmpty || output.last?.isNewline == true {
            lineNumbers[value.length] = lineNumbers.count + 1
        }
        let digits = max(2, String(lineNumbers.count).count)
        numberAttributes = [.font: font, .foregroundColor: NSColor(Theme.terminalMuted)]
        let numberWidth = (String(repeating: "0", count: digits) as NSString).size(withAttributes: numberAttributes).width
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineSpacing = 8
        paragraph.headIndent = numberWidth + 12
        paragraph.firstLineHeadIndent = paragraph.headIndent
        documentStorage.setAttributedString(NSAttributedString(string: output, attributes: [
            .font: font, .foregroundColor: NSColor(Theme.terminalInk), .paragraphStyle: paragraph
        ]))
        needsDisplay = true
        enclosingScrollView?.needsLayout = true
    }

    func measuredHeight(width: CGFloat) -> CGFloat {
        measurementContainer.size = NSSize(width: max(1, width - textContainerInset.width * 2), height: .greatestFiniteMagnitude)
        measurementLayout.ensureLayout(for: measurementContainer)
        let textHeight = max(measurementLayout.usedRect(for: measurementContainer).maxY, measurementLayout.extraLineFragmentRect.maxY)
        return ceil(textHeight + textContainerInset.height * 2)
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        guard let layoutManager, let textContainer else { return }
        let origin = textContainerOrigin
        let glyphs = layoutManager.glyphRange(forBoundingRect: dirtyRect.offsetBy(dx: -origin.x, dy: -origin.y), in: textContainer)
        layoutManager.enumerateLineFragments(forGlyphRange: glyphs) { _, used, _, range, _ in
            let character = layoutManager.characterIndexForGlyph(at: range.location)
            if let number = self.lineNumbers[character] { self.drawNumber(number, y: origin.y + used.minY) }
        }
        if let number = lineNumbers[(string as NSString).length], layoutManager.extraLineFragmentTextContainer != nil {
            drawNumber(number, y: origin.y + layoutManager.extraLineFragmentRect.minY)
        }
    }

    private func drawNumber(_ number: Int, y: CGFloat) {
        let digits = max(2, String(lineNumbers.count).count)
        let label = String(format: "%0*d", digits, number) as NSString
        label.draw(at: NSPoint(x: textContainerOrigin.x, y: y), withAttributes: numberAttributes)
    }
}

private struct TimingColumns: Layout {
    let durationWidth: CGFloat
    private let spacing: CGFloat = 18

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? 780
        let duration = subviews[2].sizeThatFits(.unspecified)
        let labelWidth = max(0, width - durationWidth - spacing * 2) / 2.65
        let label = subviews[0].sizeThatFits(ProposedViewSize(width: labelWidth, height: nil))
        return CGSize(width: width, height: max(label.height, duration.height))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let duration = subviews[2].sizeThatFits(.unspecified)
        let available = max(0, bounds.width - durationWidth - spacing * 2)
        let labelWidth = available / 2.65
        subviews[0].place(at: CGPoint(x: bounds.minX, y: bounds.midY), anchor: .leading,
                          proposal: ProposedViewSize(width: labelWidth, height: nil))
        subviews[1].place(at: CGPoint(x: bounds.minX + labelWidth + spacing, y: bounds.midY), anchor: .leading,
                          proposal: ProposedViewSize(width: available - labelWidth, height: 5))
        subviews[2].place(at: CGPoint(x: bounds.maxX, y: bounds.midY), anchor: .trailing,
                          proposal: ProposedViewSize(duration))
    }
}
