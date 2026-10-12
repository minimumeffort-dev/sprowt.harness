import SwiftUI

struct ContentView: View {
    @Bindable var session: Session

    var body: some View {
        NavigationSplitView {
            VStack(spacing: 0) {
                HStack {
                    Text("<codemod/>").font(.headline.monospaced())
                    Spacer()
                    Button { session.showingNew = true } label: { Image(systemName: "plus") }
                        .help("New codemod (⌘N)").accessibilityLabel("New codemod")
                        .disabled(!session.editable)
                }.padding()
                if session.visibleMods.isEmpty {
                    Text("No codemods").foregroundStyle(.secondary).padding()
                    Spacer()
                } else {
                    List(selection: Binding<Int64?>(get: { session.showingNew ? nil : session.active?.id }, set: {
                        if let id = $0 { session.select(id) }
                    })) {
                        ForEach(session.visibleMods) { mod in
                            Label(mod.name, systemImage: mod.closed ? "archivebox" : "chevron.left.forwardslash.chevron.right")
                                .lineLimit(2).tag(mod.id)
                        }
                    }.disabled(!session.editable)
                }
                Divider()
                Toggle("Show closed", isOn: $session.showClosed).toggleStyle(.checkbox).padding()
            }
            .navigationSplitViewColumnWidth(min: 210, ideal: 245, max: 350)
        } detail: {
            VStack(spacing: 0) {
                if let error = session.error ?? session.snapshot?.error {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).padding()
                        .background(.red.opacity(0.06))
                }
                if session.connection == .starting || session.connection == .stopping {
                    ProgressView(session.connection == .starting ? "Opening project…" : "Saving and stopping workers…")
                        .padding().frame(maxWidth: .infinity, alignment: .leading)
                }
                if session.showingNew || (session.editable && session.active == nil) {
                    NewCodemodView(session: session)
                } else if let active = session.active {
                    CodemodView(session: session, mod: active)
                } else {
                    ContentUnavailableView {
                        Label("Open a project", systemImage: "folder")
                    } actions: {
                        Button("Choose folder…") { session.chooseProject() }
                    }
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Button { session.chooseProject() } label: { Label("Open project", systemImage: "folder") }
                    .help("Open project (⌘O)").disabled(session.connection == .stopping)
            }
            ToolbarItem(placement: .principal) {
                if let project = session.snapshot?.project {
                    Text(URL(fileURLWithPath: project).lastPathComponent).font(.headline).help(project)
                }
            }
        }
        .sheet(item: Binding(get: { session.dialog }, set: { _ in })) { dialog in
            EngineDialogView(session: session, dialog: dialog)
                .interactiveDismissDisabled()
        }
    }
}

private struct NewCodemodView: View {
    @Bindable var session: Session
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("New codemod").font(.title2.weight(.semibold))
            TextEditor(text: $session.newDescription)
                .font(.body).padding(8)
                .scrollContentBackground(.hidden)
                .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 8))
                .frame(minHeight: 140, maxHeight: 220)
                .focused($focused)
                .accessibilityLabel("Describe the codemod")
            HStack {
                Text("Describe what you want to change.").foregroundStyle(.secondary)
                Spacer()
                Button("Create codemod") { session.create() }
                    .buttonStyle(.borderedProminent).keyboardShortcut(.return, modifiers: .command)
                    .disabled(!session.editable || session.busy || session.newDescription.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            Spacer()
        }.padding(24).onAppear { focused = true }
    }
}

