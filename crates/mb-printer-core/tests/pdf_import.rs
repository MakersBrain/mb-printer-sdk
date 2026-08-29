// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{export, pdf_import, raster::MonoRaster};

#[test]
fn generated_pdf_normalizes_in_pure_rust() {
    let raster = MonoRaster {
        width: 16,
        height: 8,
        pixels: vec![1; 128],
    };
    let pdf = export::pdf(&raster, 72).unwrap();
    let pages = pdf_import::normalize(pdf, 72, true, 1_000_000).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].page, 1);
    assert_eq!((pages[0].raster.width, pages[0].raster.height), (16, 8));
    assert!(pages[0].raster.pixels.iter().all(|value| *value < 8));
}

#[test]
fn multipage_and_malformed_inputs_are_bounded() {
    let raster = MonoRaster {
        width: 8,
        height: 8,
        pixels: vec![0; 64],
    };
    let pdf = export::pdf_pages(&[(&raster, 72), (&raster, 72)]).unwrap();
    let pages = pdf_import::normalize(pdf.clone(), 72, false, 1_000_000).unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].page, 1);
    assert_eq!(pages[1].page, 2);
    assert_eq!(
        pdf_import::normalize(pdf, 72, true, 1_000_000)
            .unwrap()
            .len(),
        1
    );
    assert!(pdf_import::normalize(b"not a PDF".to_vec(), 72, true, 100).is_err());
}
