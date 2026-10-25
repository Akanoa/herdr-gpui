//! GitLab: the token each GitLab host the Orchestrator reads is signed in
//! with, where it comes from, and whom GitLab takes it for; and a pasted
//! personal access token, checked with GitLab before it is saved. Every
//! lookup blocks on the credential store, `glab`, or the network, so it runs
//! on the background executor and lands back here.

use super::{SettingsWindow, ui};
use crate::{search_input::SearchInput, settings_window::Section};
use gpui::{prelude::*, *};
use herdr_orchestrator::gitlab::{
    self,
    account::{self, Source},
    saved,
};
use secrecy::SecretString;
use std::sync::Arc;

/// gitlab.com, listed when no repository names another host.
const DEFAULT_HOST: &str = "gitlab.com";

/// One host the page lists.
pub(in crate::settings_window) struct HostStatus {
    pub(in crate::settings_window) host: String,
    pub(in crate::settings_window) source: Option<Source>,
    /// Whom GitLab signs the token in as, once asked.
    pub(in crate::settings_window) user: Option<Result<String, Arc<gitlab::Error>>>,
}

/// The page's state while it shows.
#[derive(Default)]
pub(in crate::settings_window) struct GitLabSettings {
    pub(in crate::settings_window) hosts: Vec<HostStatus>,
    host: Option<Entity<SearchInput>>,
    token: Option<Entity<SearchInput>>,
    /// A save or forget under way; the buttons wait for it.
    working: bool,
    pub(in crate::settings_window) message: Option<String>,
    /// Lookups and saves under way, dropped with the page.
    _task: Option<Task<()>>,
}

#[cfg(test)]
impl GitLabSettings {
    pub(in crate::settings_window) fn host_input(&self) -> Option<Entity<SearchInput>> {
        self.host.clone()
    }

    pub(in crate::settings_window) fn token_input(&self) -> Option<Entity<SearchInput>> {
        self.token.clone()
    }
}

/// Opens Settings at GitLab, as the Orchestrator's "sign in" does.
pub(crate) fn open(source: WeakEntity<crate::HerdrWindow>, cx: &mut App) {
    crate::settings_window::open(source, cx);
    // Queued after `open`'s own deferred work, so the window exists by now.
    cx.defer(|cx| {
        let Some(handle) = cx
            .default_global::<crate::settings_window::SettingsWindowHandle>()
            .window
        else {
            return;
        };
        let _ = handle.update(cx, |view, window, cx| {
            view.select_section(Section::GitLab, window, cx);
        });
    });
}

/// Where a token came from, as the page says it.
fn source_label(source: Source) -> &'static str {
    match source {
        Source::Environment => "from GITLAB_TOKEN",
        Source::Saved => "saved here",
        Source::Cli => "from the GitLab CLI (glab)",
    }
}

/// What a host's row says.
pub(in crate::settings_window) fn status_line(status: &HostStatus) -> String {
    let Some(source) = status.source else {
        return "No token. Public projects still list; paste a token below for private ones."
            .to_owned();
    };
    let from = source_label(source);
    match &status.user {
        None => format!("Checking the token {from}\u{2026}"),
        Some(Ok(user)) => format!("Signed in as {user}, {from}."),
        Some(Err(error)) => format!("{error}. The token is {from}."),
    }
}

/// The address that creates a token Herdr can read issues with on `host`.
fn new_token_url(host: &str) -> String {
    format!("https://{host}/-/user_settings/personal_access_tokens?name=Herdr%20UI&scopes=read_api")
}

/// Every host's token and whom it signs in as. Blocks: run it off the UI thread.
fn check(hosts: Vec<String>, store: herdr_credentials::Store) -> Vec<HostStatus> {
    let mut hosts = hosts;
    if let Ok(saved) = saved::read(store) {
        hosts.extend(saved.hosts().map(str::to_owned));
    }
    if hosts.is_empty() {
        hosts.push(DEFAULT_HOST.to_owned());
    }
    hosts.sort();
    hosts.dedup();
    hosts
        .into_iter()
        .map(|host| match account::lookup(&host, store) {
            Some(found) => HostStatus {
                user: Some(account::whoami(&host, &found.token).map_err(Arc::new)),
                source: Some(found.source),
                host,
            },
            None => HostStatus {
                host,
                source: None,
                user: None,
            },
        })
        .collect()
}

