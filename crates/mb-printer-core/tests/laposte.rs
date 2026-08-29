// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    laposte::{self, NormalizedPage},
    raster::GrayRaster,
};
#[test]
fn every_format_extracts_first_and_last_slots() {
    for code in [
        "L24A",
        "L24B",
        "L21A",
        "L18A",
        "L16A",
        "L14A",
        "L12A",
        "SHEET",
        "L24A_SHEET",
    ] {
        let f = laposte::format(code).unwrap();
        let mut raster = GrayRaster::new(2100, 2970, 255);
        for slot in [0u16, f.columns as u16 * f.rows as u16 - 1] {
            let col = slot % f.columns as u16;
            let row = slot / f.columns as u16;
            let x = ((f.left_um + col as i64 * f.column_pitch_um) * 10 / 1000 + 20) as u32;
            let y = ((f.top_um + row as i64 * f.row_pitch_um) * 10 / 1000 + 20) as u32;
            for yy in y..y + 100 {
                for xx in x..x + 180 {
                    raster.pixels[(yy * raster.width + xx) as usize] = 0
                }
            }
        }
        let labels = laposte::extract(
            &[NormalizedPage {
                page: 3,
                width_um: 210_000,
                height_um: 297_000,
                raster,
            }],
            code,
        )
        .unwrap();
        assert_eq!(
            labels.iter().map(|x| x.slot).collect::<Vec<_>>(),
            vec![1, f.columns as u16 * f.rows as u16]
        );
        assert!(
            labels
                .iter()
                .all(|x| (x.width_um, x.height_um) == (63_500, 33_900))
        )
    }
}
#[test]
fn extraction_rejects_non_a4_and_empty() {
    let white = GrayRaster::new(2100, 2970, 255);
    assert!(matches!(
        laposte::extract(
            &[NormalizedPage {
                page: 1,
                width_um: 210_000,
                height_um: 297_000,
                raster: white.clone()
            }],
            "L24A"
        ),
        Err(laposte::ExtractError::Empty)
    ));
    assert!(matches!(
        laposte::extract(
            &[NormalizedPage {
                page: 2,
                width_um: 100_000,
                height_um: 150_000,
                raster: white
            }],
            "L24A"
        ),
        Err(laposte::ExtractError::NotA4 { page: 2, .. })
    ));
    assert_eq!(
        laposte::extract(
            &[NormalizedPage {
                page: 0,
                width_um: 210_000,
                height_um: 297_000,
                raster: GrayRaster::new(210, 297, 255),
            }],
            "L24A"
        ),
        Err(laposte::ExtractError::PageNumber)
    );
}

fn mark_slot(raster: &mut GrayRaster, format: laposte::Format, slot: u16) {
    let column = slot % u16::from(format.columns);
    let row = slot / u16::from(format.columns);
    let x = ((format.left_um + i64::from(column) * format.column_pitch_um)
        * i64::from(raster.width)
        / 210_000) as u32
        + 8;
    let y = ((format.top_um + i64::from(row) * format.row_pitch_um) * i64::from(raster.height)
        / 297_000) as u32
        + 8;
    for yy in y..y + 30 {
        for xx in x..x + 30 {
            raster.pixels[(yy * raster.width + xx) as usize] = 0;
        }
    }
}

#[test]
fn multipage_partial_sheets_preserve_order_and_dpi_geometry() {
    let format = laposte::format("L24A").unwrap();
    let mut page_203 = GrayRaster::new(1678, 2374, 255);
    let mut page_300 = GrayRaster::new(2480, 3508, 255);
    mark_slot(&mut page_203, format, 5);
    mark_slot(&mut page_203, format, 0);
    mark_slot(&mut page_300, format, 23);
    let stamps = laposte::extract(
        &[
            NormalizedPage {
                page: 2,
                width_um: 210_000,
                height_um: 297_000,
                raster: page_203,
            },
            NormalizedPage {
                page: 4,
                width_um: 210_000,
                height_um: 297_000,
                raster: page_300,
            },
        ],
        "L24A",
    )
    .unwrap();
    assert_eq!(
        stamps
            .iter()
            .map(|stamp| (stamp.page, stamp.slot))
            .collect::<Vec<_>>(),
        vec![(2, 1), (2, 6), (4, 24)]
    );
    assert_eq!(
        (stamps[0].raster.width, stamps[0].raster.height),
        (507, 271)
    );
    assert_eq!(
        (stamps[2].raster.width, stamps[2].raster.height),
        (750, 400)
    );
}

#[test]
fn a4_tolerance_and_cut_guide_noise_are_bounded() {
    let format = laposte::format("L24A").unwrap();
    let mut raster = GrayRaster::new(2100, 2970, 255);
    let x = (format.left_um * 10 / 1000) as u32;
    let y = (format.top_um * 10 / 1000) as u32;
    for xx in x..x + 635 {
        raster.pixels[(y * raster.width + xx) as usize] = 0;
    }
    assert_eq!(
        laposte::extract(
            &[NormalizedPage {
                page: 1,
                width_um: 211_500,
                height_um: 295_500,
                raster: raster.clone(),
            }],
            "L24A"
        ),
        Err(laposte::ExtractError::Empty),
        "ink confined to a cut guide must not count as a stamp"
    );
    assert!(matches!(
        laposte::extract(
            &[NormalizedPage {
                page: 1,
                width_um: 211_501,
                height_um: 297_000,
                raster,
            }],
            "L24A"
        ),
        Err(laposte::ExtractError::NotA4 { .. })
    ));
}
