use serde::{Deserialize, Serialize};

use super::{Finding, Report};
use crate::execution::CheckResult;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Triage {
    pub kind: String,
    pub origin: String,
    pub reason: String,
    pub acceptance: String,
    pub reproduction: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    pub id: u32,
    pub outcome: String,
    pub reason: String,
    pub evidence: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Event {
    pub fingerprint: String,
    pub status: String,
    pub reason: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Issue {
    pub id: u32,
    pub finding: Finding,
    pub status: String,
    pub attempts: u32,
    pub regression: String,
    pub history: Vec<Event>,
}

impl Issue {
    pub fn open(&self) -> bool {
        !matches!(
            self.status.as_str(),
            "resolved" | "dismissed" | "information"
        )
    }

    pub fn record(&mut self, fingerprint: &str, status: &str, reason: String) {
        self.status = status.into();
        self.history.push(Event {
            fingerprint: fingerprint.into(),
            status: status.into(),
            reason,
        });
    }
}

fn identity(f: &Finding) -> (String, String, String) {
    (
        f.owner.clone(),
        f.file.clone(),
        f.title
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Omitted issues remain open. Only a reviewed disposition can close them.
pub fn reconcile(
    previous: &[Issue],
    report: &mut Report,
    fingerprint: &str,
    checks: &[CheckResult],
) -> Result<Vec<Issue>, String> {
    let mut issues = previous.to_vec();
    let mut seen = std::collections::BTreeSet::new();
    for finding in &mut report.findings {
        if finding
            .id
            .is_some_and(|id| !previous.iter().any(|i| i.id == id))
        {
            return Err("Review referenced an unknown finding ID.".into());
        }
        let existing = finding
            .id
            .and_then(|id| issues.iter().position(|i| i.id == id))
            .or_else(|| {
                issues
                    .iter()
                    .position(|i| identity(&i.finding) == identity(finding))
            });
        if finding.id.is_some() && existing.is_none() {
            return Err("Review referenced an unknown finding ID.".into());
        }
        let index = if let Some(index) = existing {
            if issues[index].finding.owner != finding.owner {
                let owner = issues[index].finding.owner.clone();
                issues[index].record(
                    fingerprint,
                    "reassigned",
                    format!(
                        "Owner changed from {owner} to {} in the current plan.",
                        finding.owner
                    ),
                );
            }
            index
        } else {
            let id = issues.iter().map(|i| i.id).max().unwrap_or(0) + 1;
            issues.push(Issue {
                id,
                finding: finding.clone(),
                status: "investigate".into(),
                attempts: 0,
                regression: format!("Review regression {id} · {}", finding.title),
                history: vec![],
            });
            issues.len() - 1
        };
        let issue = &mut issues[index];
        if !seen.insert(issue.id) {
            return Err("Review reported the same finding more than once.".into());
        }
        finding.id = Some(issue.id);
        issue.finding = finding.clone();
        let triage = finding
            .triage
            .as_ref()
            .ok_or("Finding needs recorded triage before repair.")?;
        if triage.kind == "information" && previous.iter().any(|i| i.id == issue.id && i.open()) {
            return Err("An open finding requires an explicit dismissal with counterevidence, not an informational label.".into());
        }
        issue.record(fingerprint, &triage.kind, triage.reason.clone());
    }
    for assessment in &report.assessments {
        let issue = issues
            .iter_mut()
            .find(|i| i.id == assessment.id)
            .ok_or("Review assessed an unknown finding ID.")?;
        if !seen.insert(issue.id) {
            return Err("A finding cannot have conflicting review decisions.".into());
        }
        if !matches!(assessment.outcome.as_str(), "resolved" | "dismissed")
            || assessment.reason.trim().is_empty()
            || assessment.evidence.trim().is_empty()
            || assessment.reason.len() > 8000
            || assessment.evidence.len() > 8000
        {
            return Err(
                "Closing a finding requires a disposition, reason and current evidence.".into(),
            );
        }
        if assessment.outcome == "resolved" {
            let matching: Vec<_> = checks
                .iter()
                .filter(|c| c.check == issue.regression)
                .collect();
            if issue.attempts == 0
                || matching.is_empty()
                || matching.iter().any(|c| c.exit_code != Some(0))
            {
                return Err(format!(
                    "R{} cannot close without its passing regression on this version.",
                    issue.id
                ));
            }
        }
        issue.record(
            fingerprint,
            &assessment.outcome,
            format!("{}\n{}", assessment.reason, assessment.evidence),
        );
    }
    if issues.iter().any(|i| i.open() && !seen.contains(&i.id)) {
        return Err("Review omitted an unresolved finding. Reassess every open ID; omission is not resolution.".into());
    }
    if report.status == "clean" && issues.iter().any(Issue::open) {
        return Err("Review cannot pass with unresolved findings.".into());
    }
    if report.status != "blocked" && !issues.iter().any(Issue::open) {
        report.status = "clean".into();
    }
    Ok(issues)
}
