// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::template::{self, Context};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Corpus {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    name: String,
    template: String,
    fields: BTreeMap<String, String>,
    locale: String,
    date: String,
    output: Option<String>,
    error: Option<String>,
}

#[test]
fn shared_template_contract_is_exact() {
    let corpus: Corpus =
        serde_json::from_str(include_str!("../fixtures/template/corpus.json")).unwrap();
    for case in corpus.cases {
        let result = template::evaluate_with_context(
            &case.template,
            Context {
                fields: &case.fields,
                locale: &case.locale,
                current_date: &case.date,
            },
        );
        match (case.output, case.error) {
            (Some(output), None) => assert_eq!(result.unwrap(), output, "{}", case.name),
            (None, Some(error)) => {
                assert_eq!(result.unwrap_err().to_string(), error, "{}", case.name)
            }
            _ => panic!("invalid corpus case {}", case.name),
        }
    }
}
