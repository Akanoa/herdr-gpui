//! The account menu's GitLab page: each instance's row says where its token
//! comes from and whom it signs in as, the header switches between GitHub
//! and GitLab, and a token is refused before any lookup when the instance or
//! token typed cannot be one. The fixture window is headless, so nothing
//! here reads the keyring, runs direnv or `glab`, or reaches GitLab.
#![allow(clippy::unwrap_used)]
use super::{HostStatus, Token, merge_hosts, status_line};
use crate::{HerdrWindow, menu::Page, orchestrator::GitLabRead};
use gpui::{Entity, TestAppContext, VisualTestContext, px, size};
use herdr_orchestrator::gitlab::{self, Scheme, account::Source, cli_hosts::CliHost};
use std::{path::PathBuf, sync::Arc};

fn page(cx: &mut TestAppContext) -> (Entity<HerdrWindow>, &mut VisualTestContext) {
    let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
    cx.simulate_resize(size(px(1200.), px(900.)));
    view.update_in(cx, |view, window, cx| {
        assert!(view.open_menu(window, cx));
        view.open_gitlab_page(window, cx);
    });
    draw(cx);
    (view, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let point = cx.debug_bounds(selector).unwrap().center();
    cx.simulate_click(point, Default::default());
    draw(cx);
}

fn status(source: Option<Source>, user: Option<Result<&str, gitlab::Error>>) -> HostStatus {
    HostStatus {
        host: "gitlab.com".into(),
        plain: false,
        saved: source == Some(Source::Saved),
        checkout: Some("app".into()),
        token: match source {
            Some(source) => Token::Found {
                source,
                user: user.map(|user| user.map(str::to_owned).map_err(Arc::new)),
            },
            None => Token::None,
        },
    }
}

fn read(host: &str, scheme: Scheme, checkout: Option<&str>) -> GitLabRead {
    GitLabRead {
        host: host.into(),
        scheme,
        checkout: checkout.map(PathBuf::from),
    }
}

#[test]
fn an_instances_row_says_where_its_token_comes_from_and_whom_it_signs_in() {
    assert!(status_line(&status(None, None)).starts_with("No token."));
    assert_eq!(
        status_line(&status(Some(Source::Saved), Some(Ok("Akanoa")))),
        "Signed in as Akanoa, saved here."
    );
    assert_eq!(
        status_line(&status(Some(Source::Envrc), Some(Ok("Akanoa")))),
        "Signed in as Akanoa, from the .envrc of app."
    );
    assert_eq!(
        status_line(&status(Some(Source::Cli), None)),
        "Checking the token from the GitLab CLI (glab)\u{2026}"
    );
    assert_eq!(
        status_line(&status(
            Some(Source::Cli),
            Some(Err(gitlab::Error::Authentication))
        )),
        "GitLab refused the token from the GitLab CLI (glab). Paste a new one below."
    );
    let limited = status_line(&status(
        Some(Source::Saved),
        Some(Err(gitlab::Error::Scope)),
    ));
    assert!(limited.ends_with("The token is saved here."), "{limited}");
    let unknown = HostStatus {
        token: Token::Unknown,
        ..status(None, None)
    };
    assert_eq!(status_line(&unknown), "Looking for a token\u{2026}");
}

#[test]
fn a_refused_token_from_the_shell_or_an_envrc_is_not_fixed_by_pasting_one() {
    let error = gitlab::Error::Authentication.from_source(Source::Environment);
    let line = status_line(&status(Some(Source::Environment), Some(Err(error))));
    assert!(line.contains("Change or unset it"), "{line}");
    assert!(!line.contains("Paste"), "{line}");
    let error = gitlab::Error::Authentication.from_source(Source::Envrc);
    let line = status_line(&status(Some(Source::Envrc), Some(Err(error))));
    assert!(line.contains(".envrc of app"), "{line}");
    assert!(line.contains("Change it there"), "{line}");
}

#[test]
fn an_instance_only_read_over_http_says_no_token_is_sent() {
    let plain = HostStatus {
        plain: true,
        ..status(None, None)
    };
    assert!(status_line(&plain).starts_with("Read over plain HTTP"));
}

#[test]
fn instances_are_listed_once_whatever_their_case_and_https_wins() {
    let hosts = merge_hosts(
        [
            read("GitLab.Example.com", Scheme::Https, None),
            read("code.internal:8080", Scheme::Http, None),
            read("mixed.example.com", Scheme::Http, None),
            read("mixed.example.com", Scheme::Https, Some("/src/mixed")),
        ],
        ["gitlab.example.com".to_owned()],
        &[],
    );
    let listed: Vec<_> = hosts
        .iter()
        .map(|(host, listed)| (host.as_str(), listed.api.scheme == Scheme::Http))
        .collect();
    assert_eq!(
        listed,
        [
            ("code.internal:8080", true),
            ("gitlab.example.com", false),
            ("mixed.example.com", false),
        ]
    );
    // The first checkout on this computer is the one whose .envrc is asked.
    assert_eq!(
        hosts["mixed.example.com"].checkout,
        Some(PathBuf::from("/src/mixed"))
    );
    let empty: Vec<_> = merge_hosts([], [], &[]).into_keys().collect();
    assert_eq!(empty, ["gitlab.com"]);
}

#[test]
fn a_remote_on_an_ssh_host_lists_its_instance_and_api() {
    let cli = [CliHost {
        host: "gitlab.example.com".into(),
        api_host: Some("api.gitlab.example.com".into()),
        ssh_host: Some("gitlab-ssh.example.com".into()),
        ..CliHost::default()
    }];
    let hosts = merge_hosts(
        [read("gitlab-ssh.example.com", Scheme::Https, None)],
        ["gitlab.example.com".to_owned()],
        &cli,
    );
    let listed: Vec<_> = hosts.values().collect();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].api.instance, "gitlab.example.com");
    // The token is checked where glab sends the instance's API requests.
    assert_eq!(listed[0].api.host, "api.gitlab.example.com");
}

