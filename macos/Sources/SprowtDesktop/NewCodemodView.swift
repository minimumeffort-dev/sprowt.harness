import SwiftUI

struct NewCodemodView: View {
    @Bindable var session: Session
    @State private var editor = ComposerEditorController()

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 32)
            VStack(spacing: 24) {
                HStack {
                    Spacer(minLength: 0)
                    Text("What would you like to change?").font(Theme.font(22, weight: .semibold))
                    Spacer(minLength: 0)
                }
                VStack(spacing: 0) {
                    ComposerEditor(text: $session.newDescription, controller: editor,
                                   enabled: session.interactive && !session.busy) { create() }
                        .frame(height: 166)
                        .background(Theme.paper)
                    HStack(spacing: 8) {
                        Button { editor.toggleMenu() } label: {
                            Label("Format", systemImage: "textformat")
                        }.buttonStyle(WorkspaceButtonStyle(selected: editor.menu != nil))
                            .disabled(!session.interactive || session.busy)
                        GlyphButton(symbol: "mic", label: "Dictate") { editor.dictate() }
                            .disabled(!session.interactive || session.busy)
                        Spacer()
                        Button { create() } label: {
                            HStack(alignment: .firstTextBaseline, spacing: 10) {
                                Text("Create codemod")
                                Text("⌘↩").font(Theme.font(10))
                                Image(systemName: "arrow.up").font(.system(size: 12)).symbolBaseline(textSize: 12)
                            }
                        }.buttonStyle(WorkspaceButtonStyle(prominent: true))
                            .keyboardShortcut(.return, modifiers: .command)
                            .disabled(!session.interactive || session.busy || session.newDescription.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }.padding(14)
                }
                .background(Theme.paper, in: RoundedRectangle(cornerRadius: 12))
                .clipShape(RoundedRectangle(cornerRadius: 12))
                .overlay(RoundedRectangle(cornerRadius: 12).stroke(Theme.control))
                .overlay(alignment: .bottomLeading) {
                    if let menu = editor.menu {
                        VStack(alignment: .leading, spacing: 2) {
                            ForEach(Array(ComposerFormat.allCases.enumerated()), id: \.element) { index, format in
                                Button { editor.apply(format) } label: {
                                    HStack {
                                        Label(format.title, systemImage: format.symbol)
                                        Spacer()
                                        if let shortcut = format.shortcut {
                                            Text(shortcut).font(Theme.font(10)).foregroundStyle(Theme.muted)
                                        }
                                    }.frame(maxWidth: .infinity, alignment: .leading)
                                }.buttonStyle(WorkspaceButtonStyle(selected: index == menu.selected))
                            }
                        }.padding(6).frame(width: 192)
                            .background(Theme.panel, in: RoundedRectangle(cornerRadius: 8))
                            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.control))
                            .shadow(color: .black.opacity(0.08), radius: 12, y: 4)
                            .padding(.leading, 14).offset(y: -52)
                    }
                }
            }.frame(maxWidth: 620)
            Spacer(minLength: 32)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .overlay(alignment: .topTrailing) {
            if session.active != nil {
                GlyphButton(symbol: "xmark", label: "Return to codemod") { if let id = session.active?.id { session.select(id) } }
                    .padding(Theme.inset)
            }
        }
    }

    private func create() {
        editor.commit()
        session.create()
    }
}
