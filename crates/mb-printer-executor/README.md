# mb-printer-executor

Cross-platform, runtime-independent asynchronous execution for plans produced by
`mb-printer-core`.

Concrete transports provide writes, notifications, response waits, delays, and
disconnect behavior. The crate deliberately has no Tokio, browser, USB, serial,
or Bluetooth dependency.

`execute` is the canonical API and uses reference timing with no cancellation.
`execute_with_options` adds pacing overrides, an awaitable cancellation source,
and progress callbacks. The executor performs whole-plan preflight, physical
chunking, pacing, response collection, and validation before returning the
canonical `Progress` or `ExecuteError` result.

Cancellation is raced against every pending effect. A cancellation between
effects returns `ExecuteError::Cancelled`; cancellation while a write is in
flight returns `ExecuteError::WriteOutcomeUnknown`, because dropping the future
cannot establish whether the device accepted bytes. Execution is never retried
automatically.

```rust,no_run
# async fn print(
#     plan: &mb_printer_core::protocol::Plan,
#     transport: &mut dyn mb_printer_executor::Transport,
# ) -> Result<(), mb_printer_executor::ExecuteError> {
let progress = mb_printer_executor::execute(plan, transport).await?;
println!("wrote {} bytes", progress.bytes_written);
# Ok(())
# }
```
