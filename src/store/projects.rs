use super::*;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectActivity {
    Idle,
    Working,
    Attention,
}

#[derive(Clone, Serialize)]
pub struct ProjectSummary {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub mods: Vec<ProjectCodemod>,
    pub activity: ProjectActivity,
}

#[derive(Clone, Serialize)]
pub struct ProjectCodemod {
    pub id: i64,
    pub name: String,
    pub closed: bool,
}

impl Store {
    pub fn projects(&self) -> Result<Vec<ProjectSummary>> {
        let mut query = self
            .0
            .prepare("SELECT id, path FROM projects ORDER BY id")?;
        let mut projects = query
            .query_map([], |row| {
                let path: String = row.get(1)?;
                Ok(ProjectSummary {
                    id: row.get(0)?,
                    name: Path::new(&path)
                        .file_name()
                        .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned()),
                    path,
                    mods: Vec::new(),
                    activity: ProjectActivity::Idle,
                })
            })?
            .collect::<Result<Vec<_>>>()?;
        let mut query = self
            .0
            .prepare("SELECT id, name, closed FROM code_mods WHERE project_id=?1 ORDER BY id")?;
        for project in &mut projects {
            project.mods = query
                .query_map([project.id], |row| {
                    Ok(ProjectCodemod {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        closed: row.get(2)?,
                    })
                })?
                .collect::<Result<Vec<_>>>()?;
        }
        Ok(projects)
    }

    pub fn mod_project(&self, id: i64) -> Result<Option<i64>> {
        self.0
            .query_row(
                "SELECT project_id FROM code_mods WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn creation_draft(&self, project: i64) -> Result<String> {
        Ok(self
            .0
            .query_row(
                "SELECT text FROM project_drafts WHERE project_id=?1",
                [project],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn save_creation_draft(&self, project: i64, text: &str) -> Result<()> {
        self.0.execute("INSERT INTO project_drafts(project_id,text) VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET text=excluded.text", params![project,text])?;
        Ok(())
    }
}
