// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{capabilities, media};

fn ids(model: &str) -> Vec<String> {
    let printer = capabilities::by_id(model).unwrap();
    media::presets_for(&printer)
        .into_iter()
        .map(|preset| preset.id.clone())
        .collect()
}

#[test]
fn brother_offers_the_dk_catalogue_including_wide_only_stock() {
    let wide = ids("ql-1110nwb");
    let has = |list: &[String], id: &str| list.iter().any(|value| value == id);
    assert!(has(&wide, "62x29"));
    assert!(has(&wide, "102x152"));
    assert!(has(&wide, "103x164"));
    assert!(has(&wide, "d24"));
    assert!(has(&wide, "62"));
    // Brother lists no 104mm stock for the QL-1115NWB, roll or die-cut.
    let narrower = ids("ql-1115nwb");
    assert!(!has(&narrower, "103x164"));
    assert!(!has(&narrower, "103"));
    assert!(has(&narrower, "102x51"));
}

#[test]
fn a_roll_wider_than_the_head_is_not_offered() {
    // M110 prints 48mm, so the 50 and 60mm labels belong to the wider models.
    let narrow = ids("m110");
    assert!(narrow.iter().any(|id| id == "40x30"));
    assert!(!narrow.iter().any(|id| id == "50x80"));
    assert!(!narrow.iter().any(|id| id == "60x40"));
    assert!(ids("m200").iter().any(|id| id == "60x40"));
}

#[test]
fn tape_models_only_offer_the_widths_they_accept() {
    assert_eq!(ids("p12"), vec!["40x12", "30x12", "22x12", "12x12"]);
    let a30 = ids("a30");
    assert!(a30.iter().any(|id| id == "22x14"));
    assert!(a30.iter().any(|id| id == "15x15"));
    assert!(!ids("m110").iter().any(|id| id.ends_with("x14")));
}

#[test]
fn reported_media_resolves_to_a_named_roll() {
    let brother = capabilities::by_id("ql-1110nwb").unwrap();
    // What the printer answered over USB: 62mm wide, 29mm long, die-cut.
    let matched = media::match_media(&brother, 62., 29.).unwrap();
    assert_eq!(
        (matched.id.as_str(), matched.shape.as_str()),
        ("62x29", "rectangle")
    );
    // The descriptor's printable geometry travels with the media.
    assert_eq!(
        (matched.printable_width_dots, matched.offset_right_dots),
        (Some(696), Some(56))
    );
    // A status reply describes the tape, so the reversed orientation resolves too.
    assert_eq!(media::match_media(&brother, 29., 62.).unwrap().id, "62x29");
    // Continuous stock reports no length.
    assert_eq!(media::match_media(&brother, 62., 0.).unwrap().id, "62");
    assert!(media::match_media(&brother, 7., 3.).is_none());
    assert!(media::presets_for(&capabilities::by_id("d-series").unwrap()).len() > 5);
}
