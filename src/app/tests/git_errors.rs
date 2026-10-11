use super::*;

fn add_other_mod(app: &mut App, root: &std::path::Path) -> i64 {
    let mut code_mod = app
        .store
        .create_mod(app.project_id, "Other codemod")
        .unwrap();
    code_mod.git_root = Some(root.to_path_buf());
    let id = code_mod.id;
    app.mods.push(code_mod);
    id
}

fn fail_job(app: &mut App, id: i64, label: &'static str, detail: &'static str) {
    app.git_jobs.insert(
        id,
        Job::start(label, move |_| Err(io::Error::other(detail))),
    );
    wait_git(app);
}

#[test]
fn background_git_failure_stays_with_its_codemod_when_switching() {
    let (_data, mut app, root) = execution_app();
    app.mods[0].git_root = Some(root.clone());
    let other_id = add_other_mod(&mut app, &root);
    fail_job(
        &mut app,
        other_id,
        "publishing PR",
        "Other publication failed",
    );

    let active = rows(&screen(&mut app, 120, 36)).join("\n");
    assert!(app.worker_error().is_none(), "{active}");
    assert!(!active.contains("Other publication failed"), "{active}");
    assert_eq!(app.input.lines(), ["keep this draft"]);

    app.view = View::Mods(1);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let other = rows(&screen(&mut app, 120, 36)).join("\n");
    assert_eq!(
        app.worker_error(),
        Some("Other publication failed Work is retained; Ctrl+R retries.")
    );
    assert!(other.contains("Other publication failed"), "{other}");

    app.view = View::Mods(0);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.worker_error().is_none());
    assert_eq!(app.input.lines(), ["keep this draft"]);
    app.view = View::Mods(1);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.worker_error(),
        Some("Other publication failed Work is retained; Ctrl+R retries.")
    );
}

#[test]
fn background_git_success_preserves_active_publication_failure() {
    let (_data, mut app, root) = execution_app();
    app.mods[0].git_root = Some(root.clone());
    let active_id = app.mods[0].id;
    let other_id = add_other_mod(&mut app, &root);
    fail_job(
        &mut app,
        active_id,
        "publishing PR",
        "Active publication failed",
    );
    app.git_jobs.insert(
        other_id,
        Job::start("publishing PR", |_| Ok(git_mod::Result::Published)),
    );
    wait_git(&mut app);

    let visible = rows(&screen(&mut app, 120, 36)).join("\n");
    assert_eq!(
        app.worker_error(),
        Some("Active publication failed Work is retained; Ctrl+R retries.")
    );
    assert!(visible.contains("Active publication failed"), "{visible}");
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn successful_git_operation_clears_only_its_own_failure() {
    let (_data, mut app, root) = execution_app();
    app.mods[0].git_root = Some(root);
    let id = app.mods[0].id;
    fail_job(&mut app, id, "publishing PR", "Publication failed");
    fail_job(&mut app, id, "saving verified merge", "Merge save failed");
    app.git_jobs.insert(
        id,
        Job::start("publishing PR", |_| Ok(git_mod::Result::Published)),
    );
    wait_git(&mut app);
    let failed = rows(&screen(&mut app, 120, 36)).join("\n");
    assert_eq!(
        app.worker_error(),
        Some("Merge save failed Work is retained; Ctrl+R retries.")
    );
    assert!(failed.contains("Merge save failed"), "{failed}");

    app.git_jobs.insert(
        id,
        Job::start("saving verified merge", |_| {
            Ok(git_mod::Result::UpdateFinished)
        }),
    );
    wait_git(&mut app);
    let recovered = rows(&screen(&mut app, 120, 36)).join("\n");
    assert!(app.worker_error().is_none(), "{recovered}");
    assert!(!recovered.contains("Merge save failed"), "{recovered}");
    assert_eq!(app.input.lines(), ["keep this draft"]);
}
