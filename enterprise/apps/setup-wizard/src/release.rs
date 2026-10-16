//! Step 8: dispatch the release workflow, follow the run, and confirm the release exists
//! **with at least one asset** before showing it.

use crate::github::{GitHub, Release};
use crate::state::AppState;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(3 * 3600);

#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseCheck {
    NotFound,
    NoAssets,
    Ready,
}

/// A release counts only if it exists, is not a draft, and has at least one asset.
pub fn classify(r: Option<&Release>) -> ReleaseCheck {
    match r {
        None => ReleaseCheck::NotFound,
        Some(r) if r.draft || r.assets.is_empty() => ReleaseCheck::NoAssets,
        Some(_) => ReleaseCheck::Ready,
    }
}

pub struct Input {
    pub owner: String,
    pub repo: String,
    pub git_ref: String,
    pub version: String,
}

fn phase(app: &AppState, p: &str) {
    app.w().release.phase = p.to_string();
    app.log.info(format!("release: {p}"));
}

fn finish_err(app: &AppState, e: String) {
    let e = app.log.scrub(&e);
    app.log.error(format!("release: {e}"));
    let mut w = app.w();
    w.release.running = false;
    w.release.error = Some(e);
    w.release.phase = "failed".into();
}

pub async fn run(app: Arc<AppState>, gh: GitHub, i: Input) {
    let wf = crate::config::RELEASE_WORKFLOW;
    let t = app.timings.clone();

    // Already released? Then there is nothing to dispatch.
    match gh.release_by_tag(&i.owner, &i.repo, &i.version).await {
        Ok(Some(r)) if classify(Some(&r)) == ReleaseCheck::Ready => {
            let mut w = app.w();
            w.release.release = Some(r);
            w.release.running = false;
            w.release.phase = "released".into();
            w.release.error = None;
            drop(w);
            app.log.info(format!("release {} already exists with assets; not dispatching", i.version));
            return;
        }
        Ok(Some(_)) => {
            return finish_err(&app, format!(
                "A release tagged {} already exists but has no assets (or is a draft). Delete it on GitHub or choose a new version, then retry.",
                i.version
            ))
        }
        Ok(None) => {}
        Err(e) => return finish_err(&app, format!("Could not check existing releases: {e}")),
    }

    phase(&app, "Dispatching workflow");
    let known: HashSet<u64> = match gh.list_workflow_runs(&i.owner, &i.repo, wf).await {
        Ok(runs) => runs.iter().map(|r| r.id).collect(),
        Err(e) if e.status == Some(404) => {
            return finish_err(&app, format!(
                "Workflow {wf} was not found in {}/{}. The template must contain .github/workflows/{wf} with a workflow_dispatch trigger.",
                i.owner, i.repo
            ))
        }
        Err(e) => return finish_err(&app, format!("Could not list workflow runs: {e}")),
    };
    // Allow a little clock skew between this PC and GitHub.
    let since = crate::util::rfc3339(crate::util::now_secs().saturating_sub(120));
    if let Err(e) = gh
        .dispatch_workflow(&i.owner, &i.repo, wf, &i.git_ref, &i.version)
        .await
    {
        let hint = match e.status {
            Some(403) => " The token needs Actions: read and write.",
            Some(404) => " Check that the workflow file exists on the default branch.",
            Some(422) => " The workflow must declare `workflow_dispatch` with a `version` input.",
            _ => "",
        };
        return finish_err(&app, format!("Dispatch failed: {e}.{hint}"));
    }
    app.w().release.dispatched_at = Some(crate::util::rfc3339(crate::util::now_secs()));

    phase(&app, "Waiting for the run to appear");
    let started = Instant::now();
    let run = loop {
        tokio::time::sleep(t.release_poll).await;
        match gh.list_workflow_runs(&i.owner, &i.repo, wf).await {
            Ok(runs) => {
                if let Some(r) = runs
                    .into_iter()
                    .filter(|r| !known.contains(&r.id) && r.created_at.as_str() >= since.as_str())
                    .max_by_key(|r| r.id)
                {
                    break r;
                }
            }
            Err(e) => app
                .log
                .warn(format!("listing runs failed (will retry): {e}")),
        }
        if started.elapsed() > t.release_find_timeout {
            return finish_err(&app, "The dispatched run did not appear in time. Open the repository's Actions tab to check, then retry.".into());
        }
    };
    let run_id = run.id;
    app.w().release.run = Some(run);

    phase(&app, "Run in progress");
    let mut logged: HashSet<u64> = HashSet::new();
    let run_started = Instant::now();
    let mut errors = 0u32;
    let final_run = loop {
        tokio::time::sleep(t.release_poll).await;
        if run_started.elapsed() > RUN_TIMEOUT {
            return finish_err(&app, "The run is still not finished after 3 hours; the wizard stopped following it. Check the Actions tab, then retry.".into());
        }
        let r = match gh.get_run(&i.owner, &i.repo, run_id).await {
            Ok(r) => {
                errors = 0;
                r
            }
            Err(e) => {
                errors += 1;
                if errors >= 10 {
                    return finish_err(&app, format!("Polling the run failed repeatedly: {e}"));
                }
                app.log
                    .warn(format!("polling run failed (will retry): {e}"));
                continue;
            }
        };
        if let Ok(jobs) = gh.run_jobs(&i.owner, &i.repo, run_id).await {
            // Fetch the log tail of each job once it has completed (failed jobs first).
            let mut done: Vec<_> = jobs
                .iter()
                .filter(|j| j.status.as_deref() == Some("completed") && !logged.contains(&j.id))
                .collect();
            done.sort_by_key(|j| j.conclusion.as_deref() == Some("success"));
            for j in done {
                logged.insert(j.id);
                if let Ok(Some(text)) = gh.job_log(&i.owner, &i.repo, j.id).await {
                    let tail = crate::util::tail_lines(&text, 60);
                    let tail = app.log.scrub(&tail);
                    app.w().release.log_tail = Some(format!("[{}]\n{}", j.name, tail));
                }
            }
            app.w().release.jobs = jobs;
        }
        let completed = r.status.as_deref() == Some("completed");
        app.w().release.run = Some(r.clone());
        if completed {
            break r;
        }
    };

    if final_run.conclusion.as_deref() != Some("success") {
        return finish_err(
            &app,
            format!(
                "The release workflow finished with conclusion '{}'. See the log tail and {}.",
                final_run.conclusion.unwrap_or_else(|| "unknown".into()),
                final_run.html_url
            ),
        );
    }

    phase(&app, "Waiting for the release and its assets");
    let started = Instant::now();
    loop {
        match gh.release_by_tag(&i.owner, &i.repo, &i.version).await {
            Ok(r) if classify(r.as_ref()) == ReleaseCheck::Ready => {
                let mut w = app.w();
                w.release.release = r;
                w.release.running = false;
                w.release.error = None;
                w.release.phase = "released".into();
                drop(w);
                app.log
                    .info(format!("release {} verified with assets", i.version));
                return;
            }
            Ok(r) => {
                let what = if r.is_some() {
                    "exists but has no assets yet"
                } else {
                    "not found yet"
                };
                app.w().release.phase = format!("Release {} {what}", i.version);
            }
            Err(e) => app
                .log
                .warn(format!("checking release failed (will retry): {e}")),
        }
        if started.elapsed() > t.release_wait_after_success {
            return finish_err(&app, format!(
                "The workflow succeeded, but no release tagged {} with at least one asset appeared. Check that the workflow publishes a release for the `version` input.",
                i.version
            ));
        }
        tokio::time::sleep(t.release_poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::ReleaseAsset;

    #[test]
    fn classify_requires_assets() {
        assert_eq!(classify(None), ReleaseCheck::NotFound);
        let mut r = Release {
            tag_name: "0.1.0".into(),
            html_url: "u".into(),
            assets: vec![],
            draft: false,
        };
        assert_eq!(classify(Some(&r)), ReleaseCheck::NoAssets);
        r.assets.push(ReleaseAsset {
            name: "taro.tar.gz".into(),
            size: 1,
            browser_download_url: "d".into(),
        });
        assert_eq!(classify(Some(&r)), ReleaseCheck::Ready);
        r.draft = true;
        assert_eq!(classify(Some(&r)), ReleaseCheck::NoAssets);
    }
}
