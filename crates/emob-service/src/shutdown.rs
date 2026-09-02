//! Stopping without dropping what is in flight.
//!
//! # Why a CSMS cannot just exit
//!
//! A charging station holds a WebSocket for the length of a session. Killing the
//! process mid-transaction does not lose a request — it loses the
//! `StopTransaction` that carries the signed meter record, and the session is
//! then a kilowatt-hour nobody can bill and a driver who was charged nothing.
//!
//! So a daemon stops in two steps: it stops being **ready** — so the
//! orchestrator takes it out of rotation and no new station is routed to it —
//! and only then stops **serving**, after what is in flight has finished or a
//! deadline has passed.
//!
//! The deadline is the caller's, because the right one is a property of what the
//! daemon does: a CSMS holding a two-hour session cannot drain it, and a tariff
//! publisher can drain in a second.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// The signal to stop, shared by everything that has to.
#[derive(Debug, Clone, Default)]
pub struct Shutdown {
    token: CancellationToken,
}

impl Shutdown {
    /// A signal nobody has raised yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise it.
    pub fn stop(&self) {
        self.token.cancel();
    }

    /// Whether it has been raised.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Wait for it.
    ///
    /// What a background task selects on, and what the server's graceful
    /// shutdown is handed.
    pub async fn wait(&self) {
        self.token.cancelled().await;
    }

    /// A child signal that stops when this one does, and can stop on its own
    /// without stopping this one.
    ///
    /// What a subsystem that can be restarted independently holds.
    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            token: self.token.child_token(),
        }
    }

    /// Raise it when the orchestrator says so.
    ///
    /// `SIGTERM` is what Kubernetes sends and what a container runtime sends;
    /// `Ctrl-C` is what a developer sends. Both mean the same thing and both
    /// are watched, because a daemon that handles only one of them is a daemon
    /// that drains in development and is killed in production.
    ///
    /// Returns immediately on a platform without Unix signals, having installed
    /// only the `Ctrl-C` handler.
    pub async fn on_signal(&self) {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };

        #[cfg(unix)]
        {
            // A daemon that cannot install a handler still has to run: falling
            // back to `Ctrl-C` alone is better than refusing to start in the one
            // deployment that has no `SIGTERM` to send.
            let Ok(mut term) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            else {
                ctrl_c.await;
                self.stop();
                return;
            };
            tokio::select! {
                () = ctrl_c => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await;
        }

        self.stop();
    }

    /// Wait for the signal, then for a drain window, then return.
    ///
    /// The two-step stop: a caller marks itself unready before calling this, so
    /// the window is time the orchestrator has already stopped routing into.
    pub async fn drain(&self, window: Duration) {
        self.wait().await;
        tokio::time::sleep(window).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_raised_signal_is_visible_to_every_holder() {
        let shutdown = Shutdown::new();
        let copy = shutdown.clone();
        assert!(!copy.is_stopping());

        shutdown.stop();
        assert!(copy.is_stopping());
        // …and waiting on an already-raised signal returns rather than hanging,
        // which is what makes the order of `stop` and `wait` not matter.
        copy.wait().await;
    }

    #[tokio::test]
    async fn a_child_stops_with_its_parent_and_not_the_other_way_round() {
        let parent = Shutdown::new();
        let child = parent.child();

        child.stop();
        assert!(child.is_stopping());
        assert!(
            !parent.is_stopping(),
            "a subsystem stopping is not the daemon stopping"
        );

        let other = parent.child();
        parent.stop();
        assert!(other.is_stopping());
    }

    #[tokio::test]
    async fn a_drain_waits_for_the_signal_before_it_waits_for_the_window() {
        let shutdown = Shutdown::new();
        let draining = shutdown.clone();
        let handle = tokio::spawn(async move { draining.drain(Duration::from_millis(1)).await });

        // Nothing to drain until somebody asks.
        assert!(!handle.is_finished());
        shutdown.stop();
        handle.await.expect("the drain finished");
    }
}
