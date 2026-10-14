use super::*;
impl Worker {
    pub(super) fn stop_for_quota(
        &mut self,
        store: &mut Store,
        code_mod: &mut CodeMod,
        quota: crate::quota::Exhausted,
        rejected: Option<&str>,
    ) -> rusqlite::Result<()> {
        if let Some(source) = rejected.filter(|s| !s.is_empty())
            && self.pending.as_ref().is_some_and(|p| p.source == source)
        {
            store.pending(self.id, None)?;
            self.pending = None;
        }
        let safe = (rejected.is_none() || self.turn.is_none())
            && self.pending.is_none()
            && self.recovery.is_none();
        let checkpointed = self
            .client
            .as_mut()
            .is_none_or(|client| client.shutdown_checked().is_ok());
        self.turn = None;
        self.fail("Provider quota exhausted.".into());
        store.save_quota(&self.provider, &quota)?;
        match self.role {
            Role::Planner => {
                store.planning_status(self.mod_id, "paused")?;
                code_mod.planning = store.planning(self.mod_id)?;
            }
            Role::Reviewer => {
                if let Some(review) = &code_mod.agent_review {
                    store.review_status(self.mod_id, &review.source, "blocked")?;
                }
                code_mod.agent_review = store.review_state(self.mod_id)?;
            }
            Role::Executor => {
                if self.task_source.is_some() {
                    self.block_task(
                        store,
                        code_mod,
                        "paused",
                        "Provider quota exhausted.".into(),
                    )?;
                }
            }
        }
        if safe && checkpointed {
            if store.worker_settings(self.mod_id)?.is_some() {
                store.mark_quota_worker(self.id)?;
            }
        } else if !checkpointed {
            self.error=Some("Quota exhausted, but the worker checkpoint could not be confirmed. Retry to recover saved work before changing workers.".into());
        } else {
            self.error=Some("Quota exhausted with an unconfirmed instruction. Retry to recover delivery before changing workers.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{store::test_support::TestData, worker_settings::tests::team};
    use serde_json::json;

    #[test]
    fn quota_with_unconfirmed_checkpoint_never_releases_the_assignment() {
        let data = TestData::new();
        let mut store = data.store();
        let project = data.0.join("project");
        let project_id = store.load_project(&project).unwrap().id;
        let mut m = store.create_mod(project_id, "Plan a greeting").unwrap();
        store
            .save_worker_settings(Some(m.id), project_id, &team())
            .unwrap();
        let record = store
            .role_worker(m.id, Role::Planner, true)
            .unwrap()
            .unwrap();
        let (mut worker, _, _) = Worker::recording(&project, &m, record).unwrap();
        worker.client = Some(Client::failed_shutdown_for_test());
        worker
            .stop_for_quota(
                &mut store,
                &mut m,
                crate::quota::Exhausted {
                    bucket: "tierLimit".into(),
                    model: None,
                    resets_at_ms: None,
                },
                Some("rejected"),
            )
            .unwrap();
        assert!(store.quota_workers(m.id).unwrap().is_empty());
        assert!(
            worker
                .error
                .as_deref()
                .unwrap()
                .contains("checkpoint could not be confirmed")
        );
    }

    #[test]
    fn explicit_quota_stops_before_replacement_but_uncertain_delivery_stays_owned() {
        for pending in [false, true] {
            let data = TestData::new();
            let mut store = data.store();
            let project = data.0.join("project");
            let project_id = store.load_project(&project).unwrap().id;
            let mut m = store.create_mod(project_id, "Plan a greeting").unwrap();
            store
                .save_worker_settings(Some(m.id), project_id, &team())
                .unwrap();
            let record = store
                .role_worker(m.id, Role::Planner, true)
                .unwrap()
                .unwrap();
            let (mut worker, _, _) = Worker::recording(&project, &m, record).unwrap();
            worker.turn = Some("active".into());
            worker.status = Status::Running;
            if pending {
                let instruction = crate::store::Submission {
                    source: "uncertain-steer".into(),
                    texts: vec!["Keep my API".into()],
                };
                store.pending(worker.id, Some(&instruction.source)).unwrap();
                worker.pending = Some(instruction);
            }
            worker.receive(Event::Notification(json!({"method":"turn/completed","params":{"turn":{"id":"active","status":"failed","error":{"quota":{"limitKind":"tierLimit","resetsAtMs":crate::worker_settings::now_ms()+60_000}}}}})), &mut store, &mut m).unwrap();
            assert!(worker.status == Status::Failed && !worker.enabled && worker.turn.is_none());
            assert_eq!(m.planning.as_ref().unwrap().status, "paused");
            assert_eq!(
                store.quota_workers(m.id).unwrap().len(),
                usize::from(!pending)
            );
            if pending {
                assert!(
                    store
                        .release_quota_worker(worker.id, m.id, Role::Planner)
                        .is_err()
                );
                assert!(
                    worker
                        .error
                        .as_deref()
                        .unwrap()
                        .contains("unconfirmed instruction")
                );
            } else {
                store
                    .release_quota_worker(worker.id, m.id, Role::Planner)
                    .unwrap();
                let replacement = store
                    .role_worker(m.id, Role::Planner, true)
                    .unwrap()
                    .unwrap();
                assert_eq!(replacement.provider, "codex");
                assert_ne!(replacement.id, worker.id);
            }
        }
    }
}
