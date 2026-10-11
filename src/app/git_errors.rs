use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Errors(BTreeMap<i64, Vec<(&'static str, String)>>);

impl Errors {
    pub fn record(&mut self, mod_id: i64, operation: &'static str, error: String) {
        self.clear(mod_id, operation);
        self.0
            .entry(mod_id)
            .or_default()
            .push((operation_key(operation), error));
    }

    pub fn clear(&mut self, mod_id: i64, operation: &'static str) {
        if let Some(errors) = self.0.get_mut(&mod_id) {
            errors.retain(|(key, _)| *key != operation_key(operation));
            if errors.is_empty() {
                self.0.remove(&mod_id);
            }
        }
    }

    pub fn remove(&mut self, mod_id: i64) {
        self.0.remove(&mod_id);
    }

    pub fn contains(&self, mod_id: i64, operation: &'static str) -> bool {
        self.0.get(&mod_id).is_some_and(|errors| {
            errors
                .iter()
                .any(|(key, _)| *key == operation_key(operation))
        })
    }

    pub fn current(&self, mod_id: i64) -> Option<&str> {
        self.0.get(&mod_id)?.last().map(|(_, error)| error.as_str())
    }
}

fn operation_key(label: &'static str) -> &'static str {
    match label {
        "checking project branch" | "creating worktree" | "setting up Git" => "Git setup",
        _ => label,
    }
}
