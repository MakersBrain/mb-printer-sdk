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
    ))
}
