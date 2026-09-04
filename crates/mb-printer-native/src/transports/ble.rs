use super::*;
use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use futures_util::StreamExt as _;
use mb_printer_core::capabilities::{
    BleFlowControl, BleGattCapabilities, BleWriteType, NotificationRequirement,
};
use std::{collections::VecDeque, num::NonZeroUsize};
use tracing::Instrument as _;

const NOTIFICATION_QUEUE_CAPACITY: usize = 32;

#[derive(Debug, Default)]
struct CreditFlowState {
    credits: usize,
    maximum_payload: Option<usize>,
    pending_responses: VecDeque<Vec<u8>>,
}

impl CreditFlowState {
    fn observe(&mut self, bytes: &[u8]) -> bool {
        match bytes {
            [0x01, credits] => {
                self.credits = self.credits.saturating_add(usize::from(*credits));
                true
            }
            [0x02, low, high] => {
                let limit = usize::from(u16::from_le_bytes([*low, *high]));
                if limit != 0 {
                    self.maximum_payload = Some(limit);
                }
                true
            }
            _ => false,
        }
    }
}

async fn receive_for_credit(
    flow: &mut CreditFlowState,
    notifications: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
) -> Result<(), TransportError> {
    while flow.credits == 0 {
        let bytes = notifications.recv().await.ok_or_else(|| {
            ble_error(
                TransportErrorKind::Disconnected,
                "BLE notification stream ended",
            )
        })?;
        if !flow.observe(&bytes) {
            if flow.pending_responses.len() == NOTIFICATION_QUEUE_CAPACITY {
                return Err(ble_error(
                    TransportErrorKind::Io,
                    "BLE response queue is full while waiting for write credit",
                ));
            }
            flow.pending_responses.push_back(bytes);
        }
    }
    flow.credits -= 1;
    Ok(())
}

