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
        VStack(alignment: .leading, spacing: 20) {
            HStack {
                Text("Workers").font(Theme.font(20, weight: .semibold))
                Spacer()
                Text("\(settings.capacity) execution workers").foregroundStyle(Theme.muted)
            }
            // Leave room for native focus rings without shifting the controls.
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Planner").font(Theme.font(12, weight: .semibold))
                        selection($settings.planner, label: "Planner")
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Execution team").font(Theme.font(12, weight: .semibold))
                        ForEach(settings.executors.indices, id: \.self) { index in
                            HStack(spacing: 10) {
                                Stepper(value: $settings.executors[index].count, in: 1...max(1, 16 - settings.capacity + settings.executors[index].count)) {
                                    Text("\(settings.executors[index].count) ×").monospacedDigit().frame(width: 26)
                                }.frame(width: 75).accessibilityLabel("Worker count \(index + 1)")
                                selection($settings.executors[index].choice, label: "Execution model \(index + 1)")
                                GlyphButton(symbol: "minus", label: "Remove execution model \(index + 1)") { settings.executors.remove(at: index) }
                                    .disabled(settings.executors.count == 1)
                            }
                        }
                        Button { addModel() } label: { Label("Add model", systemImage: "plus") }
                            .buttonStyle(WorkspaceButtonStyle()).disabled(settings.capacity >= 16 || models.isEmpty)
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Reviewer").font(Theme.font(12, weight: .semibold))
                        selection($settings.reviewer, label: "Reviewer")
                    }
                    VStack(alignment: .leading, spacing: 5) {
                        Text("Keep working when quota runs out").font(Theme.font(12, weight: .semibold))
                        Text("Replace the blocked worker using another selected model. Changes apply to new assignments.")
                            .foregroundStyle(Theme.muted).fixedSize(horizontal: false, vertical: true)
                    }
                }.padding(.horizontal, 4)
            }.padding(.horizontal, -4).frame(maxHeight: 520)
            if let error = session.error { Text(error).foregroundStyle(Theme.red).textSelection(.enabled) }
            ForEach(session.snapshot?.modelCatalog.errors ?? [], id: \.self) { Text($0).foregroundStyle(Theme.red).textSelection(.enabled) }
            HStack {
                Button { session.enqueue(Command(type: "refresh_models")) } label: {
                    Label(session.snapshot?.modelCatalog.loading == true ? "Loading models…" : "Refresh models", systemImage: "arrow.clockwise")
                }.buttonStyle(WorkspaceButtonStyle()).disabled(session.snapshot?.modelCatalog.loading == true)
                Spacer()
                Button("Cancel") { session.workerSettingsDraft = nil }.buttonStyle(WorkspaceButtonStyle()).keyboardShortcut(.cancelAction)
                Button("Save team") { session.saveWorkerSettings(settings, modId: draft.modId) }
                    .buttonStyle(WorkspaceButtonStyle(prominent: true)).keyboardShortcut(.defaultAction)
                    .disabled(session.busy || models.isEmpty)
            }
        }.padding(24).frame(width: 640).font(Theme.font(12)).foregroundStyle(Theme.ink).background(Theme.paper)
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
            }.labelsHidden().frame(width: 100, alignment: .leading).accessibilityLabel("\(label) effort")
        }
    }
    private func addModel() {
        guard let model = models.first(where: { candidate in !settings.executors.contains { $0.choice.provider == candidate.provider && $0.choice.model == candidate.model } }) else { return }
        settings.executors.append(WorkerAllocation(choice: ModelChoice(provider: model.provider, model: model.model, effort: model.defaultEffort), count: 1))
    }
}
