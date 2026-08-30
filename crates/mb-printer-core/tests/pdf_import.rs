// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{export, limits::ProcessingLimits, pdf_import, raster::MonoRaster};

fn base14_helvetica_pdf() -> Vec<u8> {
    let content = "BT /F1 24 Tf 10 35 Td (Hello) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 80] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

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

#[test]
fn limit_aware_normalization_rejects_encoded_and_malformed_input() {
    let pdf = base14_helvetica_pdf();
    let limits = ProcessingLimits {
        max_resource_bytes: pdf.len() - 1,
        ..ProcessingLimits::default()
    };
    assert!(matches!(
        pdf_import::normalize_with_limits(pdf, 72, true, &limits),
        Err(pdf_import::PdfImportError::TooLarge)
    ));
    assert!(matches!(
        pdf_import::normalize_with_limits(
            b"not a PDF".to_vec(),
            72,
            true,
            &ProcessingLimits::default(),
        ),
        Err(pdf_import::PdfImportError::Invalid)
    ));
}

#[test]
fn limit_aware_normalization_bounds_pages_pixels_and_retained_grayscale() {
    let raster = MonoRaster {
        width: 8,
        height: 8,
        pixels: vec![0; 64],
    };
    let pdf = export::pdf_pages(&[(&raster, 72), (&raster, 72)]).unwrap();

    for (limits, first_page_only) in [
        (
            ProcessingLimits {
                max_pages: 1,
                ..ProcessingLimits::default()
            },
            true,
        ),
        (
            ProcessingLimits {
                max_canvas_pixels: 63,
                ..ProcessingLimits::default()
            },
            true,
        ),
        (
            ProcessingLimits {
                max_total_pixels: 127,
                ..ProcessingLimits::default()
            },
            false,
        ),
        (
            ProcessingLimits {
                max_output_bytes: 127,
                ..ProcessingLimits::default()
            },
            false,
        ),
    ] {
        assert!(matches!(
            pdf_import::normalize_with_limits(pdf.clone(), 72, first_page_only, &limits),
            Err(pdf_import::PdfImportError::TooLarge)
        ));
    }
}

#[test]
fn encrypted_pdf_has_a_distinct_stable_rejection() {
    let bytes = b"%PDF-1.7\n1 0 obj << /Encrypt 2 0 R >> endobj\n%%EOF".to_vec();
    assert!(matches!(
        pdf_import::normalize(bytes, 203, true, 1_000_000),
        Err(pdf_import::PdfImportError::Encrypted)
    ));
}

#[test]
fn standard_pdf_font_uses_the_pinned_fallback() {
    let pages = pdf_import::normalize(base14_helvetica_pdf(), 72, true, 1_000_000).unwrap();
    assert_eq!(pages.len(), 1);
    assert!(pages[0].raster.pixels.iter().any(|value| *value < 200));
}
