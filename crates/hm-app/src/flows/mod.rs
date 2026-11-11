//! What happens when the user does something: background work and the state it reports back to.

pub mod appearance;
pub mod clean;
pub mod language;
pub mod live;
pub mod load;
pub mod scan;

use std::panic::{catch_unwind, AssertUnwindSafe};

use gpui::Context;
use hm_core::t;

use crate::state::Shell;

impl Shell {
    /// Runs `work` on its own thread and hands the result back on the UI thread. A panic in the
    /// engine becomes an error the page can explain, rather than a spinner that never ends.
    pub fn run_bg<T: Send + 'static>(
        cx: &mut Context<Self>,
        work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Shell, Result<T, String>, &mut Context<Shell>) + 'static,
    ) {
        let (tx, rx) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(work));
            let result = match outcome {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(crate::logic::errors::friendly_error(&format!("{error:#}"))),
                Err(payload) => Err(payload
                    .downcast_ref::<&str>()
                    .map(|text| text.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| {
                        t!(
                            "fallo interno sin mensaje",
                            "internal failure with no message"
                        )
                        .to_string()
                    })),
            };
            let _ = tx.send(result);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.await {
                this.update(cx, |shell, cx| {
                    done(shell, result, cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn show_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.toast_serial += 1;
        let serial = self.toast_serial;
        self.toast = Some(crate::state::Toast {
            message: message.into(),
            serial,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(crate::pages::TOAST_MS))
                .await;
            this.update(cx, |shell, cx| {
                if shell
                    .toast
                    .as_ref()
                    .is_some_and(|toast| toast.serial == serial)
                {
                    shell.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}
