// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native transports, discovery, blocking integration, and durable replay for
//! the Makers' Brain printer SDK.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "blocking")]
pub mod blocking;
pub mod brother_device_settings;
pub mod brother_settings;
pub mod transports;
#[cfg(any(feature = "ble", feature = "ipp", feature = "snmp"))]
pub mod workers;

pub use mb_printer_executor::{
    Cancellation, CancellationToken, ExecuteError, ExecutionOptions, MemoryReplayStore,
    NeverCancelled, NotificationSupport, Progress, ReferenceTiming, ReplayGuard, ReplayStore,
    Transport, TransportError, TransportErrorKind, TransportFuture, WaitOutcome, WriteKind,
    execute, execute_once_with_store, execute_with_options,
};

use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

/// Durable, cross-process replay claims remain a native-only concern.
#[derive(Debug, Clone)]
pub struct FileReplayStore {
    directory: PathBuf,
}

impl FileReplayStore {
    pub fn new(directory: impl AsRef<Path>) -> std::io::Result<Self> {
        std::fs::create_dir_all(directory.as_ref())?;
        Ok(Self {
            directory: directory.as_ref().to_owned(),
        })
    }
}

impl ReplayStore for FileReplayStore {
    type Error = std::io::Error;

    fn claim(&mut self, key: &str) -> Result<bool, Self::Error> {
        let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.directory.join(digest))
        {
            Ok(mut file) => {
                file.write_all(key.as_bytes())
                    .and_then(|()| file.sync_all())?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    }
}
