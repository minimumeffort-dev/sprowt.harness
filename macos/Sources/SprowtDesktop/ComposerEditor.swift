import AppKit
import Observation
import SwiftUI

@MainActor
@Observable
final class ComposerEditorController {
    struct Menu {
        var selected = 0
        var trigger: NSRange?
    }
    var menu: Menu?
    weak var editor: ComposerTextView?

    func toggleMenu() {
        if menu == nil { menu = Menu() } else { menu = nil }
        editor?.focus()
    }

    func apply(_ format: ComposerFormat) {
        guard let editor, editor.isEditable else { return }
        let trigger = menu?.trigger
        menu = nil
        editor.focus()
        if let trigger, NSMaxRange(trigger) <= editor.string.utf16.count,
           (editor.string as NSString).substring(with: trigger) == "/" {
            editor.insertText("", replacementRange: trigger)
        }
        editor.apply(format)
    }

    func dictate() {
        menu = nil
        guard let editor, editor.isEditable else { return }
        editor.focus()
        NSApp.sendAction(NSSelectorFromString("startDictation:"), to: nil, from: editor)
    }

    func commit() {
        menu = nil
        guard let editor else { return }
        editor.focus()
        NSApp.sendAction(NSSelectorFromString("stopDictation:"), to: nil, from: editor)
        editor.unmarkText()
        editor.publish()
    }
}

struct ComposerEditor: NSViewRepresentable {
    @Binding var text: String
    let controller: ComposerEditorController
    var enabled: Bool
    let submit: () -> Void

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        let editor = ComposerTextView(frame: NSRect(x: 0, y: 0, width: 620, height: 166), textContainer: nil)
        editor.controller = controller
        editor.onChange = { text = $0 }
        editor.onSubmit = submit
        editor.isEditable = enabled
        editor.load(text)
        scroll.documentView = editor
        controller.editor = editor
        DispatchQueue.main.async { editor.focus() }
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let editor = scroll.documentView as? ComposerTextView else { return }
        editor.onChange = { text = $0 }
        editor.onSubmit = submit
        editor.isEditable = enabled
        // Echoes from the binding must not replace storage or erase native editing history.
        if editor.source != text, !editor.hasMarkedText() { editor.load(text) }
    }
}

@MainActor
final class ComposerTextView: NSTextView, NSTextViewDelegate {
    weak var controller: ComposerEditorController?
    var onChange: ((String) -> Void)?
    var onSubmit: (() -> Void)?
    private(set) var source = ""
    private var updating = false
    private let documentStorage: NSTextStorage?

    override init(frame: NSRect, textContainer: NSTextContainer?) {
        let container: NSTextContainer
        if let textContainer {
            container = textContainer
            documentStorage = nil
        } else {
            let storage = NSTextStorage()
            documentStorage = storage
            let layout = NSLayoutManager()
            storage.addLayoutManager(layout)
            container = NSTextContainer(size: NSSize(width: frame.width, height: .greatestFiniteMagnitude))
            layout.addTextContainer(container)
        }
        super.init(frame: frame, textContainer: container)
        delegate = self
        isRichText = true
        allowsUndo = true
        drawsBackground = false
        textContainerInset = NSSize(width: 16, height: 16)
        minSize = NSSize(width: 0, height: frame.height)
        maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        isVerticallyResizable = true
        isHorizontallyResizable = false
        autoresizingMask = [.width]
        self.textContainer?.widthTracksTextView = true
        insertionPointColor = Theme.nsAccent
        selectedTextAttributes = [.backgroundColor: Theme.nsSelected, .foregroundColor: Theme.nsText]
        typingAttributes = ComposerMarkdown.attributes
        isAutomaticQuoteSubstitutionEnabled = false
        isAutomaticDashSubstitutionEnabled = false
        isAutomaticTextReplacementEnabled = false
        isAutomaticSpellingCorrectionEnabled = false
        setAccessibilityLabel("Describe the codemod")
    }
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func load(_ value: String) {
        updating = true
        source = value
        textStorage?.setAttributedString(ComposerMarkdown.decode(value))
        setSelectedRange(NSRange(location: (string as NSString).length, length: 0))
        undoManager?.removeAllActions()
        updating = false
        refreshTypingAttributes()
    }

    func focus() { window?.makeFirstResponder(self) }

    func publish() {
        guard !updating, !hasMarkedText(), let storage = textStorage else { return }
        source = ComposerMarkdown.encode(storage)
        onChange?(source)
    }

    func textDidChange(_ notification: Notification) {
        guard !updating else { return }
        if !hasMarkedText(), let storage = textStorage { ComposerMarkdown.style(storage) }
        publish()
        refreshTypingAttributes()
    }

    func textViewDidChangeSelection(_ notification: Notification) {
        if controller?.menu?.trigger != nil { controller?.menu = nil }
        refreshTypingAttributes()
    }

