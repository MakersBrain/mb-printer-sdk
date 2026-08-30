// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{capabilities, media};

fn ids(model: &str) -> Vec<&'static str> {
    let printer = capabilities::by_id(model).unwrap();
    media::presets_for(&printer)
        .into_iter()
        .map(|preset| preset.id)
        .collect()
}

#[test]
fn brother_offers_the_dk_catalogue_including_wide_only_stock() {
    let wide = ids("ql-1110nwb");
    assert!(wide.contains(&"62x29"));
    assert!(wide.contains(&"102x152"));
    assert!(wide.contains(&"103x164"));
    assert!(wide.contains(&"d24"));
    // The 104mm die-cut roll is only listed for the two models that take it.
    assert!(!ids("ql-1115nwb").contains(&"103x164"));
    assert!(ids("ql-1115nwb").contains(&"102x51"));
}

#[test]
fn a_roll_wider_than_the_head_is_not_offered() {
    // M110 prints 48mm, so the 50 and 60mm labels belong to the wider models.
    let narrow = ids("m110");
    assert!(narrow.contains(&"40x30"));
    assert!(!narrow.contains(&"50x80"));
    assert!(!narrow.contains(&"60x40"));
    let wide = ids("m200");
    assert!(wide.contains(&"60x40"));
}

#[test]
fn tape_models_only_offer_the_widths_they_accept() {
    assert_eq!(ids("p12"), vec!["40x12", "30x12", "22x12", "12x12"]);
    let a30 = ids("a30");
    assert!(a30.contains(&"22x14"));
    assert!(a30.contains(&"15x15"));
    assert!(!ids("m110").iter().any(|id| id.ends_with("x14")));
}

#[test]
fn reported_media_resolves_to_a_named_roll() {
    let brother = capabilities::by_id("ql-1110nwb").unwrap();
    // What the printer answered over USB: 62mm wide, 29mm long, die-cut.
    let matched = media::match_media(&brother, 62., 29.).unwrap();
    assert_eq!((matched.id, matched.shape), ("62x29", "rectangle"));
    // A status reply describes the tape, so the reversed orientation resolves too.
    assert_eq!(media::match_media(&brother, 29., 62.).unwrap().id, "62x29");
    // Continuous stock reports no length.
    assert_eq!(media::match_media(&brother, 62., 0.).unwrap().id, "62");
    assert!(media::match_media(&brother, 7., 3.).is_none());
    assert!(media::presets_for(&capabilities::by_id("d-series").unwrap()).len() > 5);
}
