// SPDX-License-Identifier: AGPL-3.0-or-later
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import Ajv2020 from "ajv/dist/2020.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, "../..");
const load = relative => JSON.parse(fs.readFileSync(path.join(root, relative), "utf8"));
const matrix = load("fixtures/hardware/matrix.json");
const schema = load("fixtures/hardware/report.schema.json");
const template = load("fixtures/hardware/report-template.json");
const catalogue = load("crates/mb-printer-core/data/printers.json").printers.map(item => item.id).sort();

const ajv = new Ajv2020({allErrors:true,strict:true,formats:{"date-time":true}});
ajv.addKeyword("x-spdx-license");
const validateReport = ajv.compile(schema);
if (!validateReport(template)) throw new Error(`invalid hardware report template: ${ajv.errorsText(validateReport.errors)}`);
for (const pathToRemove of [["device","serialNumber"],["device","firmwareVersion"],["platform","osVersion"],["media","identifier"],["trace","artifact"],["trace","sha256"],["operator","name"],["signature","valueBase64"]]) {
  const incomplete=structuredClone(template); delete incomplete[pathToRemove[0]][pathToRemove[1]];
  if (validateReport(incomplete)) throw new Error(`report schema accepted missing ${pathToRemove.join(".")}`);
}
if (matrix.status !== "incomplete" || matrix.qualificationPolicy.automaticFamilyQualification || matrix.qualificationPolicy.automaticAliasQualification) throw new Error("hardware qualification policy became permissive");
if (JSON.stringify([...matrix.catalogIds].sort()) !== JSON.stringify(catalogue)) throw new Error("hardware matrix catalogue IDs drifted from printer definitions");
const ids = new Set();
for (const cell of matrix.cells) {
  if (ids.has(cell.id)) throw new Error(`duplicate hardware cell ${cell.id}`); ids.add(cell.id);
  if (!["unsigned","provisional","signed"].includes(cell.state)) throw new Error(`invalid state ${cell.id}`);
  if (!cell.requiredCatalogIds.length || cell.requiredCatalogIds.some(id => !matrix.catalogIds.includes(id))) throw new Error(`invalid catalogue qualification ${cell.id}`);
  if (cell.state === "signed") throw new Error(`cell ${cell.id} claims sign-off without a checked-in signed report`);
  if (cell.state === "provisional" && !cell.historicalEvidence?.missingRequiredFields?.length) throw new Error(`provisional cell ${cell.id} lacks missing-field disclosure`);
}
if (matrix.cells.filter(cell => cell.state === "provisional").length !== 3) throw new Error("the three historical successes must remain provisional");
console.log(`Hardware acceptance contract passed (${matrix.cells.length} cells, 0 signed, 3 provisional)`);
