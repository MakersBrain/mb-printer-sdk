// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(target_arch = "wasm32")]

use std::time::Duration;

use mb_printer_executor::*;
use wasm_bindgen::JsValue;

struct BrowserFake {
    // JsValue is intentionally allowed without a Send bound on Wasm.
    value: JsValue,
}

impl Transport for BrowserFake {
    fn payload_limit(&self) -> usize {
        let _ = &self.value;
        23
    }

    fn subscribe_notifications(
        &mut self,
    ) -> TransportFuture<'_, Result<NotificationSupport, TransportError>> {
        Box::pin(async { Ok(NotificationSupport::Unavailable) })
    }

    fn write<'a>(
        &'a mut self,
        _: &'a [u8],
        _: WriteKind,
    ) -> TransportFuture<'a, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }

    fn wait_response(
        &mut self,
        _: Duration,
    ) -> TransportFuture<'_, Result<WaitOutcome, TransportError>> {
        Box::pin(async { Ok(WaitOutcome::Unavailable) })
    }

    fn delay(&mut self, _: Duration) -> TransportFuture<'_, ()> {
        Box::pin(async {})
    }

    fn disconnect(&mut self) -> TransportFuture<'_, Result<(), TransportError>> {
        Box::pin(async { Ok(()) })
    }
}

fn accepts_dyn(_: &mut dyn Transport) {}

#[allow(dead_code)]
fn wasm_contract_compiles() {
    let mut fake = BrowserFake {
        value: JsValue::NULL,
    };
    accepts_dyn(&mut fake);
}
