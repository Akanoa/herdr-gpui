//! Drawing the account menu's header, shared by its GitHub and GitLab pages,
//! and the GitLab page itself.

use super::{HostStatus, status_line};
use crate::{HerdrWindow, menu::Page, search_input::SearchInput};
use gpui::{prelude::*, *};

/// The official GitLab mark, drawn in one color as the GitHub mark is.
pub(crate) fn gitlab_mark(color: u32) -> Svg {
    svg()
        .path("icons/gitlab.svg")
        .flex_none()
        .text_color(rgb(color))
}

impl HerdrWindow {
    /// The account menu's header: a tab for each provider, the shown one
    /// marked, and Close.
    pub(in crate::menu) fn render_account_tabs(&self, page: Page, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let font = &self.config.ui;
        let tab = |id: &'static str, label: &'static str, mark: Svg, shown: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(crate::config::corners::CONTROL))
                .cursor_pointer()
                .when(shown, |tab| tab.bg(rgb(theme.active)))
                .hover(|style| style.bg(rgb(theme.active)))
                .child(mark.size(px(20.)))
                .child(
                    div()
                        .text_size(px(font.size * 1.15))
                        .font_weight(if shown {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(rgb(if shown { theme.foreground } else { theme.muted }))
                        .child(label),
                )
        };
        let github_color = if page == Page::GitHub {
            theme.foreground
        } else {
            theme.muted
        };
        let gitlab_color = if page == Page::GitLab {
            theme.foreground
        } else {
            theme.muted
        };
        div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(4.))
            .p(px(10.))
            .border_b_1()
            .border_color(rgb(theme.active))
            .child(
                tab(
                    "accounts-tab-github",
                    "GitHub",
                    crate::sidebar::github_mark(github_color)
                        .debug_selector(|| "github-mark".into()),
                    page == Page::GitHub,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.menu.page = Some(Page::GitHub);
                    cx.notify();
                })),
            )
            .child(
                tab(
                    "accounts-tab-gitlab",
                    "GitLab",
                    gitlab_mark(gitlab_color),
                    page == Page::GitLab,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    if this.menu.page != Some(Page::GitLab) {
                        this.open_gitlab_page(window, cx);
                    }
                })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("github-header-close")
                    .debug_selector(|| "github-header-close".into())
                    .cursor_pointer()
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(crate::config::corners::CONTROL))
                    .when(
                        page == Page::GitHub
                            && self.menu.github_selected
                                == Some(crate::menu::github::Action::Close),
                        |style| style.bg(rgb(theme.active)),
                    )
                    .hover(|style| style.bg(rgb(theme.active)))
                    .child(crate::sidebar::label_text("Close"))
                    .on_click(cx.listener(|this, _, window, cx| this.dismiss_menu(window, cx))),
            )
    }

    fn gitlab_button(
        &self,
        id: impl Into<SharedString>,
        label: &'static str,
        primary: bool,
        enabled: bool,
    ) -> Stateful<Div> {
        let theme = &self.theme;
        let accent = crate::menu::accent(theme);
        let id: SharedString = id.into();
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .flex_none()
            .px(px(12.))
            .py(px(6.))
            .rounded(px(crate::config::corners::CONTROL))
            .border_1()
            .border_color(rgb(theme.active))
            .when(primary, |button| {
                button
                    .bg(accent)
                    .text_color(rgb(theme.background))
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .when(!enabled, |button| button.opacity(0.5))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.border_color(accent))
            })
            .child(crate::sidebar::label_text(label))
    }

    fn render_gitlab_host(&self, index: usize, status: &HostStatus, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let ready = !self.menu.gitlab.working;
        let host = status.host.clone();
        div()
            .debug_selector(move || format!("gitlab-host-{index}"))
            .flex()
            .items_start()
            .gap(px(10.))
            .py(px(8.))
            .child(gitlab_mark(theme.muted).mt(px(2.)).size(px(16.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(crate::sidebar::label_text(&status.host)),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.subtext()))
                            .child(status_line(status)),
                    ),
            )
            .when(status.saved, |row| {
                row.child(
                    self.gitlab_button(format!("gitlab-forget-{index}"), "Forget", false, ready)
                        .when(ready, |button| {
                            button.on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.forget_gitlab(host.clone(), cx);
                            }))
                        }),
                )
            })
    }

    pub(in crate::menu) fn render_gitlab_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let gitlab = &self.menu.gitlab;
        let ready = !gitlab.working;
        let field =
            |label: &'static str, selector: &'static str, input: &Option<Entity<SearchInput>>| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(label)
                    .children(input.as_ref().map(|input| {
                        div()
                            .debug_selector(move || selector.into())
                            .child(input.clone())
                    }))
            };
        let body = div()
            .id("gitlab-body")
            .debug_selector(|| "gitlab-body".into())
            .min_h_0()
            .overflow_y_scroll()
            .p(px(16.))
            .flex()
            .flex_col()
            .children(gitlab.problem.clone().map(|problem| {
                div()
                    .debug_selector(|| "gitlab-problem".into())
                    .mb(px(8.))
                    .text_color(rgb(theme.ink(theme.palette[1])))
                    .child(problem)
            }))
            .children(
                gitlab
                    .hosts
                    .iter()
                    .enumerate()
                    .map(|(index, status)| self.render_gitlab_host(index, status, cx)),
            )
            .child(
                div()
                    .debug_selector(|| "gitlab-token".into())
                    .mt(px(12.))
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(rgb(theme.active))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(div().text_color(rgb(theme.muted)).child(
                        "A token comes from GITLAB_TOKEN in the project's .envrc (through \
                             direnv), then in your shell, then a token saved here, then glab. \
                             A pasted token needs the read_api scope; GitLab is asked whom it \
                             signs in as before it is saved, only ever over HTTPS.",
                    ))
                    .child(field("Instance", "gitlab-host-input", &gitlab.host))
                    .child(field(
                        "Personal access token",
                        "gitlab-token-input",
                        &gitlab.token,
                    )),
            )
            .children(gitlab.message.clone().map(|message| {
                div()
                    .debug_selector(|| "gitlab-message".into())
                    .mt(px(10.))
                    .p(px(10.))
                    .rounded(px(crate::config::corners::CONTROL))
                    .bg(rgb(theme.background))
                    .child(message)
            }));
        let footer = div()
            .flex_none()
            .p(px(12.))
            .border_t_1()
            .border_color(rgb(theme.active))
            .flex()
            .justify_end()
            .child(
                self.gitlab_button("gitlab-save", "Check and save", true, ready)
                    .when(ready, |button| {
                        button.on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.save_gitlab(cx);
                        }))
                    }),
            );
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .child(self.render_account_tabs(Page::GitLab, cx))
            .child(body)
            .child(footer)
    }
}
