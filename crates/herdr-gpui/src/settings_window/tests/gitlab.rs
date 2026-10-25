//! Settings > GitLab: each host's row says where its token comes from and
//! whom it signs in as, and a token is refused before any lookup when the
//! host or token typed cannot be one. The fixture window is headless, so
//! nothing here reads the keyring or runs `glab`.
use super::*;
use crate::settings_window::pages::gitlab::{HostStatus, Token, merge_hosts, status_line};
use herdr_orchestrator::gitlab::{self, Scheme, account::Source};

fn page(cx: &mut TestAppContext) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    let (view, cx) = cx.add_window_view(skill_fixture);
    view.update_in(cx, |view, window, cx| {
        view.select_section(Section::GitLab, window, cx);
    });
    cx.simulate_resize(size(px(960.), px(1600.)));
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
        token: match source {
            Some(source) => Token::Found {
                source,
                user: user.map(|user| user.map(str::to_owned).map_err(Arc::new)),
            },
            None => Token::None,
        },
    }
}

#[test]
fn a_hosts_row_says_where_its_token_comes_from_and_whom_it_signs_in() {
    assert!(status_line(&status(None, None)).starts_with("No token."));
    assert_eq!(
        status_line(&status(Some(Source::Saved), Some(Ok("Akanoa")))),
        "Signed in as Akanoa, saved here."
    );
    assert_eq!(
        status_line(&status(Some(Source::Cli), None)),
        "Checking the token from the GitLab CLI (glab)\u{2026}"
    );
    let refused = status_line(&status(
        Some(Source::Cli),
        Some(Err(gitlab::Error::Authentication)),
    ));
    assert_eq!(
        refused,
        "GitLab refused the token from the GitLab CLI (glab). Paste a new one below."
    );
    let limited = status_line(&status(
        Some(Source::Saved),
        Some(Err(gitlab::Error::Scope)),
    ));
    assert!(limited.ends_with("The token is saved here."), "{limited}");
    assert!(
        status_line(&status(Some(Source::Environment), Some(Ok("me"))))
            .ends_with("from GITLAB_TOKEN.")
    );
    let unknown = HostStatus {
        token: Token::Unknown,
        ..status(None, None)
    };
    assert_eq!(status_line(&unknown), "Looking for a token\u{2026}");
}

#[test]
fn a_refused_gitlab_token_variable_is_not_fixed_by_pasting_one() {
    let error = gitlab::Error::Authentication.from_source(Source::Environment);
    assert!(matches!(error, gitlab::Error::EnvironmentRefused));
    let line = status_line(&status(Some(Source::Environment), Some(Err(error))));
    assert!(line.contains("Change or unset it"), "{line}");
    assert!(!line.contains("Paste"), "{line}");
}

#[test]
fn a_host_only_read_over_http_says_no_token_is_sent() {
    let plain = HostStatus {
        plain: true,
        ..status(None, None)
    };
    assert!(status_line(&plain).starts_with("Read over plain HTTP"));
}

#[test]
fn hosts_are_listed_once_whatever_their_case_and_https_wins() {
    let hosts = merge_hosts(
        [
            ("GitLab.Example.com".to_owned(), Scheme::Https),
            ("code.internal:8080".to_owned(), Scheme::Http),
            ("mixed.example.com".to_owned(), Scheme::Http),
            ("mixed.example.com".to_owned(), Scheme::Https),
        ],
        ["gitlab.example.com".to_owned()],
    );
    let hosts: Vec<_> = hosts.into_iter().collect();
    assert_eq!(
        hosts,
        [
            ("code.internal:8080".to_owned(), true),
            ("gitlab.example.com".to_owned(), false),
            ("mixed.example.com".to_owned(), false),
        ]
    );
    let empty: Vec<_> = merge_hosts([], []).into_iter().collect();
    assert_eq!(empty, [("gitlab.com".to_owned(), false)]);
}

#[gpui::test]
fn the_page_lists_gitlab_dot_com_and_the_token_form(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    for selector in [
        "gitlab-host-0",
        "gitlab-new-token-0",
        "gitlab-token",
        "gitlab-host-input",
        "gitlab-token-input",
        "gitlab-save",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(cx.debug_bounds("gitlab-forget-0").is_none());
    view.read_with(cx, |view, _| {
        assert_eq!(view.gitlab.hosts.len(), 1);
        assert_eq!(view.gitlab.hosts[0].host, "gitlab.com");
    });
}

#[gpui::test]
fn only_a_saved_token_can_be_forgotten(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    view.update(cx, |view, cx| {
        view.gitlab.hosts = vec![status(Some(Source::Saved), Some(Ok("Akanoa")))];
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("gitlab-forget-0").is_some());
    view.update(cx, |view, cx| {
        view.gitlab.hosts = vec![status(Some(Source::Cli), Some(Ok("Akanoa")))];
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("gitlab-forget-0").is_none());
    // GITLAB_TOKEN wins over a saved token, which can still be forgotten.
    view.update(cx, |view, cx| {
        view.gitlab.hosts = vec![HostStatus {
            saved: true,
            ..status(Some(Source::Environment), Some(Ok("Akanoa")))
        }];
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("gitlab-forget-0").is_some());
}

#[gpui::test]
fn saving_without_a_token_or_with_a_bad_host_asks_nothing(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    click(cx, "gitlab-save");
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.gitlab.message.as_deref(),
            Some("Paste a personal access token first.")
        );
    });
    view.update(cx, |view, cx| {
        let host = view.gitlab.host_input().unwrap();
        host.update(cx, |input, cx| {
            input.set_text_selected("gitlab.com/group", cx)
        });
        let token = view.gitlab.token_input().unwrap();
        token.update(cx, |input, cx| input.set_text_selected("glpat-x", cx));
    });
    click(cx, "gitlab-save");
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.gitlab.message.as_deref(),
            Some("\"gitlab.com/group\" is not a GitLab host name")
        );
        // A refused save keeps what was typed, so it can be corrected.
        let token = view.gitlab.token_input().unwrap();
        assert_eq!(token.read(cx).text(), "glpat-x");
    });
}

#[gpui::test]
fn a_headless_window_saves_no_token(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    view.update(cx, |view, cx| {
        let token = view.gitlab.token_input().unwrap();
        token.update(cx, |input, cx| input.set_text_selected("glpat-x", cx));
    });
    click(cx, "gitlab-save");
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.gitlab.message.as_deref(),
            Some("This window does not keep GitLab tokens.")
        );
        // Nothing was sent or saved, so what was typed stays.
        let token = view.gitlab.token_input().unwrap();
        assert_eq!(token.read(cx).text(), "glpat-x");
    });
}

#[gpui::test]
fn a_host_only_read_over_http_takes_no_token(cx: &mut TestAppContext) {
    let (view, cx) = page(cx);
    view.update(cx, |view, cx| {
        view.gitlab.hosts = vec![HostStatus {
            host: "code.internal:8080".into(),
            plain: true,
            saved: false,
            token: Token::None,
        }];
        let host = view.gitlab.host_input().unwrap();
        host.update(cx, |input, cx| {
            input.set_text_selected("code.internal:8080", cx)
        });
        let token = view.gitlab.token_input().unwrap();
        token.update(cx, |input, cx| input.set_text_selected("glpat-x", cx));
    });
    click(cx, "gitlab-save");
    view.read_with(cx, |view, cx| {
        let message = view.gitlab.message.as_deref().unwrap();
        assert!(message.contains("use an https:// remote"), "{message}");
        let token = view.gitlab.token_input().unwrap();
        assert_eq!(token.read(cx).text(), "glpat-x");
    });
}
