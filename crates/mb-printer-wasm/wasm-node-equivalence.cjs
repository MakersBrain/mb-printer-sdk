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
const plan = JSON.parse(wasm.renderProtocolPlan(documentJson, "m03"));
if (plan.protocol !== "m-series" || plan.actions.length === 0) throw new Error("WASM plan diverged");
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
const a4 = structuredClone(fixture.document);
a4.media = {width:210000,height:297000,unit:"micrometre",dpi:36,orientation:"portrait",printableBounds:{x:0,y:0,width:210000,height:297000},shape:"rectangle"};
a4.elements[0].transform = {x:0,y:0,width:210000,height:297000};
const stamps = JSON.parse(wasm.extractLaPostePdf("L24A", wasm.renderPdf(JSON.stringify(a4)), 36));
if (stamps.length !== 24 || stamps[0].sourcePage !== 1 || stamps[0].slot !== 1 || stamps[23].slot !== 24) throw new Error("WASM La Poste PDF extraction diverged");
console.log("Node/WASM shared fixture equivalence passed");