    private func refreshTypingAttributes() {
        guard !updating, !hasMarkedText() else { return }
        var attrs = ComposerMarkdown.attributes
        let caret = selectedRange().location
        if let storage = textStorage, storage.length > 0 {
            let index = min(caret, storage.length - 1)
            let existing = storage.attributes(at: index, effectiveRange: nil)
            attrs[.composerBlock] = existing[.composerBlock] ?? ""
            attrs[.composerInline] = existing[.composerInline] ?? 0
            attrs[.composerCodeGroup] = existing[.composerCodeGroup]
        }
        let sample = NSMutableAttributedString(string: " ", attributes: attrs)
        ComposerMarkdown.style(sample)
        typingAttributes = sample.attributes(at: 0, effectiveRange: nil)
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        if string.isEmpty {
            let placeholder = "Describe the change…"
            placeholder.draw(at: NSPoint(x: textContainerInset.width + 5, y: textContainerInset.height),
                             withAttributes: [.font: ComposerMarkdown.baseFont, .foregroundColor: Theme.nsMuted])
        }
    }

    override func keyDown(with event: NSEvent) {
        if event.modifierFlags.intersection(.deviceIndependentFlagsMask).contains(.command), !hasMarkedText(),
           let format = ["b": ComposerFormat.bold, "i": .italic, "e": .inlineCode][event.charactersIgnoringModifiers ?? ""] {
            controller?.menu = nil
            apply(format)
            return
        }
        if event.modifierFlags.intersection(.deviceIndependentFlagsMask).contains(.command), event.keyCode == 36 {
            controller?.commit()
            onSubmit?()
            return
        }
        if controller?.menu != nil, !hasMarkedText() {
            switch event.keyCode {
            case 53: controller?.menu = nil; return
            case 125: controller?.menu?.selected = min((controller?.menu?.selected ?? 0) + 1, ComposerFormat.allCases.count - 1); return
            case 126: controller?.menu?.selected = max((controller?.menu?.selected ?? 0) - 1, 0); return
            case 36:
                controller?.apply(ComposerFormat.allCases[controller?.menu?.selected ?? 0])
                return
            default: controller?.menu = nil
            }
        }
        let selection = selectedRange()
        let line = (string as NSString).paragraphRange(for: selection)
        let prefix = (string as NSString).substring(with: NSRange(location: line.location, length: selection.location - line.location))
        let slash = event.characters == "/" && selection.length == 0 && prefix.trimmingCharacters(in: .whitespaces).isEmpty
            && !(typingAttributes[.composerBlock] as? String ?? "").hasPrefix("code:")
            && (typingAttributes[.composerInline] as? Int ?? 0) & 4 == 0
        super.keyDown(with: event)
        if slash { controller?.menu = .init(trigger: NSRange(location: selection.location, length: 1)) }
    }

    override func paste(_ sender: Any?) {
        controller?.menu = nil
        guard let value = NSPasteboard.general.string(forType: .string), !value.isEmpty else { return }
        let normalized = value.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        insertText(ComposerMarkdown.decode(normalized), replacementRange: selectedRange())
    }

