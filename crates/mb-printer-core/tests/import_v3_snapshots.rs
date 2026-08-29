// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{Document, importer};
use serde_json::json;

#[test]
fn nested_groups_and_resources_match_shared_snapshot() {
    let value = importer::import_v3(include_str!("../fixtures/v3/nested-groups.json")).unwrap();
    let elements = value["elements"].as_array().unwrap();
    let memberships = elements
        .iter()
        .filter_map(|element| {
            Some((
                element["id"].as_str()?.to_owned(),
                json!(element["groupId"].as_str()?),
            ))
        })
        .collect::<serde_json::Map<_, _>>();
    let actual = json!({
        "_license":"SPDX-License-Identifier: AGPL-3.0-or-later",
        "elementIds":elements.iter().map(|element| element["id"].clone()).collect::<Vec<_>>(),
        "memberships":memberships,
        "resourceMediaTypes":value["resources"].as_array().unwrap().iter().map(|resource| resource["mediaType"].clone()).collect::<Vec<_>>(),
        "resourceCount":value["resources"].as_array().unwrap().len(),
    });
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/v3/nested-groups.snapshot.json")).unwrap();
    assert_eq!(actual, expected);
    let document: Document = serde_json::from_value(value).unwrap();
    document.validate().unwrap();
}
