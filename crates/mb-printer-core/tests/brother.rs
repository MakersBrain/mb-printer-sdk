// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    capabilities,
    protocol::{self, Action, BrotherMedia, Options, Raster},
};
#[test]
fn packbits_reference_cases() {
    assert_eq!(protocol::packbits(&[]), b"");
    assert_eq!(protocol::packbits(&[1]), [0, 1]);
    assert_eq!(protocol::packbits(&[0; 300]), [130, 0, 130, 0, 211, 0]);
    assert_eq!(
        protocol::packbits(&(0..60).collect::<Vec<_>>()),
        std::iter::once(59).chain(0..60).collect::<Vec<_>>()
    )
}
#[test]
fn brother_job_has_exact_python_preamble_and_line_encoding() {
    let p = capabilities::by_id("ql-1110nwb").unwrap();
    let r = Raster {
        width_bytes: 162,
        height: 1,
        data: {
            let mut x = vec![0; 162];
            x[0] = 0x80;
            x
        },
    };
    let o = Options {
        brother_media: Some(BrotherMedia {
            width_mm: 62,
            length_mm: 29,
            continuous: false,
            feed_margin: 0,
        }),
        ..Default::default()
    };
    let plan = protocol::plan(&p, &r, &o).unwrap();
    let commands: Vec<_> = plan
        .actions
        .iter()
        .filter_map(|a| {
            if let Action::CommandWrite { bytes, .. } = a {
                Some(bytes.as_slice())
            } else {
                None
            }
        })
        .collect();
    assert!(plan.actions.iter().any(|a| matches!(
        a,
        Action::WaitForResponse {
            timeout_ms: 3000,
            validation: protocol::ResponseValidation::BrotherStatus32,
            ..
        }
    )));
    assert_eq!(commands[0], [0x1b, 0x69, 0x61, 1]);
    assert_eq!(commands[1], vec![0; 200]);
    assert!(
        commands
            .iter()
            .any(|x| *x == [0x1b, 0x69, 0x7a, 0xce, 0x0b, 62, 29, 1, 0, 0, 0, 0, 0])
    );
    assert!(commands.iter().any(|x| *x == [0x1b, 0x69, 0x4d, 0x40]));
    assert!(commands.iter().any(|x| *x == [0x4d, 2]));
    assert_eq!(commands.last().unwrap(), &&[0x1a]);
    let payload = plan
        .actions
        .iter()
        .find_map(|a| {
            if let Action::RasterWrite { bytes, .. } = a {
                Some(bytes)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(&payload[..3], [0x67, 0, 6]);
    assert_eq!(payload.last(), Some(&0x01))
}
#[test]
fn captured_brother_status_decodes() {
    let b = [
        0x80, 0x20, 0x42, 0x34, 0x44, 0x30, 0, 0, 0, 0, 0x3e, 0x0b, 0, 0, 3, 0, 0, 0x1d, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let s = protocol::brother_parse_status(&b).unwrap();
    assert_eq!(
        (s.media_width_mm, s.media_length_mm, s.media_type),
        (62, 29, "die-cut")
    );
    assert!(s.errors.is_empty());
    assert!(protocol::brother_parse_status(&[0; 32]).is_err());
    let mut trailing = b.to_vec();
    trailing.push(0);
    assert!(protocol::brother_parse_status(&trailing).is_err());
}
