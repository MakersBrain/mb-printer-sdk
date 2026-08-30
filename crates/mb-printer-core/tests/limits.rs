// SPDX-License-Identifier: AGPL-3.0-or-later
use mb_printer_core::{
    Document,
    document::{Resource, ResourceDecodeError, ValidationError},
    export,
    limits::ProcessingLimits,
    raster::MonoRaster,
    render, resources,
};

const DOCUMENT: &str = r#"{"version":4,"name":"limits","media":{"width":10000,"height":10000,"unit":"micrometre","dpi":203,"orientation":"portrait","printableBounds":{"x":0,"y":0,"width":10000,"height":10000},"shape":"rectangle"},"coordinateSystem":{"unit":"micrometre","origin":"top-left","rounding":"half-away-from-zero"},"elements":[{"type":"rectangle","id":"r","transform":{"x":1000,"y":1000,"width":8000,"height":8000},"zOrder":0,"strokeWidth":250,"fill":false}],"resources":[],"fields":[],"extensions":{}}"#;

#[test]
fn validation_rejects_collection_and_encoded_resource_limits() {
    let mut document = Document::from_json(DOCUMENT).unwrap();
    let mut limits = ProcessingLimits {
        max_elements: 0,
        ..ProcessingLimits::default()
    };
    assert!(matches!(
        document.validate_with_limits(&limits),
        Err(errors) if errors.iter().any(|error| matches!(error, ValidationError::Limit("elements")))
    ));

    document.elements.clear();
    document.resources.push(Resource {
        id: "oversized".into(),
        media_type: "image/png".into(),
        sha256: String::new(),
        data_base64: "AAAA".into(),
    });
    limits.max_elements = 1;
    limits.max_resource_bytes = 3;
    let errors = document.validate_with_limits(&limits).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, ValidationError::Limit("encoded resource bytes"))),
        "{errors:?}"
    );
}

#[test]
fn resource_decode_and_normalization_are_bounded_before_raster_allocation() {
    let resource = Resource {
        id: "oversized".into(),
        media_type: "image/png".into(),
        sha256: String::new(),
        data_base64: "AAAA".into(),
    };
    let limits = ProcessingLimits {
        max_resource_bytes: 3,
        ..ProcessingLimits::default()
    };
    assert!(matches!(
        resource.decoded_bytes_with_limits(&limits),
        Err(ResourceDecodeError::EncodedTooLarge)
    ));
    assert!(matches!(
        resources::normalize_with_limits(&resource, &limits),
        Err(resources::ResourceError::TooLarge)
    ));
}

#[test]
fn render_and_export_respect_canvas_page_and_output_limits() {
    let document = Document::from_json(DOCUMENT).unwrap();
    let limits = ProcessingLimits {
        max_canvas_pixels: 1,
        ..ProcessingLimits::default()
    };
    assert!(render::render_with_limits(&document, Default::default(), &limits).is_err());

    let raster = MonoRaster::try_new(8, 8, 1_000).unwrap();
    let mut limits = ProcessingLimits {
        max_pages: 1,
        ..ProcessingLimits::default()
    };
    assert!(export::pdf_pages_with_limits(&[(&raster, 203), (&raster, 203)], &limits).is_err());

    limits.max_pages = 2;
    limits.max_output_bytes = 16;
    assert!(matches!(
        export::png_with_limits(&raster, 203, &limits),
        Err(export::ExportError::OutputTooLarge { .. })
    ));
    assert!(matches!(
        export::pdf_with_limits(&raster, 203, &limits),
        Err(export::ExportError::OutputTooLarge { .. })
    ));
}
