//! Looking for a newer HealthyMac and installing it. The engine is in [`crate::update`].

use std::sync::Arc;
use std::time::Duration;

use gpui::Context;
use hm_core::{t, tf};

use crate::state::Shell;
use crate::update::{self, Check, InstallTarget, Progress, Release};

/// The first automatic check waits for the app to settle; after that, once a day.
const FIRST_CHECK: Duration = Duration::from_secs(8);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing {
        release: Release,
        progress: Arc<Progress>,
    },
    /// `release` is set when the check worked and the installation did not, so it can be retried.
    Failed {
        message: String,
        release: Option<Release>,
    },
}

impl UpdateState {
    /// The release on offer, whatever is happening with it.
    pub fn release(&self) -> Option<&Release> {
        match self {
            UpdateState::Available(release) | UpdateState::Installing { release, .. } => {
                Some(release)
            }
            UpdateState::Failed { release, .. } => release.as_ref(),
            _ => None,
        }
    }
}

impl Shell {
    /// Checks a few seconds after launch and then once a day, while the setting is on. Never in a
    /// demo, where nothing real is shown.
    pub fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        if self.mode.demo {
            return;
        }
        cx.spawn(async move |this, cx| {
            let mut wait = FIRST_CHECK;
            loop {
                cx.background_executor().timer(wait).await;
                wait = CHECK_EVERY;
                let alive = this.update(cx, |shell, cx| {
                    if shell.ctx.config.check_updates {
                        shell.check_for_updates(false, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// `manual` is a check the user asked for: it says when there is nothing new, and reports
    /// errors. An automatic one stays quiet unless it finds something.
    pub fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.mode.demo {
            if manual {
                self.show_toast(
                    t!(
                        "En el modo demo no se buscan actualizaciones.",
                        "Demo mode doesn't look for updates."
                    ),
                    cx,
                );
            }
            return;
        }
        if matches!(
            self.settings.update,
            UpdateState::Checking | UpdateState::Installing { .. }
        ) {
            return;
        }
        let previous = std::mem::replace(&mut self.settings.update, UpdateState::Checking);
        cx.notify();
        Shell::run_bg(cx, update::check, move |shell, result, cx| {
            shell.settings.update = match result {
                Ok(Check::Available(release)) => UpdateState::Available(release),
                Ok(Check::UpToDate) => {
                    if manual {
                        shell.show_toast(
                            tf!(
                                "Tienes la última versión: {}.",
                                "You're on the latest version: {}.",
                                update::current_version()
                            ),
                            cx,
                        );
                    }
                    UpdateState::UpToDate
                }
                Err(message) if manual => UpdateState::Failed {
                    message,
                    release: None,
                },
                // A failed background check isn't worth a message: the next one may work.
                Err(_) => match previous {
                    UpdateState::Checking => UpdateState::Idle,
                    other => other,
                },
            };
        });
    }

    /// Why this copy can't install the update itself, if it can't.
    pub fn update_blocker(&self) -> Option<String> {
        let release = self.settings.update.release()?;
        if self.mode.is_simulated() {
            return Some(
                t!(
                    "En simulación no se instala nada.",
                    "Nothing gets installed in simulation mode."
                )
                .to_string(),
            );
        }
        if release.asset.is_none() {
            return Some(
                t!(
                    "Esta versión no trae un instalador para este Mac: descárgala o compílala desde su página.",
                    "This release has no installer for this Mac: download or build it from its page."
                )
                .to_string(),
            );
        }
        match update::install_target() {
            InstallTarget::Ready(_) => None,
            InstallTarget::NotBundled => Some(
                t!(
                    "Esta copia no se ejecuta desde una app instalada, así que no puede reemplazarse sola.",
                    "This copy isn't running from an installed app, so it can't replace itself."
                )
                .to_string(),
            ),
            InstallTarget::NotWritable(folder) => Some(tf!(
                "No tenemos permiso para escribir en {}: descárgala desde su página.",
                "We aren't allowed to write to {}: download it from its page.",
                folder.display()
            )),
        }
    }

    /// Downloads and checks the new version, then quits so a helper can put it in place and open it.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        let Some(release) = self.settings.update.release().cloned() else {
            return;
        };
        if matches!(self.settings.update, UpdateState::Installing { .. })
            || self.update_blocker().is_some()
        {
            return;
        }
        let InstallTarget::Ready(target) = update::install_target() else {
            return;
        };
        let progress = Arc::new(Progress::default());
        self.settings.update = UpdateState::Installing {
            release: release.clone(),
            progress: progress.clone(),
        };
        cx.notify();
        // Redraws while it runs, so the bar moves with the download.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let running = this
                .update(cx, |shell, cx| {
                    let running = matches!(shell.settings.update, UpdateState::Installing { .. });
                    if running {
                        cx.notify();
                    }
                    running
                })
                .unwrap_or(false);
            if !running {
                break;
            }
        })
        .detach();
        let staged_release = release.clone();
        Shell::run_bg(
            cx,
            move || {
                let staged = update::download_and_stage(&staged_release, &progress)?;
                update::replace_after_exit(&staged, &target)
            },
            move |shell, result, cx| match result {
                Ok(()) => cx.quit(),
                Err(message) => {
                    shell.settings.update = UpdateState::Failed {
                        message,
                        release: Some(release),
                    }
                }
            },
        );
    }
}
