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

#[test]
fn streaming_transports_drop_the_chunk_pacing() {
    use mb_printer_core::protocol::{self, Action, Options, Raster};
    let printer = capabilities::by_id("m110").unwrap();
    let raster = Raster {
        width_bytes: 48,
        height: 8,
        data: vec![0; 48 * 8],
    };
    let delays = |options: &Options| {
        protocol::plan(&printer, &raster, options)
            .unwrap()
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::RasterWrite {
                    delay_after_each_physical_write_ms,
                    ..
                } => Some(*delay_after_each_physical_write_ms),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let paced = Options::default();
    assert_eq!(delays(&paced), vec![20]);
    let streaming = Options {
        streaming: true,
        ..Options::default()
    };
    assert_eq!(delays(&streaming), vec![0]);
}

#[test]
fn the_compressed_raster_frames_every_block_with_its_length() {
    use mb_printer_core::protocol::{self, Action, Options, Raster};
    let printer = capabilities::by_id("m110").unwrap();
    // Two blocks: the encoder cuts every 4096 bytes of packed raster.
    let raster = Raster {
        width_bytes: 48,
        height: 100,
        data: vec![0x5a; 4800],
    };
    let payload = protocol::lzo_raster(&raster).unwrap();
    assert_eq!(&payload[..4], [48, 0, 100, 0]);
    let first =
        usize::from(payload[4]) + usize::from(payload[5]) * 256 + usize::from(payload[6]) * 65536;
    let second_at = 7 + first;
    let second = usize::from(payload[second_at]) + usize::from(payload[second_at + 1]) * 256;
    assert_eq!(payload.len(), second_at + 3 + second);
    // Repeated bytes compress, so the wire carries far less than the raster.
    assert!(payload.len() < raster.data.len() / 4);

    let plan = protocol::plan(
        &printer,
        &raster,
        &Options {
            lzo: true,
            ..Options::default()
        },
    )
    .unwrap();
    let commands: Vec<&[u8]> = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            Action::CommandWrite { bytes, .. } => Some(bytes.as_slice()),
            _ => None,
        })
        .collect();
    assert!(commands.contains(&[0x1f, 0x11, 0x35, 1].as_slice()));
    assert!(commands.contains(&[0x1d, 0x76, 0x30, 0].as_slice()));
    assert!(commands.contains(&[0x1f, 0x11, 0x35, 0].as_slice()));
    assert!(
        plan.actions
            .iter()
            .any(|action| matches!(action, Action::RasterWrite { bytes, .. } if bytes == &payload))
    );
}
