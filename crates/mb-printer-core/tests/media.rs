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
fn a_model_is_offered_only_the_stock_it_accepts() {
    // The M110 prints 48mm but takes 50mm media, which the vendor states; the
    // 60 and 70mm stock belongs to the wider M200.
    let narrow = ids("m110");
    assert!(narrow.iter().any(|id| id == "50x80"));
    assert!(narrow.iter().any(|id| id == "40x30"));
    assert!(!narrow.iter().any(|id| id == "60x40"));
    assert!(!narrow.iter().any(|id| id == "70x80"));
    let wide = ids("m200");
    assert!(wide.iter().any(|id| id == "70x80"));
    assert_eq!(
        media::max_media_width_mm(&capabilities::by_id("m110").unwrap()),
        Some(50.)
    );
}

#[test]
fn tape_models_only_offer_the_widths_they_accept() {
    let tape_widths = |model: &str| {
        let printer = capabilities::by_id(model).unwrap();
        let mut widths: Vec<u16> = media::presets_for(&printer)
            .into_iter()
            .filter_map(|preset| preset.tape_width_mm)
            .collect();
        widths.sort_unstable();
        widths.dedup();
        widths
    };
    // The P12 takes 12mm tape, so nothing wider is offered; the A30 also takes 14 and 15.
    assert_eq!(tape_widths("p12"), vec![6, 12]);
    let a30 = tape_widths("a30");
    assert!(a30.contains(&14) && a30.contains(&15));
    // Label stock is not tape and carries no tape width.
    assert!(tape_widths("m110").is_empty());
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
