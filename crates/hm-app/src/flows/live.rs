//! The live monitor and everything polled while a page is visible.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::Context;
use hm_system::monitor::Monitor;
use hm_system::types::SystemSnapshot;

use crate::state::{push_history, Page, Shell};

impl Shell {
    /// Starts the thread that owns the `Monitor` and the one-second tick that decides what to poll.
    pub fn start_background_loops(&mut self, cx: &mut Context<Self>) {
        let (request_tx, request_rx) = mpsc::channel::<()>();
        let (snapshot_tx, mut snapshot_rx) = futures::channel::mpsc::unbounded::<SystemSnapshot>();
        self.monitor_tx = Some(request_tx);

        let demo = self.mode.demo;
        std::thread::spawn(move || {
            let mut monitor = (!demo).then(Monitor::new);
            let mut demo_tick = 0u64;
            while request_rx.recv().is_ok() {
                let snapshot = match monitor.as_mut() {
                    Some(monitor) => monitor.refresh(),
                    None => {
                        demo_tick += 1;
                        crate::demo::system::snapshot(demo_tick)
                    }
                };
                if snapshot_tx.unbounded_send(snapshot).is_err() {
                    break;
                }
            }
        });

        cx.spawn(async move |this, cx| {
            while let Some(snapshot) = snapshot_rx.next().await {
                let alive = this.update(cx, |shell, cx| {
                    shell.on_snapshot(snapshot);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this.update(cx, |shell, cx| shell.tick(cx)).is_err() {
                break;
            }
        })
        .detach();

        self.request_snapshot();
    }

    fn request_snapshot(&mut self) {
        if self.live.in_flight {
            return;
        }
        if let Some(tx) = &self.monitor_tx {
            if tx.send(()).is_ok() {
                self.live.in_flight = true;
            }
        }
    }

    fn on_snapshot(&mut self, snapshot: SystemSnapshot) {
        self.live.in_flight = false;
        push_history(&mut self.live.cpu, snapshot.cpu.usage_percent);
        let total = snapshot.memory.total.max(1) as f32;
        push_history(&mut self.live.memory, snapshot.memory.used as f32 / total);
        push_history(&mut self.live.rx, snapshot.network.rx_bytes_per_sec as f32);
        push_history(&mut self.live.tx, snapshot.network.tx_bytes_per_sec as f32);
        self.live.snapshot = Some(snapshot);
        self.refresh_health();
    }

    pub fn reclaimable_total(&self) -> u64 {
        self.findings
            .iter()
            .filter(|finding| crate::logic::selection::is_actionable(finding))
            .map(|finding| finding.size)
            .fold(0, u64::saturating_add)
    }

    /// Recomputes the health score, and animates the ring when it moves.
    pub fn refresh_health(&mut self) {
        let Some(snapshot) = &self.live.snapshot else {
            return;
        };
        // The same count the real score uses, so the demo's numbers agree with the rest of the page.
        let reclaimable = hm_system::health::reclaimable_bytes(&self.findings);
        let report = if self.mode.demo {
            crate::demo::system::health(reclaimable)
        } else {
            hm_system::health::compute(snapshot, reclaimable)
        };
        let score = report.score as f32;
        if self.live.health.is_none() || (score - self.live.ring_to).abs() >= 1.0 {
            self.live.ring_from = if self.live.health.is_some() {
                self.live.ring_to
            } else {
                0.0
            };
            self.live.ring_to = score;
            self.live.ring_key += 1;
        }
        self.live.health = Some(report);
    }

    /// Runs once a second. Everything polled is only polled while its page is on screen.
    fn tick(&mut self, cx: &mut Context<Self>) {
        match self.page {
            Page::Monitor | Page::Resumen => self.request_snapshot(),
            Page::Modelos => {
                let due = self
                    .models
                    .runtimes_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(5));
                if due && !self.models.runtimes_loading && self.models.inventory.data.is_some() {
                    self.poll_runtimes(cx);
                }
            }
            Page::Puertos if !self.ports.ports.loading && self.ports.ports.is_stale(3) => {
                self.load_ports(cx);
            }
            _ => {}
        }
    }

    fn poll_runtimes(&mut self, cx: &mut Context<Self>) {
        self.models.runtimes_loading = true;
        self.models.runtimes_at = Some(Instant::now());
        let ctx = self.fresh_ctx();
        let demo = self.mode.demo;
        Shell::run_bg(
            cx,
            move || {
                Ok(if demo {
                    crate::demo::ai::runtimes()
                } else {
                    hm_ai::runtime::status(&ctx)
                })
            },
            |shell, result, _cx| {
                shell.models.runtimes_loading = false;
                if let (Ok(runtimes), Some(inventory)) =
                    (result, shell.models.inventory.data.as_mut())
                {
                    // A status poll is fast because it does not measure. Keep the last measurement.
                    let measured: Vec<_> = inventory
                        .runtimes
                        .iter()
                        .map(|runtime| (runtime.kind, runtime.storage_bytes))
                        .collect();
                    inventory.runtimes = runtimes;
                    for runtime in &mut inventory.runtimes {
                        if let Some((_, bytes)) =
                            measured.iter().find(|(kind, _)| *kind == runtime.kind)
                        {
                            runtime.storage_bytes = *bytes;
                        }
                    }
                }
            },
        );
    }
}
