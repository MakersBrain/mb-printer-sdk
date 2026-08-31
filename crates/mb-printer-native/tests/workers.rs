// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(any(feature = "ble", feature = "ipp", feature = "snmp"))]

use mb_printer_native::workers::{BoundedHardwareWorkers, HardwareOperationLimits, WorkerError};
use std::{thread, time::Duration};

#[test]
fn blocking_hardware_workers_are_bounded_and_keep_capacity_until_exit() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let workers = BoundedHardwareWorkers::new(1).unwrap();
        let slow_workers = workers.clone();
        let slow = tokio::spawn(async move {
            slow_workers
                .run(
                    HardwareOperationLimits {
                        queue_timeout: Duration::from_secs(1),
                        device_timeout: Duration::from_millis(20),
                    },
                    |_| thread::sleep(Duration::from_millis(80)),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        let queued = workers
            .run(
                HardwareOperationLimits {
                    queue_timeout: Duration::from_millis(10),
                    device_timeout: Duration::from_secs(1),
                },
                |_| (),
            )
            .await;
        assert_eq!(queued, Err(WorkerError::QueueTimeout));
        assert_eq!(slow.await.unwrap(), Err(WorkerError::OperationTimeout));
    });
}
