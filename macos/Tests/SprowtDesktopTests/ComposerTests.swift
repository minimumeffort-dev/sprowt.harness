import AppKit
import Foundation
import Testing
@testable import SprowtDesktop

@Test @MainActor func markdownSourceAndSupportedStylesRoundTripWithoutLosingRequests() {
    for source in [
        "Add a view in /Users/horia/src/foo_bar.swift 🌱\nKeep the full request.",
        "# Overview\n\nUse **bold**, *italic*, and **mixed _emphasis_** with café 👩🏽‍💻.",
        "- First\n- Second\n- [ ] Pending\n- [x] Complete\n",
        "Literal \\*asterisks\\* and \\`ticks\\` with a C:\\Users\\folder path.",
        "Links [keep their source](https://example.com) and ~~unsupported syntax~~ stay intact.",
        "![Image alt](image.png) and <https://example.com> keep their source.",
        "```swift\nlet text = \"**literal** _literal_ `ticks` 🌱\"\n```\n\nNext paragraph.",
        "````\n```\ninner fence\n```\n````", "```swift\nfirst\n```\n```swift\nsecond\n```", "# ", "```\n```",
        "Use `` `ticks` `` and `code` without altering punctuation: [], {}, /, $.",
    ] {
        #expect(ComposerMarkdown.encode(ComposerMarkdown.decode(source)) == source)
    }
    let text = ComposerMarkdown.decode("# Heading\n- **Bold** and *italic*\n- [ ] Task\n`code`")
    #expect(text.string == "Heading\n• Bold and italic\n☐ Task\ncode")
    let bold = (text.string as NSString).range(of: "Bold")
    #expect(text.attribute(.composerInline, at: bold.location, effectiveRange: nil) as? Int == 1)
    let italic = (text.string as NSString).range(of: "italic")
    #expect(text.attribute(.composerInline, at: italic.location, effectiveRange: nil) as? Int == 2)
    let code = (text.string as NSString).range(of: "code")
    #expect(text.attribute(.composerInline, at: code.location, effectiveRange: nil) as? Int == 4)
}

@Test @MainActor func richFormattingKeepsUnicodeSelectionAndSupportsNativeUndo() {
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 500, height: 250), styleMask: [.titled], backing: .buffered, defer: false)
    let editor = ComposerTextView(frame: window.contentView!.bounds, textContainer: nil)
    window.contentView = editor
    editor.load("Change café 👩🏽‍💻 controls in src/foo_bar.swift")
    let selected = (editor.string as NSString).range(of: "café 👩🏽‍💻")
    editor.setSelectedRange(selected)
    editor.apply(.bold)
    #expect(editor.string == "Change café 👩🏽‍💻 controls in src/foo_bar.swift")
    #expect(editor.source == "Change **café 👩🏽‍💻** controls in src/foo_bar.swift")
    #expect(editor.selectedRange() == selected)
    #expect(NSFontManager.shared.traits(of: editor.textStorage!.attribute(.font, at: selected.location, effectiveRange: nil) as! NSFont).contains(.boldFontMask))
    editor.undoManager?.undo()
    #expect(editor.source == "Change café 👩🏽‍💻 controls in src/foo_bar.swift")
    editor.undoManager?.redo()
    #expect(editor.source == "Change **café 👩🏽‍💻** controls in src/foo_bar.swift")
    editor.setSelectedRange(selected)
    editor.apply(.italic)
    #expect(editor.source == "Change ***café 👩🏽‍💻*** controls in src/foo_bar.swift")
    editor.apply(.bold)
    #expect(editor.source == "Change *café 👩🏽‍💻* controls in src/foo_bar.swift")
}

