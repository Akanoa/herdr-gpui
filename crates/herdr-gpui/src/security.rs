//! The activity bar's Security section: the private vulnerability reports
//! outsiders filed (`reports`) and the open Dependabot alerts of every
//! github.com repository the window's workspaces hold, most severe first.
//! Its icon joins the bar only once GitHub reports either on for one of
//! them, and stays for the session so the bar does not shift when a later
//! read fails.
//!
//! The repositories are the ones the Pull requests and Issues sections list
//! (`HerdrWindow::repo_specs`), read with the same GitHub accounts, but the
//! alerts live here only: the orchestrator's database is a contract shared
//! with agent-launcher and has no place for them. A background read runs
//! every [`INTERVAL`], or when Refresh asks; drawing reads what it left.

mod actions;
mod fetch;
mod list;
mod model;
mod page;
mod pane;
mod reports;
mod section;

#[cfg(test)]
mod tests;

pub(crate) use model::{Alert, Alerts, Repo, Severity};
pub(crate) use page::{Item, Tab};
pub(crate) use reports::{Report, Reports};

use crate::{HerdrWindow, browser::WebUrl, sidebar::ActivityPanel};
use gpui::{prelude::*, *};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

/// How often the alerts are read again. They change when a dependency is
/// bumped or an advisory is published, not from minute to minute.
const INTERVAL: Duration = Duration::from_secs(15 * 60);
/// How soon to look again while there is nothing to read: no GitHub account
/// yet, or no workspace holding a repository.
const WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Github(Box<crate::Error>),
    #[error("GitHub's answer had no repository in it")]
    Output,
    #[error("Cannot read GitHub's alerts listing: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Whether GitHub refused for the account's spent quota.
    fn rate_limited(&self) -> bool {
        matches!(self, Self::Github(error) if matches!(**error, crate::Error::GitHubRateLimit { .. }))
    }
}

/// The section's state in a window.
pub(crate) struct Security {
    repos: Vec<Repo>,
    /// When the last read finished, for "Updated … ago".
    read_at: Option<SystemTime>,
    /// When the next read is due; `None` reads at the next tick.
    next: Option<Instant>,
    task: Option<Task<()>>,
    /// Whether any repository had Dependabot alerts or private reporting
    /// on, this session.
    available: bool,
    /// The GitHub logins `repos` was read as. Private reports are only for
    /// the accounts that may see them, so what one sign-in read is dropped
    /// once the window reads GitHub as anyone else.
    accounts: Vec<String>,
    /// Tells a read in flight to stop when the window goes.
    cancelled: Arc<AtomicBool>,
    scroll: ScrollHandle,
    /// The alert or report whose page is shown beside the list.
    opened: Option<page::Opened>,
    /// Whether the page takes the whole section, the list folded away.
    list_hidden: bool,
    /// Pages read this session, by item.
    pages: HashMap<Item, page::Loaded>,
    page_scroll: ScrollHandle,
}

