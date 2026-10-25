//! GitLab: the token each GitLab host the Orchestrator reads is signed in
//! with, where it comes from, and whom GitLab takes it for; and a pasted
//! personal access token, checked with GitLab before it is saved. Every
//! lookup blocks on the credential store, `glab`, or the network, so it runs
//! on the background executor and lands back here.

use super::{SettingsWindow, ui};
use crate::{search_input::SearchInput, settings_window::Section};
use gpui::{prelude::*, *};
use herdr_orchestrator::gitlab::{
    self, Api, Scheme,
    account::{self, Source},
    cli_hosts::{self, CliHost},
    saved,
};
use secrecy::SecretString;
use std::{collections::BTreeMap, sync::Arc};

/// gitlab.com, listed when no repository names another host.
const DEFAULT_HOST: &str = "gitlab.com";
/// What a save or forget says in a window that keeps no saved credential.
const NO_STORE: &str = "This window does not keep GitLab tokens.";

/// What is known of a host's token.
pub(in crate::settings_window) enum Token {
    /// Not looked up yet.
    Unknown,
    None,
    Found {
        /// The token the host is read with, which may not be the saved one.
        source: Source,
        /// Whom GitLab signs the token in as, once asked.
        user: Option<Result<String, Arc<gitlab::Error>>>,
    },
}

/// One host the page lists.
pub(in crate::settings_window) struct HostStatus {
    pub(in crate::settings_window) host: String,
    /// Every remote reads the host over plain HTTP, so no token is sent to
    /// it or checked with it.
    pub(in crate::settings_window) plain: bool,
    /// A token is saved for the host, so Forget is offered even while
    /// `GITLAB_TOKEN` takes precedence over it.
    pub(in crate::settings_window) saved: bool,
    pub(in crate::settings_window) token: Token,
}

/// The page's state while it shows.
#[derive(Default)]
pub(in crate::settings_window) struct GitLabSettings {
    pub(in crate::settings_window) hosts: Vec<HostStatus>,
    /// The source window's hosts, which the host field starts at first.
    preferred: Vec<String>,
    host: Option<Entity<SearchInput>>,
    token: Option<Entity<SearchInput>>,
    /// A save or forget under way; the buttons wait for it.
    working: bool,
    pub(in crate::settings_window) message: Option<String>,
    /// Why the saved tokens could not be read, shown above the hosts.
    pub(in crate::settings_window) problem: Option<String>,
    /// The saved-token read under way, replaced by the next one.
    _task: Option<Task<()>>,
    /// Each host's lookup, landing on its own row as it finishes, so one
    /// slow host never holds the others. Replaced by the next check.
    _lookups: Vec<Task<()>>,
    /// The save or forget under way. Kept apart from lookups, which reopening
    /// the page starts again, so its result is still shown and acted on.
    _save: Option<Task<()>>,
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
    crate::settings_window::open_at(source, Section::GitLab, cx);
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
    if status.plain {
        return "Read over plain HTTP, so no token is sent. Use an https:// remote for private \
                projects."
            .to_owned();
    }
    let (source, user) = match &status.token {
        Token::Unknown => return "Looking for a token\u{2026}".to_owned(),
        Token::None => {
            return "No token. Public projects still list; paste a token below for private ones."
                .to_owned();
        }
        Token::Found { source, user } => (*source, user),
    };
    let from = source_label(source);
    match user {
        None => format!("Checking the token {from}\u{2026}"),
        Some(Ok(user)) => format!("Signed in as {user}, {from}."),
        // The error's own text sends the user here, so this page says it plainly.
        Some(Err(error)) if matches!(**error, gitlab::Error::Authentication) => {
            format!("GitLab refused the token {from}. Paste a new one below.")
        }
        // A pasted token is only tried after GITLAB_TOKEN, so it cannot help.
        Some(Err(error)) if matches!(**error, gitlab::Error::EnvironmentRefused) => {
            "GitLab refused GITLAB_TOKEN. Change or unset it: a token saved here is only tried \
             after it."
                .to_owned()
        }
        Some(Err(error)) => format!("{error}. The token is {from}."),
    }
}

/// The address that creates a token Herdr can read issues with on `host`.
fn new_token_url(host: &str) -> String {
    format!("https://{host}/-/user_settings/personal_access_tokens?name=Herdr%20UI&scopes=read_api")
}

