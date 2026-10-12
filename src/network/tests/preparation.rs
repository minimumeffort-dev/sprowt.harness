use super::*;

#[test]
fn preparation_permission_pauses_sending_attempts_and_survives_restart() {
    for index in [0, 1] {
        let (data, mut store, m, root, workers) = fixture();
        let run = &m.execution.as_ref().unwrap().tasks[index];
        let policy = crate::sandbox::network(&root).unwrap();
        let global = fs::read(data.0.join("network.json")).unwrap();
        for _ in 0..2 {
            let error = require_preparation(
                &store,
                &root,
                m.id,
                workers[index],
                &run.source,
                request("models.example.org"),
                &policy,
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                error.to_string(),
                "Network access pending: models.example.org. Review network access before preparing this task."
            );
        }
        let records = store.network_requests(m.id).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task, run.id);
        assert_eq!(records[0].worker, workers[index]);
        assert_eq!(records[0].domains, ["models.example.org"]);
        assert_eq!(
            store.execution(m.id).unwrap().unwrap().tasks[index].status,
            "sending"
        );
        store
            .finish_task(m.id, &run.source, "paused", "Preparation needs access", &[])
            .unwrap();
        drop(store);
        let mut store = data.store();
        let access = store.network_requests(m.id).unwrap().remove(0);
        assert_eq!(access.status, "pending");
        store.decide_network(m.id, access.id, true).unwrap();
        store.resume_network(&access).unwrap().unwrap();
        let plan = m.planning.as_ref().unwrap().plan.as_ref().unwrap();
        let fresh = store
            .task_input(m.id, workers[index], plan)
            .unwrap()
            .unwrap();
        assert_ne!(fresh.source, run.source);
        let policy = crate::sandbox::network(&root).unwrap();
        require_preparation(
            &store,
            &root,
            m.id,
            workers[index],
            &fresh.source,
            request("models.example.org"),
            &policy,
        )
        .unwrap();
        assert_eq!(fs::read(data.0.join("network.json")).unwrap(), global);
        assert!(store.network_requests(m.id).unwrap().is_empty());
    }
}

#[test]
fn preparation_permission_preserves_denial_and_rejects_stale_or_foreign_attempts() {
    let (data, mut store, m, root, workers) = fixture();
    let source = &m.execution.as_ref().unwrap().tasks[0].source;
    let policy = crate::sandbox::network(&root).unwrap();
    for (worker, attempt) in [
        (workers[1], source.clone()),
        (workers[0], "00000004-0099-4000-8000-000000000001".into()),
    ] {
        assert!(
            require_preparation(
                &store,
                &root,
                m.id,
                worker,
                &attempt,
                request("models.example.org"),
                &policy
            )
            .is_err()
        );
    }
    assert!(store.network_requests(m.id).unwrap().is_empty());
    require_preparation(
        &store,
        &root,
        m.id,
        workers[0],
        source,
        request("models.example.org"),
        &policy,
    )
    .unwrap_err();
    let access = store.network_requests(m.id).unwrap().remove(0);
    store.decide_network(m.id, access.id, false).unwrap();
    drop(store);
    let store = data.store();
    let error = require_preparation(
        &store,
        &root,
        m.id,
        workers[0],
        source,
        request("models.example.org"),
        &policy,
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("Network access denied:"));
    assert_eq!(store.network_requests(m.id).unwrap().len(), 1);
    assert!(grants(&root).unwrap().is_empty());
}

#[test]
fn preparation_permission_requests_only_missing_hosts_and_respects_global_patterns() {
    let (_data, store, m, root, workers) = fixture();
    let policy = BTreeMap::from([
        ("*.example.org".into(), "allow".into()),
        ("blocked.example.com".into(), "deny".into()),
    ]);
    let source = &m.execution.as_ref().unwrap().tasks[0].source;
    require_preparation(
        &store,
        &root,
        m.id,
        workers[0],
        source,
        request("cdn.example.org"),
        &policy,
    )
    .unwrap();
    assert!(store.network_requests(m.id).unwrap().is_empty());
    let mixed = Request::parse(
        json!({"domains":["cdn.example.org","blocked.example.com"],"reason":"Prepare model"}),
    )
    .unwrap();
    require_preparation(&store, &root, m.id, workers[0], source, mixed, &policy).unwrap_err();
    assert_eq!(
        store.network_requests(m.id).unwrap()[0].domains,
        ["blocked.example.com"]
    );
}
