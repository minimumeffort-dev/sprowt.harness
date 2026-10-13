// GitHub PR feedback, parsed at the gh boundary into display-ready items.
// Read-only: nothing here replies, resolves, or changes the PR.

use std::{io, path::Path, sync::atomic::AtomicBool};

use serde_json::Value;

use crate::git_mod::{self, GitMod};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feedback {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub actionable: bool,
    pub file: Option<String>,
    pub line: Option<u32>,
}

// shortcut: newest 50 entries per collection, newest head commit only; add
// cursor pagination if a PR ever exceeds that in practice.
const QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){reviews(last:50){nodes{databaseId author{login} body state url}} comments(last:50){nodes{databaseId author{login} body url}} reviewThreads(last:50){nodes{isResolved comments(first:1){nodes{databaseId author{login} body path line originalLine url}}}} commits(last:1){nodes{commit{statusCheckRollup{contexts(first:50){nodes{__typename ... on CheckRun{name status conclusion detailsUrl} ... on StatusContext{context state targetUrl}}}}}}}}}}";

pub fn fetch(
    root: &Path,
    state: &GitMod,
    program: &Path,
    cancelled: &AtomicBool,
) -> io::Result<Vec<Feedback>> {
    let url = state
        .pr
        .as_deref()
        .ok_or_else(|| io::Error::other("This mod has no PR."))?;
    let (owner, repo, number) = parse_pr_url(url)
        .ok_or_else(|| io::Error::other("Cannot read GitHub feedback for this PR URL."))?;
    let text = git_mod::gh(
        root,
        state,
        program,
        &[
            "api",
            "graphql",
            "-f",
            &format!("query={QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={repo}"),
            "-F",
            &format!("number={number}"),
        ],
        cancelled,
    )?;
    let value: Value = serde_json::from_str(&text)?;
    if value["data"]["repository"]["pullRequest"].is_null() {
        return Err(io::Error::other("GitHub returned no PR feedback."));
    }
    Ok(parse(&value))
}

fn parse_pr_url(url: &str) -> Option<(String, String, u64)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let [owner, repo, "pull", number] = parts.as_slice() else {
        return None;
    };
    if !git_mod::valid_slug(&format!("{owner}/{repo}")) {
        return None;
    }
    Some((
        (*owner).into(),
        (*repo).into(),
        number.parse().ok().filter(|n| *n > 0)?,
    ))
}

fn nodes<'a>(value: &'a Value, path: &[&str]) -> Vec<&'a Value> {
    let mut current = value;
    for key in path {
        current = &current[key];
    }
    current
        .as_array()
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

fn title(body: &str, fallback: &str) -> String {
    body.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(80).collect())
        .unwrap_or_else(|| fallback.into())
}

fn detail(body: &str, url: Option<&str>) -> String {
    let text: String = body.trim().chars().take(2000).collect();
    match url {
        Some(url) if !url.is_empty() => format!("{text}\n{url}"),
        _ => text,
    }
}

fn author(item: &Value) -> &str {
    item["author"]["login"].as_str().unwrap_or("someone")
}

