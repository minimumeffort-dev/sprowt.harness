use crate::{
    appearance::Appearance,
    store::{ProjectSummary, Store},
};
use serde::Serialize;
use std::{
    io::{self, Write},
    path::Path,
};

#[derive(Serialize)]
struct ProjectCatalog {
    protocol_version: u32,
    projects: Vec<ProjectSummary>,
    appearance: Appearance,
}

pub fn run(
    data_dir: Option<&Path>,
    remove: Option<i64>,
    output: &mut impl Write,
) -> io::Result<()> {
    let database = match data_dir {
        Some(root) => root.join("state.db"),
        None => Store::local_path()?,
    };
    let store = Store::open(&database)?;
    if let Some(id) = remove {
        store.remove_project(id).map_err(io::Error::other)?;
    }
    let catalog = ProjectCatalog {
        protocol_version: 1,
        projects: store.projects().map_err(io::Error::other)?,
        appearance: store.appearance().map_err(io::Error::other)?,
    };
    serde_json::to_writer(&mut *output, &catalog)?;
    writeln!(output)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    fn catalog(data: &TestData, remove: Option<i64>) -> serde_json::Value {
        let mut output = Vec::new();
        run(Some(&data.0), remove, &mut output).unwrap();
        serde_json::from_slice(&output).unwrap()
    }

    #[test]
    fn catalog_contract_matches_the_native_fixture() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/example/todo")).unwrap().id;
        store.create_mod(project, "Keep saved work").unwrap();
        store.load_project(Path::new("/example/notes")).unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/projects.json")).unwrap();
        assert_eq!(catalog(&data, None), fixture);
    }

    #[test]
    fn startup_catalog_lists_saved_projects_without_owning_or_loading_workspaces() {
        let data = TestData::new();
        assert_eq!(
            catalog(&data, None),
            serde_json::json!({"protocol_version":1,"projects":[],"appearance":"system"})
        );
        let mut store = data.store();
        let path = data.0.join("project");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("keep.txt"), "keep project files").unwrap();
        let project = store.load_project(&path).unwrap().id;
        let codemod = store.create_mod(project, "Saved work").unwrap().id;
        store.save_draft(codemod, "Unsent instruction").unwrap();
        store.save_creation_draft(project, "Next change").unwrap();
        store.set_appearance(Appearance::Dark).unwrap();
        let lock = crate::engine::ownership::acquire(&data.0.join("state.db"), &path).unwrap();
        let initial = catalog(&data, None);
        assert_eq!(initial["appearance"], "dark");
        assert_eq!(initial["projects"][0]["id"], project);
        assert_eq!(initial["projects"][0]["mods"][0]["id"], codemod);
        assert_eq!(initial["projects"][0]["name"], "project");
        assert_eq!(
            catalog(&data, Some(project))["projects"],
            serde_json::json!([])
        );
        assert_eq!(catalog(&data, None)["projects"], serde_json::json!([]));
        assert_eq!(
            std::fs::read_to_string(path.join("keep.txt")).unwrap(),
            "keep project files"
        );
        let saved = store.load_project(&path).unwrap();
        assert_eq!(saved.mods[0].id, codemod);
        assert_eq!(saved.mods[0].draft, "Unsent instruction");
        assert_eq!(store.creation_draft(project).unwrap(), "Next change");
        assert_eq!(
            catalog(&data, None)["projects"],
            serde_json::json!([]),
            "Reloading stored work must not restore sidebar membership"
        );
        drop(lock);
        let _session = crate::session::Session::from_engine(
            crate::engine::Engine::load(path, data.store()).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog(&data, None)["projects"][0]["id"], project);
    }
}
