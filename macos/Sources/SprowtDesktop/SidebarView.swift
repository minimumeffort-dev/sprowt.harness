import SwiftUI

struct SidebarView: View {
    @Bindable var session: Session

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(session.showClosed ? "Closed codemods" : "Projects")
                .font(Theme.font(13, weight: .semibold)).foregroundStyle(Theme.muted)
                .padding(.horizontal, 9)
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    ForEach(session.snapshot?.projects ?? []) { project in
                        projectGroup(project)
                    }
                }
            }
        }.padding(.horizontal, 10).padding(.top, 20).padding(.bottom, 10)
            .frame(maxHeight: .infinity).background(Theme.sidebar)
    }

    private func projectGroup(_ project: ProjectSummary) -> some View {
        let expanded = session.snapshot?.expandedProject == project.id
        let showActivity = project.id != session.snapshot?.projectId && project.activity != .idle
        let accessibilityLabel = "\(project.name), \(expanded ? "expanded" : "collapsed")" + (showActivity ? ", \(project.activity.label)" : "")
        let mods = project.mods.filter { $0.closed == session.showClosed }
        return VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 2) {
                Button { session.toggleProject(project.id) } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "folder").frame(width: 18, height: 18)
                        Text(project.name).lineLimit(1)
                        Spacer(minLength: 0)
                        if showActivity {
                            Image(systemName: project.activity == .attention ? "exclamationmark.circle.fill" : "circle.fill")
                                .font(.system(size: project.activity == .attention ? 11 : 6))
                                .foregroundStyle(project.activity == .attention ? Theme.amber : Theme.accent)
                                .help(project.activity.label).accessibilityLabel(project.activity.label)
                        }
                    }.font(Theme.font(12, weight: .medium))
                        .frame(maxWidth: .infinity, minHeight: 30, alignment: .leading)
                        .contentShape(Rectangle())
                }.buttonStyle(.plain).help(project.path)
                    .accessibilityLabel(accessibilityLabel)
                    .disabled(!session.editable || session.busy)
                Menu {
                    Button("Open project") { session.selectProject(project.id) }
                    Button(session.showClosed ? "Show active codemods" : "Show closed codemods") {
                        session.showClosed.toggle()
                        if !expanded { session.toggleProject(project.id) }
                    }
                } label: { Image(systemName: "ellipsis").frame(width: 26, height: 30) }
                    .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
                    .help("Project actions").accessibilityLabel("Actions for \(project.name)")
                    .disabled(!session.interactive || session.busy)
                Button { session.beginCreation(in: project.id) } label: {
                    Image(systemName: "square.and.pencil").frame(width: 26, height: 30)
                }.buttonStyle(.plain).help("New codemod in \(project.name)" + (session.snapshot?.projectId == project.id ? " (⌘N)" : ""))
                    .accessibilityLabel("New codemod in \(project.name)")
                    .disabled(!session.canBeginCreation)
            }.foregroundStyle(Theme.ink).padding(.horizontal, 8)
            if expanded {
                if mods.isEmpty {
                    Text(session.showClosed ? "No closed codemods" : "No codemods")
                        .font(Theme.font(12)).foregroundStyle(Theme.muted)
                        .padding(.leading, 35).padding(.vertical, 8)
                }
                ForEach(mods) { mod in
                    let selected = !session.showingNew && session.active?.id == mod.id
                    Button { session.select(mod.id) } label: {
                        HStack(alignment: .top, spacing: 8) {
                            Image(systemName: mod.closed ? "archivebox" : "diamond")
                                .font(.system(size: 11)).foregroundStyle(Theme.muted)
                                .frame(width: 16, height: Theme.scaledSize(12) + 3)
                            Text(mod.name).lineLimit(2).multilineTextAlignment(.leading)
                                .font(Theme.font(12, weight: selected ? .semibold : .regular))
                            Spacer(minLength: 0)
                        }.frame(maxWidth: .infinity, alignment: .leading)
                    }.buttonStyle(WorkspaceButtonStyle(selected: selected))
                        .padding(.leading, 17).disabled(!session.interactive || session.busy)
                }
            }
        }
    }
}

struct NavigationRail: View {
    @Bindable var session: Session
    @Binding var sidebarVisible: Bool

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "leaf").font(.system(size: 20, weight: .medium))
                .foregroundStyle(Theme.accent).frame(height: 36).accessibilityLabel("Sprowt")
            Button { sidebarVisible = session.showClosed || !sidebarVisible; session.showClosed = false } label: {
                Image(systemName: "folder").frame(width: 34, height: 34)
            }.help("Projects").accessibilityLabel("Projects")
                .background(sidebarVisible && !session.showClosed ? Theme.selected : .clear, in: RoundedRectangle(cornerRadius: 7))
            Button { session.chooseProject() } label: {
                Image(systemName: "folder.badge.plus").frame(width: 34, height: 34)
            }.help("Open project…").accessibilityLabel("Open project")
                .disabled(session.connection == .starting || session.connection == .stopping || session.busy)
            Button { session.showClosed.toggle(); sidebarVisible = true } label: {
                Image(systemName: "archivebox").frame(width: 34, height: 34)
            }.help("Closed codemods").accessibilityLabel("Closed codemods")
                .background(session.showClosed ? Theme.selected : .clear, in: RoundedRectangle(cornerRadius: 7))
                .disabled(!session.editable)
            Spacer(minLength: 12)
            Menu {
                Text("Local profile")
                Picker("Appearance", selection: Binding(get: { session.snapshot?.appearance ?? .system }, set: { session.setAppearance($0) })) {
                    ForEach(Appearance.allCases, id: \.self) { Text($0.label).tag($0) }
                }.disabled(!session.editable)
            } label: {
                Text(String(NSFullUserName().first ?? "S").uppercased())
                    .font(Theme.font(12, weight: .semibold))
                    .frame(width: 30, height: 30)
            }.menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
                .frame(width: 32, height: 32).background(Theme.selected, in: Circle())
                .help("Local profile and appearance").accessibilityLabel("Local profile and appearance")
        }.buttonStyle(.plain).font(.system(size: 15)).foregroundStyle(Theme.muted)
            .padding(.vertical, 14).frame(width: 52).frame(maxHeight: .infinity)
            .background(Theme.sidebar)
            .overlay(alignment: .trailing) { Rectangle().fill(Theme.line).frame(width: 1) }
    }
}
