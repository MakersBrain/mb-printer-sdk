// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    capabilities, protocol,
    raster::{Fit, MonoRaster, Rotation},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Matrix {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    model: String,
    density: u8,
    copies: u16,
    continuous: bool,
    rotation: String,
    alignment: String,
    mtu: usize,
}
#[test]
fn shared_protocol_option_matrix_is_deterministic_and_mtu_feasible() {
    let matrix: Matrix =
        serde_json::from_str(include_str!("../fixtures/protocol/matrix.json")).unwrap();
    for case in matrix.cases {
        let printer = capabilities::by_id(&case.model).unwrap();
        let mut image = MonoRaster {
            width: 16,
            height: 8,
            pixels: (0..128).map(|i| u8::from(i % 5 == 0)).collect(),
        };
        if case.rotation == "clockwise90" {
            image = image.rotate(Rotation::Clockwise90);
        }
        let fit = match case.alignment.as_str() {
            "left" => Fit::Left,
            "center" => Fit::Center,
            "right" => Fit::Right,
            _ => panic!("bad matrix alignment"),
        };
        if printer.protocol != capabilities::Protocol::Tspl {
            image = image
                .place_on_head(printer.width_px().unwrap_or(image.width), fit, 0, 0)
                .unwrap();
        }
        let data = image.pack_msb().unwrap();
        let raster = protocol::Raster {
            width_bytes: image.width.div_ceil(8) as u16,
            height: image.height,
            data,
        };
        let options = protocol::Options {
            density: case.density,
            copies: case.copies,
            continuous: case.continuous,
            brother_media: (printer.protocol == capabilities::Protocol::Brother).then_some(
                protocol::BrotherMedia {
                    width_mm: 62,
                    length_mm: 29,
                    continuous: false,
                    feed_margin: 0,
                },
            ),
            ..Default::default()
        };
        let first = protocol::plan(&printer, &raster, &options).unwrap();
        let second = protocol::plan(&printer, &raster, &options).unwrap();
        assert_eq!(first, second, "{}", case.model);
        assert!(case.mtu > 0);
        let oversized=first.actions.iter().any(|action|matches!(action,protocol::Action::CommandWrite{bytes,atomic:true,..}if bytes.len()>case.mtu));
        assert_eq!(
            oversized,
            case.model == "ql-1100" && case.mtu == 23,
            "{} at MTU {}",
            case.model,
            case.mtu
        );
    }
}
