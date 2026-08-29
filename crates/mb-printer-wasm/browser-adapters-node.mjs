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
const boundaries = {...transport(),payloadLimit:3};
await executePlan([{action:"raster-write",bytes:[0,1,2,3,4,5,6,7,8,9],logical_chunk:4,delay_after_each_physical_write_ms:0}],boundaries);
assert.deepEqual(boundaries.calls.map(value => value.length),[3,1,3,1,2],"physical splitting must restart at logical boundaries");
await assert.rejects(() => executePlan([], transport(), undefined, undefined, {additionalDelayMs:1,unsafeDiagnosticReductionMs:1}));
const realSetTimeout = globalThis.setTimeout, observedDelays = [];
globalThis.setTimeout = (callback, milliseconds) => { observedDelays.push(milliseconds); queueMicrotask(callback); return 1; };
try {
  await executePlan([{action:"delay",milliseconds:20}],transport(),undefined,undefined,{additionalDelayMs:5});
  await executePlan([{action:"delay",milliseconds:20}],transport(),undefined,undefined,{unsafeDiagnosticReductionMs:7});
} finally { globalThis.setTimeout = realSetTimeout; }
assert.deepEqual(observedDelays,[25,13],"safe increases and explicit unsafe diagnostic reductions are exact");

const boundaryPlan = [{action:"subscribe-notifications"},command([1]),wait()];
for (const failedOperation of ["subscribe","write","wait"]) {
  const disconnected = {
    payloadLimit:4,calls:[],
    async subscribeNotifications(){this.calls.push("subscribe");if(failedOperation==="subscribe")throw new Error("disconnect");return true},
    async write(bytes){this.calls.push("write");if(failedOperation==="write")throw new Error("disconnect")},
    async waitForResponse(){this.calls.push("wait");if(failedOperation==="wait")throw new Error("disconnect");return {kind:"response",bytes:Uint8Array.of(1)}},
  };
  const result = await executePlan(boundaryPlan,disconnected);
  assert.equal(result.status,failedOperation==="subscribe"?"cancelled-before-send":"outcome-unknown");
  assert.equal(disconnected.calls.at(-1),failedOperation,"execution stops exactly at the failed boundary");
}
const everyEffectPlan=[{action:"subscribe-notifications"},command([1]),{action:"raster-write",bytes:[2,3,4,5],logical_chunk:4,delay_after_each_physical_write_ms:0},wait()];
for(let failAt=0;failAt<5;failAt++){
  let effect=0;
  const disconnected={payloadLimit:2,calls:[],async subscribeNotifications(){this.calls.push("subscribe");if(effect++===failAt)throw new Error("disconnect");return true},async write(bytes){this.calls.push(`write:${bytes.length}`);if(effect++===failAt)throw new Error("disconnect")},async waitForResponse(){this.calls.push("wait");if(effect++===failAt)throw new Error("disconnect");return{kind:"response",bytes:Uint8Array.of(1)}}};
  const result=await executePlan(everyEffectPlan,disconnected);
  assert.notEqual(result.status,"completed",`disconnect at effect ${failAt} must stop the plan`);
  assert.equal(disconnected.calls.length,failAt+1,`no effect follows disconnect ${failAt}`);
}
const firstConnection = {...transport(),async write(){throw new Error("disconnect")}};
assert.equal((await executePlan([command([1])],firstConnection)).status,"outcome-unknown");
const explicitReconnection = transport();
assert.equal((await executePlan([command([1])],explicitReconnection)).status,"completed","a fresh explicit job may use a new connection");
assert.equal(explicitReconnection.calls.filter(Array.isArray).length,1,"the failed connection is never replayed automatically");

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