@Test @MainActor func blockFormattingCodeAndEditingRetainTheFullRequest() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    editor.load("First 👩🏽‍💻\nSecond `literal`\n\nThird")
    editor.setSelectedRange((editor.string as NSString).range(of: "First 👩🏽‍💻\nSecond literal"))
    editor.apply(.bullets)
    #expect(editor.string == "• First 👩🏽‍💻\n• Second literal\n\nThird")
    #expect(editor.source == "- First 👩🏽‍💻\n- Second `literal`\n\nThird")
    editor.apply(.checklist)
    #expect(editor.source == "- [ ] First 👩🏽‍💻\n- [ ] Second `literal`\n\nThird")
    editor.apply(.codeBlock)
    #expect(editor.source == "```\nFirst 👩🏽‍💻\nSecond literal\n```\n\nThird")
    #expect(ComposerMarkdown.decode(editor.source).string == editor.string)
    editor.load("Literal path /Users/horia/src/foo_bar.swift and `` `code` ``.")
    editor.setSelectedRange(NSRange(location: 0, length: 0))
    editor.insertText("Keep ", replacementRange: editor.selectedRange())
    #expect(editor.source == "Keep Literal path /Users/horia/src/foo_bar.swift and `` `code` ``.")
}

@Test @MainActor func emptyStylesMenuFocusAndMarkedCompositionPreserveTheDraft() {
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 500, height: 250), styleMask: [.titled], backing: .buffered, defer: false)
    let editor = ComposerTextView(frame: window.contentView!.bounds, textContainer: nil)
    let controller = ComposerEditorController()
    controller.editor = editor
    editor.controller = controller
    window.contentView = editor
    editor.load("")
    controller.toggleMenu()
    #expect(window.firstResponder === editor)
    controller.apply(.bold)
    editor.insertText("Unicode 🌱", replacementRange: editor.selectedRange())
    #expect(editor.source == "**Unicode 🌱**")
    editor.load("Keep draft")
    editor.setSelectedRange(NSRange(location: 4, length: 0))
    controller.toggleMenu()
    controller.toggleMenu()
    #expect(editor.source == "Keep draft")
    #expect(editor.selectedRange() == NSRange(location: 4, length: 0))
    editor.setMarkedText("仮", selectedRange: NSRange(location: 1, length: 0), replacementRange: editor.selectedRange())
    let committed = editor.source
    editor.apply(.heading)
    #expect(editor.source == committed)
    editor.unmarkText()
    editor.publish()
    #expect(editor.source == "Keep仮 draft")
}

@Test @MainActor func formattedDescriptionUsesTheExistingCommandBridge() throws {
    let source = "# Outcome\n\n- [ ] Preserve **Unicode 🌱**\n\n```rust\nlet path = \"/Users/horia/src/foo_bar.rs\";\n```"
    let text = ComposerMarkdown.encode(ComposerMarkdown.decode(source))
    let request = Request(id: 1, command: Command(type: "new_mod", description: text))
    let json = try #require(JSONSerialization.jsonObject(with: request.encodedLine()) as? [String: Any])
    let command = try #require(json["command"] as? [String: Any])
    #expect(command["description"] as? String == source)
    #expect(command["type"] as? String == "new_mod")
}

@Test @MainActor func editsInsideFormattedParagraphsDoNotSplitMarkupOrCorruptPaths() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    editor.load("# Heading 🌱\n- **Bold** in /Users/horia/foo_bar_baz.swift\n\n```swift\nlet value = 1\n```\n")
    editor.setSelectedRange(NSRange(location: 0, length: 0))
    editor.insertText("New ", replacementRange: editor.selectedRange())
    #expect(editor.source == "# New Heading 🌱\n- **Bold** in /Users/horia/foo_bar_baz.swift\n\n```swift\nlet value = 1\n```\n")
    let value = (editor.string as NSString).range(of: "value")
    editor.setSelectedRange(value)
    editor.insertText("result", replacementRange: value)
    #expect(editor.source == "# New Heading 🌱\n- **Bold** in /Users/horia/foo_bar_baz.swift\n\n```swift\nlet result = 1\n```\n")
    editor.setSelectedRange((editor.string as NSString).range(of: "Bold"))
    editor.apply(.italic)
    #expect(editor.source == "# New Heading 🌱\n- ***Bold*** in /Users/horia/foo_bar_baz.swift\n\n```swift\nlet result = 1\n```\n")
}