impl Default for Security {
    fn default() -> Self {
        Self {
            repos: Vec::new(),
            read_at: None,
            next: None,
            task: None,
            available: false,
            accounts: Vec::new(),
            cancelled: Arc::default(),
            scroll: ScrollHandle::new(),
            opened: None,
            list_hidden: false,
            pages: HashMap::new(),
            page_scroll: ScrollHandle::new(),
        }
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Security {
    /// Whether the bar shows the section's icon.
    pub(crate) fn available(&self) -> bool {
        self.available
    }

    /// The repositories as last read.
    pub(crate) fn repos(&self) -> &[Repo] {
        &self.repos
    }

    pub(crate) fn read_at(&self) -> Option<SystemTime> {
        self.read_at
    }

    pub(crate) fn busy(&self) -> bool {
        self.task.is_some()
    }

    /// Open critical and high alerts, and every report in triage whatever
    /// its rating, since each waits on a reply: the bar's count.
    pub(crate) fn urgent(&self) -> usize {
        let alerts = self
            .repos
            .iter()
            .flat_map(Repo::open)
            .filter(|alert| alert.severity.urgent())
            .count();
        alerts
            + self
                .repos
                .iter()
                .map(|repo| repo.reported().len())
                .sum::<usize>()
    }

    /// When the newest of what [`Self::urgent`] counts was opened, so the
    /// bar can tell whether it arrived since the section was last open.
    pub(crate) fn urgent_since(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let alerts = self
            .repos
            .iter()
            .flat_map(Repo::open)
            .filter(|alert| alert.severity.urgent())
            .filter_map(|alert| alert.created_at);
        let reports = self
            .repos
            .iter()
            .flat_map(Repo::reported)
            .filter_map(|report| report.created_at);
        alerts.chain(reports).max()
    }

    /// Installs `repos` as a read would, for tests outside this module.
    #[cfg(test)]
    pub(crate) fn read_for_test(&mut self, repos: Vec<Repo>) {
        self.install(repos, Instant::now(), SystemTime::now());
    }

    /// Reads again at the next tick.
    pub(crate) fn refresh(&mut self) {
        self.next = None;
    }

    fn due(&self, now: Instant) -> bool {
        self.task.is_none() && self.next.is_none_or(|next| now >= next)
    }

    /// Drops what was read, and the answer of a read in flight with it:
    /// dropping its task drops the update that would install it.
    fn forget(&mut self) {
        self.repos.clear();
        self.read_at = None;
        self.next = None;
        self.task = None;
        self.opened = None;
        self.pages.clear();
    }

    /// What a read found, replacing the last one.
    fn install(&mut self, repos: Vec<Repo>, now: Instant, at: SystemTime) {
        self.available |= repos.iter().any(Repo::enabled);
        self.repos = repos;
        self.keep_opened();
        self.read_at = Some(at);
        self.next = Some(now + INTERVAL);
        self.task = None;
    }
}

impl HerdrWindow {
    /// Runs every tick: starts a read when one is due and there is a
    /// repository to read with an account to read it.
    pub(crate) fn poll_security(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let accounts = self.github_logins();
        if accounts != self.security.accounts {
            self.security.forget();
            self.security.accounts = accounts;
            cx.notify();
        }
        if !self.activity_bar_shown() || !self.security.due(now) {
            return;
        }
        let specs: Vec<_> = self
            .repo_specs()
            .into_iter()
            .filter(|spec| spec.request.token.is_some())
            .collect();
        if specs.is_empty() {
            // Nothing left to read: the last read's repositories are no
            // longer open, or no longer readable with any sign-in.
            if !self.security.repos.is_empty() {
                self.security.install(Vec::new(), now, SystemTime::now());
                cx.notify();
            }
            self.security.next = Some(now + WAIT);
            return;
        }
        let cancelled = self.security.cancelled.clone();
        let work = cx
            .background_executor()
            .spawn(async move { fetch::read(&specs, &cancelled) });
        self.security.task = Some(cx.spawn(async move |this, cx| {
            let repos = work.await;
            let _ = this.update(cx, |this, cx| {
                this.security
                    .install(repos, Instant::now(), SystemTime::now());
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Every login the window reads GitHub as, on this device and on its
    /// hosts, sorted.
    fn github_logins(&self) -> Vec<String> {
        let mut logins: Vec<String> = std::iter::once(&self.menu.github)
            .chain(self.menu.github_hosts.values())
            .chain(
                self.menu
                    .github_accounts
                    .iter()
                    .map(|account| &account.auth),
            )
            .filter_map(|auth| auth.profile.as_ref())
            .map(|profile| profile.login.clone())
            .collect();
        logins.sort_unstable();
        logins.dedup();
        logins
    }

    /// Whether the bar lists the section: once alerts are on somewhere, or
    /// while it is the one shown, so a stored or asked-for choice is never
    /// left without its icon.
    pub(crate) fn security_listed(&self) -> bool {
        self.security.available() || self.activity.panel == ActivityPanel::Security
    }

    /// Shows `url`, an alert's or repository's page, in a browser tab of the
    /// focused workspace, and the terminals behind it; Back returns here.
    fn open_security_page(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(url) = WebUrl::try_from(url) else {
            return;
        };
        let Some((_, workspace)) = self.browser_key() else {
            self.show_flash(crate::window::Flash::warning("Open a workspace first"), cx);
            return;
        };
        let endpoint = self.endpoints[self.selected_endpoint].id.clone();
        let opened = self.open_workspace_page(&endpoint, &workspace, url, window, cx);
        // The system browser leaves the section shown; a tab is behind it.
        if opened == crate::browser::PageOpened::Tab {
            self.activity.pick(ActivityPanel::Workspaces);
            self.activity.return_to = Some(ActivityPanel::Security);
            self.save_chrome();
        }
        cx.notify();
    }
}
