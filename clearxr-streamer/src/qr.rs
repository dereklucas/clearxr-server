use std::io::Cursor;

use anyhow::Context;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use image::{DynamicImage, ImageFormat, Luma};
use qrcode::QrCode;
use serde_json::json;

use crate::models::BarcodePayload;

pub fn render_pairing_qr_data_url(payload: &BarcodePayload) -> anyhow::Result<String> {
    let qr_payload = json!({
        "token": payload.client_token,
        "digest": payload.certificate_fingerprint,
    });

    let qr_code = QrCode::new(serde_json::to_vec(&qr_payload)?)
        .context("failed to encode the pairing QR payload")?;

    let image = qr_code
        .render::<Luma<u8>>()
        .min_dimensions(320, 320)
        .quiet_zone(true)
        .build();

    let dynamic_image = DynamicImage::ImageLuma8(image);
    let mut png_bytes = Cursor::new(Vec::new());
    dynamic_image.write_to(&mut png_bytes, ImageFormat::Png)?;

    Ok(format!(
        "data:image/png;base64,{}",
        BASE64_STANDARD.encode(png_bytes.into_inner())
    ))
}

#[cfg(test)]
mod tests {
    use super::render_pairing_qr_data_url;
    use crate::models::BarcodePayload;

    #[test]
    fn pairing_qr_is_returned_as_data_url() {
        let payload = BarcodePayload {
            client_token: "client-token".to_string(),
            certificate_fingerprint: "sha256:fingerprint".to_string(),
        };

        let data_url = render_pairing_qr_data_url(&payload).unwrap();

        assert!(data_url.starts_with("data:image/png;base64,"));
    }
}