/// The instances listed, by normalized name, each with its API: a remote's
/// host becomes the instance whose `ssh_host` glab names it, and a saved
/// instance keeps glab's `api_host` for it. An instance is plain only when
/// every remote reads it over HTTP, since one HTTPS remote is enough to
/// send a token.
pub(in crate::settings_window) fn merge_hosts(
    listed: impl IntoIterator<Item = (String, Scheme)>,
    saved: impl IntoIterator<Item = String>,
    cli: &[CliHost],
) -> BTreeMap<String, Api> {
    let mut hosts = BTreeMap::<String, Api>::new();
    let listed = listed
        .into_iter()
        .map(|(host, scheme)| Api::new(&host, scheme, cli));
    let saved = saved.into_iter().map(|host| Api::of_instance(&host, cli));
    for mut api in listed.chain(saved) {
        api.instance = saved::normalize_host(&api.instance).unwrap_or(api.instance);
        match hosts.get_mut(&api.instance) {
            Some(known) if known.scheme == Scheme::Http && api.scheme == Scheme::Https => {
                *known = api;
            }
            Some(_) => {}
            None => {
                hosts.insert(api.instance.clone(), api);
            }
        }
    }
    if hosts.is_empty() {
        hosts.insert(DEFAULT_HOST.to_owned(), Api::of_instance(DEFAULT_HOST, cli));
    }
    hosts
}

