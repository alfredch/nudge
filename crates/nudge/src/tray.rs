//! Tray icon (StatusNotifierItem). GNOME needs the AppIndicator extension;
//! without a tray host nudge runs on without it.

use std::time::Duration;

use ksni::menu::{StandardItem, SubMenu};
use ksni::{Category, MenuItem, Status as ItemStatus, ToolTip, TrayMethods};
use nudge_api::Message;
use tracing::{debug, info};

use crate::control::{Command, Control, describe, local_time};
use crate::status::{Status, StatusRx};

const PAUSES: [(&str, Option<Duration>); 4] = [
    ("15 minutes", Some(Duration::from_secs(15 * 60))),
    ("1 hour", Some(Duration::from_secs(3600))),
    ("8 hours", Some(Duration::from_secs(8 * 3600))),
    ("Until resumed", None),
];

/// Icon names per state: nudge's own (installed by the packages under
/// `hicolor/symbolic/apps`) and Adwaita fallbacks for uninstalled builds.
struct Icons {
    connected: &'static str,
    disconnected: &'static str,
    paused: &'static str,
    alert: &'static str,
}

const OWN_ICONS: Icons = Icons {
    connected: "io.github.alfredch.Nudge-symbolic",
    disconnected: "io.github.alfredch.Nudge-offline-symbolic",
    paused: "io.github.alfredch.Nudge-paused-symbolic",
    alert: "io.github.alfredch.Nudge-alert-symbolic",
};

const FALLBACK_ICONS: Icons = Icons {
    connected: "preferences-system-notifications-symbolic",
    disconnected: "network-offline-symbolic",
    paused: "notifications-disabled-symbolic",
    alert: "dialog-warning-symbolic",
};

/// Whether nudge's icons are installed in one of the XDG data dirs.
fn own_icons_installed() -> bool {
    let home = directories::BaseDirs::new().map(|d| d.data_dir().to_path_buf());
    let system = std::env::var_os("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(std::env::split_paths(&system))
        .any(|dir| {
            dir.join("icons/hicolor/symbolic/apps")
                .join(format!("{}.svg", OWN_ICONS.connected))
                .is_file()
        })
}

/// Starts the tray and keeps it in sync with the status.
pub async fn spawn(control: Control, mut status: StatusRx) {
    let icons = if own_icons_installed() {
        &OWN_ICONS
    } else {
        debug!("nudge icons not installed, using theme icons");
        &FALLBACK_ICONS
    };
    let tray = Tray {
        status: status.borrow().clone(),
        control,
        icons,
    };
    let handle = match tray.spawn().await {
        Ok(handle) => handle,
        Err(e) => {
            info!("no system tray available, running without tray icon: {e}");
            return;
        }
    };
    info!("tray icon active");
    tokio::spawn(async move {
        while status.changed().await.is_ok() {
            let new = status.borrow_and_update().clone();
            if handle.update(|t| t.status = new).await.is_none() {
                debug!("tray service stopped");
                return;
            }
        }
    });
}

struct Tray {
    status: Status,
    control: Control,
    icons: &'static Icons,
}

impl Tray {
    fn state(&self) -> String {
        if self.status.paused() {
            format!("Paused {}", describe(self.status.pause))
        } else if self.status.connected_since.is_some() {
            "Connected".to_owned()
        } else {
            "Disconnected, reconnecting".to_owned()
        }
    }

    fn send(command: impl Fn() -> Command + Send + 'static) -> Box<dyn Fn(&mut Self) + Send> {
        Box::new(move |tray: &mut Self| tray.control.send(command()))
    }

    fn message_item(m: &Message, label: String) -> MenuItem<Self> {
        let id = m.id;
        StandardItem {
            label: mnemonic_safe(&label),
            activate: Self::send(move || Command::Show(id)),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for Tray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "nudge".to_owned()
    }

    fn title(&self) -> String {
        "Nudge".to_owned()
    }

    fn category(&self) -> Category {
        Category::Communications
    }

    fn status(&self) -> ItemStatus {
        if self.status.alerts.is_empty() {
            ItemStatus::Active
        } else {
            ItemStatus::NeedsAttention
        }
    }

    fn icon_name(&self) -> String {
        if self.status.paused() {
            self.icons.paused
        } else if self.status.connected_since.is_some() {
            self.icons.connected
        } else {
            self.icons.disconnected
        }
        .to_owned()
    }

    fn attention_icon_name(&self) -> String {
        self.icons.alert.to_owned()
    }

    fn tool_tip(&self) -> ToolTip {
        let mut description = self.state();
        if !self.status.alerts.is_empty() {
            description += &format!(
                "\n{} unacknowledged emergency message(s)",
                self.status.alerts.len()
            );
        }
        description += "\nUnofficial Pushover Open Client";
        ToolTip {
            title: "Nudge".to_owned(),
            description,
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut menu = vec![
            StandardItem {
                label: mnemonic_safe(&self.state()),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ];

        for m in &self.status.alerts {
            menu.push(Self::message_item(
                m,
                format!("⚠ {}: {} (not acknowledged)", m.app, m.display_title()),
            ));
        }
        menu.push(MenuItem::Separator);

        let recent: Vec<MenuItem<Self>> = self
            .status
            .recent
            .iter()
            .map(|m| {
                Self::message_item(
                    m,
                    format!("{}  {}: {}", local_time(m.date), m.app, m.display_title()),
                )
            })
            .collect();
        menu.push(
            SubMenu {
                label: "Recent messages".to_owned(),
                enabled: !recent.is_empty(),
                submenu: recent,
                ..Default::default()
            }
            .into(),
        );
        menu.push(
            StandardItem {
                label: "Open history".to_owned(),
                activate: Self::send(|| Command::OpenHistory),
                ..Default::default()
            }
            .into(),
        );
        menu.push(MenuItem::Separator);

        if self.status.paused() {
            menu.push(
                StandardItem {
                    label: "Resume notifications".to_owned(),
                    activate: Self::send(|| Command::Resume),
                    ..Default::default()
                }
                .into(),
            );
        } else {
            menu.push(
                SubMenu {
                    label: "Pause notifications".to_owned(),
                    submenu: PAUSES
                        .iter()
                        .map(|&(label, duration)| {
                            StandardItem {
                                label: label.to_owned(),
                                activate: Self::send(move || Command::Pause(duration)),
                                ..Default::default()
                            }
                            .into()
                        })
                        .collect(),
                    ..Default::default()
                }
                .into(),
            );
        }
        menu.push(MenuItem::Separator);
        menu.push(
            StandardItem {
                label: "Quit Nudge".to_owned(),
                activate: Self::send(|| Command::Quit),
                ..Default::default()
            }
            .into(),
        );
        menu
    }
}

/// Menu labels treat `_` as an access-key marker; keep it literal.
fn mnemonic_safe(label: &str) -> String {
    label.replace('_', "__")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underscores_are_literal() {
        assert_eq!(mnemonic_safe("disk_full"), "disk__full");
    }
}
