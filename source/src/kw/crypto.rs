//! kw XOR 加密工具

/// kw 歌词接口 XOR 加密
/// key: "yeelion"
pub mod kw_lyric_crypto {
    const KEY: &[u8] = b"yeelion";

    pub fn encrypt(data: &[u8]) -> Vec<u8> {
        data.iter()
            .enumerate()
            .map(|(i, b)| b ^ KEY[i % KEY.len()])
            .collect()
    }

    pub fn decrypt(data: &[u8]) -> Vec<u8> {
        encrypt(data) // XOR is symmetric
    }
}