/// An instance's token and whom it signs in as. Blocks on `glab` and the
/// network: run it off the UI thread.
fn check_host(api: &Api, saved: Option<SecretString>) -> Token {
    match account::lookup_with(&api.instance, saved) {
        Some(found) => Token::Found {
            user: Some(
                account::whoami(api, &found.token)
                    .map_err(|error| Arc::new(error.from_source(found.source))),
            ),
            source: found.source,
        },
        None => Token::None,
    }
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

    /// The store saved tokens live in, or `None` when the source window is
    /// headless or gone: such a window touches no saved credential.
    fn gitlab_store(&self, cx: &App) -> Option<herdr_credentials::Store> {
        self.source
            .read_with(cx, |window, _| window.reads_credentials())
            .unwrap_or_default()
            .then(|| self.config.gitlab.store())
    }

    /// Lists every main window's GitLab hosts and those with a saved
    /// token, then looks each up on its own. Without a store the page still
    /// lists the hosts, with no token.
    fn check_gitlab(&mut self, cx: &mut Context<Self>) {
        let listed = crate::HerdrWindow::all_orchestrator_gitlab_hosts(cx);
        self.gitlab.preferred = self
            .source
            .read_with(cx, |window, cx| window.orchestrator_gitlab_hosts(cx))
            .unwrap_or_default()
            .into_iter()
            .map(|(host, _)| saved::normalize_host(&host).unwrap_or(host))
            .collect();
        self.gitlab._lookups.clear();
        let Some(store) = self.gitlab_store(cx) else {
            self.gitlab._task = None;
            self.gitlab.problem = None;
            self.show_gitlab_hosts(&merge_hosts(listed, [], &[]), |_| false, || Token::None);
            return;
        };
        self.show_gitlab_hosts(
            &merge_hosts(listed.clone(), [], &[]),
            |_| false,
            || Token::Unknown,
        );
        let read = cx.background_spawn(async move { (saved::read(store), cli_hosts::read()) });
        self.gitlab._task = Some(cx.spawn(async move |this, cx| {
            let (saved, cli) = read.await;
            let _ = this.update(cx, |this, cx| {
                this.gitlab.problem = saved.as_ref().err().map(|error| match error {
                    gitlab::Error::Record => {
                        format!("{error}. Saving or forgetting a token replaces them.")
                    }
                    error => error.to_string(),
                });
                let saved = saved.unwrap_or_default();
                let hosts = merge_hosts(listed, saved.hosts().map(str::to_owned), &cli);
                this.show_gitlab_hosts(&hosts, |host| saved.get(host).is_some(), || Token::Unknown);
                this.gitlab._lookups = hosts
                    .into_values()
                    .filter(|api| api.scheme == Scheme::Https)
                    .map(|api| {
                        let token = saved.get(&api.instance).cloned();
                        this.look_up_gitlab_host(api, token, cx)
                    })
                    .collect();
                this.finish_gitlab_check(cx);
                cx.notify();
            });
        }));
    }

    /// Lists `hosts`, each with `token`, except that a plain host has none.
    fn show_gitlab_hosts(
        &mut self,
        hosts: &BTreeMap<String, Api>,
        saved: impl Fn(&str) -> bool,
        token: impl Fn() -> Token,
    ) {
        self.gitlab.hosts = hosts
            .iter()
            .map(|(host, api)| {
                let plain = api.scheme == Scheme::Http;
                HostStatus {
                    saved: saved(host),
                    token: if plain { Token::None } else { token() },
                    host: host.clone(),
                    plain,
                }
            })
            .collect();
    }

    fn look_up_gitlab_host(
        &self,
        api: Api,
        saved: Option<SecretString>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let host = api.instance.clone();
        let task = cx.background_spawn(async move { check_host(&api, saved) });
        cx.spawn(async move |this, cx| {
            let token = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(status) = this.gitlab.hosts.iter_mut().find(|s| s.host == host) {
                    status.token = token;
                }
                this.finish_gitlab_check(cx);
                cx.notify();
            });
        })
    }

    /// Once every host is looked up, an empty host field starts at the first
    /// host still needing a token, the source window's hosts first.
    fn finish_gitlab_check(&mut self, cx: &mut Context<Self>) {
        let hosts = &self.gitlab.hosts;
        if hosts
            .iter()
            .any(|status| matches!(status.token, Token::Unknown))
        {
            return;
        }
        let Some(input) = &self.gitlab.host else {
            return;
        };
        if !input.read(cx).text().is_empty() {
            return;
        }
        let needs = |status: &&HostStatus| {
            !status.plain
                && !matches!(
                    status.token,
                    Token::Found {
                        user: Some(Ok(_)),
                        ..
                    }
                )
        };
        let preferred = &self.gitlab.preferred;
        let Some(status) = hosts
            .iter()
            .filter(needs)
            .find(|status| preferred.contains(&status.host))
            .or_else(|| hosts.iter().find(needs))
        else {
            return;
        };
        let host = status.host.clone();
        input.update(cx, |input, cx| input.set_text_selected(&host, cx));
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
        // A token sent to an HTTP address could be read on the way, and a
        // host only read over HTTP would never send it.
        let plain = typed.to_ascii_lowercase().starts_with("http://")
            || self
                .gitlab
                .hosts
                .iter()
                .any(|status| status.plain && status.host == host);
        if plain {
            self.gitlab.message = Some(gitlab::Error::InsecureHost(host).to_string());
            cx.notify();
            return;
        }
        let token = token_input.read(cx).text().trim().to_owned();
        if token.is_empty() {
            self.gitlab.message = Some("Paste a personal access token first.".to_owned());
            cx.notify();
            return;
        }
        let Some(store) = self.gitlab_store(cx) else {
            self.gitlab.message = Some(NO_STORE.to_owned());
            cx.notify();
            return;
        };
        let token = SecretString::from(token);
        // The field keeps the token until it is saved: GitLab shows a token
        // only once, so a failed check must not lose it.
        self.run_gitlab(cx, true, move || {
            // glab may serve the instance's API elsewhere, or only over HTTP.
            let api = Api::of_instance(&host, &cli_hosts::read());
            let user = account::whoami(&api, &token)?;
            saved::save(store, &host, Some(token))?;
            Ok(format!(
                "Saved. GitLab at {host} signs this token in as {user}."
            ))
        });
    }

    fn forget_gitlab(&mut self, host: String, cx: &mut Context<Self>) {
        let Some(store) = self.gitlab_store(cx) else {
            self.gitlab.message = Some(NO_STORE.to_owned());
            cx.notify();
            return;
        };
        self.run_gitlab(cx, false, move || {
            saved::save(store, &host, None)?;
            Ok(format!("Forgot the token saved for {host}."))
        });
    }

    /// Runs a save or forget off the UI thread, then says how it went, looks
    /// every host up again, and has every window's Orchestrator read GitLab
    /// again. A successful save also empties the token field.
    fn run_gitlab(
        &mut self,
        cx: &mut Context<Self>,
        saves_token: bool,
        work: impl FnOnce() -> Result<String, gitlab::Error> + Send + 'static,
    ) {
        self.gitlab.working = true;
        self.gitlab.message = None;
        let task = cx.background_spawn(async move { work() });
        self.gitlab._save = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.gitlab.working = false;
                if saves_token
                    && result.is_ok()
                    && let Some(input) = &this.gitlab.token
                {
                    input.update(cx, SearchInput::clear);
                }
                this.gitlab.message = Some(match result {
                    Ok(done) => done,
                    Err(error) => error.to_string(),
                });
                crate::HerdrWindow::refresh_all_orchestrators(cx);
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
            if status.saved {
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
            .children(self.gitlab.problem.clone().map(|problem| {
                div()
                    .debug_selector(|| "gitlab-problem".into())
                    .min_w_0()
                    .child(self.control_note(problem))
            }))
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
