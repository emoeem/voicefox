//! 网易云音乐 eapi/weapi 加密工具
//!
//! eapi: AES-128-ECB + MD5 签名
//! 参考: lx-music src/renderer/utils/musicSdk/wy/

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};

/// eapi 加密
/// key: "e82ckenh8dichen8" (ASCII bytes, 16 bytes)
/// 格式: url-36cd479b6b5-json_data-36cd479b6b5-md5_digest
/// 然后 AES-128-ECB 加密，结果转 hex upper
pub fn eapi(url: &str, data: &serde_json::Value) -> String {
    use md5::Digest;

    let text = serde_json::to_string(data).unwrap();
    let message = format!("nobody{}use{}md5forencrypt", url, text);
    let digest = format!("{:x}", md5::Md5::digest(message.as_bytes()));
    let data_str = format!("{}-36cd479b6b5-{}-36cd479b6b5-{}", url, text, digest);

    // AES-128-ECB encrypt with key "e82ckenh8dichen8"
    let key = b"e82ckenh8dichen8";
    let padded = pkcs7_pad(data_str.as_bytes(), 16);
    let encrypted = aes_ecb_encrypt(&padded, key);
    hex::encode_upper(encrypted)
}

/// PKCS7 padding
fn pkcs7_pad(data: &[u8], block_size: usize) -> Vec<u8> {
    let pad_len = block_size - (data.len() % block_size);
    let mut padded = data.to_vec();
    padded.extend(std::iter::repeat_n(pad_len as u8, pad_len));
    padded
}

/// AES-128-ECB encrypt (manual block-by-block)
fn aes_ecb_encrypt(data: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut result = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        result.extend_from_slice(&block);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkcs7_pad() {
        let data = b"hello";
        let padded = pkcs7_pad(data, 16);
        assert_eq!(padded.len(), 16);
        assert_eq!(&padded[0..5], b"hello");
        assert_eq!(padded[5], 11);
        assert_eq!(padded[15], 11);
    }

    #[test]
    fn test_pkcs7_pad_exact_block() {
        let data = b"hello12345678901"; // 16 bytes
        let padded = pkcs7_pad(data, 16);
        assert_eq!(padded.len(), 32);
        assert_eq!(padded[16], 16);
    }

    #[test]
    fn test_eapi_basic() {
        let url = "/api/search/song/list/page";
        let data = serde_json::json!({"keyword": "test", "limit": 30});
        let result = eapi(url, &data);
        // Should be hex-encoded uppercase, non-empty
        assert!(!result.is_empty());
        assert!(result.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