async fn receive_flow_response(
    flow: &mut CreditFlowState,
    notifications: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    timeout: Duration,
) -> Result<WaitOutcome, TransportError> {
    if let Some(bytes) = flow.pending_responses.pop_front() {
        return Ok(WaitOutcome::Response(bytes));
    }
    let response = async {
        loop {
            let bytes = notifications.recv().await.ok_or_else(|| {
                ble_error(
                    TransportErrorKind::Disconnected,
                    "BLE notification stream ended",
                )
            })?;
            if !flow.observe(&bytes) {
                return Ok(WaitOutcome::Response(bytes));
            }
        }
    };
    match tokio::time::timeout(timeout, response).await {
        Ok(outcome) => outcome,
        Err(_) => Ok(WaitOutcome::Timeout),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BtleplugConnectOptions {
    pub scan_timeout: Duration,
    pub payload_limit: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotificationState {
    Unsupported,
    Available,
    Subscribed,
    Disconnected,
}

fn ble_error(kind: TransportErrorKind, message: &'static str) -> TransportError {
    TransportError::new(kind, message)
}

fn validate_write_state(
    state: NotificationState,
    length: usize,
    limit: NonZeroUsize,
) -> Result<(), TransportError> {
    if state == NotificationState::Disconnected {
        return Err(ble_error(
            TransportErrorKind::Disconnected,
            "BLE transport is disconnected",
        ));
    }
    if length > limit.get() {
        return Err(ble_error(
            TransportErrorKind::InvalidConfiguration,
            "BLE write exceeds the declared payload limit",
        ));
    }
    Ok(())
}

async fn wait_notification_state(
    state: &mut NotificationState,
    notifications: &mut Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    timeout: Duration,
) -> Result<WaitOutcome, TransportError> {
    match *state {
        NotificationState::Unsupported | NotificationState::Available => {
            return Ok(WaitOutcome::Unavailable);
        }
        NotificationState::Disconnected => {
            return Err(ble_error(
                TransportErrorKind::Disconnected,
                "BLE transport is disconnected",
            ));
        }
        NotificationState::Subscribed => {}
    }
    let receiver = notifications.as_mut().ok_or_else(|| {
        ble_error(
            TransportErrorKind::InvalidConfiguration,
            "BLE notification state is inconsistent",
        )
    })?;
    match tokio::time::timeout(timeout, receiver.recv()).await {
        Err(_) => Ok(WaitOutcome::Timeout),
        Ok(Some(bytes)) => Ok(WaitOutcome::Response(bytes)),
        Ok(None) => {
            *state = NotificationState::Disconnected;
            Err(ble_error(
                TransportErrorKind::Disconnected,
                "BLE notification stream ended",
            ))
        }
    }
}

fn select_characteristics(
    characteristics: &[Characteristic],
    capabilities: &BleGattCapabilities,
) -> Result<(Characteristic, Option<Characteristic>), TransportError> {
    if capabilities.write_type != BleWriteType::WithoutResponse {
        return Err(ble_error(
            TransportErrorKind::InvalidConfiguration,
            "BLE profile has an unsupported write type",
        ));
    }

    let write = characteristics
        .iter()
        .find(|item| {
            item.uuid == capabilities.write_characteristic
                && item
                    .properties
                    .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
        })
        .cloned();
    let write = match write {
        Some(write) => write,
        None if characteristics
            .iter()
            .any(|item| item.uuid == capabilities.write_characteristic) =>
        {
            return Err(ble_error(
                TransportErrorKind::InvalidConfiguration,
                "BLE write characteristic does not support write without response",
            ));
        }
        None => {
            return Err(ble_error(
                TransportErrorKind::Connection,
                "BLE write characteristic was not found",
            ));
        }
    };

    let notification = match &capabilities.notification {
        None => None,
        Some(profile) => match characteristics
            .iter()
            .find(|item| item.uuid == profile.characteristic)
            .cloned()
        {
            Some(characteristic) if characteristic.properties.contains(CharPropFlags::NOTIFY) => {
                Some(characteristic)
            }
            Some(_) => {
                return Err(ble_error(
                    TransportErrorKind::InvalidConfiguration,
                    "BLE notification characteristic does not support notifications",
                ));
            }
            None if profile.requirement == NotificationRequirement::Optional => None,
            None => {
                return Err(ble_error(
                    TransportErrorKind::Connection,
                    "required BLE notification characteristic was not found",
                ));
            }
        },
    };

    if capabilities.flow_control.is_some() && notification.is_none() {
        return Err(ble_error(
            TransportErrorKind::Connection,
            "BLE flow control requires a notification characteristic",
        ));
    }

    Ok((write, notification))
}

/// Async BLE discovery for applications that own the Tokio runtime.
pub async fn discover_btleplug_async(
    scan_timeout: Duration,
) -> Result<Vec<DiscoveredPrinter>, TransportError> {
    let manager = btleplug::platform::Manager::new().await.map_err(|_| {
        ble_error(
            TransportErrorKind::Connection,
            "could not initialize the BLE manager",
        )
    })?;
    let adapters = manager.adapters().await.map_err(|_| {
        ble_error(
            TransportErrorKind::Connection,
            "could not enumerate BLE adapters",
        )
    })?;
    let mut found = Vec::new();
    for adapter in adapters {
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|_| ble_error(TransportErrorKind::Connection, "BLE scan could not start"))?;
        tokio::time::sleep(scan_timeout).await;
        let peripherals = adapter.peripherals().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not enumerate BLE peripherals",
            )
        })?;
        for peripheral in peripherals {
            let properties = peripheral.properties().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not read BLE peripheral properties",
                )
            })?;
            let address = peripheral.address().to_string();
            found.push(DiscoveredPrinter {
                transport: "ble",
                id: address.clone(),
                name: properties.and_then(|value| value.local_name),
                endpoint: address,
            });
        }
    }
    found.sort_by(|left, right| left.endpoint.cmp(&right.endpoint));
    found.dedup_by(|left, right| left.endpoint == right.endpoint);
    Ok(found)
}

/// A Tokio-native GATT transport configured exclusively from model capabilities.
pub struct BtleplugTransport {
    peripheral: btleplug::platform::Peripheral,
    write: Characteristic,
    notify: Option<Characteristic>,
    notification_state: NotificationState,
    notifications: Option<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    forwarding_task: Option<tokio::task::JoinHandle<()>>,
    payload_limit: NonZeroUsize,
    credit_flow: Option<CreditFlowState>,
}