#[gpui::test]
fn the_page_lists_gitlab_dot_com_and_the_token_form(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    for selector in [
        "accounts-tab-github",
        "accounts-tab-gitlab",
        "gitlab-host-0",
        "gitlab-token",
        "gitlab-host-input",
        "gitlab-token-input",
        "gitlab-save",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(cx.debug_bounds("gitlab-forget-0").is_none());
    view.read_with(cx, |view, _| {
        assert_eq!(view.menu.gitlab.hosts.len(), 1);
        assert_eq!(view.menu.gitlab.hosts[0].host, "gitlab.com");
    });
}

#[gpui::test]
fn the_header_switches_between_github_and_gitlab(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    click(cx, "accounts-tab-github");
    view.read_with(cx, |view, _| assert!(view.menu.page == Some(Page::GitHub)));
    assert!(cx.debug_bounds("github-body").is_some());
    click(cx, "accounts-tab-gitlab");
    view.read_with(cx, |view, _| assert!(view.menu.page == Some(Page::GitLab)));
    assert!(cx.debug_bounds("gitlab-body").is_some());
    click(cx, "github-header-close");
    view.read_with(cx, |view, _| assert!(view.menu.page.is_none()));
}

#[gpui::test]
fn only_a_saved_token_can_be_forgotten(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    let show = |view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, status: HostStatus| {
        view.update(cx, |view, cx| {
            view.menu.gitlab.hosts = vec![status];
            cx.notify();
        });
        draw(cx);
    };
    show(&view, cx, status(Some(Source::Saved), Some(Ok("Akanoa"))));
    assert!(cx.debug_bounds("gitlab-forget-0").is_some());
    show(&view, cx, status(Some(Source::Cli), Some(Ok("Akanoa"))));
    assert!(cx.debug_bounds("gitlab-forget-0").is_none());
    // An .envrc token wins over a saved one, which can still be forgotten.
    let shadowed = HostStatus {
        saved: true,
        ..status(Some(Source::Envrc), Some(Ok("Akanoa")))
    };
    show(&view, cx, shadowed);
    assert!(cx.debug_bounds("gitlab-forget-0").is_some());
}

fn type_in(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext, host: &str, token: &str) {
    view.update(cx, |view, cx| {
        let gitlab = &view.menu.gitlab;
        let (host_input, token_input) =
            (gitlab.host.clone().unwrap(), gitlab.token.clone().unwrap());
        host_input.update(cx, |input, cx| input.set_text_selected(host, cx));
        token_input.update(cx, |input, cx| input.set_text_selected(token, cx));
    });
}

fn message_and_token(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> (String, String) {
    view.read_with(cx, |view, cx| {
        let gitlab = &view.menu.gitlab;
        (
            gitlab.message.clone().unwrap_or_default(),
            gitlab.token.as_ref().unwrap().read(cx).text().to_owned(),
        )
    })
}

#[gpui::test]
fn a_save_that_cannot_be_checked_asks_nothing_and_keeps_the_token(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    type_in(&view, cx, "gitlab.com", "");
    click(cx, "gitlab-save");
    assert_eq!(
        message_and_token(&view, cx).0,
        "Paste a personal access token first."
    );
    for (host, expected) in [
        (
            "gitlab.com/group",
            "\"gitlab.com/group\" is not a GitLab host name",
        ),
        ("http://code.internal:8080", "use an https:// remote"),
        // A headless window saves nothing.
        ("gitlab.com", "This window does not keep GitLab tokens."),
    ] {
        type_in(&view, cx, host, "glpat-x");
        click(cx, "gitlab-save");
        let (message, token) = message_and_token(&view, cx);
        assert!(message.contains(expected), "{host}: {message}");
        assert_eq!(token, "glpat-x", "{host}");
    }
}

#[gpui::test]
fn an_instance_only_read_over_http_takes_no_token(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    view.update(cx, |view, cx| {
        view.menu.gitlab.hosts = vec![HostStatus {
            host: "code.internal:8080".into(),
            plain: true,
            saved: false,
            checkout: None,
            token: Token::None,
        }];
        cx.notify();
    });
    type_in(&view, cx, "code.internal:8080", "glpat-x");
    click(cx, "gitlab-save");
    let (message, token) = message_and_token(&view, cx);
    assert!(message.contains("use an https:// remote"), "{message}");
    assert_eq!(token, "glpat-x");
}
