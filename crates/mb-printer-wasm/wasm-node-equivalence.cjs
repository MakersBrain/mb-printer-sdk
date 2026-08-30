// SPDX-License-Identifier: AGPL-3.0-or-later
const fs = require("node:fs");
const path = require("node:path");
const wasm = require(process.argv[2]);
const fixture = JSON.parse(fs.readFileSync(path.join(__dirname, "../../fixtures/wasm/equivalence.json"), "utf8"));
const documentJson = JSON.stringify(fixture.document);
if (wasm.validateDocument(documentJson) !== "[]") throw new Error("WASM validation diverged");
for (const name of fs.readdirSync(path.join(__dirname, "../../fixtures/v4/invalid-semantic"))) {
  const invalid = fs.readFileSync(path.join(__dirname, "../../fixtures/v4/invalid-semantic", name), "utf8");
  if (wasm.validateDocument(invalid) === "[]") throw new Error(`WASM accepted semantic fixture ${name}`);
}
const packed = Buffer.from(wasm.renderPacked(documentJson)).toString("hex");
if (packed !== fixture.expectedPackedHex) throw new Error(`WASM raster diverged: ${packed}`);
const renderGoldens = JSON.parse(fs.readFileSync(path.join(__dirname, "../../fixtures/wasm/render-goldens.json"), "utf8"));
for (const test of renderGoldens.cases) {
  const bytes = Buffer.from(wasm.renderPacked(JSON.stringify(test.document)));
  const digest = require("node:crypto").createHash("sha256").update(bytes).digest("hex");
  if (bytes.length !== test.expectedPackedLength || digest !== test.expectedPackedSha256) throw new Error(`WASM broad render golden diverged: ${test.name}`);
}
const imported = JSON.parse(wasm.importV3(JSON.stringify({version:3,widthMm:8,heightMm:1,dotsPerMm:10,elements:[]})));
if (imported.version !== 4 || imported.media.width !== 8000) throw new Error("WASM v3 import diverged");
if (wasm.evaluateTemplate("{{name|trim|upper}}", '{"name":" mb "}') !== "MB") throw new Error("WASM template diverged");
const templateCorpus = JSON.parse(fs.readFileSync(path.join(__dirname, "../../fixtures/template/corpus.json"), "utf8"));
for (const test of templateCorpus.cases) {
  try {
    const actual = wasm.evaluateTemplateContext(test.template, JSON.stringify(test.fields), test.locale, test.date);
    if (test.error || actual !== test.output) throw new Error(`${test.name}: ${actual}`);
  } catch (error) {
    if (!test.error || !String(error).includes(test.error)) throw error;
  }
}
const materializeFixture = JSON.parse(fs.readFileSync(path.join(__dirname, "../mb-printer-core/fixtures/materialize/parity.json"), "utf8"));
const materializeDocument = JSON.stringify(materializeFixture.document);
const materialized = JSON.parse(wasm.materializeRecord(
  materializeDocument,
  JSON.stringify(materializeFixture.records[0]),
  JSON.stringify(materializeFixture.options),
));
const materializedValues = materialized.elements.filter(item => ["text", "barcode", "qr-code"].includes(item.type)).map(item => item.text ?? item.data);
if (JSON.stringify(materializedValues) !== JSON.stringify(materializeFixture.expected.recordValues)) throw new Error("WASM document materialization diverged");
const zonePlan = JSON.parse(wasm.planZoneBatch(materializeDocument, JSON.stringify({recordCount: materializeFixture.records.length, zoneIds: materializeFixture.zoneIds})));
if (JSON.stringify(zonePlan) !== JSON.stringify(materializeFixture.expected.plan)) throw new Error("WASM zone batch plan diverged");
const zonePages = JSON.parse(wasm.materializeZoneBatch(materializeDocument, JSON.stringify(materializeFixture.records), JSON.stringify({...materializeFixture.options, zoneIds: materializeFixture.zoneIds})));
if (JSON.stringify(zonePages.map(page => page.name)) !== JSON.stringify(materializeFixture.expected.pageNames)) throw new Error("WASM zone batch materialization diverged");
let materializeError;
try { wasm.planZoneBatch(materializeDocument, JSON.stringify({recordCount: 1, zoneIds: ["missing"]})); } catch (error) { materializeError = error; }
if (!materializeError || materializeError.version !== 1 || materializeError.code !== "batch.unknown_zone" || materializeError.details.index !== 0) throw new Error("WASM materialization error is not structured");
const plan = JSON.parse(wasm.renderProtocolPlan(documentJson, "m03"));
if (plan.protocol !== "m-series" || plan.actions.length === 0) throw new Error("WASM plan diverged");
const execution = JSON.parse(fs.readFileSync(path.join(__dirname,"../mb-printer-native/tests/fixtures/execution-contract.json"),"utf8"));
const matrixBytes = Array.from({length:execution.raster.widthBytes*execution.raster.height},(_,index)=>index&255);
const debugValidation = value => value === "brother-status32" ? "BrotherStatus32" : "AnyNotification";
for (const model of execution.models) {
  const actions = JSON.parse(wasm.protocolPlan(model,execution.raster.widthBytes,execution.raster.height,JSON.stringify(matrixBytes))).actions;
  for (const payload of execution.payloads) {
    const events=[];
    for (const action of actions) {
      if(action.action==="job-boundary")events.push(`b:${action.kind==="start"?"Start":"End"}`);
      else if(action.action==="subscribe-notifications")events.push("s");
      else if(action.action==="delay")events.push(`d:${action.milliseconds}`);
      else if(action.action==="command-write")events.push(`w:${Buffer.from(action.bytes).toString("hex")}`);
      else if(action.action==="raster-write")for(let logical=0;logical<action.bytes.length;logical+=action.logical_chunk)for(let physical=logical;physical<Math.min(logical+action.logical_chunk,action.bytes.length);physical+=payload){events.push(`w:${Buffer.from(action.bytes.slice(physical,Math.min(physical+payload,logical+action.logical_chunk,action.bytes.length))).toString("hex")}`);events.push(`d:${action.delay_after_each_physical_write_ms}`)}
      else if(action.action==="wait-for-response")events.push(`q:${action.timeout_ms}:${action.fallback_delay_ms}:${debugValidation(action.validation)}`);
    }
    const digest=require("node:crypto").createHash("sha256").update(JSON.stringify(events)).digest("hex");
    if(digest!==execution.expectedSha256[`${model}@${payload}`])throw new Error(`native/WASM physical matrix diverged: ${model}@${payload}`);
  }
}
const optionPlan = JSON.parse(wasm.renderProtocolPlanWithOptions(documentJson, "m03", JSON.stringify({copies:2,density:8})));
if (optionPlan.actions.filter(action => action.action === "command-write" && action.name === "ESC @ init").length !== 2) throw new Error("WASM copies option diverged");
if (!optionPlan.actions.some(action => action.action === "command-write" && action.name === "GS | density" && action.bytes.join() === "29,124,8")) throw new Error("WASM density option diverged");
const invalidName = structuredClone(fixture.document); invalidName.name = "";
if (wasm.validateDocument(JSON.stringify(invalidName)) === "[]") throw new Error("WASM accepted empty document name");
if (!Buffer.from(wasm.renderPng(documentJson)).subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex"))) throw new Error("WASM PNG diverged");
if (!Buffer.from(wasm.renderPdf(documentJson)).subarray(0, 8).equals(Buffer.from("%PDF-1.4"))) throw new Error("WASM PDF diverged");
const batchPdf = wasm.renderBatchPdf(JSON.stringify([fixture.document, fixture.document]));
const normalizedBatch = JSON.parse(wasm.normalizePdf(batchPdf, 254, false));
if (normalizedBatch.length !== 2 || normalizedBatch[0].rasterWidth !== 80 || normalizedBatch[0].sourcePage !== 1 || normalizedBatch[1].sourcePage !== 2) throw new Error("WASM multipage PDF normalization/provenance diverged");
const first = JSON.parse(wasm.normalizePdf(batchPdf, 254, true));
if (first.length !== 1) throw new Error("WASM first-page PDF normalization diverged");
let malformedRejected = false;
try { wasm.normalizePdf(Uint8Array.from([1, 2, 3]), 72, true); } catch { malformedRejected = true; }
if (!malformedRejected) throw new Error("WASM accepted malformed PDF");
const sheetLayout = JSON.stringify({kind:"explicit",id:"node-sheet",paperWidthUm:20000,paperHeightUm:10000,slots:[{xUm:1000,yUm:1000,widthUm:8000,heightUm:1000}]});
const sheetOptions = JSON.stringify({firstSlot:0,dpi:254});
const sheetPlan = JSON.parse(wasm.planSheet(JSON.stringify({itemCount:1,labelWidthUm:8000,labelHeightUm:1000}),sheetLayout,sheetOptions));
if (sheetPlan.pageCount !== 1 || sheetPlan.layout.slots[0].xUm !== 1000) throw new Error("WASM sheet plan diverged");
if (!Buffer.from(wasm.buildSheetPdf(JSON.stringify([fixture.document]),sheetLayout,sheetOptions)).subarray(0,8).equals(Buffer.from("%PDF-1.4"))) throw new Error("WASM sheet PDF diverged");
let sheetError;
try { wasm.planSheet(JSON.stringify({itemCount:1,labelWidthUm:8000,labelHeightUm:1000}),sheetLayout,JSON.stringify({firstSlot:0,dpi:0})); } catch (error) { sheetError = error; }
if (!sheetError || sheetError.code !== "sheet.invalid_dpi" || typeof sheetError.details !== "object") throw new Error("WASM sheet error is not structured");
const a4 = structuredClone(fixture.document);
a4.media = {width:210000,height:297000,unit:"micrometre",dpi:36,orientation:"portrait",printableBounds:{x:0,y:0,width:210000,height:297000},shape:"rectangle"};
a4.elements[0].transform = {x:0,y:0,width:210000,height:297000};
const stamps = JSON.parse(wasm.extractLaPostePdf("L24A", wasm.renderPdf(JSON.stringify(a4)), 36));
if (stamps.length !== 24 || stamps[0].sourcePage !== 1 || stamps[0].slot !== 1 || stamps[23].slot !== 24) throw new Error("WASM La Poste PDF extraction diverged");
console.log("Node/WASM shared fixture equivalence passed");
