//! mg 播放 URL 获取
//!
//! 流程:
//! 1. POST https://c.musicapp.migu.cn/MIGUM2.0/v1.0/content/resourceinfo.do → 获取 newRateFormats
//! 2. 根据 quality 选择对应 formatType，读取与之配对的体积字段
//! 3. 封面从 albumImgs[0] 取
//! 4. 取直链：resourseinfo 里带 url 就直接用，否则调 listenSong.do 拿 302 Location
//!    （需要 contentId + toneFlag + resourceType，无需登录；VIP 曲目该接口会报错）

use crate::http::SendWithRetry;
use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};

use super::super::http;

/// 咪咕下载接口：返回 302，`Location` 就是 CDN 直链。
const LISTEN_SONG_API: &str = "http://app.pd.nf.migu.cn/MIGUM2.0/v1.0/content/sub/listenSong.do";
/// 官方客户端里内置的公共 userId，匿名请求走这个即可。
const MAGIC_USER_ID: &str = "15548614588710179085069";
const LISTEN_SONG_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 9_1 like Mac OS X) AppleWebKit/601.1.46 (KHTML, like Gecko) Version/9.0 Mobile/13B143 Safari/601.1";
const LISTEN_SONG_REFERER: &str = "http://music.migu.cn/";
const STRATEGY_URL: &str = "https://c.musicapp.migu.cn/strategy/listen-url/h5/v2.4";
const MIGU_MAGIC: &[u8] = b"\xab\xcd\x01";
const MIGU_KEY: &[u8] = b"Jk8qzuePiJ1qE3mDYhLQ3T73DtDoAhLP";

/// Quality → mg formatType 映射
fn quality_to_format(quality: Quality) -> &'static str {
    match quality {
        Quality::Low128 => "PQ",
        Quality::High320 => "HQ",
        Quality::Flac => "SQ",
        Quality::Flac24 => "ZQ",
    }
}

/// 同一音质在响应里可能有多个文件变体（通用 / Android / iOS）。
///
/// 各变体的体积字段名不同，必须与所选 URL 变体配对读取：
/// `url` 配 `size`、`androidUrl` 配 `androidSize`、`iosUrl` 配 `iosSize`。
/// 实测 SQ 的 `size`/`androidSize` 是 FLAC（31529675），`iosSize` 是 m4a（31931278），
/// 长度并不相同，配错会误判完整性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileVariant {
    /// 通用地址，客户端按设备自行协商，优先使用。
    Generic,
    Android,
    Ios,
}

impl FileVariant {
    /// 候选顺序：通用 → Android → iOS。
    const ORDER: [FileVariant; 3] = [FileVariant::Generic, FileVariant::Android, FileVariant::Ios];

    fn url_key(self) -> &'static str {
        match self {
            FileVariant::Generic => "url",
            FileVariant::Android => "androidUrl",
            FileVariant::Ios => "iosUrl",
        }
    }

    /// 体积字段优先级，旧版接口用 `asize`/`isize`，新版用 `androidSize`/`iosSize`。
    fn size_keys(self) -> &'static [&'static str] {
        match self {
            FileVariant::Generic => &[
                "size",
                "fileSize",
                "filesize",
                "androidSize",
                "iosSize",
                "asize",
                "isize",
            ],
            FileVariant::Android => &["androidSize", "asize", "size", "fileSize", "filesize"],
            FileVariant::Ios => &["iosSize", "isize", "size", "fileSize", "filesize"],
        }
    }
}

fn format_type_matches(format: &serde_json::Value, quality: Quality) -> bool {
    let fmt_type = format["formatType"].as_str().unwrap_or("");
    fmt_type == quality_to_format(quality) || (quality == Quality::Flac24 && fmt_type == "ZQ24")
}

fn format_url(format: &serde_json::Value, variant: FileVariant) -> Option<String> {
    format[variant.url_key()]
        .as_str()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

/// 读取与 URL 变体配对的体积字段。
fn extract_size(format: &serde_json::Value, variant: FileVariant) -> Option<u64> {
    variant
        .size_keys()
        .iter()
        .find_map(|key| parse_size(&format[*key]))
}

fn parse_size(value: &serde_json::Value) -> Option<u64> {
    if let Some(bytes) = value.as_u64() {
        return (bytes > 0).then_some(bytes);
    }
    value.as_str().and_then(parse_size_text)
}

/// 解析体积字段：可能是纯字节数（`"4317311"`），也可能带单位（`"3.5MB"`）。
fn parse_size_text(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let digits_end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits_end);
    let value: f64 = number.trim().parse().ok()?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = (value * multiplier).round();
    (bytes >= 1.0).then_some(bytes as u64)
}

