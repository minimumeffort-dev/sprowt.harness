use serde::Serialize;

/// Shared workspace destinations. Selection and keyboard focus belong to each shell.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceView {
    #[default]
    Conversation,
    Changes,
    Checks,
    Findings,
    Agents,
}

impl WorkspaceView {
    pub const ALL: [Self; 5] = [
        Self::Conversation,
        Self::Changes,
        Self::Checks,
        Self::Findings,
        Self::Agents,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation => "Conversation",
            Self::Changes => "Changes",
            Self::Checks => "Checks",
            Self::Findings => "Findings",
            Self::Agents => "Agents",
        }
    }
}