private func composerKey(_ characters: String, code: UInt16, modifiers: NSEvent.ModifierFlags = []) -> NSEvent {
    NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: modifiers, timestamp: 0,
                     windowNumber: 0, context: nil, characters: characters, charactersIgnoringModifiers: characters,
                     isARepeat: false, keyCode: code)!
}

@Test @MainActor func slashMenuCancelsLiteralPathsAndAppliesOnlyItsTrigger() {
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 500, height: 250), styleMask: [.titled], backing: .buffered, defer: false)
    let editor = ComposerTextView(frame: window.contentView!.bounds, textContainer: nil)
    window.contentView = editor
    editor.focus()
    let controller = ComposerEditorController()
    controller.editor = editor
    editor.controller = controller
    editor.load("")
    editor.keyDown(with: composerKey("/", code: 44))
    #expect(editor.source == "/")
    #expect(controller.menu?.trigger == NSRange(location: 0, length: 1))
    editor.keyDown(with: composerKey("", code: 53))
    #expect(editor.source == "/")
    #expect(controller.menu == nil)
    editor.load("")
    editor.keyDown(with: composerKey("/", code: 44))
    editor.keyDown(with: composerKey("U", code: 32))
    #expect(editor.source == "/U")
    #expect(controller.menu == nil)
    editor.load("Keep request\n")
    editor.keyDown(with: composerKey("/", code: 44))
    controller.apply(.heading)
    editor.insertText("Outcome", replacementRange: editor.selectedRange())
    #expect(editor.source == "Keep request\n# Outcome")
    editor.load("```\n\n```")
    editor.setSelectedRange(NSRange(location: 0, length: 0))
    editor.keyDown(with: composerKey("/", code: 44))
    #expect(controller.menu == nil)
    #expect(editor.source == "```\n/\n```\n")
}

@Test @MainActor func keyboardFormattingAndReturnFollowDocumentEditingRules() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    editor.load("Selected 🌱")
    editor.setSelectedRange(editor.textStorage!.fullRange)
    editor.keyDown(with: composerKey("b", code: 11, modifiers: .command))
    #expect(editor.source == "**Selected 🌱**")
    editor.keyDown(with: composerKey("i", code: 34, modifiers: .command))
    #expect(editor.source == "***Selected 🌱***")
    let italicFont = editor.textStorage!.attribute(.font, at: 0, effectiveRange: nil) as! NSFont
    #expect(italicFont.fontDescriptor.symbolicTraits.contains(.italic) || editor.textStorage!.attribute(.obliqueness, at: 0, effectiveRange: nil) as? Double == 0.15)
    editor.keyDown(with: composerKey("e", code: 14, modifiers: .command))
    #expect(editor.source == "***`Selected 🌱`***")
    editor.load("# Heading")
    editor.insertNewline(nil)
    editor.insertText("Body", replacementRange: editor.selectedRange())
    #expect(editor.source == "# Heading\nBody")
    editor.load("")
    editor.apply(.heading)
    editor.apply(.heading)
    editor.insertText("Body", replacementRange: editor.selectedRange())
    #expect(editor.source == "Body")
    editor.load("- First")
    editor.insertNewline(nil)
    editor.insertText("Second", replacementRange: editor.selectedRange())
    #expect(editor.source == "- First\n- Second")
    editor.insertNewline(nil)
    editor.insertNewline(nil)
    editor.insertText("Body", replacementRange: editor.selectedRange())
    #expect(editor.source == "- First\n- Second\nBody")
}

