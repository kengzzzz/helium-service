use sha2::{Digest, Sha256};

pub(crate) fn sha256_hex(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn sha256_u32_words(value: &str) -> [u32; 8] {
    let digest = Sha256::digest(value.as_bytes());
    let mut words = [0; 8];
    for (index, chunk) in digest.chunks_exact(4).enumerate() {
        words[index] = u32::from_le_bytes(chunk.try_into().expect("sha256 chunk is 4 bytes"));
    }
    words
}

pub(crate) fn resource_tag(value: &str) -> String {
    let words = sha256_u32_words(value);
    format!(
        "\"{}{}{}\"",
        to_base36(words[0]),
        to_base36(words[1]),
        to_base36(words[2])
    )
}

fn to_base36(mut value: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }

    let mut chars = Vec::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        chars.push(match digit {
            0..=9 => (b'0' + digit) as char,
            _ => (b'a' + digit - 10) as char,
        });
        value /= 36;
    }
    chars.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_matches_javascript_uint32array_format() {
        assert_eq!(resource_tag("hello"), "\"1foxyr04288rjbpv3fq\"");
    }
}
