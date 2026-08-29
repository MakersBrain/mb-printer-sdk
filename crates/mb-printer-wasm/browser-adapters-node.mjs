// SPDX-License-Identifier: AGPL-3.0-or-later
import { executePlan } from "./dist/browser-adapters.js";
import assert from "node:assert/strict";

const command = bytes => ({action:"command-write",name:"test",bytes,atomic:true});
const wait = (validation="any-notification", fallback_delay_ms=0) =>
  ({action:"wait-for-response",timeout_ms:1,fallback_delay_ms,validation});
const transport = (response={kind:"unavailable"}) => ({
  payloadLimit: 4, calls: [],
  async subscribeNotifications() { this.calls.push("subscribe"); return response.kind !== "unavailable"; },
  async write(bytes) { this.calls.push([...bytes]); },
  async waitForResponse() { this.calls.push("wait"); return response; },
});

const atomic = transport();
await assert.rejects(() => executePlan([command([1]), command([1,2,3,4,5])], atomic));
assert.deepEqual(atomic.calls, [], "whole-plan preflight must precede transport access");

let mock = transport({kind:"response",bytes:Uint8Array.of(7)});
assert.equal((await executePlan([command([1]),wait()],mock)).status,"completed");
const brother = new Uint8Array(32); brother.set([0x80,0x20,0x42]);
mock = transport({kind:"response",bytes:brother});
assert.equal((await executePlan([command([1]),wait("brother-status32")],mock)).status,"completed");
mock = transport({kind:"response",bytes:new Uint8Array(31)});
assert.equal((await executePlan([command([1]),wait("brother-status32")],mock)).status,"outcome-unknown");
mock = transport({kind:"response",bytes:new Uint8Array(32)});
assert.equal((await executePlan([command([1]),wait("brother-status32")],mock)).status,"outcome-unknown");
const trailingBrother = new Uint8Array(33); trailingBrother.set([0x80,0x20,0x42]);
mock = transport({kind:"response",bytes:trailingBrother});
assert.equal((await executePlan([command([1]),wait("brother-status32")],mock)).status,"outcome-unknown");
mock = transport({kind:"unavailable"});
assert.equal((await executePlan([wait("any-notification",1)],mock)).status,"completed");
mock = transport({kind:"timeout"});
assert.equal((await executePlan([wait("any-notification",5)],mock)).status,"outcome-unknown","timeout must not use unavailable fallback");
mock = transport({kind:"timeout"});
assert.equal((await executePlan([wait("brother-status32")],mock)).status,"completed","Brother timeout is a frozen best-effort preflight");
mock = transport({kind:"unavailable"});
assert.equal((await executePlan([wait("brother-status32")],mock)).status,"completed","Brother unavailable status matches native policy");

const separated = {...transport(), payloadLimit:2, commandPayloadLimit:5};
const splitRaster = {action:"raster-write",bytes:[1,2,3,4,5],logical_chunk:5,delay_after_each_physical_write_ms:0};
assert.equal((await executePlan([command([1,2,3,4,5]),splitRaster],separated)).status,"completed");
assert.deepEqual(separated.calls, [[1,2,3,4,5],[1,2],[3,4],[5]], "commands remain atomic while raster follows its physical limit");

let controller = new AbortController(); controller.abort(); mock = transport();
assert.equal((await executePlan([command([1])],mock,undefined,controller.signal)).status,"cancelled-before-send");
assert.deepEqual(mock.calls, []);
controller = new AbortController(); mock = transport();
setTimeout(() => controller.abort(),2);
const partial = await executePlan([command([1]),{action:"delay",milliseconds:100}],mock,undefined,controller.signal);
assert.equal(partial.status,"cancelled-partial"); assert.equal(partial.bytesWritten,1);

controller = new AbortController(); let writes = 0;
mock = {...transport(), async write() { writes++; await new Promise(resolve => setTimeout(resolve,100)); }};
setTimeout(() => controller.abort(),2);
const unknown = await executePlan([command([1])],mock,undefined,controller.signal);
assert.equal(unknown.status,"outcome-unknown"); assert.equal(writes,1,"ambiguous write must not retry");
console.log("Node browser-adapter execution semantics passed");