@Test @MainActor func nestedInlineEditsRoundTripTheirVisibleStylesAndEscapes() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    editor.load("**bold _italic_** and **other** _outside_ with \\*literal\\*.")
    editor.setSelectedRange(NSRange(location: 0, length: 0))
    editor.insertText("New ", replacementRange: editor.selectedRange())
    let restored = ComposerMarkdown.decode(editor.source)
    #expect(restored.string == editor.string)
    for word in ["bold", "italic", "other", "outside", "literal"] {
        let range = (editor.string as NSString).range(of: word)
        #expect(restored.attribute(.composerInline, at: range.location, effectiveRange: nil) as? Int == editor.textStorage!.attribute(.composerInline, at: range.location, effectiveRange: nil) as? Int)
    }
    #expect(editor.source.contains("\\*literal\\*"))
    editor.load("[a **literal**](https://foo_bar_baz.example) and ~~struck~~")
    editor.setSelectedRange(NSRange(location: 0, length: 0))
    editor.insertText("Keep ", replacementRange: editor.selectedRange())
    #expect(editor.source == "Keep [a **literal**](https://foo_bar_baz.example) and ~~struck~~")
}

@Test @MainActor func unsupportedMarkdownRetainsAllSourceAfterEditing() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    for source in ["![Image alt](image.png)", "<https://example.com>", "<b>HTML</b>", "&amp; an entity", "~~strike~~", "[label](https://foo_bar.example)"] {
        editor.load(source)
        #expect(editor.string == source)
        editor.setSelectedRange(NSRange(location: 0, length: 0))
        editor.insertText("Keep ", replacementRange: editor.selectedRange())
        #expect(editor.source == "Keep " + source)
    }
}

@Test @MainActor func adjacentCodeBlocksKeepTheirIdentityWhileEditing() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    editor.load("```swift\nfirst\n```\n```swift\nsecond\n```\n")
    let second = (editor.string as NSString).range(of: "second")
    editor.setSelectedRange(second)
    editor.insertText("changed", replacementRange: second)
    #expect(editor.source == "```swift\nfirst\n```\n```swift\nchanged\n```\n")
}

@Test @MainActor func mixedBlocksApplyOneFormatAndCodeBlocksCanReturnToBody() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    editor.load("# Heading\nBody")
    editor.setSelectedRange(editor.textStorage!.fullRange)
    editor.apply(.heading)
    #expect(editor.source == "# Heading\n# Body")
    editor.apply(.heading)
    #expect(editor.source == "Heading\nBody")
    editor.load("```swift\ncode 🌱\n```\n")
    editor.setSelectedRange(editor.textStorage!.fullRange)
    editor.apply(.codeBlock)
    #expect(editor.source == "code 🌱\n")
}

@Test @MainActor func nativeItalicUsesItsOwnFaceAndTogglesBackToBody() throws {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    editor.load("Italic text")
    editor.setSelectedRange(editor.textStorage!.fullRange)
    editor.apply(.italic)
    let font = try #require(editor.textStorage!.attribute(.font, at: 0, effectiveRange: nil) as? NSFont)
    #expect(font.familyName == Theme.composerFont.familyName)
    #expect(font.fontDescriptor.symbolicTraits.contains(.italic))
    #expect(editor.textStorage!.attribute(.obliqueness, at: 0, effectiveRange: nil) == nil)
    #expect(editor.source == "*Italic text*")
    editor.apply(.italic)
    let plain = try #require(editor.textStorage!.attribute(.font, at: 0, effectiveRange: nil) as? NSFont)
    #expect(!plain.fontDescriptor.symbolicTraits.contains(.italic))
    #expect(editor.source == "Italic text")
}

