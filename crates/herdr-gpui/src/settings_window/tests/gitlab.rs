//! Settings > GitLab: each host's row says where its token comes from and
//! whom it signs in as, and a token is refused before any lookup when the
//! host or token typed cannot be one. The fixture window is headless, so
//! nothing here reads the keyring or runs `glab`.
use super::*;
use crate::settings_window::pages::gitlab::{HostStatus, status_line};
use herdr_orchestrator::gitlab::{self, account::Source};

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
        source,
        user: user.map(|user| user.map(str::to_owned).map_err(Arc::new)),
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
    assert!(refused.starts_with("GitLab refused the token"), "{refused}");
    assert!(refused.ends_with("The token is from the GitLab CLI (glab)."));
    assert!(
        status_line(&status(Some(Source::Environment), Some(Ok("me"))))
            .ends_with("from GITLAB_TOKEN.")
    );
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