fn push_candidate(candidates: &mut Vec<String>, url: &str) {
    let url = url.trim();
    if url.is_empty() || candidates.iter().any(|existing| existing == url) {
        return;
    }
    candidates.push(url.to_string());
}

/// 咪咕用 URL 片段/查询参数标记试听片段，这类地址不是完整音频，必须拒掉。
fn explicit_preview_url(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return false;
    }
    // 不引入 url crate：手工拆出 path 与 query 就够用。
    let after_scheme = raw.split_once("://").map_or(raw, |(_, rest)| rest);
    let (path_with_host, query) = match after_scheme.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (after_scheme, None),
    };
    let path = path_with_host.split_once('/').map_or("", |(_, path)| path);
    for segment in path.split('/') {
        if matches!(
            segment.to_ascii_lowercase().as_str(),
            "preview" | "audition" | "trail_audio"
        ) {
            return true;
        }
    }
    let Some(query) = query else {
        return false;
    };
    query.split('&').any(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if !matches!(
            key.to_ascii_lowercase().as_str(),
            "preview" | "is_preview" | "node_is_preview" | "audition" | "trail_audio"
        ) {
            return false;
        }
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

/// 解析咪咕 H5 strategy 接口的轻量加密响应。
fn decrypt_strategy_response(raw: &[u8]) -> Result<serde_json::Value, FetchError> {
    let plain = if raw.starts_with(MIGU_MAGIC) && raw.len() >= 4 {
        let seed = raw[3];
        raw[4..]
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                byte.wrapping_add(seed)
                    .wrapping_sub(MIGU_KEY[index % MIGU_KEY.len()])
            })
            .collect::<Vec<_>>()
    } else {
        raw.to_vec()
    };
    serde_json::from_slice(&plain).map_err(|error| FetchError::Parse(error.to_string()))
}

/// 新版 H5 策略接口。优先拿官方返回的完整直链，失败再回退旧 listenSong.do。
async fn fetch_strategy_url(
    resource_id: &str,
    copyright_id: &str,
    tone_flag: &str,
    resource_type: &str,
) -> Result<(String, Option<u64>), FetchError> {
    let client = http::client();
    let resp = client
        .get(STRATEGY_URL)
        .query(&[
            ("contentId", resource_id),
            ("copyrightId", copyright_id),
            ("resourceType", resource_type),
            ("netType", "01"),
            ("toneFlag", tone_flag),
            ("scene", ""),
            ("lowerQualityContentId", resource_id),
        ])
        .header("Content-Type", "application/json;charset=UTF-8")
        .header("birth", "h5page")
        .header("signature", "1")
        .header("User-Agent", LISTEN_SONG_UA)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let json = decrypt_strategy_response(&bytes)?;
    let data = &json["data"];
    let url = data["url"]
        .as_str()
        .map(str::trim)
        .filter(|url| url.starts_with("http"));
    let Some(url) = url else {
        return Err(FetchError::NotFound);
    };
    let duration = data["song"]["duration"]
        .as_u64()
        .or_else(|| data["duration"].as_u64());
    Ok((url.to_string(), duration))
}

