use std::{collections::BTreeSet, fs, path::Path};

use super::Repair;
use crate::{
    execution::{Execution, TaskRun},
    plan::Plan,
    workspace,
};

pub(super) fn validate(
    execution: &Execution,
    run: &TaskRun,
    plan: &Plan,
    repair: &Repair,
) -> Result<(), String> {
    if !matches!(run.status.as_str(), "running" | "checking") {
        return Err(
            "Missing file assignments must be reported by the current running task.".into(),
        );
    }
    let root = &execution.workspace;
    let results = crate::checks::observed::results(root, run.id, &run.source)
        .map_err(|error| format!("Cannot verify missing-file evidence: {error}"))?;
    if !results.iter().any(|result| {
        result.check == repair.check
            && result.command == repair.command
            && result.exit_code.is_some_and(|code| code != 0)
            && result.missing_runtime.is_none()
    }) {
        return Err("A missing file assignment needs a fresh failed run_task_checks result for this exact task attempt, source and declared command. Reproduce the failure before requesting repair.".into());
    }
    let read = |suffix: &str| -> Result<workspace::Snapshot, String> {
        let bytes = fs::read(root.join(format!("task-drafts/{}{suffix}.json", run.id)))
            .map_err(|error| format!("Cannot read saved task source: {error}"))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("Cannot read saved task source: {error}"))
    };
    let base = read("-base")?;
    let current = read("")?;
    let mut paths = BTreeSet::new();
    for file in &repair.files {
        let path = Path::new(file);
        if !paths.insert(path)
            || path
                .components()
                .collect::<std::path::PathBuf>()
                .as_os_str()
                != file.as_str()
            || !workspace::source_path(root, path)
            || workspace::generated_metadata(path)
        {
            return Err(format!(
                "Missing assignment requires unique exact source files: {file}"
            ));
        }
        if plan
            .tasks
            .iter()
            .flat_map(|task| &task.files)
            .any(|scope| path.starts_with(scope) || Path::new(scope).starts_with(path))
        {
            return Err(format!(
                "Cannot claim {file}: it overlaps an existing task's file ownership."
            ));
        }
        let original = base.iter().find(|(saved, _, _)| saved == path);
        if original.is_none()
            || original != current.iter().find(|(saved, _, _)| saved == path)
            || original.is_some_and(|(_, _, mode)| *mode > 0o777)
        {
            return Err(format!(
                "Cannot claim {file}: it must be an existing, unchanged regular source file in this task. New files, directories and prior out-of-scope edits require a revised plan."
            ));
        }
    }
    Ok(())
}
