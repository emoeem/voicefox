//! kw 歌词获取（zlib 压缩 + XOR 解密）
//!
//! API: GET http://newlyric.kuwo.cn/newlyric.lrc?{encryptedParams}
//!
//! 响应格式 (lx-music 最新): "tp=content\r\n\r\n" + zlib 压缩数据
//!   - 普通歌词 (不含 lrcx): zlib 解压 → UTF-8
//!   - 逐字歌词 (lrcx=1):   zlib 解压 → base64 解码 → XOR 解密

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::traits::source::FetchError;

use super::crypto::kw_lyric_crypto;
use super::super::crypto;
use super::super::http;

/// 加密请求参数：XOR → base64
fn encrypt_params(params: &str) -> String {
    let encrypted = kw_lyric_crypto::encrypt(params.as_bytes());
    crypto::base64_encode(&encrypted)
}

/// 请求原始歌词数据
///
/// `with_lrcx`: 是否请求逐字歌词（lrcx=1）
async fn fetch_raw_lyric(song_id: &str, with_lrcx: bool) -> Result<String, FetchError> {
    // 构造明文参数
    let mut params = format!(
        "user=12345,web,web,web&requester=localhost&req=1&rid=MUSIC_{}",
        song_id
    );
    if with_lrcx {
        params.push_str("&lrcx=1");
    }

    // 加密参数
    let encrypted = encrypt_params(&params);
    let url = format!("http://newlyric.kuwo.cn/newlyric.lrc?{}", encrypted);

    let client = http::client();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    // 读取原始字节（lx-music 最新 API 返回二进制 zlib 压缩数据）
    let raw_bytes = resp
        .bytes()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    if raw_bytes.is_empty() {
        return Err(FetchError::NotFound);
    }

    // 跳过 "tp=content\r\n\r\n" 头部
    let compressed = match raw_bytes.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(pos) => &raw_bytes[pos + 4..],
        None => &raw_bytes[..],
    };

    // zlib 解压
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut decoder = ZlibDecoder::new(compressed);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| FetchError::Parse(format!("zlib decompress failed: {}", e)))?;

    if !with_lrcx {
        // 普通歌词：歌词响应是 GB18030 编码
        let (cow, _, _) = encoding_rs::GB18030.decode(&decompressed);
        Ok(cow.into_owned())
    } else {
        // lrcx 逐字歌词：过滤非ASCII字节 → Base64 解码 → XOR 解密
        let base64_str: String = decompressed.iter()
            .filter(|b| b.is_ascii())
            .map(|&b| b as char)
            .collect();
        let base64_decoded = crate::crypto::base64_decode(&base64_str)
            .map_err(|e| FetchError::Parse(format!("base64 decode failed: {}", e)))?;
        let decrypted = super::crypto::kw_lyric_crypto::decrypt(&base64_decoded);
        // lrcx 解密后也是 GB18030 编码
        let (cow, _, _) = encoding_rs::GB18030.decode(&decrypted);
        Ok(cow.into_owned())
    }
}

pub async fn get_lyric(song: &SongInfo) -> Result<LyricData, FetchError> {
    let song_id = &song.id;

    // 先请求逐字歌词（lrcx=1），失败不阻断
    let lxlyric = match fetch_raw_lyric(song_id, true).await {
        Ok(text) => Some(text),
        Err(_) => None, // lrcx 失败就跳过，不影响主流程
    };

    // 再请求普通歌词（不含 lrcx）
    let lyric = fetch_raw_lyric(song_id, false).await.unwrap_or_default();

    Ok(LyricData {
        lyric,
        tlyric: None,
        rlyric: None,
        lxlyric,
        raw_lrc: None,
    })
}
