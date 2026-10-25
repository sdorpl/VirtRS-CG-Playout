// Base64 (RFC 4648, standard alphabet) decoding for the DevTools screencast frames.

const INVALID: u8 = 0xff;

const fn table() -> [u8; 256] {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [INVALID; 256];
    let mut i = 0;
    while i < 64 {
        table[alphabet[i] as usize] = i as u8;
        i += 1;
    }
    table
}

static TABLE: [u8; 256] = table();

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut chunks = bytes.chunks_exact(4);
    for chunk in &mut chunks {
        let mut acc = 0u32;
        for &b in chunk {
            let value = TABLE[b as usize];
            if value == INVALID {
                return Err("bad base64".into());
            }
            acc = acc << 6 | u32::from(value);
        }
        out.extend_from_slice(&acc.to_be_bytes()[1..]);
    }
    let rest = chunks.remainder();
    if rest.len() == 1 {
        return Err("bad base64 length".into());
    }
    if !rest.is_empty() {
        let mut acc = 0u32;
        for &b in rest {
            let value = TABLE[b as usize];
            if value == INVALID {
                return Err("bad base64".into());
            }
            acc = acc << 6 | u32::from(value);
        }
        acc <<= 6 * (4 - rest.len());
        out.extend_from_slice(&acc.to_be_bytes()[1..rest.len()]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes_with_and_without_padding() {
        assert_eq!(decode("").unwrap(), b"");
        assert_eq!(decode("Zg==").unwrap(), b"f");
        assert_eq!(decode("Zm8=").unwrap(), b"fo");
        assert_eq!(decode("Zm9v").unwrap(), b"foo");
        assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decode("+/+/").unwrap(), [0xfb, 0xff, 0xbf]);
        assert!(decode("Zm9v!").is_err());
        assert!(decode("Z").is_err());
    }
}
