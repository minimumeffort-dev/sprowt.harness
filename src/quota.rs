use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Exhausted {
    pub bucket: String,
    pub model: Option<String>,
    pub resets_at_ms: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct Observation {
    pub bucket: String,
    pub model: Option<String>,
    pub exhausted: bool,
    pub resets_at_ms: Option<i64>,
    pub observed_at_ms: i64,
}

pub fn codex_limits(value: &Value, observed_at_ms: i64) -> Vec<Observation> {
    let limits = value["rateLimitsByLimitId"]
        .as_object()
        .map(|m| m.values().collect::<Vec<_>>())
        .unwrap_or_else(|| vec![&value["rateLimits"]]);
    limits
        .into_iter()
        .filter_map(|limit| {
            let windows = [&limit["primary"], &limit["secondary"]]
                .into_iter()
                .filter(|w| w["usedPercent"].as_f64().is_some())
                .collect::<Vec<_>>();
            let reached =
                !limit["rateLimitReachedType"].is_null() || limit["spendControlReached"] == true;
            if windows.is_empty() && !reached {
                return None;
            }
            let exhausted = reached
                || windows
                    .iter()
                    .any(|w| w["usedPercent"].as_f64().unwrap() >= 100.0);
            let resets_at_ms = windows
                .iter()
                .filter(|w| w["usedPercent"].as_f64().unwrap() >= 100.0)
                .filter_map(|w| w["resetsAt"].as_i64()?.checked_mul(1000))
                .max();
            let bucket = limit["limitId"].as_str().unwrap_or("codex").to_owned();
            let model = limit["normalModelSlug"].as_str().map(str::to_owned);
            Some(Observation {
                bucket,
                model,
                exhausted,
                resets_at_ms,
                observed_at_ms,
            })
        })
        .collect()
}
pub fn muse_limits(value: &Value) -> Vec<Observation> {
    let usage = &value["usage"];
    let Some(observed_at_ms) = usage["observedAtMs"].as_i64() else {
        return vec![];
    };
    let windows = [&usage["weekly"], &usage["window"]]
        .into_iter()
        .filter(|w| w["usedPercent"].as_f64().is_some())
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return vec![];
    }
    let exhausted = windows
        .iter()
        .any(|w| w["usedPercent"].as_f64().unwrap() >= 100.0);
    let resets_at_ms = windows
        .iter()
        .filter(|w| w["usedPercent"].as_f64().unwrap() >= 100.0)
        .filter_map(|w| w["resetsAtMs"].as_i64())
        .max();
    vec![Observation {
        bucket: "tierLimit".into(),
        model: None,
        exhausted,
        resets_at_ms,
        observed_at_ms,
    }]
}
// Only structured provider quota signals can change assignment ownership.
pub fn from_error(provider: &str, error: &Value, model: Option<&str>) -> Option<Exhausted> {
    match provider {
        "muse" => {
            let quota = error.get("quota")?;
            Some(Exhausted {
                bucket: quota["limitKind"].as_str()?.into(),
                model: None,
                resets_at_ms: Some(quota["resetsAtMs"].as_i64()?),
            })
        }
        "codex" if error["codexErrorInfo"] == "usageLimitExceeded" => {
            let mut limits = codex_limits(&error["sprowtRateLimits"], 0)
                .into_iter()
                .filter(|l| l.exhausted && l.model.as_deref().is_none_or(|m| Some(m) == model))
                .collect::<Vec<_>>();
            limits.sort_by_key(|l| l.model.is_some());
            Some(
                limits
                    .first()
                    .map(|l| Exhausted {
                        bucket: l.bucket.clone(),
                        model: l.model.clone(),
                        resets_at_ms: l.resets_at_ms,
                    })
                    .unwrap_or(Exhausted {
                        bucket: "unknown".into(),
                        model: None,
                        resets_at_ms: None,
                    }),
            )
        }
        _ => None,
    }
}
#[derive(Debug)]
pub struct Rejected(pub Exhausted);
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Provider quota exhausted.")
    }
}
impl std::error::Error for Rejected {}
pub fn rejection(error: &std::io::Error) -> Option<Exhausted> {
    error
        .get_ref()?
        .downcast_ref::<Rejected>()
        .map(|e| e.0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_explicit_exhaustion_is_quota_and_unrelated_model_limits_do_not_select_the_bucket() {
        for error in [
            json!({"message":"quota exhausted","status":429}),
            json!({"codexErrorInfo":"rateLimitExceeded"}),
            json!({"codexErrorInfo":"httpConnectionFailed"}),
        ] {
            assert!(from_error("codex", &error, Some("gpt-6.1-sol")).is_none());
        }
        let error = json!({"codexErrorInfo":"usageLimitExceeded","sprowtRateLimits":{"rateLimitsByLimitId":{"a":{"limitId":"astra","normalModelSlug":"gpt-6-astra","rateLimitReachedType":"creditsDepleted","primary":null,"secondary":null}}}});
        let quota = from_error("codex", &error, Some("gpt-6-astra")).unwrap();
        assert_eq!(quota.bucket, "astra");
        assert_eq!(quota.model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(
            from_error("codex", &error, Some("gpt-6.1-sol"))
                .unwrap()
                .bucket,
            "unknown"
        );
        let muse = from_error(
            "muse",
            &json!({"kind":"modelError","quota":{"limitKind":"tierLimit","resetsAtMs":123456}}),
            None,
        )
        .unwrap();
        assert_eq!(muse.resets_at_ms, Some(123456));
        assert!(
            from_error(
                "muse",
                &json!({"kind":"modelError","message":"quota"}),
                None
            )
            .is_none()
        );
    }
}
