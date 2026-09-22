//! Bounded decoding before model protocol inspection and forwarding.

use std::io::Read;

use axum::http::StatusCode;
use bytes::Bytes;

pub(super) fn decode_body(
    encoding: Option<&str>,
    body: Bytes,
    limit: usize,
) -> Result<Bytes, (StatusCode, &'static str)> {
    let encoding = encoding.unwrap_or("identity").trim();
    if encoding.eq_ignore_ascii_case("identity") {
        return Ok(body);
    }
    if !encoding.eq_ignore_ascii_case("zstd") {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported request Content-Encoding",
        ));
    }
    let invalid = || (StatusCode::BAD_REQUEST, "invalid zstd request body");
    let mut decoder = zstd::stream::read::Decoder::new(body.as_ref()).map_err(|_| invalid())?;
    // Bound decoder window memory independently of the decoded output limit.
    decoder.window_log_max(27).map_err(|_| invalid())?;
    let mut decoded = Vec::new();
    decoder
        .take(limit as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(|_| invalid())?;
    if decoded.len() > limit {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "decoded LLM request body exceeds limit",
        ));
    }
    Ok(Bytes::from(decoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_zstd_without_losing_unknown_fields() {
        let json = br#"{"model":"test","future_field":{"nested":[1,2]}}"#;
        let encoded = zstd::stream::encode_all(&json[..], 1).unwrap();
        assert_eq!(
            decode_body(Some("zstd"), encoded.into(), 1024)
                .unwrap()
                .as_ref(),
            json
        );
    }

    #[test]
    fn rejects_invalid_encoding_data_and_decompression_expansion() {
        assert_eq!(
            decode_body(Some("gzip"), Bytes::new(), 8).unwrap_err().0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(
            decode_body(Some("zstd"), Bytes::from_static(b"invalid"), 8)
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        let encoded = zstd::stream::encode_all(&[0u8; 4096][..], 1).unwrap();
        assert_eq!(
            decode_body(Some("zstd"), encoded.into(), 64).unwrap_err().0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