    func apply(_ format: ComposerFormat) {
        guard isEditable, !hasMarkedText(), let storage = textStorage else { return }
        let selection = selectedRange()
        breakUndoCoalescing()
        if let bit = format.inlineStyle {
            if selection.length == 0 {
                guard !(typingAttributes[.composerBlock] as? String ?? "").hasPrefix("code:") else { return }
                let current = typingAttributes[.composerInline] as? Int ?? 0
                typingAttributes[.composerInline] = current ^ bit
                let sample = NSMutableAttributedString(string: " ", attributes: typingAttributes)
                ComposerMarkdown.style(sample)
                typingAttributes = sample.attributes(at: 0, effectiveRange: nil)
                return
            }
            let replacement = NSMutableAttributedString(attributedString: storage.attributedSubstring(from: selection))
            var enabled = true, hasProse = false
            replacement.enumerateAttributes(in: replacement.fullRange) { attrs, _, _ in
                guard !(attrs[.composerBlock] as? String ?? "").hasPrefix("code:") else { return }
                hasProse = true
                if (attrs[.composerInline] as? Int ?? 0) & bit == 0 { enabled = false }
            }
            guard hasProse else { return }
            replacement.enumerateAttributes(in: replacement.fullRange) { attrs, range, _ in
                guard !(attrs[.composerBlock] as? String ?? "").hasPrefix("code:") else { return }
                let bits = attrs[.composerInline] as? Int ?? 0
                replacement.addAttribute(.composerInline, value: enabled ? bits & ~bit : bits | bit, range: range)
                if bit == 4, enabled, bits & 4 != 0 { ComposerMarkdown.escapeLiteralCharacters(in: replacement, range: range) }
            }
            replace(selection, with: replacement, selection: selection)
            return
        }
        let paragraphs = (string as NSString).paragraphRange(for: selection)
        if paragraphs.length == 0 {
            let old = typingAttributes[.composerBlock] as? String ?? ""
            typingAttributes[.composerBlock] = old == format.block ? "" : format.block
            typingAttributes[.composerInline] = 0
            typingAttributes[.composerCodeGroup] = (typingAttributes[.composerBlock] as? String ?? "").hasPrefix("code:") ? UUID().uuidString : nil
            if let marker = ComposerMarkdown.marker(format.block) { insertText(marker, replacementRange: selection) }
            let sample = NSMutableAttributedString(string: " ", attributes: typingAttributes)
            ComposerMarkdown.style(sample)
            typingAttributes = sample.attributes(at: 0, effectiveRange: nil)
            return
        }
        let replacement = NSMutableAttributedString(attributedString: storage.attributedSubstring(from: paragraphs))
        var allFormatted = true
        replacement.enumerateAttribute(.composerBlock, in: replacement.fullRange) { value, _, _ in
            let block = value as? String ?? ""
            if format == .codeBlock ? !block.hasPrefix("code:") : block != format.block { allFormatted = false }
        }
        let kind = allFormatted ? "" : format.block
        var position = 0
        var newSelection = selection
        let codeGroup = kind.hasPrefix("code:") ? UUID().uuidString : nil
        while position < replacement.length {
            var range = (replacement.string as NSString).paragraphRange(for: NSRange(location: position, length: 0))
            let old = replacement.attribute(.composerBlock, at: position, effectiveRange: nil) as? String ?? ""
            let removed = ComposerMarkdown.marker(old).flatMap { (replacement.string as NSString).substring(with: range).hasPrefix($0) ? $0 : nil } ?? ""
            let marker = ComposerMarkdown.marker(kind) ?? ""
            let delta = (marker as NSString).length - (removed as NSString).length
            let absoluteStart = paragraphs.location + position
            if absoluteStart <= selection.location { newSelection.location += delta }
            else if absoluteStart < NSMaxRange(selection) { newSelection.length += delta }
            replacement.replaceCharacters(in: NSRange(location: position, length: (removed as NSString).length), with: NSAttributedString(string: marker, attributes: ComposerMarkdown.attributes))
            range.length += delta
            replacement.addAttribute(.composerBlock, value: kind, range: range)
            if kind.hasPrefix("code:") {
                replacement.addAttribute(.composerInline, value: 0, range: range)
                replacement.removeAttribute(.composerEscape, range: range)
            } else if old.hasPrefix("code:") {
                ComposerMarkdown.escapeLiteralCharacters(in: replacement, range: range)
            }
            if let codeGroup { replacement.addAttribute(.composerCodeGroup, value: codeGroup, range: range) }
            else { replacement.removeAttribute(.composerCodeGroup, range: range) }
            replacement.removeAttribute(.composerOrigin, range: range)
            position = NSMaxRange(range)
        }
        newSelection.location = max(paragraphs.location, newSelection.location)
        newSelection.length = max(0, min(newSelection.length, replacement.length + paragraphs.location - newSelection.location))
        replace(paragraphs, with: replacement, selection: newSelection)
    }

    private func replace(_ range: NSRange, with replacement: NSMutableAttributedString, selection: NSRange) {
        ComposerMarkdown.style(replacement)
        insertText(replacement, replacementRange: range)
        setSelectedRange(selection)
        breakUndoCoalescing()
        publish()
    }

    override func insertNewline(_ sender: Any?) {
        guard !hasMarkedText(), textStorage != nil else { super.insertNewline(sender); return }
        let selection = selectedRange()
        let paragraph = (string as NSString).paragraphRange(for: selection)
        let kind = typingAttributes[.composerBlock] as? String ?? ""
        if kind.hasPrefix("#") {
            insertText(NSAttributedString(string: "\n", attributes: ComposerMarkdown.attributes), replacementRange: selection)
            typingAttributes = ComposerMarkdown.attributes
            return
        }
        guard let marker = ComposerMarkdown.marker(kind) else { super.insertNewline(sender); return }
        if (string as NSString).substring(with: paragraph).trimmingCharacters(in: .whitespacesAndNewlines) == marker.trimmingCharacters(in: .whitespaces) {
            let replacement = NSMutableAttributedString(string: "", attributes: ComposerMarkdown.attributes)
            replace(paragraph, with: replacement, selection: NSRange(location: paragraph.location, length: 0))
            typingAttributes = ComposerMarkdown.attributes
        } else {
            var attrs = typingAttributes
            attrs[.composerInline] = 0
            insertText(NSAttributedString(string: "\n" + marker, attributes: attrs), replacementRange: selection)
        }
    }
}
