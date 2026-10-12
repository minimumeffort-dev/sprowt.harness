use super::*;

fn add_other_mod(app: &mut App, root: &std::path::Path) -> i64 {
    let mut code_mod = app
        .engine
        .store
        .create_mod(app.engine.project_id, "Other codemod")
        .unwrap();
    code_mod.git_root = Some(root.to_path_buf());
    let id = code_mod.id;
    app.engine.mods.push(code_mod);
    id
}

fn fail_job(app: &mut App, id: i64, label: &'static str, detail: &'static str) {
    app.engine.git_jobs.insert(
        id,
        Job::start(label, move |_| Err(io::Error::other(detail))),
    );
    wait_git(app);
}

#[test]
fn background_git_failure_stays_with_its_codemod_when_switching() {
    let (_data, mut app, root) = execution_app();
    app.engine.mods[0].git_root = Some(root.clone());
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

    app.engine.view = View::Mods(1);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let other = rows(&screen(&mut app, 120, 36)).join("\n");
    assert_eq!(
        app.worker_error(),
        Some("Other publication failed Work is retained; Ctrl+R retries.")
    );
    assert!(other.contains("Other publication failed"), "{other}");

    app.engine.view = View::Mods(0);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.worker_error().is_none());
    assert_eq!(app.input.lines(), ["keep this draft"]);
    app.engine.view = View::Mods(1);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.worker_error(),
        Some("Other publication failed Work is retained; Ctrl+R retries.")
    );
}

#[test]
fn background_git_success_preserves_active_publication_failure() {
    let (_data, mut app, root) = execution_app();
    app.engine.mods[0].git_root = Some(root.clone());
    let active_id = app.engine.mods[0].id;
    let other_id = add_other_mod(&mut app, &root);
    fail_job(
        &mut app,
        active_id,
        "publishing PR",
        "Active publication failed",
    );
    app.engine.git_jobs.insert(
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
    app.engine.mods[0].git_root = Some(root);
    let id = app.engine.mods[0].id;
    fail_job(&mut app, id, "publishing PR", "Publication failed");
    fail_job(&mut app, id, "saving verified merge", "Merge save failed");
    app.engine.git_jobs.insert(
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

    let (resume, waiting) = std::sync::mpsc::channel();
    app.engine.git_jobs.insert(
        id,
        Job::start("saving verified merge", move |_| {
            waiting.recv().unwrap();
            Ok(git_mod::Result::UpdateFinished)
        }),
    );
    app.poll_git_jobs().unwrap();
    assert_eq!(
        app.worker_error(),
        Some("Merge save failed Work is retained; Ctrl+R retries.")
    );
    resume.send(()).unwrap();
    wait_git(&mut app);
    let recovered = rows(&screen(&mut app, 120, 36)).join("\n");
    assert!(app.worker_error().is_none(), "{recovered}");
    assert!(!recovered.contains("Merge save failed"), "{recovered}");
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn git_setup_retry_clears_the_initial_failure_even_when_its_label_changes() {
    for saved in [false, true] {
        let (_data, mut app, root) = execution_app();
        commit_project(&app.engine.project);
        app.engine.mods[0].git_root = Some(root.clone());
        let id = app.engine.mods[0].id;
        if saved {
            std::fs::write(root.join("project-setup"), "saved").unwrap();
        }
        fail_job(
            &mut app,
            id,
            if saved {
                "adopting saved codemod"
            } else {
                "checking project branch"
            },
            "Git setup failed",
        );
        assert!(app.worker_error().unwrap().contains("Git setup failed"));
        key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);

        let visible = rows(&screen(&mut app, 120, 36)).join("\n");
        assert!(app.worker_error().is_none(), "{visible}");
        assert!(!visible.contains("Git setup failed"), "{visible}");
        assert_eq!(app.git_state().unwrap().phase, "ready");
        assert!(git_mod::checkout(&root).join("hello.sh").exists());
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(app.engine.workers.is_empty());
    }
}

#[test]
fn publication_writer_recovery_keeps_the_publish_error_until_publication_succeeds() {
    let (_data, mut app, root) = execution_app();
    commit_project(&app.engine.project);
    git_mod::prepare(
        &app.engine.project,
        &root,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let id = app.engine.mods[0].id;
    app.engine.mods[0].git_root = Some(root.clone());
    app.engine
        .git_states
        .insert(id, git_mod::load(&root).unwrap());
    mark_review_clean(&mut app);
    app.engine.review = Some(workspace::review(&root).unwrap());
    let draft = crate::publication::tests::seed(&root, &app.engine.mods[0].description);
    fail_job(&mut app, id, "writing commit and PR text", "Writer failed");
    assert_eq!(
        app.worker_error(),
        Some("Writer failed Work is retained; Ctrl+S retries.")
    );
    fail_job(&mut app, id, "publishing PR", "Push failed");
    app.engine.git_jobs.insert(
        id,
        Job::start("writing commit and PR text", move |_| {
            Ok(git_mod::Result::PublicationDraft(draft))
        }),
    );
    wait_git(&mut app);
    assert!(matches!(app.engine.view, View::Publish));
    assert_eq!(
        app.worker_error(),
        Some("Push failed Work is retained; Ctrl+R retries.")
    );
    app.engine.git_jobs.insert(
        id,
        Job::start("publishing PR", |_| Ok(git_mod::Result::Published)),
    );
    wait_git(&mut app);
    assert!(app.worker_error().is_none());
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn saving_a_merge_keeps_recovery_for_an_unresolved_update_failure() {
    let (data, project, root, target, plan) = crate::mod_sync::tests::fixture();
    let flag = std::sync::atomic::AtomicBool::new(false);
    crate::mod_sync::prepare(&root, &target, &plan, &flag).unwrap();
    let mut update = crate::mod_sync::load(&root).unwrap().unwrap();
    update.installed = true;
    update.imported = true;
    crate::mod_sync::save(&root, &update).unwrap();
    let mut store = data.store();
    let project_id = store.load_project(&project).unwrap().id;
    let code_mod = store.create_mod(project_id, "Recover update").unwrap();
    let id = code_mod.id;
    store.save_git_root(id, &root).unwrap();
    store.save_draft(id, "keep this draft").unwrap();
    let mut app = App::load(project, false, store).unwrap();
    fail_job(&mut app, id, "updating from target branch", "Update failed");
    fail_job(&mut app, id, "saving verified merge", "Merge save failed");
    let fingerprint = workspace::review(&root).unwrap().fingerprint;
    app.engine.git_jobs.insert(
        id,
        Job::start("saving verified merge", move |cancelled| {
            crate::mod_sync::finish(&root, &fingerprint, cancelled)?;
            Ok(git_mod::Result::UpdateFinished)
        }),
    );
    wait_git(&mut app);

    assert_eq!(
        app.worker_error(),
        Some("Update failed Work is retained; Ctrl+R retries.")
    );
    assert_eq!(app.action_dock().primary, Some(Action::RetryGit));
    let visible = rows(&screen(&mut app, 120, 36)).join("\n");
    assert!(visible.contains("Update failed"), "{visible}");
    assert!(!visible.contains("Merge save failed"), "{visible}");
    assert_eq!(app.input.lines(), ["keep this draft"]);
}