/// 调 listenSong.do 拿 302 直链。
///
/// `tone_flag` 就是 resourceinfo 里的 formatType（PQ/HQ/SQ/ZQ），
/// `resource_type` 取该 format 自己的值（MP3 档是 `2`，无损档常见 `E`）。
async fn fetch_listen_url(
    resource_id: &str,
    tone_flag: &str,
    resource_type: &str,
) -> Result<String, FetchError> {
    let client = http::client_without_redirect();
    let resp = client
        .get(LISTEN_SONG_API)
        .query(&[
            ("toneFlag", tone_flag),
            ("netType", "00"),
            ("userId", MAGIC_USER_ID),
            ("ua", "Android_migu"),
            ("version", "5.1"),
            ("copyrightId", "0"),
            ("contentId", resource_id),
            ("resourceType", resource_type),
            ("channel", "0"),
        ])
        .header("User-Agent", LISTEN_SONG_UA)
        .header("Referer", LISTEN_SONG_REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let status = resp.status();
    if status.is_redirection() {
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(FetchError::NotFound)?;
        if explicit_preview_url(location) {
            return Err(FetchError::NotFound);
        }
        return Ok(location.to_string());
    }

    // 非 3xx：接口用 JSON 里的 code 说明原因（VIP/无版权/参数问题）。
    let body = resp.text().await.unwrap_or_default();
    let code = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| json["code"].as_str().map(str::to_string))
        .unwrap_or_else(|| status.as_u16().to_string());
    tracing::debug!(
        "咪咕 listenSong 未返回直链: toneFlag={tone_flag} resourceType={resource_type} code={code}"
    );
    Err(FetchError::NotFound)
}

/// 探测直链真实大小：优先 HEAD，拿不到长度再用 `Range: bytes=0-0`。
///
/// 咪咕匿名请求实测**一律只发 PQ(128k)**：toneFlag=HQ/SQ/ZQ 都返回同一个
/// 3.99MB 的 mp3。若照抄 resourceinfo 的 HQ/SQ 体积声明，下载侧会因为
/// 「实际比声明短」判为完整性失败，所以这里以服务端实际提供的文件为准。
async fn probe_served_size(url: &str) -> Option<u64> {
    let client = http::client();
    let request = |builder: reqwest::RequestBuilder| {
        builder
            .header("User-Agent", LISTEN_SONG_UA)
            .header("Referer", LISTEN_SONG_REFERER)
    };
    if let Ok(resp) = request(client.head(url)).send().await
        && resp.status().is_success()
        && let Some(length) = resp.content_length().filter(|&length| length > 0)
    {
        return Some(length);
    }
    let resp = request(client.get(url))
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .await
        .ok()?;
    if resp.status().as_u16() == 206
        && let Some(total) = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next())
            .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return Some(total);
    }
    resp.content_length().filter(|&length| length > 0)
}

/// formatType → 音质（与服务端实际提供的文件对应）。
fn format_type_quality(format_type: &str) -> Option<Quality> {
    match format_type.trim() {
        "PQ" => Some(Quality::Low128),
        "HQ" => Some(Quality::High320),
        "SQ" => Some(Quality::Flac),
        "ZQ" | "ZQ24" => Some(Quality::Flac24),
        _ => None,
    }
}

/// 用实际文件大小反查 formats 里声明的档位，得出「真正拿到的是哪一档」。
fn quality_for_size(formats: &[serde_json::Value], served_size: u64) -> Option<Quality> {
    for format in formats {
        if format_type_quality(format["formatType"].as_str().unwrap_or_default()).is_none() {
            continue;
        }
        for variant in FileVariant::ORDER {
            if extract_size(format, variant) == Some(served_size) {
                return format_type_quality(format["formatType"].as_str().unwrap_or_default());
            }
        }
    }
    None
}

