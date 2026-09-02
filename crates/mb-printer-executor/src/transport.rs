// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{future::Future, pin::Pin, time::Duration};

use crate::TransportError;

#[cfg(not(target_arch = "wasm32"))]
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(target_arch = "wasm32")]
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    Command,
    Raster,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSupport {
    Subscribed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    Response(Vec<u8>),
    Timeout,
    Unavailable,
}

#[cfg(not(target_arch = "wasm32"))]
pub trait Transport: Send {
    fn payload_limit(&self) -> usize;
    fn command_limit(&self) -> usize {
        self.payload_limit()
    }
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>>;
    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        kind: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>>;
    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>>;
    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()>;
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>>;
}

#[cfg(target_arch = "wasm32")]
pub trait Transport {
    fn payload_limit(&self) -> usize;
    fn command_limit(&self) -> usize {
        self.payload_limit()
    }
    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>>;
    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        kind: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>>;
    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>>;
    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()>;
    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>>;
}

#[cfg(not(target_arch = "wasm32"))]
pub trait Cancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
    fn cancelled(&self) -> TransportFuture<'_, ()>;
}

#[cfg(target_arch = "wasm32")]
pub trait Cancellation {
    fn is_cancelled(&self) -> bool;
    fn cancelled(&self) -> TransportFuture<'_, ()>;
}

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