impl SettingsWindow {
    /// The page shows: make its fields, and look up every host's token.
    pub(in crate::settings_window) fn open_gitlab(&mut self, cx: &mut Context<Self>) {
        let field = |placeholder: &'static str, masked: bool, cx: &mut Context<Self>| {
            let input = cx.new(SearchInput::new);
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, cx);
                input.set_masked(masked, cx);
            });
            input
        };
        if self.gitlab.host.is_none() {
            self.gitlab.host = Some(field(DEFAULT_HOST, false, cx));
        }
        if self.gitlab.token.is_none() {
            self.gitlab.token = Some(field("glpat-\u{2026}", true, cx));
        }
        for input in [&self.gitlab.host, &self.gitlab.token]
            .into_iter()
            .flatten()
        {
            input.update(cx, |input, cx| {
                input.set_appearance(self.config.terminal.clone(), self.theme.clone(), cx);
            });
        }
        self.check_gitlab(cx);
    }

    /// Looks every host up again: the Orchestrator's GitLab hosts and those
    /// with a saved token. A headless window, or none, touches no saved
    /// credential; the page still lists gitlab.com.
    fn check_gitlab(&mut self, cx: &mut Context<Self>) {
        let (live, hosts) = self
            .source
            .read_with(cx, |window, cx| {
                (
                    window.reads_credentials(),
                    window.orchestrator_gitlab_hosts(cx),
                )
            })
            .unwrap_or_default();
        let shown = if hosts.is_empty() {
            vec![DEFAULT_HOST.to_owned()]
        } else {
            hosts.clone()
        };
        self.gitlab.hosts = shown
            .into_iter()
            .map(|host| HostStatus {
                host,
                source: None,
                user: None,
            })
            .collect();
        if !live {
            return;
        }
        let store = self.config.gitlab.store();
        let task = cx.background_spawn(async move { check(hosts, store) });
        self.gitlab._task = Some(cx.spawn(async move |this, cx| {
            let hosts = task.await;
            let _ = this.update(cx, |this, cx| {
                // The host field starts at the first host still needing a token.
                if let Some(input) = &this.gitlab.host
                    && input.read(cx).text().is_empty()
                    && let Some(status) = hosts
                        .iter()
                        .find(|status| !matches!(status.user, Some(Ok(_))))
                {
                    let host = status.host.clone();
                    input.update(cx, |input, cx| input.set_text_selected(&host, cx));
                }
                this.gitlab.hosts = hosts;
                this.gitlab.working = false;
                cx.notify();
            });
        }));
    }

    /// Checks the pasted token with GitLab, then saves it for the host typed.
    fn save_gitlab(&mut self, cx: &mut Context<Self>) {
        let (Some(host_input), Some(token_input)) = (&self.gitlab.host, &self.gitlab.token) else {
            return;
        };
        let typed = host_input.read(cx).text().trim().to_owned();
        let typed = if typed.is_empty() {
            DEFAULT_HOST.to_owned()
        } else {
            typed
        };
        let Some(host) = saved::normalize_host(&typed) else {
            self.gitlab.message = Some(gitlab::Error::Host(typed).to_string());
            cx.notify();
            return;
        };
        let token = token_input.read(cx).text().trim().to_owned();
        if token.is_empty() {
            self.gitlab.message = Some("Paste a personal access token first.".to_owned());
            cx.notify();
            return;
        }
        token_input.update(cx, SearchInput::clear);
        let token = SecretString::from(token);
        let store = self.config.gitlab.store();
        self.run_gitlab(cx, move || {
            let user = account::whoami(&host, &token)?;
            saved::save(store, &host, Some(token))?;
            Ok(format!(
                "Saved. GitLab at {host} signs this token in as {user}."
            ))
        });
    }

    fn forget_gitlab(&mut self, host: String, cx: &mut Context<Self>) {
        let store = self.config.gitlab.store();
        self.run_gitlab(cx, move || {
            saved::save(store, &host, None)?;
            Ok(format!("Forgot the token saved for {host}."))
        });
    }

    /// Runs a save or forget off the UI thread, then says how it went, looks
    /// every host up again, and has the Orchestrator read GitLab again.
    fn run_gitlab(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> Result<String, gitlab::Error> + Send + 'static,
    ) {
        self.gitlab.working = true;
        self.gitlab.message = None;
        let task = cx.background_spawn(async move { work() });
        self.gitlab._task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.gitlab.working = false;
                this.gitlab.message = Some(match result {
                    Ok(done) => done,
                    Err(error) => error.to_string(),
                });
                let _ = this
                    .source
                    .update(cx, |window, cx| window.refresh_orchestrators(cx));
                this.check_gitlab(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(in crate::settings_window) fn render_gitlab(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let ready = !self.gitlab.working;
        let hosts = self.gitlab.hosts.iter().enumerate().map(|(index, status)| {
            let host = status.host.clone();
            let url = new_token_url(&host);
            let mut row = ui::row(theme, format!("gitlab-host-{index}"), status.host.clone())
                .description(status_line(status))
                .control(
                    ui::button(
                        theme,
                        format!("gitlab-new-token-{index}"),
                        "Create token",
                        true,
                    )
                    .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            if status.source == Some(Source::Saved) {
                row = row.control(
                    ui::button(theme, format!("gitlab-forget-{index}"), "Forget", ready).when(
                        ready,
                        |button| {
                            button.on_click(cx.listener(move |this, _, _, cx| {
                                this.forget_gitlab(host.clone(), cx)
                            }))
                        },
                    ),
                );
            }
            row
        });
        let field =
            |label: &'static str, selector: &'static str, input: &Option<Entity<SearchInput>>| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().w(px(80.)).flex_none().child(label))
                    .children(input.as_ref().map(|input| {
                        div()
                            .debug_selector(move || selector.into())
                            .flex_1()
                            .min_w_0()
                            .child(input.clone())
                    }))
            };
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(ui::section(theme, "Hosts", None))
            .children(hosts)
            .child(
                self.control_card("Add a token")
                    .debug_selector(|| "gitlab-token".into())
                    .child(self.control_note(
                        "A personal access token with the read_api scope. GitLab is asked whom it \
                         signs in as before it is saved. A token is only ever sent over HTTPS, and \
                         GITLAB_TOKEN still takes precedence for its host.",
                    ))
                    .child(field("Host", "gitlab-host-input", &self.gitlab.host))
                    .child(field("Token", "gitlab-token-input", &self.gitlab.token))
                    .child(div().flex().justify_end().child(
                        ui::button(theme, "gitlab-save", "Check and save", ready).when(
                            ready,
                            |button| {
                                button.on_click(cx.listener(|this, _, _, cx| this.save_gitlab(cx)))
                            },
                        ),
                    ))
                    .children(self.gitlab.message.clone().map(|message| {
                        div()
                            .debug_selector(|| "gitlab-message".into())
                            .min_w_0()
                            .child(message)
                    })),
            )
    }
}
