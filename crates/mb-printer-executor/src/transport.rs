// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{future::Future, pin::Pin, time::Duration};

use crate::TransportError;

#[cfg(not(target_arch = "wasm32"))]
/// Boxed transport operation future; native operations must be [`Send`].
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(target_arch = "wasm32")]
/// Boxed transport operation future for the single-threaded WebAssembly target.
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Semantic class of bytes passed to [`Transport::write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    /// Atomic or chunkable printer command bytes.
    Command,
    /// Chunkable raster payload bytes.
    Raster,
}

/// Result of requesting transport notification subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSupport {
    /// Notifications are subscribed and may produce responses.
    Subscribed,
    /// This transport cannot receive notifications.
    Unavailable,
}

/// Result of one bounded response wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// A response frame was received.
    Response(Vec<u8>),
    /// The wait elapsed without a response.
    Timeout,
    /// Responses are unavailable or the response stream ended normally.
    Unavailable,
}

#[cfg(not(target_arch = "wasm32"))]
/// Asynchronous printer I/O boundary implemented by native transports.
pub trait Transport: Send {
    /// Maximum payload bytes accepted by one write.
    fn payload_limit(&self) -> usize;
    /// Maximum bytes accepted by one atomic command write.
    fn command_limit(&self) -> usize {
        self.payload_limit()
    }
    /// Subscribes to response notifications when the transport supports them.
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>>;
    /// Writes bytes once without retrying an ambiguous failure.
    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        kind: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>>;
    /// Waits for at most `timeout` for one response frame.
    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>>;
    /// Delays execution using the host runtime.
    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()>;
    /// Closes the transport and releases its resources.
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>>;
}

#[cfg(target_arch = "wasm32")]
/// Asynchronous printer I/O boundary implemented by WebAssembly host adapters.
pub trait Transport {
    /// Maximum payload bytes accepted by one write.
    fn payload_limit(&self) -> usize;
    /// Maximum bytes accepted by one atomic command write.
    fn command_limit(&self) -> usize {
        self.payload_limit()
    }
    /// Subscribes to response notifications when the transport supports them.
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>>;
    /// Writes bytes once without retrying an ambiguous failure.
    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        kind: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>>;
    /// Waits for at most `timeout` for one response frame.
    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>>;
    /// Delays execution using the host event loop.
    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()>;
    /// Closes the transport and releases its resources.
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>>;
}

#[cfg(not(target_arch = "wasm32"))]
/// Cooperative cancellation source observed at executor effect boundaries.
pub trait Cancellation: Send + Sync {
    /// Returns whether cancellation has already been requested.
    fn is_cancelled(&self) -> bool;
    /// Resolves when cancellation is requested.
    fn cancelled(&self) -> TransportFuture<'_, ()>;
}

#[cfg(target_arch = "wasm32")]
/// Cooperative cancellation source observed at executor effect boundaries.
pub trait Cancellation {
    /// Returns whether cancellation has already been requested.
    fn is_cancelled(&self) -> bool;
    /// Resolves when cancellation is requested.
    fn cancelled(&self) -> TransportFuture<'_, ()>;
}

/// Cancellation source that never resolves, used by default execution.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeverCancelled;

impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancelled(&self) -> TransportFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native_token {
    use super::{Cancellation, TransportFuture};
    use futures_util::task::AtomicWaker;
    use std::{
        future::poll_fn,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::Poll,
    };

    /// Cloneable, race-free cancellation token for native execution.
    #[derive(Debug, Clone, Default)]
    pub struct CancellationToken {
        inner: Arc<Inner>,
    }

    #[derive(Debug, Default)]
    struct Inner {
        cancelled: AtomicBool,
        waker: AtomicWaker,
    }

    impl CancellationToken {
        /// Requests cancellation and wakes a currently registered executor.
        pub fn cancel(&self) {
            self.inner.cancelled.store(true, Ordering::Release);
            self.inner.waker.wake();
        }
    }

    impl Cancellation for CancellationToken {
        fn is_cancelled(&self) -> bool {
            self.inner.cancelled.load(Ordering::Acquire)
        }

        fn cancelled(&self) -> TransportFuture<'_, ()> {
            Box::pin(poll_fn(|cx| {
                if self.is_cancelled() {
                    return Poll::Ready(());
                }
                self.inner.waker.register(cx.waker());
                if self.is_cancelled() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }))
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native_token::CancellationToken;
