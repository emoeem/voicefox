//! mg 签名工具

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};

use super::super::crypto;

const MRC_DELTA: i64 = 2_654_435_769;
const MRC_MIN_LENGTH: usize = 32;
const MRC_MAX_CONTAINER_DEPTH: usize = 4;
const MRC_KEY: [i64; 9] = [
    27_303_562_373_562_475,
    18_014_862_372_307_051,
    22_799_692_160_172_081,
    34_058_940_340_699_235,
    30_962_724_186_095_721,
    27_303_523_720_101_991,
    27_303_523_720_101_998,
    31_244_139_033_526_382,
    28_992_395_054_481_524,
];

/// 生成咪咕搜索签名
pub fn mg_sign(keyword: &str) -> (String, String) {
    let device_id = "963B7AA0D21511ED807EE5846EC87D20";
    let signature_md5 = "6cdc72a439cef99a3418d2a78aa28c73";

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();

    let sign = crypto::md5(&format!(
        "{keyword}{signature_md5}yyapp2d16148780a1dcc7408e06336b98cfd50{device_id}{timestamp}"
    ));

    (sign, timestamp)
}

pub fn decrypt_mrc(data: &[u8]) -> Result<String, String> {
    decode_mrc_container(data, 0)
}

fn decode_mrc_container(data: &[u8], depth: usize) -> Result<String, String> {
    if data.is_empty() {
        return Ok(String::new());
    }
    if depth > MRC_MAX_CONTAINER_DEPTH {
        return Err("MRC container nesting is too deep".to_string());
    }

    if let Some(text) = decode_text(data)
        && looks_like_mrc(&text)
    {
        return Ok(trim_text_padding(text));
    }

    if let Ok(text) = std::str::from_utf8(data) {
        let compact = text
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        if compact.len() >= MRC_MIN_LENGTH
            && compact.len().is_multiple_of(16)
            && compact.iter().all(u8::is_ascii_hexdigit)
        {
            return decrypt_mrc_hex(&compact);
        }

        if let Some(decoded) = decode_base64_container(&compact)
            && let Ok(text) = decode_mrc_container(&decoded, depth + 1)
        {
            return Ok(text);
        }
    }

    for decompressed in decompress_containers(data) {
        if let Ok(text) = decode_mrc_container(&decompressed, depth + 1) {
            return Ok(text);
        }
    }

    decode_text(data)
        .map(trim_text_padding)
        .ok_or_else(|| "unsupported MRC container".to_string())
}

fn decrypt_mrc_hex(data: &[u8]) -> Result<String, String> {
    let mut words = data
        .as_chunks::<16>()
        .0
        .iter()
        .map(|chunk| {
            let value = std::str::from_utf8(chunk).map_err(|error| error.to_string())?;
            u64::from_str_radix(value, 16)
                .map(|value| value as i64)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    tea_decrypt(&mut words);

    let mut utf16 = words
        .into_iter()
        .flat_map(|word| {
            let bytes = word.to_le_bytes();
            [
                u16::from_le_bytes([bytes[0], bytes[1]]),
                u16::from_le_bytes([bytes[2], bytes[3]]),
                u16::from_le_bytes([bytes[4], bytes[5]]),
                u16::from_le_bytes([bytes[6], bytes[7]]),
            ]
        })
        .collect::<Vec<_>>();
    while utf16.last() == Some(&0) {
        utf16.pop();
    }
    String::from_utf16(&utf16)
        .map(trim_text_padding)
        .map_err(|error| format!("MRC utf16 decode failed: {error}"))
}

fn decode_base64_container(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 8
        || !data.len().is_multiple_of(4)
        || !data
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return None;
    }
    std::str::from_utf8(data)
        .ok()
        .and_then(|text| crypto::base64_decode(text).ok())
        .filter(|decoded| !decoded.is_empty() && decoded.as_slice() != data)
}

fn decompress_containers(data: &[u8]) -> Vec<Vec<u8>> {
    let mut results = Vec::new();
    if data.starts_with(&[0x1f, 0x8b]) {
        if let Some(decoded) = read_compressed(GzDecoder::new(data)) {
            results.push(decoded);
        }
        return results;
    }
    if data.starts_with(&[0x78])
        && let Some(decoded) = read_compressed(ZlibDecoder::new(data))
    {
        results.push(decoded);
    }
    if let Some(decoded) = read_compressed(DeflateDecoder::new(data))
        && !results.iter().any(|value| value == &decoded)
    {
        results.push(decoded);
    }
    results
}

fn read_compressed(mut reader: impl Read) -> Option<Vec<u8>> {
    let mut decoded = Vec::new();
    reader
        .read_to_end(&mut decoded)
        .ok()
        .filter(|_| !decoded.is_empty())
        .map(|_| decoded)
}

fn decode_text(data: &[u8]) -> Option<String> {
    if data.starts_with(&[0xff, 0xfe]) {
        return decode_utf16(&data[2..], true);
    }
    if data.starts_with(&[0xfe, 0xff]) {
        return decode_utf16(&data[2..], false);
    }
    if let Ok(text) = std::str::from_utf8(data) {
        return Some(text.to_string());
    }
    if data.len().is_multiple_of(2) {
        let even_zeroes = data.iter().step_by(2).filter(|byte| **byte == 0).count();
        let odd_zeroes = data
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|byte| **byte == 0)
            .count();
        let pairs = data.len() / 2;
        if odd_zeroes > pairs / 4 {
            return decode_utf16(data, true);
        }
        if even_zeroes > pairs / 4 {
            return decode_utf16(data, false);
        }
    }
    None
}

