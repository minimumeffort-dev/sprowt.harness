import SwiftUI

struct SidebarView: View {
    @Bindable var session: Session

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Menu {
                Button("Open project…", systemImage: "folder.badge.plus") { session.chooseProject() }
                    .disabled(session.connection == .stopping)
                Divider()
                Button(session.showClosed ? "Active codemods" : "Closed codemods",
                       systemImage: session.showClosed ? "diamond" : "archivebox") { session.showClosed.toggle() }
                    .disabled(!session.interactive)
            } label: {
                Text("Sprowt").font(Theme.font(19, weight: .semibold))
            }.menuStyle(.borderlessButton).menuIndicator(.visible).fixedSize()
                .accessibilityLabel("Sprowt menu").help("Sprowt menu")
                .padding(.horizontal, 9).padding(.bottom, 20)
            Button { session.beginCreation() } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: "square.and.pencil").font(.system(size: 13)).symbolBaseline(textSize: 12)
                    Text("New codemod")
                    Spacer()
                    Text("⌘N").font(Theme.font(10)).foregroundStyle(Theme.muted)
                }
            }.buttonStyle(WorkspaceButtonStyle(selected: session.showingNew || session.active == nil))
                .disabled(!session.canBeginCreation).padding(.bottom, 22)
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "folder").font(.system(size: 12)).foregroundStyle(Theme.muted).symbolBaseline(textSize: 12)
                Text(session.snapshot.map { URL(fileURLWithPath: $0.project).lastPathComponent } ?? "Project")
                    .font(Theme.font(12, weight: .medium)).lineLimit(1)
                    .help(session.snapshot?.project ?? "")
                Spacer(minLength: 0)
            }.padding(.horizontal, 9).padding(.bottom, 12)
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    if session.visibleMods.isEmpty {
                        Text(session.showClosed ? "No closed codemods" : "No codemods")
                            .font(Theme.font(12)).foregroundStyle(Theme.muted).padding(9)
                    }
                    ForEach(session.visibleMods) { mod in
                        let selected = !session.showingNew && session.active?.id == mod.id
                        Button { session.select(mod.id) } label: {
                            HStack(alignment: .firstTextBaseline, spacing: 8) {
                                Image(systemName: mod.closed ? "archivebox" : "diamond")
                                    .font(.system(size: 11)).foregroundStyle(Theme.muted).frame(width: 13).symbolBaseline(textSize: 12)
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(mod.name).lineLimit(2).font(Theme.font(12, weight: selected ? .semibold : .medium))
                                    Text(mod.status).font(Theme.font(10)).foregroundStyle(Theme.muted).lineLimit(2)
                                    if !mod.workers.isEmpty {
                                        Text("\(mod.workers.count) agents").font(Theme.font(10)).foregroundStyle(Theme.muted)
                                    }
                                }
                                Spacer(minLength: 0)
                            }.frame(maxWidth: .infinity, alignment: .leading)
                        }.buttonStyle(WorkspaceButtonStyle(selected: selected)).disabled(!session.interactive)
                        if selected && !mod.workers.isEmpty {
                            VStack(alignment: .leading, spacing: 1) {
                                ForEach(mod.workers) { worker in
                                    Button { session.workspace.show(.activity, target: .worker(worker.id)) } label: {
                                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                                            Image(systemName: "arrow.turn.down.right").font(.system(size: 10)).symbolBaseline(textSize: 10)
                                            VStack(alignment: .leading, spacing: 3) {
                                                Text("\(worker.provider.capitalized) · \(worker.role)").lineLimit(2)
                                                if let id = worker.taskId, let task = session.active?.plan?.tasks.first(where: { $0.id == id }) {
                                                    Text(task.title).lineLimit(1)
                                                } else { Text(worker.state).lineLimit(1) }
                                            }
                                            Spacer(minLength: 0)
                                        }.font(Theme.font(10)).foregroundStyle(Theme.muted)
                                            .frame(maxWidth: .infinity, alignment: .leading)
                                    }.buttonStyle(WorkspaceButtonStyle(selected: session.workspace.target == .worker(worker.id)))
                                        .accessibilityLabel("Inspect \(worker.provider) \(worker.role) \(worker.id)")
                                }
                            }.padding(.leading, 20).padding(.bottom, 6)
                                .overlay(alignment: .leading) { Rectangle().fill(Theme.line).frame(width: 1).padding(.leading, 16) }
                        }
                    }
                }
            }
            Spacer(minLength: 8)
        }.padding(.horizontal, 12).padding(.top, 18).padding(.bottom, 10)
            .frame(maxHeight: .infinity).background(Theme.sidebar)
    }
}
