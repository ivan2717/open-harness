use agent_core::{MAX_IMAGE_BYTES, ModelImage, ToolOutput};

fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([0; 9]);
    bytes
}

#[test]
fn image_validation_bounds_encoded_bytes_and_declared_pixel_dimensions() {
    assert!(ModelImage::from_bytes("image/png", &png_header(2048, 2048)).is_ok());
    for (width, height) in [(0, 100), (3000, 3000), (8193, 1), (u32::MAX, u32::MAX)] {
        assert!(ModelImage::from_bytes("image/png", &png_header(width, height)).is_err());
    }
    assert!(ModelImage::new("image/png", "not base64").is_err());
    assert!(ModelImage::from_bytes("image/jpeg", &png_header(10, 10)).is_err());
    assert!(ModelImage::from_bytes("image/svg+xml", b"<svg/>").is_err());
    assert!(ModelImage::from_bytes("image/png", &vec![0; MAX_IMAGE_BYTES + 1]).is_err());
    let image = ModelImage::from_bytes("image/png", &png_header(10, 10)).unwrap();
    assert!(
        ToolOutput {
            text: "observed".into(),
            images: vec![image.clone(); 5]
        }
        .validate()
        .is_err()
    );
    assert!(!format!("{image:?}").contains(&image.data_base64));
}

#[test]
fn jpeg_dimensions_are_checked_before_pixel_decompression() {
    // Minimal SOF header, sufficient for dimension probing; no raster decoding.
    let mut jpeg = vec![
        0xff, 0xd8, 0xff, 0xc0, 0x00, 0x08, 8, 0, 32, 0, 32, 1, 0xff, 0xd9,
    ];
    assert!(ModelImage::from_bytes("image/jpeg", &jpeg).is_ok());
    jpeg[7..9].copy_from_slice(&8000u16.to_be_bytes());
    jpeg[9..11].copy_from_slice(&8000u16.to_be_bytes());
    assert!(ModelImage::from_bytes("image/jpeg", &jpeg).is_err());
    jpeg[4..6].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(ModelImage::from_bytes("image/jpeg", &jpeg).is_err());
}

#[test]
fn gif_and_webp_canvas_dimensions_use_the_same_pixel_limit() {
    let mut gif = b"GIF89a\x20\0\x20\0\0\0\0".to_vec();
    assert!(ModelImage::from_bytes("image/gif", &gif).is_ok());
    gif[6..10].copy_from_slice(&[0xff; 4]);
    assert!(ModelImage::from_bytes("image/gif", &gif).is_err());
    let mut webp = b"RIFF\x16\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0\x1f\0\0\x1f\0\0".to_vec();
    assert!(ModelImage::from_bytes("image/webp", &webp).is_ok());
    webp[24..30].fill(0xff);
    assert!(ModelImage::from_bytes("image/webp", &webp).is_err());
}

#[test]
fn image_references_preserve_identity_but_cannot_be_used_as_model_input() {
    let image = ModelImage::from_bytes("image/png", &png_header(10, 10)).unwrap();
    let reference = image.as_reference().unwrap();
    assert_eq!(reference, image);
    assert_eq!(reference.as_reference().unwrap(), reference);
    assert_eq!(reference.reference_id().unwrap().len(), 64);
    assert!(reference.validate().is_err());
    let other = ModelImage::from_bytes("image/png", &png_header(11, 10)).unwrap();
    assert_ne!(reference, other);
    let restored: ModelImage =
        serde_json::from_str(&serde_json::to_string(&reference).unwrap()).unwrap();
    assert_eq!(restored, image);
    assert_eq!(restored.reference_id(), reference.reference_id());
}
