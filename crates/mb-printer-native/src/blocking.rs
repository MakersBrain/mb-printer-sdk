// SPDX-License-Identifier: AGPL-3.0-or-later
//! Optional synchronous facade backed by one dedicated runtime-owning thread.

#[cfg(feature = "ble")]
use crate::transports::ble::{BtleplugConnectOptions, BtleplugTransport};
use crate::{
    ExecuteError, NeverCancelled, Progress, ReferenceTiming, Transport, TransportError,
    TransportErrorKind, execute_with_options,
};
#[cfg(feature = "ble")]
use mb_printer_core::capabilities::BleGattCapabilities;
use mb_printer_core::protocol::Plan;
use mb_printer_executor::ExecutionOptions;
use std::{
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    thread::JoinHandle,
    time::Duration,
};

enum Command {
    Execute(Plan, SyncSender<ExecutionEvent>),
    Shutdown(SyncSender<Result<(), TransportError>>),
}

enum ExecutionEvent {
    Progress(Progress),
    Finished(Result<Progress, ExecuteError>),
}

pub struct BlockingPrinterClient {
    commands: SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl BlockingPrinterClient {
    #[cfg(feature = "ble")]
    pub fn connect_btleplug(
        address: String,
        capabilities: BleGattCapabilities,
        options: BtleplugConnectOptions,
    ) -> Result<Self, TransportError> {
        let (commands, receiver) = sync_channel(8);
        let (ready_tx, ready_rx) = sync_channel(1);
        let worker = std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => {
                    let _ = ready_tx.send(Err(TransportError::new(
                        TransportErrorKind::Io,
                        "blocking worker runtime initialization failed",
                    )));
                    return;
                }
            };
            match runtime.block_on(BtleplugTransport::connect(&address, &capabilities, options)) {
                Ok(transport) => {
                    let _ = ready_tx.send(Ok(()));
                    run_worker(&runtime, Box::new(transport), receiver);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            }
        });
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(TransportError::new(
                    TransportErrorKind::Disconnected,
                    "blocking worker terminated during connection",
                ))
            }
        }
    }

    /// Construct the facade around an already connected async transport.
    /// Useful for non-BLE transports and deterministic contract tests.
    pub fn from_transport<T: Transport + 'static>(transport: T) -> Result<Self, TransportError> {
        let (commands, receiver) = sync_channel(8);
        let (ready_tx, ready_rx) = sync_channel(1);
        let worker = std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => {
                    let _ = ready_tx.send(Err(TransportError::new(
                        TransportErrorKind::Io,
                        "blocking worker runtime initialization failed",
                    )));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            run_worker(&runtime, Box::new(transport), receiver);
        });
        ready_rx.recv().map_err(|_| {
            TransportError::new(
                TransportErrorKind::Disconnected,
                "blocking worker terminated during initialization",
            )
        })??;
        Ok(Self {
            commands,
            worker: Some(worker),
        })
    }

    pub fn execute(&mut self, plan: Plan) -> Result<Progress, ExecuteError> {
        self.execute_with_progress(plan, |_| {})
    }

    pub fn execute_with_progress<F>(
        &mut self,
        plan: Plan,
        mut progress: F,
    ) -> Result<Progress, ExecuteError>
    where
        F: FnMut(&Progress),
    {
        let (events, receiver) = sync_channel(8);
        self.commands
            .send(Command::Execute(plan, events))
            .map_err(|_| worker_execute_error())?;
        loop {
            match receiver.recv() {
                Ok(ExecutionEvent::Progress(update)) => progress(&update),
                Ok(ExecutionEvent::Finished(result)) => return result,
                Err(_) => return Err(worker_execute_error()),
            }
        }
    }

    pub fn disconnect(mut self) -> Result<(), TransportError> {
        let result = self.request_shutdown(None);
        if result.is_ok()
            && let Some(worker) = self.worker.take()
        {
            worker.join().map_err(|_| {
                TransportError::new(TransportErrorKind::Io, "blocking worker panicked")
            })?;
        }
        result
    }

    fn request_shutdown(&mut self, timeout: Option<Duration>) -> Result<(), TransportError> {
        if self.worker.is_none() {
            return Ok(());
        }
        let (reply, receiver) = sync_channel(1);
        self.commands.send(Command::Shutdown(reply)).map_err(|_| {
            TransportError::new(TransportErrorKind::Disconnected, "blocking worker stopped")
        })?;
        match timeout {
            Some(timeout) => receiver.recv_timeout(timeout).map_err(|_| {
                TransportError::new(TransportErrorKind::Timeout, "blocking shutdown timed out")
            })?,
            None => receiver.recv().map_err(|_| {
                TransportError::new(TransportErrorKind::Disconnected, "blocking worker stopped")
            })?,
        }
    }
}

impl Drop for BlockingPrinterClient {
    fn drop(&mut self) {
        if self.worker.is_none() {
            return;
        }
        if self.request_shutdown(Some(Duration::from_secs(1))).is_ok()
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        } else {
            self.worker.take();
        }
    }
}

fn run_worker(
    runtime: &tokio::runtime::Runtime,
    mut transport: Box<dyn Transport>,
    receiver: Receiver<Command>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Execute(plan, events) => {
                let callback_events = events.clone();
                let result = runtime.block_on(execute_with_options(
                    &plan,
                    transport.as_mut(),
                    ExecutionOptions {
                        timing: ReferenceTiming::Preserve,
                        cancellation: &NeverCancelled,
                    },
                    move |progress| {
                        let _ = callback_events.send(ExecutionEvent::Progress(progress.clone()));
                    },
                ));
                let _ = events.send(ExecutionEvent::Finished(result));
            }
            Command::Shutdown(reply) => {
                let result = runtime.block_on(transport.disconnect());
                let _ = reply.send(result);
                break;
            }
        }
    }
}

fn worker_execute_error() -> ExecuteError {
    ExecuteError::Transport {
        progress: Progress::default(),
        source: TransportError::new(TransportErrorKind::Disconnected, "blocking worker stopped"),
    }
}
