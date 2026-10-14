use super::*;
use crate::{
    quota::{Exhausted, Observation},
    scheduler::tests::fixture,
};
use serde_json::json;

pub(crate) fn choice(provider: Provider, model: &str) -> ModelChoice {
    ModelChoice {
        provider,
        model: model.into(),
        effort: "high".into(),
    }
}
pub(crate) fn team() -> WorkerSettings {
    WorkerSettings {
        planner: choice(Provider::Muse, "muse-wonder-1.0"),
        executors: vec![
            Allocation {
                choice: choice(Provider::Codex, "gpt-6.1-sol"),
                count: 1,
            },
            Allocation {
                choice: choice(Provider::Muse, "muse-wonder-1.0"),
                count: 2,
            },
        ],
        reviewer: choice(Provider::Codex, "gpt-6-astra"),
    }
}
pub(crate) fn catalog() -> catalog::Catalog {
    catalog::Catalog {
        models: team()
            .choices()
            .cloned()
            .chain(std::iter::once(choice(Provider::Codex, "gpt-6-astra")))
            .chain(std::iter::once(choice(Provider::Codex, "gpt-6.1-sol")))
            .map(|c| catalog::Model {
                provider: c.provider,
                model: c.model.clone(),
                label: c.model,
                efforts: vec!["high".into(), "xhigh".into()],
                default_effort: "high".into(),
            })
            .collect(),
        loading: false,
        errors: vec![],
    }
}
fn exhausted() -> Exhausted {
    Exhausted {
        bucket: "tierLimit".into(),
        model: None,
        resets_at_ms: Some(now_ms() + 60_000),
    }
}

