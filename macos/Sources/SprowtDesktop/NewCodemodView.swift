import SwiftUI

struct NewCodemodView: View {
    @Bindable var session: Session
    @FocusState private var focused: Bool

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
                    TextEditor(text: $session.newDescription)
                        .font(Theme.font(14)).padding(16)
                        .scrollContentBackground(.hidden).background(Theme.paper)
                        .frame(height: 166).focused($focused)
                        .accessibilityLabel("Describe the codemod")
                    HStack {
                        Spacer()
                        Button { session.create() } label: {
                            HStack(spacing: 10) {
                                Text("Create codemod")
                                Text("⌘↩").font(Theme.font(10)).foregroundStyle(Theme.muted)
                                Image(systemName: "arrow.up").font(.system(size: 12))
                            }
                        }.buttonStyle(WorkspaceButtonStyle(prominent: true))
                            .keyboardShortcut(.return, modifiers: .command)
                            .disabled(!session.interactive || session.busy || session.newDescription.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }.padding(14)
                }
                .background(Theme.paper, in: RoundedRectangle(cornerRadius: 12))
                .clipShape(RoundedRectangle(cornerRadius: 12))
                .overlay(RoundedRectangle(cornerRadius: 12).stroke(Theme.line))
            }.frame(maxWidth: 620)
            Spacer(minLength: 32)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .overlay(alignment: .topTrailing) {
            if session.active != nil {
                GlyphButton(symbol: "xmark", label: "Return to codemod") { session.showingNew = false }
                    .padding(Theme.inset)
            }
        }
        .onAppear { focused = true }
    }
}