/// resourceinfo.do 的 `resourceId` 必须是 contentId：传 copyrightId 会返回空
/// `resource` 数组。老数据只有 copyrightId 时退回使用，兼容历史缓存。
fn resource_id(song: &SongInfo) -> Option<&str> {
    song.extra
        .get("contentId")
        .or_else(|| song.extra.get("copyrightId"))
        .map(String::as_str)
        .filter(|id| !id.trim().is_empty())
}

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let client = http::client();

    let resource_id = resource_id(song).ok_or(FetchError::NotFound)?;

    let url = "https://c.musicapp.migu.cn/MIGUM2.0/v1.0/content/resourceinfo.do?resourceType=2";

    let resp = client
        .post(url)
        .header("User-Agent", "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36")
        .form(&[("resourceId", resource_id)])
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(FetchError::NotFound);
    }

    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;

    let resource = match &json["resource"] {
        serde_json::Value::Array(arr) if !arr.is_empty() => &arr[0],
        _ => return Err(FetchError::NotFound),
    };

    // 封面从 albumImgs 取
    let cover_url = resource["albumImgs"]
        .as_array()
        .and_then(|imgs| imgs.first())
        .and_then(|img| img.as_str())
        .map(|s| s.to_string());

    let formats = match &resource["newRateFormats"] {
        serde_json::Value::Array(arr) => arr,
        _ => return Err(FetchError::NotFound),
    };

    // 目标音质排在最前，其余格式作为备用地址兜底，保持响应顺序。
    let target_index = formats.iter().position(|f| format_type_matches(f, quality));
    let mut ordered: Vec<usize> = target_index.into_iter().collect();
    ordered.extend((0..formats.len()).filter(|index| Some(*index) != target_index));

    let mut candidate_urls: Vec<String> = Vec::new();
    let mut primary: Option<(String, Option<u64>)> = None;
    for index in ordered {
        let format = &formats[index];
        for variant in FileVariant::ORDER {
            let Some(url) = format_url(format, variant) else {
                continue;
            };
            // 主地址只取目标音质，其它格式只作为候选，避免静默降质成为首选。
            if primary.is_none() && Some(index) == target_index {
                primary = Some((url.clone(), extract_size(format, variant)));
            }
            push_candidate(&mut candidate_urls, &url);
        }
    }

    // 当前线上接口的 resourceinfo 通常不再回直链（只有大小），
    // 这种情况改调 listenSong.do，用 302 的 Location 拿 CDN 直链，
    // 并以服务端实际给出的文件为准（匿名只有 PQ，见 probe_served_size 注释）。
    let mut achieved_quality = quality;
    let (play_url, declared_size) = match primary {
        Some(resolved) => resolved,
        None => {
            let format = target_index
                .map(|index| &formats[index])
                .ok_or(FetchError::NotFound)?;
            let tone_flag = format["formatType"]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or(FetchError::NotFound)?;
            let resource_type = format["resourceType"]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(match quality {
                    Quality::Flac | Quality::Flac24 => "E",
                    Quality::Low128 | Quality::High320 => "2",
                });
            let copyright_id = song
                .extra
                .get("copyrightId")
                .map(String::as_str)
                .unwrap_or("0");
            let strategy =
                fetch_strategy_url(resource_id, copyright_id, tone_flag, resource_type).await;
            let url = match strategy {
                Ok((url, duration)) => {
                    if let (Some(expected), Some(actual)) =
                        (Some(song.duration.as_secs()), duration)
                        && expected > 0
                        && actual > 0
                        && expected.abs_diff(actual) > 5
                    {
                        tracing::debug!(
                            "咪咕 strategy 返回时长异常: expected={expected}s actual={actual}s"
                        );
                    }
                    url
                }
                Err(error) => {
                    tracing::debug!("咪咕 H5 strategy 不可用（{error}），回退 listenSong.do");
                    fetch_listen_url(resource_id, tone_flag, resource_type).await?
                }
            };
            let served_size = probe_served_size(&url).await;
            if let Some(size) = served_size {
                if let Some(actual) = quality_for_size(formats, size) {
                    achieved_quality = actual;
                }
                if achieved_quality != quality {
                    tracing::debug!(
                        "咪咕请求 {quality:?} 实际拿到 {achieved_quality:?}（{size} 字节）"
                    );
                }
            }
            // 有探测结果就用实测大小（精确校验），否则退回元数据里的 android 变体体积。
            (
                url,
                served_size.or_else(|| extract_size(format, FileVariant::Android)),
            )
        }
    };
    // 实测大小是 CDN 真实长度，不需要 advisory；只有元数据声明的无损档位需要。
    let size_is_advisory = declared_size.is_some()
        && matches!(quality, Quality::Flac | Quality::Flac24)
        && achieved_quality == quality;

    let qualities: Vec<Quality> = song.qualities.iter().copied().collect();

    Ok(SongUrl {
        url: play_url,
        quality: achieved_quality,
        duration: song.duration,
        cover_url,
        qualities,
        headers: vec![],
        size: declared_size,
        size_is_advisory,
        md5: None,
        candidate_urls,
        max_chunk_size: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq_format() -> serde_json::Value {
        serde_json::json!({
            "formatType": "SQ",
            "format": "011002",
            "size": "31529675",
            "androidSize": "31529675",
            "iosSize": "31931278",
            "url": "http://freetyst.nf.migu.cn/a.flac",
            "androidUrl": "http://freetyst.nf.migu.cn/a-android.flac",
            "iosUrl": "http://freetyst.nf.migu.cn/a-ios.m4a",
        })
    }

    #[test]
    fn size_is_paired_with_the_chosen_url_variant() {
        let format = sq_format();
        assert_eq!(extract_size(&format, FileVariant::Generic), Some(31529675));
        assert_eq!(extract_size(&format, FileVariant::Android), Some(31529675));
        assert_eq!(extract_size(&format, FileVariant::Ios), Some(31931278));
    }

    #[test]
    fn plain_byte_text_and_human_sizes_are_supported() {
        assert_eq!(parse_size_text("4317311"), Some(4317311));
        assert_eq!(parse_size_text(" 4317311 "), Some(4317311));
        assert_eq!(parse_size_text("3.5MB"), Some(3670016));
        assert_eq!(parse_size_text("1 GB"), Some(1073741824));
        assert_eq!(parse_size_text("0"), None);
        assert_eq!(parse_size_text(""), None);
        assert_eq!(parse_size_text("abc"), None);
    }

    #[test]
    fn missing_sizes_do_not_hide_a_usable_url() {
        let format = serde_json::json!({"formatType": "PQ", "url": "http://cdn/pq.mp3"});
        assert_eq!(extract_size(&format, FileVariant::Generic), None);
        assert_eq!(
            format_url(&format, FileVariant::Generic).as_deref(),
            Some("http://cdn/pq.mp3")
        );
    }

    #[test]
    fn flac24_accepts_the_zq24_format_type() {
        let format = serde_json::json!({"formatType": "ZQ24"});
        assert!(format_type_matches(&format, Quality::Flac24));
        assert!(!format_type_matches(&format, Quality::Flac));
    }

    #[test]
    fn candidate_urls_are_deduplicated() {
        let mut candidates = Vec::new();
        push_candidate(&mut candidates, "http://cdn/a.flac");
        push_candidate(&mut candidates, "http://cdn/a.flac");
        push_candidate(&mut candidates, "  ");
        assert_eq!(candidates, vec!["http://cdn/a.flac".to_string()]);
    }

    #[test]
    fn content_id_wins_over_copyright_id() {
        let mut song = SongInfo::new(
            "3790007".to_string(),
            lx_core::model::source::SourceId::Mg,
            "晴天".to_string(),
            "周杰伦".to_string(),
        );
        song.extra
            .insert("copyrightId".to_string(), "60054701923".to_string());
        assert_eq!(resource_id(&song), Some("60054701923"));

        song.extra
            .insert("contentId".to_string(), "600902000006889366".to_string());
        assert_eq!(resource_id(&song), Some("600902000006889366"));

        song.extra.insert("contentId".to_string(), "  ".to_string());
        song.extra.remove("copyrightId");
        assert_eq!(resource_id(&song), None);
    }

    #[test]
    fn served_size_maps_back_to_the_real_quality() {
        let formats = vec![
            serde_json::json!({"formatType": "PQ", "size": "3997991"}),
            serde_json::json!({"formatType": "HQ", "size": "9994661"}),
            serde_json::json!({"formatType": "SQ", "size": "30477128", "androidSize": "30477128", "iosSize": "31246068"}),
        ];
        // 匿名咪咕只发 PQ：即使请求的是 HQ/SQ，也要按实际文件校正音质。
        assert_eq!(quality_for_size(&formats, 3997991), Some(Quality::Low128));
        assert_eq!(quality_for_size(&formats, 9994661), Some(Quality::High320));
        assert_eq!(quality_for_size(&formats, 30477128), Some(Quality::Flac));
        assert_eq!(quality_for_size(&formats, 12345), None);
    }

    #[test]
    fn preview_urls_are_recognised() {
        assert!(explicit_preview_url(
            "https://cdn.migu.cn/audition/2019/a.mp3"
        ));
        assert!(explicit_preview_url("https://cdn.migu.cn/a.mp3?preview=1"));
        assert!(explicit_preview_url(
            "https://cdn.migu.cn/a.mp3?node_is_preview=true"
        ));
        // 正常地址与「显式关闭」的标记都算完整音频。
        assert!(!explicit_preview_url("https://cdn.migu.cn/public/a.mp3"));
        assert!(!explicit_preview_url("https://cdn.migu.cn/a.mp3?preview=0"));
        assert!(!explicit_preview_url(""));
    }
}