#[test]
fn team_capacity_mix_and_assignment_pins_survive_settings_edits_and_restart() {
    let (data, mut store, id, plan) = fixture(&["auto", "auto", "auto", "auto"], false);
    store.save_worker_settings(Some(id), 1, &team()).unwrap();
    let workers = store
        .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
        .unwrap();
    assert_eq!(
        workers
            .iter()
            .map(|w| w.provider.as_str())
            .collect::<Vec<_>>(),
        ["codex", "muse", "muse"]
    );
    for worker in &workers {
        let input = store.task_input(id, worker.id, &plan).unwrap().unwrap();
        store.pending(worker.id, None).unwrap();
        store
            .task_status(id, &input.source, "running", Some("accepted"))
            .unwrap();
    }
    let mut updated = team();
    updated.executors = vec![Allocation {
        choice: choice(Provider::Codex, "gpt-6-astra"),
        count: 1,
    }];
    store.save_worker_settings(Some(id), 1, &updated).unwrap();
    let busy = workers.iter().map(|w| w.id).collect::<Vec<_>>();
    assert_eq!(
        store
            .schedule_workers(id, &plan, &busy, &[], &["codex", "muse"])
            .unwrap()
            .iter()
            .map(|w| w.id)
            .collect::<Vec<_>>(),
        busy
    );
    assert_eq!(
        store.worker_choice(workers[1].id).unwrap().unwrap().model,
        "muse-wonder-1.0"
    );
    drop(store);
    let store = data.store();
    assert_eq!(store.worker_settings(id).unwrap(), Some(updated));
    assert_eq!(
        store.worker_choice(workers[0].id).unwrap().unwrap().model,
        "gpt-6.1-sol"
    );
    assert_eq!(
        store
            .execution(id)
            .unwrap()
            .unwrap()
            .tasks
            .iter()
            .filter(|t| t.status == "running")
            .count(),
        3
    );
}
#[test]
fn safe_quota_replacement_keeps_cap_and_receipts_without_replaying_acknowledged_mail() {
    let (data, mut store, id, plan) = fixture(&["auto", "auto", "auto"], false);
    store.save_worker_settings(Some(id), 1, &team()).unwrap();
    let workers = store
        .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
        .unwrap();
    let old = &workers[1];
    let input = store.task_input(id, old.id, &plan).unwrap().unwrap();
    store.pending(old.id, None).unwrap();
    store
        .task_status(id, &input.source, "paused", None)
        .unwrap();
    let task = store.execution(id).unwrap().unwrap().tasks[1].id;
    store.0.execute("INSERT INTO worker_messages(mod_id,recipient_task,from_task,to_task,kind,body,request_key,delivered_by,acknowledged_by) VALUES(?1,?2,'task0','task1','update','Preserve the v2 contract','contract',?3,?3)",params![id,task,old.id]).unwrap();
    let queued = store.enqueue(id, "Keep accepted API version two").unwrap();
    store.steer_to(id, &[queued.id], &[old.id]).unwrap();
    let steering = store.next_steering(id, old.id).unwrap().unwrap();
    store.pending(old.id, Some(&steering.source)).unwrap();
    store.acknowledge(old.id, &steering).unwrap();
    store.save_quota("muse", &exhausted()).unwrap();
    store.mark_quota_worker(old.id).unwrap();
    drop(store);
    let mut store = data.store();
    assert_eq!(store.quota_workers(id).unwrap()[0].0, old.id);
    store
        .release_quota_worker(old.id, id, Role::Executor)
        .unwrap();
    let selected = store
        .schedule_workers(
            id,
            &plan,
            &[workers[0].id, workers[2].id],
            &[old.id],
            &["codex", "muse"],
        )
        .unwrap();
    assert_eq!(selected.len(), 3);
    let new = selected
        .iter()
        .find(|w| w.id != workers[0].id && w.id != workers[2].id)
        .unwrap();
    assert_eq!(new.provider, "codex");
    assert_ne!(new.id, old.id);
    let next = store.task_input(id, new.id, &plan).unwrap().unwrap();
    assert_eq!(next.source, input.source);
    assert!(
        next.texts
            .join("\n")
            .contains("Previously acknowledged coordination")
    );
    assert!(next.texts.join("\n").contains("Preserve the v2 contract"));
    assert_eq!(store.mailbox(id).unwrap()[0].acknowledged, Some(old.id));
    assert_eq!(
        next.texts
            .join("\n")
            .matches("Keep accepted API version two")
            .count(),
        1
    );
    assert!(store.next_steering(id, new.id).unwrap().is_none());
    let plan_source = store.planning(id).unwrap().unwrap().source;
    assert_eq!(
        store
            .submission(id, &plan_source)
            .unwrap()
            .texts
            .join("\n")
            .matches("Keep accepted API version two")
            .count(),
        1
    );
    assert_eq!(
        store
            .0
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE mod_id=?1 AND item_id=?2",
                params![id, steering.source],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}
#[test]
fn uncertain_delivery_never_releases_and_role_recovery_precedes_quota_filtering() {
    let (_data, mut store, id, _) = fixture(&["auto"], false);
    store.save_worker_settings(Some(id), 1, &team()).unwrap();
    let planner = store.role_worker(id, Role::Planner, true).unwrap().unwrap();
    assert_eq!(planner.provider, "muse");
    store
        .pending(planner.id, Some("uncertain-steering"))
        .unwrap();
    store.save_quota("muse", &exhausted()).unwrap();
    assert_eq!(
        store
            .role_worker(id, Role::Planner, true)
            .unwrap()
            .unwrap()
            .id,
        planner.id
    );
    assert!(
        store
            .release_quota_worker(planner.id, id, Role::Planner)
            .is_err()
    );
    store.pending(planner.id, None).unwrap();
    store
        .release_quota_worker(planner.id, id, Role::Planner)
        .unwrap();
    let replacement = store.role_worker(id, Role::Planner, true).unwrap().unwrap();
    assert_eq!(replacement.provider, "codex");
    assert_eq!(
        store.worker_choice(replacement.id).unwrap().unwrap().model,
        "gpt-6.1-sol"
    );
    let reviewer = store
        .role_worker(id, Role::Reviewer, true)
        .unwrap()
        .unwrap();
    assert_eq!(
        store.worker_choice(reviewer.id).unwrap().unwrap().model,
        "gpt-6-astra"
    );
}
#[test]
fn quota_buckets_reset_independently_and_stale_or_unknown_usage_never_clears_them() {
    let (data, store, _, _) = fixture(&["auto"], false);
    let before = now_ms();
    let sol = choice(Provider::Codex, "gpt-6.1-sol");
    let astra = choice(Provider::Codex, "gpt-6-astra");
    let limits = crate::quota::codex_limits(
        &json!({"rateLimitsByLimitId":{"a":{"limitId":"sol","normalModelSlug":"gpt-6.1-sol","primary":{"usedPercent":100,"resetsAt":(before+60000)/1000}},"b":{"limitId":"astra","normalModelSlug":"gpt-6-astra","primary":{"usedPercent":30,"resetsAt":(before+60000)/1000}}}}),
        before,
    );
    store.observe_quota("codex", &limits).unwrap();
    assert!(store.quota_blocked(&sol).unwrap());
    assert!(!store.quota_blocked(&astra).unwrap());
    store
        .observe_quota(
            "codex",
            &[Observation {
                bucket: "sol".into(),
                model: Some(sol.model.clone()),
                exhausted: false,
                resets_at_ms: None,
                observed_at_ms: before - 1,
            }],
        )
        .unwrap();
    assert!(store.quota_blocked(&sol).unwrap());
    drop(store);
    let store = data.store();
    assert!(store.quota_blocked(&sol).unwrap());
    store
        .observe_quota(
            "codex",
            &[Observation {
                bucket: "sol".into(),
                model: Some(sol.model.clone()),
                exhausted: false,
                resets_at_ms: None,
                observed_at_ms: before + 1,
            }],
        )
        .unwrap();
    assert!(!store.quota_blocked(&sol).unwrap());
    store
        .save_quota(
            "muse",
            &Exhausted {
                resets_at_ms: Some(before - 1),
                ..exhausted()
            },
        )
        .unwrap();
    assert!(
        !store
            .quota_blocked(&choice(Provider::Muse, "muse-wonder-1.0"))
            .unwrap()
    );
    assert!(crate::quota::muse_limits(&json!({})).is_empty());
}
#[test]
fn catalogs_validate_models_efforts_and_overflow_before_persistence() {
    let mut settings = team();
    assert!(settings.validate(&catalog()).is_ok());
    settings.executors[0].count = usize::MAX;
    assert_eq!(
        settings.validate(&catalog()).unwrap_err(),
        "Choose between 1 and 16 simultaneous execution workers."
    );
    let mut settings = team();
    settings.planner.effort = "invented".into();
    assert!(
        settings
            .validate(&catalog())
            .unwrap_err()
            .contains("does not support")
    );
    let models=catalog::parse(Provider::Muse,&json!({"models":[{"modelId":"muse-wonder-full-1.0","displayLabel":"Wonder Full","variants":["high","max"],"defaultReasoningEffort":"max"}]})).unwrap();
    assert_eq!(models[0].efforts, ["high", "max"]);
    assert_eq!(models[0].default_effort, "max");
}
#[test]
fn legacy_migration_leaves_existing_models_and_two_slots_until_settings_are_saved() {
    let (data, store, id, _) = fixture(&["auto"], false);
    let worker = store.worker_for(id, Role::Planner).unwrap();
    store.save_thread(worker.id, "native-existing").unwrap();
    store.0.execute_batch("DROP TABLE worker_settings;DROP TABLE worker_team_drafts;DROP TABLE worker_choices;DROP TABLE quota_cooldowns;DROP TABLE quota_workers;").unwrap();
    drop(store);
    let mut store = data.store();
    assert_eq!(store.executor_capacity(id).unwrap(), 2);
    assert_eq!(store.worker_settings(id).unwrap(), None);
    assert_eq!(
        store
            .worker_provider(id, Role::Planner, 0, "codex")
            .unwrap()
            .thread_id
            .as_deref(),
        Some("native-existing")
    );
    store.save_worker_settings(None, 1, &team()).unwrap();
    let new = store
        .create_mod(1, "Selected team before planning")
        .unwrap();
    assert_eq!(store.worker_settings(new.id).unwrap(), Some(team()));
}

#[test]
fn role_settings_changes_apply_after_current_assignment_and_retries_keep_the_pin() {
    let (data, store, id, _) = fixture(&["auto"], false);
    store.save_worker_settings(Some(id), 1, &team()).unwrap();
    let old = store.role_worker(id, Role::Planner, true).unwrap().unwrap();
    store.planning_status(id, "paused").unwrap();
    let mut settings = team();
    settings.planner = choice(Provider::Codex, "gpt-6-astra");
    store.save_worker_settings(Some(id), 1, &settings).unwrap();
    drop(store);
    let mut store = data.store();
    assert_eq!(
        store
            .role_worker(id, Role::Planner, true)
            .unwrap()
            .unwrap()
            .id,
        old.id
    );
    store.restart_plan(id).unwrap();
    let next = store.role_worker(id, Role::Planner, true).unwrap().unwrap();
    assert_ne!(next.id, old.id);
    assert_eq!(
        store.worker_choice(next.id).unwrap().unwrap(),
        settings.planner
    );
}

#[test]
fn a_new_review_gets_a_fresh_identity_even_when_its_model_is_unchanged() {
    let (_data, mut store, mut m, _) = crate::review::tests::fixture();
    let fingerprint = m.execution.as_ref().unwrap().fingerprint.clone().unwrap();
    store.save_worker_settings(Some(m.id), 1, &team()).unwrap();
    store.begin_review(m.id, &fingerprint).unwrap();
    m.agent_review = store.review_state(m.id).unwrap();
    let first = store
        .role_worker(m.id, Role::Reviewer, true)
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .role_worker(m.id, Role::Reviewer, true)
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    store.begin_review(m.id, &fingerprint).unwrap();
    let next = store
        .role_worker(m.id, Role::Reviewer, true)
        .unwrap()
        .unwrap();
    assert_ne!(next.id, first.id);
    assert_eq!(
        store.worker_choice(next.id).unwrap(),
        store.worker_choice(first.id).unwrap()
    );
}