@Test @MainActor func emphasisKeepsWhitespaceOutsideMarkersAcrossMixedAndMultilineSelections() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    for (input, expected) in [(" hello ", " **hello** "), ("hello ", "**hello** "),
                              (" hello", " **hello**"), ("  ", "  "),
                              (" first \n\n second ", " **first** \n\n **second** ")] {
        editor.load(input)
        editor.setSelectedRange(editor.textStorage!.fullRange)
        editor.apply(.bold)
        #expect(editor.source == expected)
        #expect(ComposerMarkdown.decode(editor.source).string == editor.string)
        if let nonspace = editor.string.firstIndex(where: { !$0.isWhitespace }) {
            let index = (String(editor.string[..<nonspace]) as NSString).length
            #expect(ComposerMarkdown.decode(editor.source).attribute(.composerInline, at: index, effectiveRange: nil) as? Int == 1)
        }
    }
    editor.load(" **bold _italic_** ")
    editor.setSelectedRange(editor.textStorage!.fullRange)
    editor.apply(.italic)
    let restored = ComposerMarkdown.decode(editor.source)
    #expect(restored.string == editor.string)
    let bold = (editor.string as NSString).range(of: "bold")
    let italic = (editor.string as NSString).range(of: "italic")
    #expect(restored.attribute(.composerInline, at: bold.location, effectiveRange: nil) as? Int == 3)
    #expect(restored.attribute(.composerInline, at: italic.location, effectiveRange: nil) as? Int == 3)
}

@Test @MainActor func fencedCodeDoesNotShowEmphasisThatMarkdownCannotPreserve() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    let source = "```swift\nlet code = 1\n```\n"
    editor.load(source)
    editor.setSelectedRange((editor.string as NSString).range(of: "code"))
    for format in [ComposerFormat.bold, .italic, .inlineCode] { editor.apply(format) }
    #expect(editor.source == source)
    let code = (editor.string as NSString).range(of: "code")
    let font = editor.textStorage!.attribute(.font, at: code.location, effectiveRange: nil) as! NSFont
    #expect(!font.fontDescriptor.symbolicTraits.contains(.bold))
    #expect(!font.fontDescriptor.symbolicTraits.contains(.italic))
    #expect(editor.textStorage!.attribute(.composerInline, at: code.location, effectiveRange: nil) as? Int == 0)
    #expect(editor.textStorage!.attribute(.obliqueness, at: code.location, effectiveRange: nil) == nil)
    #expect(ComposerMarkdown.decode(editor.source).string == editor.string)
}

@Test @MainActor func removingCodeFormattingPreservesLiteralMarkupAndItsUnstyledText() {
    let editor = ComposerTextView(frame: .zero, textContainer: nil)
    let literal = "**literal** and a*b*c with a_b_c, [link](url), <tag>, ~~strike~~, &amp; and \\path\n# heading\n- list\n"
    for format in [ComposerFormat.codeBlock, .heading, .bullets, .checklist] {
        editor.load("```\n" + literal + "```\n")
        editor.setSelectedRange(editor.textStorage!.fullRange)
        editor.apply(format)
        let visible = editor.string
        let restored = ComposerMarkdown.decode(editor.source)
        #expect(restored.string == visible)
        let value = (restored.string as NSString).range(of: "literal")
        #expect(restored.attribute(.composerInline, at: value.location, effectiveRange: nil) as? Int == 0)
        if format == .codeBlock { #expect(visible == literal) }
    }
    editor.load("`*literal* and a_b_c`")
    editor.setSelectedRange(editor.textStorage!.fullRange)
    editor.apply(.inlineCode)
    #expect(editor.string == "*literal* and a_b_c")
    #expect(editor.source == "\\*literal\\* and a\\_b\\_c")
    let restored = ComposerMarkdown.decode(editor.source)
    #expect(restored.string == editor.string)
    #expect(restored.attribute(.composerInline, at: 0, effectiveRange: nil) as? Int == 0)
}

@Test @MainActor func longLiteralEscapingPreservesUnicodeAndHyphensAcrossLines() {
    let literal = String(repeating: "long-line-🌱 ", count: 6000) + "**literal**\n  - literal list\n"
    let text = NSMutableAttributedString(string: literal, attributes: ComposerMarkdown.attributes)
    ComposerMarkdown.escapeLiteralCharacters(in: text, range: text.fullRange)
    let encoded = ComposerMarkdown.encode(text)
    #expect(text.string == literal)
    #expect(ComposerMarkdown.decode(encoded).string == literal)
    #expect(encoded.hasSuffix("\\*\\*literal\\*\\*\n  \\- literal list\n"))
}