impl BtleplugTransport {
    pub async fn connect(
        address: &str,
        capabilities: &BleGattCapabilities,
        options: BtleplugConnectOptions,
    ) -> Result<Self, TransportError> {
        let manager = btleplug::platform::Manager::new().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not initialize the BLE manager",
            )
        })?;
        let adapters = manager.adapters().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not enumerate BLE adapters",
            )
        })?;
        let mut selected = None;
        for adapter in adapters {
            adapter
                .start_scan(ScanFilter::default())
                .await
                .map_err(|_| {
                    ble_error(TransportErrorKind::Connection, "BLE scan could not start")
                })?;
            tokio::time::sleep(options.scan_timeout).await;
            let peripherals = adapter.peripherals().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not enumerate BLE peripherals",
                )
            })?;
            for peripheral in peripherals {
                if peripheral
                    .address()
                    .to_string()
                    .eq_ignore_ascii_case(address)
                {
                    selected = Some(peripheral);
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }

        let peripheral = selected.ok_or_else(|| {
            ble_error(
                TransportErrorKind::Connection,
                "requested BLE peripheral was not found",
            )
        })?;
        let connected = peripheral.is_connected().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not inspect BLE connection state",
            )
        })?;
        if !connected {
            peripheral.connect().await.map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not connect to BLE peripheral",
                )
            })?;
        }
        peripheral.discover_services().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not discover BLE services",
            )
        })?;

        let characteristics: Vec<_> = peripheral.characteristics().into_iter().collect();
        let (write, notify) = match select_characteristics(&characteristics, capabilities) {
            Ok(selected) => selected,
            Err(error) => {
                let _ = peripheral.disconnect().await;
                return Err(error);
            }
        };
        let notification_state = if notify.is_some() {
            NotificationState::Available
        } else {
            NotificationState::Unsupported
        };

        Ok(Self {
            peripheral,
            write,
            notify,
            notification_state,
            notifications: None,
            forwarding_task: None,
            payload_limit: options.payload_limit,
            credit_flow: (capabilities.flow_control == Some(BleFlowControl::PhomemoCredit))
                .then(CreditFlowState::default),
        })
    }

    async fn subscribe_inner(&mut self) -> Result<NotificationSupport, TransportError> {
        match self.notification_state {
            NotificationState::Unsupported => return Ok(NotificationSupport::Unavailable),
            NotificationState::Subscribed => return Ok(NotificationSupport::Subscribed),
            NotificationState::Disconnected => {
                return Err(ble_error(
                    TransportErrorKind::Disconnected,
                    "BLE transport is disconnected",
                ));
            }
            NotificationState::Available => {}
        }

        let characteristic = self.notify.as_ref().cloned().ok_or_else(|| {
            ble_error(
                TransportErrorKind::InvalidConfiguration,
                "BLE notification characteristic is missing",
            )
        })?;
        let mut stream = self.peripheral.notifications().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not open BLE notification stream",
            )
        })?;
        self.peripheral
            .subscribe(&characteristic)
            .await
            .map_err(|_| {
                ble_error(
                    TransportErrorKind::Connection,
                    "could not subscribe to BLE notifications",
                )
            })?;

        let expected_uuid = characteristic.uuid;
        let (sender, receiver) = tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let task = tokio::spawn(
            async move {
                while let Some(notification) = stream.next().await {
                    if notification.uuid == expected_uuid
                        && sender.send(notification.value).await.is_err()
                    {
                        break;
                    }
                }
            }
            .instrument(tracing::Span::current()),
        );
        self.notifications = Some(receiver);
        self.forwarding_task = Some(task);
        self.notification_state = NotificationState::Subscribed;
        Ok(NotificationSupport::Subscribed)
    }

    async fn write_inner(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        validate_write_state(self.notification_state, bytes.len(), self.payload_limit)?;
        if let Some(flow) = &mut self.credit_flow {
            if self.notification_state != NotificationState::Subscribed {
                return Err(ble_error(
                    TransportErrorKind::InvalidConfiguration,
                    "BLE credit flow requires notification subscription before writing",
                ));
            }
            let notifications = self.notifications.as_mut().ok_or_else(|| {
                ble_error(
                    TransportErrorKind::InvalidConfiguration,
                    "BLE credit notification receiver is missing",
                )
            })?;
            if let Err(error) = receive_for_credit(flow, notifications).await {
                self.notification_state = NotificationState::Disconnected;
                return Err(error);
            }
            if flow
                .maximum_payload
                .is_some_and(|maximum| bytes.len() > maximum)
            {
                return Err(ble_error(
                    TransportErrorKind::InvalidConfiguration,
                    "BLE write exceeds the printer-advertised flow limit",
                ));
            }
        }
        self.peripheral
            .write(&self.write, bytes, WriteType::WithoutResponse)
            .await
            .map_err(|_| ble_error(TransportErrorKind::Io, "BLE write failed"))
    }

    async fn wait_inner(&mut self, timeout: Duration) -> Result<WaitOutcome, TransportError> {
        if let Some(flow) = &mut self.credit_flow {
            if self.notification_state != NotificationState::Subscribed {
                return Ok(WaitOutcome::Unavailable);
            }
            let notifications = self.notifications.as_mut().ok_or_else(|| {
                ble_error(
                    TransportErrorKind::InvalidConfiguration,
                    "BLE credit notification receiver is missing",
                )
            })?;
            let outcome = receive_flow_response(flow, notifications, timeout).await;
            if outcome
                .as_ref()
                .is_err_and(|error| error.kind == TransportErrorKind::Disconnected)
            {
                self.notification_state = NotificationState::Disconnected;
            }
            return outcome;
        }
        wait_notification_state(
            &mut self.notification_state,
            &mut self.notifications,
            timeout,
        )
        .await
    }

    async fn disconnect_inner(&mut self) -> Result<(), TransportError> {
        if self.notification_state == NotificationState::Disconnected {
            return Ok(());
        }
        self.notification_state = NotificationState::Disconnected;
        if let Some(task) = self.forwarding_task.take() {
            task.abort();
        }
        self.notifications.take();
        self.peripheral.disconnect().await.map_err(|_| {
            ble_error(
                TransportErrorKind::Connection,
                "could not disconnect BLE peripheral",
            )
        })
    }
}