private struct CodemodView: View {
    @Bindable var session: Session
    let mod: CodemodDetail
    @State private var queueOpen = false
    @FocusState private var composerFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text(mod.name).font(.title2.weight(.semibold)).textSelection(.enabled)
                    Spacer()
                    if !mod.workers.isEmpty {
                        Label("\(mod.workers.count) agents", systemImage: "person.2").foregroundStyle(.secondary)
                    }
                    if let actions = session.snapshot?.actions, actions.count > 3 {
                        Menu {
                            ForEach(Array(actions.dropFirst(3)), id: \.identity) { action in
                                Button(action.label) { session.perform(action) }
                            }
                        } label: { Image(systemName: "ellipsis.circle") }
                            .menuStyle(.borderlessButton).fixedSize()
                            .accessibilityLabel("More codemod actions")
                            .disabled(!session.editable || session.busy)
                    }
                }
                if let snapshot = session.snapshot {
                    if !snapshot.status.isEmpty {
                        Text(snapshot.status).font(.subheadline.weight(.medium))
                    }
                    if !snapshot.detail.isEmpty { Text(snapshot.detail).foregroundStyle(.secondary) }
                    if !snapshot.evidence.isEmpty { Text(snapshot.evidence).font(.caption).foregroundStyle(.secondary) }
                }
                if !mod.workers.isEmpty { Text(mod.workers.joined(separator: "\n")).font(.caption).foregroundStyle(.secondary) }
            }.padding(20)
            Divider()
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        ForEach(Array(mod.messages.enumerated()), id: \.offset) { _, message in
                            MessageView(message: message)
                        }
                        if let plan = mod.plan { PlanView(plan: plan) }
                        if !mod.checks.isEmpty { ChecksView(checks: mod.checks) }
                        if !mod.findings.isEmpty { FindingsView(findings: mod.findings) }
                        if !mod.history.isEmpty {
                            DisclosureGroup("Saved history") {
                                VStack(alignment: .leading, spacing: 20) {
                                    ForEach(Array(mod.history.enumerated()), id: \.offset) { _, message in
                                        MessageView(message: message)
                                    }
                                }.padding(.top, 12)
                            }.foregroundStyle(.secondary)
                        }
                        Color.clear.frame(height: 1).id("bottom")
                    }.padding(20).frame(maxWidth: .infinity, alignment: .leading)
                }
                .onChange(of: mod.messages.count) { _, _ in proxy.scrollTo("bottom", anchor: .bottom) }
                .onChange(of: mod.id) { _, _ in proxy.scrollTo("bottom", anchor: .bottom) }
            }
            Divider()
            VStack(alignment: .leading, spacing: 10) {
                if !mod.steering.isEmpty {
                    DisclosureGroup("Steering: waiting for workers (\(mod.steering.count))") {
                        ForEach(Array(mod.steering.enumerated()), id: \.offset) { _, text in
                            Text(text).frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                        }
                    }.foregroundStyle(.secondary)
                }
                HStack(spacing: 8) {
                    ForEach(Array((session.snapshot?.actions ?? []).prefix(3)), id: \.identity) { action in
                        Button(action.label) { session.perform(action) }
                            .disabled(!session.editable || session.busy)
                    }
                    Spacer()
                    if !mod.queue.isEmpty {
                        Button("Queue (\(mod.queue.count))") { queueOpen = true }
                            .disabled(!session.editable)
                    }
                }
                TextEditor(text: Binding(get: { session.draft }, set: { session.editDraft($0) }))
                    .font(.body).padding(6).frame(height: 76)
                    .scrollContentBackground(.hidden)
                    .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 6))
                    .accessibilityLabel("Instruction").focused($composerFocused)
                    .disabled(!session.canCompose)
                HStack {
                    Spacer()
                    Button("Send") { session.submit(); composerFocused = true }
                        .keyboardShortcut(.return, modifiers: .command)
                        .disabled(!session.canCompose || session.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    Text("⌘↩").font(.caption).foregroundStyle(.tertiary)
                }
            }.padding(16)
        }
        .sheet(isPresented: $queueOpen, onDismiss: { composerFocused = true }) {
            QueueView(session: session, modId: mod.id)
        }
        .onChange(of: mod.queue.isEmpty) { _, empty in if empty { queueOpen = false } }
        .onChange(of: mod.id) { _, _ in queueOpen = false }
    }
}

private struct MessageView: View {
    let message: ConversationMessage
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if message.role != "user" {
                Text(([message.model, message.role].compactMap { $0 }).joined(separator: " · "))
                    .font(.caption.weight(.medium)).foregroundStyle(.secondary)
            }
            Text(message.role == "user" ? "> \(message.body)" : message.body)
                .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
        }
        .padding(message.role == "user" ? 10 : 0)
        .background(message.role == "user" ? Color.primary.opacity(0.04) : Color.clear,
                    in: RoundedRectangle(cornerRadius: 6))
    }
}
