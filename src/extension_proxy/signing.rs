use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::ServiceError;

type HmacSha256 = Hmac<Sha256>;

pub(crate) fn sign(secret: &[u8], url: &str, expiry: u64) -> Result<String, ServiceError> {
    let mut mac = HmacSha256::new_from_slice(secret).map_err(ServiceError::internal)?;
    mac.update(payload(url, expiry)?.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

pub(crate) fn verify(
    secret: &[u8],
    url: &str,
    expiry: u64,
    signature: &str,
) -> Result<bool, ServiceError> {
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| crate::extension_proxy::bad_request("signature verification failed"))?;
    let mut mac = HmacSha256::new_from_slice(secret).map_err(ServiceError::internal)?;
    mac.update(payload(url, expiry)?.as_bytes());
    Ok(mac.verify_slice(&signature).is_ok())
}

fn payload(url: &str, expiry: u64) -> Result<String, ServiceError> {
    let url = serde_json::to_string(url).map_err(ServiceError::internal)?;
    Ok(format!("{{\"url\":{url},\"expiry\":{expiry}}}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_round_trip_uses_urlsafe_base64_without_padding() {
        let secret = b"abcdefghijklmnopqrstuvwxyz123456";
        let url = "https://example.com/file.crx";
        let sig = sign(secret, url, 123).unwrap();

        assert!(!sig.contains('='));
        assert!(verify(secret, url, 123, &sig).unwrap());
        assert!(!verify(secret, url, 124, &sig).unwrap());
    }
}