impl Transport for BtleplugTransport {
    fn payload_limit(&self) -> usize {
        self.credit_flow
            .as_ref()
            .and_then(|flow| flow.maximum_payload)
            .map_or(self.payload_limit.get(), |maximum| {
                maximum.min(self.payload_limit.get())
            })
    }

    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        Box::pin(self.subscribe_inner())
    }

    fn write<'a>(
        &'a mut self,
        bytes: &'a [u8],
        _kind: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        Box::pin(self.write_inner(bytes))
    }

    fn wait_response(
        &mut self,
        timeout: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        Box::pin(self.wait_inner(timeout))
    }

    fn delay(&mut self, duration: Duration) -> TransportFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }

    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(self.disconnect_inner())
    }
}

impl Drop for BtleplugTransport {
    fn drop(&mut self) {
        self.notification_state = NotificationState::Disconnected;
        if let Some(task) = self.forwarding_task.take() {
            task.abort();
        }
        self.notifications.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mb_printer_core::capabilities::{BleNotification, BleWriteType};
    use std::collections::BTreeSet;
    use uuid::Uuid;

    const FF02: &str = "0000ff02-0000-1000-8000-00805f9b34fb";
    const FF03: &str = "0000ff03-0000-1000-8000-00805f9b34fb";

    fn characteristic(uuid: &str, properties: CharPropFlags) -> Characteristic {
        Characteristic {
            uuid: Uuid::parse_str(uuid).unwrap(),
            service_uuid: Uuid::nil(),
            properties,
            descriptors: BTreeSet::new(),
        }
    }

    fn profile(requirement: NotificationRequirement) -> BleGattCapabilities {
        BleGattCapabilities {
            write_characteristic: Uuid::parse_str(FF02).unwrap(),
            write_type: BleWriteType::WithoutResponse,
            notification: Some(BleNotification {
                characteristic: Uuid::parse_str(FF03).unwrap(),
                requirement,
            }),
            flow_control: None,
        }
    }

    #[tokio::test]
    async fn credit_flow_separates_control_frames_and_gates_each_write() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        let mut flow = CreditFlowState::default();
        sender.send(vec![0x02, 20, 0]).await.unwrap();
        sender.send(vec![0x1a, 0x08, 0xa2]).await.unwrap();
        sender.send(vec![0x01, 1]).await.unwrap();

        receive_for_credit(&mut flow, &mut receiver).await.unwrap();
        assert_eq!(flow.maximum_payload, Some(20));
        assert_eq!(flow.credits, 0);
        assert_eq!(
            receive_flow_response(&mut flow, &mut receiver, Duration::ZERO)
                .await
                .unwrap(),
            WaitOutcome::Response(vec![0x1a, 0x08, 0xa2])
        );

        sender.send(vec![0x01, 2]).await.unwrap();
        receive_for_credit(&mut flow, &mut receiver).await.unwrap();
        receive_for_credit(&mut flow, &mut receiver).await.unwrap();
        assert_eq!(flow.credits, 0);
    }