pub fn parse(value: &Value) -> Vec<Feedback> {
    let pr = &value["data"]["repository"]["pullRequest"];
    let mut feedback = Vec::new();
    for (index, thread) in nodes(pr, &["reviewThreads", "nodes"])
        .into_iter()
        .enumerate()
    {
        if thread["isResolved"] == true {
            continue;
        }
        let Some(comment) = nodes(thread, &["comments", "nodes"]).into_iter().next() else {
            continue;
        };
        let body = comment["body"].as_str().unwrap_or_default();
        if body.trim().is_empty() {
            continue;
        }
        let path = comment["path"].as_str().unwrap_or_default();
        let line = comment["line"]
            .as_u64()
            .or_else(|| comment["originalLine"].as_u64());
        let location = match line {
            Some(line) => format!("{path}:{line}"),
            None => path.into(),
        };
        feedback.push(Feedback {
            id: format!(
                "thread:{}",
                comment["databaseId"].as_u64().unwrap_or(index as u64)
            ),
            title: format!("{location} · {}", author(comment)),
            detail: detail(body, comment["url"].as_str()),
            status: "unresolved thread".into(),
            actionable: true,
            file: (!path.is_empty()).then(|| path.to_owned()),
            line: line.and_then(|line| u32::try_from(line).ok()),
        });
    }
    for (index, review) in nodes(pr, &["reviews", "nodes"]).into_iter().enumerate() {
        let state = review["state"].as_str().unwrap_or_default();
        let body = review["body"].as_str().unwrap_or_default();
        let (status, actionable) = match state {
            "CHANGES_REQUESTED" => ("changes requested", true),
            "COMMENTED" => ("review comment", true),
            "APPROVED" => ("approved", false),
            _ => continue,
        };
        if body.trim().is_empty() && state != "CHANGES_REQUESTED" {
            continue;
        }
        feedback.push(Feedback {
            id: format!(
                "review:{}",
                review["databaseId"].as_u64().unwrap_or(index as u64)
            ),
            title: title(body, &format!("{} by {}", status, author(review))),
            detail: detail(body, review["url"].as_str()),
            status: status.into(),
            actionable,
            file: None,
            line: None,
        });
    }
    for (index, comment) in nodes(pr, &["comments", "nodes"]).into_iter().enumerate() {
        let body = comment["body"].as_str().unwrap_or_default();
        if body.trim().is_empty() {
            continue;
        }
        feedback.push(Feedback {
            id: format!(
                "comment:{}",
                comment["databaseId"].as_u64().unwrap_or(index as u64)
            ),
            title: title(body, "PR comment"),
            detail: detail(body, comment["url"].as_str()),
            status: "PR comment".into(),
            actionable: false,
            file: None,
            line: None,
        });
    }
    let commits = nodes(pr, &["commits", "nodes"]);
    if let Some(commit) = commits.last() {
        for context in nodes(
            &commit["commit"]["statusCheckRollup"],
            &["contexts", "nodes"],
        ) {
            let (name, failed, url) = match context["__typename"].as_str() {
                Some("CheckRun") => (
                    context["name"].as_str().unwrap_or_default(),
                    context["status"] == "COMPLETED"
                        && matches!(
                            context["conclusion"].as_str(),
                            Some(
                                "FAILURE"
                                    | "TIMED_OUT"
                                    | "CANCELLED"
                                    | "ACTION_REQUIRED"
                                    | "STARTUP_FAILURE"
                            )
                        ),
                    context["detailsUrl"].as_str(),
                ),
                Some("StatusContext") => (
                    context["context"].as_str().unwrap_or_default(),
                    matches!(context["state"].as_str(), Some("FAILURE" | "ERROR")),
                    context["targetUrl"].as_str(),
                ),
                _ => continue,
            };
            if !failed || name.is_empty() {
                continue;
            }
            feedback.push(Feedback {
                id: format!("check:{name}"),
                title: name.into(),
                detail: detail("This GitHub check failed on the published commit.", url),
                status: "check failed".into(),
                actionable: true,
                file: None,
                line: None,
            });
        }
    }
    feedback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keeps_actionable_feedback_and_drops_resolved_and_passing() {
        let value: Value = serde_json::json!({"data":{"repository":{"pullRequest":{
            "reviews":{"nodes":[
                {"databaseId":10,"author":{"login":"rev"},"body":"Please rename this.\nIt reads as a verb.","state":"CHANGES_REQUESTED","url":"https://example.test/r10"},
                {"databaseId":11,"author":{"login":"ok"},"body":"","state":"APPROVED","url":"https://example.test/r11"},
                {"databaseId":12,"author":{"login":"gone"},"body":"Old take","state":"DISMISSED","url":"https://example.test/r12"}
            ]},
            "comments":{"nodes":[
                {"databaseId":20,"author":{"login":"bot"},"body":"Coverage is 91%.","url":"https://example.test/c20"}
            ]},
            "reviewThreads":{"nodes":[
                {"isResolved":false,"comments":{"nodes":[{"databaseId":30,"author":{"login":"rev"},"body":"This drops the error.","path":"src/main.rs","line":42,"originalLine":40,"url":"https://example.test/t30"}]}},
                {"isResolved":true,"comments":{"nodes":[{"databaseId":31,"author":{"login":"rev"},"body":"Fixed already.","path":"src/lib.rs","line":3,"url":"https://example.test/t31"}]}}
            ]},
            "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                {"__typename":"CheckRun","name":"unit tests","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://example.test/check1"},
                {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://example.test/check2"},
                {"__typename":"CheckRun","name":"build","status":"IN_PROGRESS","conclusion":null,"detailsUrl":"https://example.test/check3"},
                {"__typename":"StatusContext","context":"deploy preview","state":"ERROR","targetUrl":"https://example.test/check4"}
            ]}}}}]}
        }}}});
        let feedback = parse(&value);
        let summary: Vec<(&str, &str, bool)> = feedback
            .iter()
            .map(|item| (item.id.as_str(), item.status.as_str(), item.actionable))
            .collect();
        assert_eq!(
            summary,
            [
                ("thread:30", "unresolved thread", true),
                ("review:10", "changes requested", true),
                ("comment:20", "PR comment", false),
                ("check:unit tests", "check failed", true),
                ("check:deploy preview", "check failed", true),
            ]
        );
        assert_eq!(feedback[0].title, "src/main.rs:42 · rev");
        assert_eq!(
            feedback[0].detail,
            "This drops the error.\nhttps://example.test/t30"
        );
        assert_eq!(feedback[1].title, "Please rename this.");
        assert_eq!(feedback.iter().filter(|item| item.actionable).count(), 4);
    }

    #[test]
    fn parse_tolerates_missing_collections() {
        let value: Value = serde_json::json!({"data":{"repository":{"pullRequest":{}}}});
        assert!(parse(&value).is_empty());
    }

    #[test]
    fn fetch_asks_gh_for_the_saved_pr_and_parses_the_reply() {
        let data = crate::store::test_support::TestData::new();
        let root = data.0.join("root");
        let repo = data.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let program = data.0.join("fake-gh");
        let reply = data.0.join("reply.json");
        std::fs::write(
            &reply,
            serde_json::json!({"data":{"repository":{"pullRequest":{
                "reviews":{"nodes":[]},
                "comments":{"nodes":[]},
                "reviewThreads":{"nodes":[{"isResolved":false,"comments":{"nodes":[{"databaseId":5,"author":{"login":"rev"},"body":"Handle the empty case.","path":"src/lib.rs","line":9,"url":"https://example.test/t5"}]}}]},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[]}}}}]}
            }}}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n  *\"number=7\"*) ;;\n  *) echo \"unexpected args: $*\" >&2; exit 1 ;;\nesac\ncat '{}'\n",
                reply.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let state = GitMod {
            repo,
            branch: "sprowt/test".into(),
            base: "base".into(),
            base_branch: Some("main".into()),
            phase: "published".into(),
            fingerprint: None,
            head: None,
            pr: Some("https://github.com/fixture/project/pull/7".into()),
            draft: false,
            removing: false,
            publishing: false,
            closing: false,
            discarding: false,
            continuing: false,
            published_head: None,
            checkpoint: None,
        };
        let feedback = fetch(&root, &state, &program, &AtomicBool::new(false)).unwrap();
        assert_eq!(feedback.len(), 1);
        assert_eq!(feedback[0].id, "thread:5");
        assert_eq!(feedback[0].title, "src/lib.rs:9 · rev");
        assert!(feedback[0].actionable);
    }

    #[test]
    fn pr_url_requires_the_saved_github_shape() {
        assert_eq!(
            parse_pr_url("https://github.com/fixture/project/pull/7"),
            Some(("fixture".into(), "project".into(), 7))
        );
        for url in [
            "https://github.com/fixture/project/pull/0",
            "https://github.com/fixture/project/issues/7",
            "https://example.test/fixture/project/pull/7",
            "https://github.com/fixture/pull/7",
        ] {
            assert_eq!(parse_pr_url(url), None, "{url}");
        }
    }
}
