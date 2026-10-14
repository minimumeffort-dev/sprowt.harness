import SwiftUI

struct ModelChoice: Codable, Equatable, Hashable, Sendable {
    var provider: String
    var model: String
    var effort: String
}
struct WorkerAllocation: Codable, Equatable, Sendable {
    var choice: ModelChoice
    var count: Int
}
struct WorkerSettings: Codable, Equatable, Sendable {
    var planner: ModelChoice
    var executors: [WorkerAllocation]
    var reviewer: ModelChoice
    var capacity: Int { executors.reduce(0) { $0 + $1.count } }
}
struct WorkerModel: Decodable, Identifiable, Sendable {
    let provider: String
    let model: String
    let label: String
    let efforts: [String]
    let defaultEffort: String
    var id: String { "\(provider):\(model)" }
}
struct ModelCatalog: Decodable, Sendable {
    let models: [WorkerModel]
    let loading: Bool
    let errors: [String]
}
struct WorkerSettingsDraft: Identifiable {
    let id = UUID()
    let modId: Int64?
    let settings: WorkerSettings
}

struct WorkerSettingsView: View {
    @Bindable var session: Session
    let draft: WorkerSettingsDraft
    @State private var settings: WorkerSettings

    init(session: Session, draft: WorkerSettingsDraft) {
        self.session = session
        self.draft = draft
        _settings = State(initialValue: draft.settings)
    }
    private var models: [WorkerModel] { session.snapshot?.modelCatalog.models ?? [] }
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 5) {
                    if let mod = session.active, mod.id == draft.modId {
                        Text(mod.name).font(Theme.font(12)).foregroundStyle(Theme.muted).lineLimit(2)
                    }
                    Text("Workers").font(Theme.font(19, weight: .semibold))
                }
                Spacer()
                Text("\(settings.capacity) workers").font(Theme.font(13)).foregroundStyle(Theme.accent)
            }.padding(24)
            WorkspaceDivider()
            ViewThatFits(in: .vertical) {
                settingsBody.fixedSize(horizontal: false, vertical: true)
                ScrollView { settingsBody }
            }.frame(maxHeight: 520)
            WorkspaceDivider()
            HStack {
                VStack(alignment: .leading, spacing: 5) {
                    Text("Changes apply to new assignments.").font(Theme.font(12)).foregroundStyle(Theme.muted)
                    Button { session.enqueue(Command(type: "refresh_models")) } label: {
                        Label(session.snapshot?.modelCatalog.loading == true ? "Loading models…" : "Refresh models", systemImage: "arrow.clockwise")
                    }.buttonStyle(.plain).font(Theme.font(12)).foregroundStyle(Theme.accent)
                        .disabled(session.snapshot?.modelCatalog.loading == true)
                }
                Spacer()
                Button("Cancel") { session.workerSettingsDraft = nil }.buttonStyle(WorkspaceButtonStyle()).keyboardShortcut(.cancelAction)
                Button("Save team") { session.saveWorkerSettings(settings, modId: draft.modId) }
                    .buttonStyle(WorkspaceButtonStyle(prominent: true)).keyboardShortcut(.defaultAction)
                    .disabled(session.busy || models.isEmpty)
            }.padding(.horizontal, 24).padding(.vertical, 14).background(Theme.sidebar)
        }.frame(width: 640).font(Theme.font(13)).foregroundStyle(Theme.ink).background(Theme.paper)
    }

    private var settingsBody: some View {
        VStack(alignment: .leading, spacing: 0) {
            roleRow("Planner", choice: $settings.planner)
            WorkspaceDivider()
            executionTeam
            WorkspaceDivider()
            roleRow("Reviewer", choice: $settings.reviewer)
            WorkspaceDivider()
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "repeat").foregroundStyle(Theme.accent).padding(.top, 2)
                VStack(alignment: .leading, spacing: 5) {
                    Text("Keep working when quota runs out").font(Theme.font(13, weight: .medium))
                    Text("Replace the blocked worker using another model in this team.")
                        .font(Theme.font(12)).foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                }
            }.padding(.horizontal, 24).padding(.vertical, 16)
            if session.error != nil || !(session.snapshot?.modelCatalog.errors.isEmpty ?? true) {
                VStack(alignment: .leading, spacing: 8) {
                    if let error = session.error { Text(error) }
                    ForEach(session.snapshot?.modelCatalog.errors ?? [], id: \.self) { Text($0) }
                }.foregroundStyle(Theme.red).textSelection(.enabled).padding(.horizontal, 24).padding(.bottom, 16)
            }
        }
    }

    private func roleRow(_ label: String, choice: Binding<ModelChoice>) -> some View {
        HStack(spacing: 10) {
            Text(label).font(Theme.font(13, weight: .medium)).frame(width: 95, alignment: .leading)
            selection(choice, label: label)
            Color.clear.frame(width: 33).accessibilityHidden(true)
        }.padding(.horizontal, 24).padding(.vertical, 14)
    }

    private var executionTeam: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Execution team").font(Theme.font(13, weight: .medium))
                Spacer()
                Text("Maximum running together").font(Theme.font(12)).foregroundStyle(Theme.muted)
            }
            ForEach(settings.executors.indices, id: \.self) { index in
                HStack(spacing: 10) {
                    Stepper(value: $settings.executors[index].count, in: 1...max(1, 16 - settings.capacity + settings.executors[index].count)) {
                        Text("\(settings.executors[index].count) ×").monospacedDigit()
                    }.frame(width: 95).accessibilityLabel("Worker count \(index + 1)")
                    selection($settings.executors[index].choice, label: "Execution model \(index + 1)")
                    GlyphButton(symbol: "minus", label: "Remove execution model \(index + 1)") { settings.executors.remove(at: index) }
                        .disabled(settings.executors.count == 1)
                }
            }
            Button { addModel() } label: { Label("Add model", systemImage: "plus") }
                .buttonStyle(.plain).foregroundStyle(Theme.accent).padding(.vertical, 4)
                .disabled(settings.capacity >= 16 || models.isEmpty)
        }.padding(.horizontal, 24).padding(.vertical, 18)
    }
    private func selection(_ choice: Binding<ModelChoice>, label: String) -> some View {
        HStack(spacing: 10) {
            Picker(label, selection: Binding(get: { "\(choice.wrappedValue.provider):\(choice.wrappedValue.model)" }, set: { id in
                guard let model = models.first(where: { $0.id == id }) else { return }
                choice.wrappedValue = ModelChoice(provider: model.provider, model: model.model, effort: model.defaultEffort)
            })) {
                if !models.contains(where: { $0.provider == choice.wrappedValue.provider && $0.model == choice.wrappedValue.model }) {
                    Text(choice.wrappedValue.model).tag("\(choice.wrappedValue.provider):\(choice.wrappedValue.model)")
                }
                ForEach(["codex", "muse"], id: \.self) { provider in
                    Section(provider.capitalized) { ForEach(models.filter { $0.provider == provider }) { model in Text(model.label).tag(model.id) } }
                }
            }.labelsHidden().accessibilityLabel(label).frame(maxWidth: .infinity, alignment: .leading)
            Picker("\(label) effort", selection: choice.effort) {
                let efforts = models.first(where: { $0.provider == choice.wrappedValue.provider && $0.model == choice.wrappedValue.model })?.efforts ?? [choice.wrappedValue.effort]
                ForEach(efforts, id: \.self) { Text($0).tag($0) }
            }.labelsHidden().frame(width: 92, alignment: .leading).accessibilityLabel("\(label) effort")
        }.controlSize(.large).frame(minHeight: 32)
    }
    private func addModel() {
        guard let model = models.first(where: { candidate in !settings.executors.contains { $0.choice.provider == candidate.provider && $0.choice.model == candidate.model } }) else { return }
        settings.executors.append(WorkerAllocation(choice: ModelChoice(provider: model.provider, model: model.model, effort: model.defaultEffort), count: 1))
    }
}
