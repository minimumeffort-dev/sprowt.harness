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
                                }
                                Spacer(minLength: 0)
                            }.frame(maxWidth: .infinity, alignment: .leading)
                        }.buttonStyle(WorkspaceButtonStyle(selected: selected)).disabled(!session.interactive)
                            .accessibilityLabel("\(mod.name), \(mod.status)")
                    }
                }
            }
            Spacer(minLength: 8)
        }.padding(.horizontal, 12).padding(.top, 18).padding(.bottom, 10)
            .frame(maxHeight: .infinity).background(Theme.sidebar)
    }
}