    #[test]
    fn characteristic_selection_requires_write_without_response() {
        let only_write = characteristic(FF02, CharPropFlags::WRITE);
        let error =
            select_characteristics(&[only_write], &profile(NotificationRequirement::Optional))
                .unwrap_err();
        assert_eq!(error.kind, TransportErrorKind::InvalidConfiguration);

        let correct = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
        let (write, notify) =
            select_characteristics(&[correct], &profile(NotificationRequirement::Optional))
                .unwrap();
        assert_eq!(write.uuid, Uuid::parse_str(FF02).unwrap());
        assert!(notify.is_none());
    }

    #[test]
    fn optional_missing_notification_is_unavailable_but_required_is_an_error() {
        let write = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
        let (_, notify) = select_characteristics(
            std::slice::from_ref(&write),
            &profile(NotificationRequirement::Optional),
        )
        .unwrap();
        assert!(notify.is_none());

        let error = select_characteristics(&[write], &profile(NotificationRequirement::Required))
            .unwrap_err();
        assert_eq!(error.kind, TransportErrorKind::Connection);

        let mut credit_profile = profile(NotificationRequirement::Optional);
        credit_profile.flow_control = Some(BleFlowControl::PhomemoCredit);
        let write = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
        let error = select_characteristics(&[write], &credit_profile).unwrap_err();
        assert_eq!(error.kind, TransportErrorKind::Connection);
    }

    #[tokio::test]
    async fn notification_state_distinguishes_unavailable_timeout_response_and_disconnect() {
        let mut unsupported = NotificationState::Unsupported;
        assert_eq!(
            wait_notification_state(&mut unsupported, &mut None, Duration::ZERO)
                .await
                .unwrap(),
            WaitOutcome::Unavailable
        );

        let (sender, receiver) = tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let mut notifications = Some(receiver);
        let mut subscribed = NotificationState::Subscribed;
        sender.send(vec![9]).await.unwrap();
        assert_eq!(
            wait_notification_state(&mut subscribed, &mut notifications, Duration::from_secs(1),)
                .await
                .unwrap(),
            WaitOutcome::Response(vec![9])
        );
        assert_eq!(
            wait_notification_state(&mut subscribed, &mut notifications, Duration::ZERO)
                .await
                .unwrap(),
            WaitOutcome::Timeout
        );
        drop(sender);
        assert!(
            wait_notification_state(&mut subscribed, &mut notifications, Duration::from_secs(1),)
                .await
                .is_err()
        );
        assert_eq!(subscribed, NotificationState::Disconnected);
    }

    #[test]
    fn write_validation_rejects_oversize_and_disconnected_before_platform_io() {
        let limit = NonZeroUsize::new(20).unwrap();
        assert!(validate_write_state(NotificationState::Available, 20, limit).is_ok());
        assert_eq!(
            validate_write_state(NotificationState::Available, 21, limit)
                .unwrap_err()
                .kind,
            TransportErrorKind::InvalidConfiguration
        );
        assert_eq!(
            validate_write_state(NotificationState::Disconnected, 1, limit)
                .unwrap_err()
                .kind,
            TransportErrorKind::Disconnected
        );
    }

    #[test]
    fn configured_notification_must_have_notify_property() {
        let write = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
        let wrong_notify = characteristic(FF03, CharPropFlags::READ);
        let error = select_characteristics(
            &[write, wrong_notify],
            &profile(NotificationRequirement::Optional),
        )
        .unwrap_err();
        assert_eq!(error.kind, TransportErrorKind::InvalidConfiguration);
    }

    #[test]
    fn exact_ff02_ff03_profile_selects_both_characteristics() {
        let unrelated =
            characteristic("00002a00-0000-1000-8000-00805f9b34fb", CharPropFlags::WRITE);
        let write = characteristic(FF02, CharPropFlags::WRITE_WITHOUT_RESPONSE);
        let notify = characteristic(FF03, CharPropFlags::NOTIFY);
        let (selected_write, selected_notify) = select_characteristics(
            &[unrelated, write.clone(), notify.clone()],
            &profile(NotificationRequirement::Optional),
        )
        .unwrap();
        assert_eq!(selected_write, write);
        assert_eq!(selected_notify, Some(notify));
    }
}
