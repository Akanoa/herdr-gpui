//! Removing a device whose provider can delete it (see `cloud::deletion`)
//! deletes its machine there by default, with everything it bills for; Remove
//! from Herdr only, beside it, just forgets the device, and the machine keeps
//! running until it is deleted elsewhere.
use super::{HostMenu, Saved};
use crate::{HerdrWindow, cloud::Deletion};
use gpui::{prelude::*, *};

impl HostMenu {
    /// How removing this device deletes its machine, if its provider can.
    pub(super) fn deletion(&self) -> Option<Deletion> {
        match &self.saved {
            Saved::Cloud { provider, .. } => crate::cloud::deletion(*provider),
            Saved::Ssh { .. } => None,
        }
    }

    fn machine_noun(&self) -> Option<&'static str> {
        match &self.saved {
            Saved::Cloud { id, .. } => Some((self.deletion()?.noun)(id)),
            Saved::Ssh { .. } => None,
        }
    }
}

/// The default button's text: Delete for a device its provider can delete.
pub(super) fn remove_label(host: &HostMenu) -> String {
    match host.machine_noun() {
        Some(noun) => format!("Delete {noun}"),
        None => "Remove".to_owned(),
    }
}

/// What deleting the device does, or `None` when it cannot be deleted.
pub(super) fn removal_note(host: &HostMenu) -> Option<String> {
    let Saved::Cloud { provider, .. } = &host.saved else {
        return None;
    };
    let deletion = host.deletion()?;
    let noun = host.machine_noun()?;
    Some(format!(
        "Delete the {noun} {} ({}) at {}{} and remove it from this computer? Its files are \
         not kept, and this cannot be undone.",
        host.label,
        host.target,
        crate::cloud::name(*provider),
        deletion.leftovers,
    ))
}

impl HerdrWindow {
    /// Remove from Herdr only, for a device its provider can delete: the
    /// secondary answer.
    pub(super) fn remove_only_button(
        &self,
        host: &HostMenu,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        host.deletion()?;
        Some(
            div()
                .id("remove-device-only")
                .debug_selector(|| "remove-device-only".into())
                .p(px(6.))
                .cursor_pointer()
                .child("Remove from Herdr only")
                .on_click(cx.listener(|this, _, window, cx| {
                    if let Some(host) = &mut this.menu.host {
                        host.destroy = false;
                    }
                    this.confirm_remove_device(window, cx);
                })),
        )
    }
}
