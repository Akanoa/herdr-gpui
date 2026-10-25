//! A source waiting for a sign-in offers it from its chip, naming the
//! provider, so GitLab opens its own page rather than GitHub's.
use super::*;
use crate::orchestrator::{
    Request, SourceStatus, SyncState, Timing,
    view::{Event, Look, OrchestratorView},
};
use gpui::{Modifiers, TestAppContext};
use std::{cell::RefCell, rc::Rc};

#[gpui::test]
fn a_gitlab_source_waiting_for_a_token_asks_for_the_gitlab_sign_in(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let request = Request {
        target: herdr_client::ConnectTarget::Local,
        checkout: dir.path().to_string_lossy().into_owned(),
        workspace_id: "w1".into(),
        token: None,
        data_root: Some(dir.path().join("data")),
        gitlab_store: Default::default(),
        timing: Timing::default(),
    };
    let config = crate::config::Config::default();
    let look = Look {
        theme: crate::config::Theme::default(),
        ui: config.ui.clone(),
        mono: config.terminal.clone(),
    };
    let (view, cx) = cx.add_window_view(|_, cx| OrchestratorView::with_repo(request, look, cx));
    cx.simulate_resize(gpui::size(gpui::px(1100.), gpui::px(600.)));
    let asked = Rc::new(RefCell::new(Vec::new()));
    let seen = asked.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &Event, _| {
            if let Event::SignIn(provider) = event {
                seen.borrow_mut().push(*provider);
            }
        })
        .detach();
        view.update(cx, |view, cx| {
            let gitlab = SourceKey {
                provider: Provider::Gitlab,
                host: "gitlab.com".into(),
                repository: "group/app".into(),
            };
            view.repo_mut().snapshot.sources = vec![SourceStatus {
                key: gitlab,
                state: SyncState::SignIn,
            }];
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("orchestrator-sign-in-github").is_none());
    let chip = cx.debug_bounds("orchestrator-sign-in-gitlab").unwrap();
    cx.simulate_click(chip.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(*asked.borrow(), [Provider::Gitlab]);
    let hosts: Vec<String> =
        cx.update(|_, cx| view.read(cx).gitlab_hosts().map(str::to_owned).collect());
    assert_eq!(hosts, ["gitlab.com"]);
}
