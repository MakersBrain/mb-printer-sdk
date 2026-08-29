// SPDX-License-Identifier: AGPL-3.0-or-later
import fs from "node:fs";
import Ajv2020 from "ajv/dist/2020.js";
const read = (path) => JSON.parse(fs.readFileSync(new URL(path, import.meta.url), "utf8"));
const validate = new Ajv2020({ strict: true, allErrors: true }).compile(read("../../schema/mb-label-v4.schema.json"));
if (!validate(read("../../fixtures/v4/valid/all-elements.json"))) throw new Error(JSON.stringify(validate.errors));
if (validate(read("../../fixtures/v4/invalid/unknown-element-field.json"))) throw new Error("invalid corpus document passed schema");
for (const name of fs.readdirSync(new URL("../../fixtures/v4/invalid-semantic/", import.meta.url))) {
  const document = read(`../../fixtures/v4/invalid-semantic/${name}`);
  if (!validate(document)) throw new Error(`semantic fixture ${name} must remain schema-valid: ${JSON.stringify(validate.errors)}`);
}
