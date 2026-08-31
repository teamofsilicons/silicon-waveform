//! Cross-platform process shutdown coordination.

use std::future::Future;

/// Waits for `SIGINT` or `SIGTERM` and then resolves.
///
/// On platforms without Unix signals, only `SIGINT` is observed. Failure to
/// install either supported signal handler resolves that handler so the process
/// fails safe into graceful shutdown instead of running without coordination.
pub async fn signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(?error, "failed to install SIGINT handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                if signal.recv().await.is_none() {
                    tracing::warn!("SIGTERM signal stream ended unexpectedly");
                }
            }
            Err(error) => tracing::error!(?error, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    let source = wait_for_signal(interrupt, terminate).await;
    tracing::info!(signal = source.as_str(), "shutdown signal received");
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SignalSource {
    Interrupt,
    Terminate,
}

impl SignalSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
        }
    }
}

async fn wait_for_signal<I, T>(interrupt: I, terminate: T) -> SignalSource
where
    I: Future<Output = ()>,
    T: Future<Output = ()>,
{
    tokio::pin!(interrupt);
    tokio::pin!(terminate);

    tokio::select! {
        () = &mut interrupt => SignalSource::Interrupt,
        () = &mut terminate => SignalSource::Terminate,
    }
}

#[cfg(test)]
mod tests {
    use std::future::{pending, ready};

    use super::{SignalSource, wait_for_signal};

    #[tokio::test]
    async fn interrupt_resolves_shutdown() {
        let source = wait_for_signal(ready(()), pending()).await;

        assert_eq!(source, SignalSource::Interrupt);
    }

    #[tokio::test]
    async fn terminate_resolves_shutdown() {
        let source = wait_for_signal(pending(), ready(())).await;

        assert_eq!(source, SignalSource::Terminate);
    }
}
