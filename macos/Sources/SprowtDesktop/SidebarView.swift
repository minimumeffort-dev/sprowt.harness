import SwiftUI

struct SidebarView: View {
    @Bindable var session: Session
    @State private var projectExpanded = true

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { session.beginCreation() } label: {
                HStack(spacing: 8) {
                    Image(systemName: "plus").font(.system(size: 13))
                    Text("New codemod")
                    Spacer()
                    Text("⌘N").font(Theme.font(10)).foregroundStyle(Theme.muted)
                }
            }.buttonStyle(WorkspaceButtonStyle()).disabled(!session.canBeginCreation).padding(.bottom, 22)
            SectionHeading(title: "<codemod/>", detail: String(session.visibleMods.count)).padding(.horizontal, 9).padding(.bottom, 8)
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    if let project = session.snapshot?.project {
                        Button { projectExpanded.toggle() } label: {
                            HStack(spacing: 7) {
                                Image(systemName: projectExpanded ? "chevron.down" : "chevron.right").font(.system(size: 9))
                                Text(URL(fileURLWithPath: project).lastPathComponent).lineLimit(1)
                                Spacer()
                            }
                        }.buttonStyle(WorkspaceButtonStyle()).help(project)
                    }
                    if projectExpanded {
                        if session.visibleMods.isEmpty {
                            Text("No codemods").font(Theme.font(12)).foregroundStyle(Theme.muted).padding(9)
                        }
                        ForEach(session.visibleMods) { mod in
                            let selected = !session.showingNew && session.active?.id == mod.id
                            Button { session.select(mod.id) } label: {
                                HStack(alignment: .top, spacing: 8) {
                                    Image(systemName: mod.closed ? "archivebox" : "chevron.left.forwardslash.chevron.right")
                                        .font(.system(size: 11)).foregroundStyle(Theme.muted).frame(width: 13).padding(.top, 3)
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
                                            HStack(alignment: .top, spacing: 6) {
                                                Image(systemName: "arrow.turn.down.right").font(.system(size: 10)).padding(.top, 2)
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
            }
            Spacer(minLength: 8)
            WorkspaceDivider().padding(.bottom, 8)
            Button { session.showClosed.toggle() } label: {
                HStack(spacing: 8) {
                    Image(systemName: "archivebox").font(.system(size: 12))
                    Text(session.showClosed ? "Hide closed codemods" : "Closed codemods")
                    Spacer()
                    Text(String(session.snapshot?.mods.filter(\.closed).count ?? 0)).font(Theme.font(10))
                }.foregroundStyle(Theme.muted)
            }.buttonStyle(WorkspaceButtonStyle(selected: session.showClosed))
            Button { session.chooseProject() } label: {
                HStack(spacing: 8) {
                    Image(systemName: "folder").font(.system(size: 12))
                    Text("Open project")
                    Spacer()
                    Text("⌘O").font(Theme.font(10))
                }.foregroundStyle(Theme.muted)
            }.buttonStyle(WorkspaceButtonStyle()).disabled(session.connection == .stopping)
        }.padding(.horizontal, 8).padding(.top, 18).padding(.bottom, 10)
            .frame(maxHeight: .infinity).background(Theme.sidebar)
    }
}