fn decode_utf16(data: &[u8], little_endian: bool) -> Option<String> {
    if !data.len().is_multiple_of(2) {
        return None;
    }
    let units = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| {
            if little_endian {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units).ok()
}

fn looks_like_mrc(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with('[')
            && line
                .get(1..)
                .and_then(|value| value.split_once(','))
                .is_some_and(|(start, _)| start.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

fn trim_text_padding(mut text: String) -> String {
    while text.ends_with('\0') {
        text.pop();
    }
    text
}

fn tea_decrypt(data: &mut [i64]) {
    if data.is_empty() {
        return;
    }
    let length = data.len();
    let mut next = data[0];
    let mut sum = (6i64 + 52i64 / length as i64).wrapping_mul(MRC_DELTA);
    while sum != 0 {
        let key_index = ((sum >> 2) & 3) as usize;
        for index in (1..length).rev() {
            let previous = data[index - 1];
            next = data[index].wrapping_sub(mix(
                previous,
                next,
                sum,
                MRC_KEY[(index & 3) ^ key_index],
            ));
            data[index] = next;
        }
        let previous = data[length - 1];
        next = data[0].wrapping_sub(mix(previous, next, sum, MRC_KEY[key_index]));
        data[0] = next;
        sum = sum.wrapping_sub(MRC_DELTA);
    }
}

fn mix(previous: i64, next: i64, sum: i64, key: i64) -> i64 {
    (next ^ sum).wrapping_add(previous ^ key)
        ^ ((previous >> 5) ^ next.wrapping_shl(2))
            .wrapping_add((next >> 3) ^ previous.wrapping_shl(4))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::{MRC_DELTA, MRC_KEY, decrypt_mrc, mix};

    #[test]
    fn leaves_plain_mrc_untouched() {
        let text = "[1000,500]你(1000,250)好(1250,250)";
        assert_eq!(decrypt_mrc(text.as_bytes()).unwrap(), text);
    }

    #[test]
    fn decrypts_encrypted_mrc_hex() {
        let plaintext = "[1000,500]你(1000,250)好(1250,250)\n";
        let encrypted = encrypt_fixture(plaintext);
        assert_eq!(decrypt_mrc(encrypted.as_bytes()).unwrap(), plaintext);
    }

    #[test]
    fn unwraps_base64_and_zlib_mrc_containers() {
        let plaintext = "[1000,500]你(1000,250)好(1250,250)\n";
        let encrypted = encrypt_fixture(plaintext);
        let encoded = crate::crypto::base64_encode(encrypted.as_bytes());
        assert_eq!(decrypt_mrc(encoded.as_bytes()).unwrap(), plaintext);

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(plaintext.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(decrypt_mrc(&compressed).unwrap(), plaintext);
    }

    #[test]
    fn reads_utf16le_plain_mrc() {
        let plaintext = "[1000,500]你(1000,250)好(1250,250)";
        let mut encoded = vec![0xff, 0xfe];
        encoded.extend(
            plaintext
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        assert_eq!(decrypt_mrc(&encoded).unwrap(), plaintext);
    }

    fn encrypt_fixture(plaintext: &str) -> String {
        let mut utf16 = plaintext.encode_utf16().collect::<Vec<_>>();
        while !utf16.len().is_multiple_of(4) {
            utf16.push(0);
        }
        let mut words = utf16
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| {
                i64::from_le_bytes([
                    chunk[0] as u8,
                    (chunk[0] >> 8) as u8,
                    chunk[1] as u8,
                    (chunk[1] >> 8) as u8,
                    chunk[2] as u8,
                    (chunk[2] >> 8) as u8,
                    chunk[3] as u8,
                    (chunk[3] >> 8) as u8,
                ])
            })
            .collect::<Vec<_>>();
        tea_encrypt(&mut words);
        words
            .into_iter()
            .map(|word| format!("{:016x}", word as u64))
            .collect()
    }

    fn tea_encrypt(data: &mut [i64]) {
        let length = data.len();
        let mut previous = data[length - 1];
        let rounds = 6 + 52 / length as i64;
        let mut sum = 0i64;
        for _ in 0..rounds {
            sum = sum.wrapping_add(MRC_DELTA);
            let key_index = ((sum >> 2) & 3) as usize;
            for index in 0..length - 1 {
                let next = data[index + 1];
                previous = data[index].wrapping_add(mix(
                    previous,
                    next,
                    sum,
                    MRC_KEY[(index & 3) ^ key_index],
                ));
                data[index] = previous;
            }
            let next = data[0];
            previous = data[length - 1].wrapping_add(mix(
                previous,
                next,
                sum,
                MRC_KEY[((length - 1) & 3) ^ key_index],
            ));
            data[length - 1] = previous;
        }
    }
}
