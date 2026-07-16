//! JS 音源 MusicSource 实现
//!
//! 兼容 lx-music user API v3 的 musicInfo 请求结构，并在 Node 子进程
//! 异常退出后自动重启一次再重试。

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource, SearchError, SearchResult, SongUrl};

use super::engine::JsEngine;

pub struct JsSource {
    name: String,
    engine: Arc<std::sync::Mutex<JsEngine>>,
    default_source: String,
}

impl JsSource {
    pub fn new(name: String, engine: JsEngine, default_source: String) -> Self {
        Self {
            name,
            engine: Arc::new(std::sync::Mutex::new(engine)),
            default_source,
        }
    }

    fn get_source(&self, song: &SongInfo) -> String {
        song.extra
            .get("source")
            .cloned()
            .or_else(|| (song.source != SourceId::Local).then(|| song.source.as_str().to_string()))
            .unwrap_or_else(|| self.default_source.clone())
    }

    fn call_with_retry(
        engine: &Arc<std::sync::Mutex<JsEngine>>,
        action: &str,
        source: &str,
        info: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let first_result = {
            let guard = engine
                .lock()
                .map_err(|error| format!("JS 引擎锁失败: {error}"))?;
            guard.call_action(action, source, info)
        };
        let error = match first_result {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };

        let engine_broken = [
            "子进程",
            "写入子进程",
            "读取子进程",
            "响应超时",
            "音源处理超时",
            "no request handler",
        ]
        .iter()
        .any(|marker| error.contains(marker));
        if !engine_broken {
            return Err(error);
        }

        let path = {
            let guard = engine
                .lock()
                .map_err(|lock_error| format!("JS 引擎锁失败: {lock_error}"))?;
            guard.source_path().to_string()
        };
        std::thread::sleep(Duration::from_millis(250));
        let restarted = JsEngine::new(&path)?;
        {
            let mut guard = engine
                .lock()
                .map_err(|lock_error| format!("JS 引擎锁失败: {lock_error}"))?;
            *guard = restarted;
        }
        let guard = engine
            .lock()
            .map_err(|lock_error| format!("JS 引擎锁失败: {lock_error}"))?;
        guard.call_action(action, source, info)
    }

    fn music_info(&self, song: &SongInfo) -> serde_json::Value {
        let mut info = serde_json::Map::new();
        for (key, value) in &song.extra {
            info.insert(key.clone(), serde_json::Value::String(value.clone()));
        }

        let hash = song
            .extra
            .get("hash")
            .or_else(|| song.extra.get("FileHash"))
            .or_else(|| song.extra.get("HQFileHash"))
            .cloned();
        if let Some(hash) = hash {
            info.entry("hash")
                .or_insert(serde_json::Value::String(hash));
        }

        info.insert("songmid".into(), serde_json::Value::String(song.id.clone()));
        info.entry("songId")
            .or_insert(serde_json::Value::String(song.id.clone()));
        info.insert("name".into(), serde_json::Value::String(song.name.clone()));
        info.insert(
            "singer".into(),
            serde_json::Value::String(song.singer.clone()),
        );
        info.insert(
            "albumName".into(),
            serde_json::Value::String(song.album_name.clone()),
        );
        info.insert(
            "albumId".into(),
            serde_json::Value::String(song.album_id.clone()),
        );
        info.insert(
            "interval".into(),
            serde_json::Value::String(format_duration(song.duration)),
        );
        info.insert(
            "_interval".into(),
            serde_json::Value::Number(song.duration.as_secs().into()),
        );
        info.insert(
            "source".into(),
            serde_json::Value::String(self.get_source(song)),
        );

        let mut types = Vec::new();
        let mut types_by_quality = serde_json::Map::new();
        for quality in &song.qualities {
            let quality_name = quality_name(*quality);
            let hash = quality_hash(song, *quality);
            let mut quality_info = serde_json::Map::new();
            quality_info.insert("size".into(), serde_json::Value::Null);
            if let Some(hash) = hash {
                quality_info.insert("hash".into(), serde_json::Value::String(hash.to_string()));
            }

            let mut quality_item = quality_info.clone();
            quality_item.insert(
                "type".into(),
                serde_json::Value::String(quality_name.to_string()),
            );
            types.push(serde_json::Value::Object(quality_item));
            types_by_quality.insert(
                quality_name.to_string(),
                serde_json::Value::Object(quality_info),
            );
        }
        info.insert("types".into(), serde_json::Value::Array(types));
        info.insert("_types".into(), serde_json::Value::Object(types_by_quality));
        info.insert(
            "typeUrl".into(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
        info.entry("otherSource").or_insert(serde_json::Value::Null);
        info.entry("lrc").or_insert(serde_json::Value::Null);

        if let Some(cover_url) = &song.cover_url {
            info.entry("img")
                .or_insert_with(|| serde_json::Value::String(cover_url.clone()));
            info.entry("picUrl")
                .or_insert_with(|| serde_json::Value::String(cover_url.clone()));
        }
        if let Some(mrc_url) = song.extra.get("mrcurl") {
            info.entry("mrcUrl")
                .or_insert_with(|| serde_json::Value::String(mrc_url.clone()));
        }

        serde_json::Value::Object(info)
    }
}

fn quality_name(quality: Quality) -> &'static str {
    match quality {
        Quality::Low128 => "128k",
        Quality::High320 => "320k",
        Quality::Flac => "flac",
        Quality::Flac24 => "flac24bit",
    }
}

fn quality_hash(song: &SongInfo, quality: Quality) -> Option<&str> {
    let key = match quality {
        Quality::Low128 => "FileHash",
        Quality::High320 => "HQFileHash",
        Quality::Flac => "SQFileHash",
        Quality::Flac24 => "ResFileHash",
    };
    song.extra.get(key).map(String::as_str)
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn quality_candidates(quality: Quality) -> &'static [(&'static str, Quality)] {
    match quality {
        Quality::Flac24 => &[
            ("flac24bit", Quality::Flac24),
            ("hires", Quality::Flac24),
            ("flac", Quality::Flac),
            ("320k", Quality::High320),
            ("128k", Quality::Low128),
        ],
        Quality::Flac => &[
            ("flac", Quality::Flac),
            ("320k", Quality::High320),
            ("128k", Quality::Low128),
        ],
        Quality::High320 => &[("320k", Quality::High320), ("128k", Quality::Low128)],
        Quality::Low128 => &[("128k", Quality::Low128)],
    }
}

#[async_trait]
impl MusicSource for JsSource {
    fn id(&self) -> SourceId {
        SourceId::Local
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let info = serde_json::json!({
            "keyword": keyword,
            "page": page,
            "limit": limit,
        });
        let engine = Arc::clone(&self.engine);
        let source = self.default_source.clone();

        tokio::task::spawn_blocking(move || {
            match Self::call_with_retry(&engine, "search", &source, &info) {
                Ok(result) => parse_js_search_result(&result),
                Err(error) => Err(SearchError::Other(error)),
            }
        })
        .await
        .unwrap_or_else(|error| Err(SearchError::Other(error.to_string())))
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        let engine = Arc::clone(&self.engine);
        let duration = song.duration;
        let source = self.get_source(song);
        let music_info = self.music_info(song);

        tokio::task::spawn_blocking(move || {
            let declared_qualities = {
                let guard = engine
                    .lock()
                    .map_err(|error| FetchError::Other(error.to_string()))?;
                if !guard.supports_source(&source) {
                    return Err(FetchError::Other(format!("JS 音源不支持平台 {source}")));
                }
                guard.supported_qualities(&source)
            };
            let (quality_str, actual_quality) = quality_candidates(quality)
                .iter()
                .find(|(name, _)| declared_qualities.iter().any(|item| item == name))
                .copied()
                .ok_or_else(|| {
                    FetchError::Other(format!(
                        "JS 音源不支持请求音质，平台 {source} 支持: {}",
                        declared_qualities.join(", ")
                    ))
                })?;
            let info = serde_json::json!({
                "type": quality_str,
                "musicInfo": music_info,
            });

            match Self::call_with_retry(&engine, "musicUrl", &source, &info) {
                Ok(result) => {
                    let url = result
                        .as_str()
                        .or_else(|| result["url"].as_str())
                        .or_else(|| result["data"]["url"].as_str())
                        .filter(|url| !url.is_empty())
                        .ok_or(FetchError::NotFound)?
                        .to_string();

                    if !url.starts_with("http://") && !url.starts_with("https://") {
                        return Err(FetchError::NotFound);
                    }

                    Ok(SongUrl {
                        url,
                        quality: actual_quality,
                        duration,
                        cover_url: extract_named_image_url(&result),
                        qualities: declared_qualities
                            .iter()
                            .filter_map(|quality| match quality.as_str() {
                                "128k" => Some(Quality::Low128),
                                "320k" => Some(Quality::High320),
                                "flac" => Some(Quality::Flac),
                                "flac24bit" | "hires" => Some(Quality::Flac24),
                                _ => None,
                            })
                            .collect(),
                    })
                }
                Err(error) => Err(FetchError::Other(error)),
            }
        })
        .await
        .unwrap_or_else(|error| Err(FetchError::Other(error.to_string())))
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        let info = serde_json::json!({
            "type": "lyric",
            "musicInfo": self.music_info(song),
        });
        let engine = Arc::clone(&self.engine);
        let source = self.get_source(song);

        tokio::task::spawn_blocking(move || {
            match Self::call_with_retry(&engine, "lyric", &source, &info) {
                Ok(result) => {
                    let data = result.get("data").unwrap_or(&result);
                    Ok(LyricData {
                        lyric: data["lyric"].as_str().unwrap_or_default().to_string(),
                        tlyric: data["tlyric"].as_str().map(str::to_string),
                        rlyric: data["rlyric"].as_str().map(str::to_string),
                        lxlyric: data["lxlyric"].as_str().map(str::to_string),
                        raw_lrc: None,
                    })
                }
                Err(_) => Ok(LyricData::default()),
            }
        })
        .await
        .unwrap_or_else(|_| Ok(LyricData::default()))
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        let info = serde_json::json!({
            "type": "pic",
            "musicInfo": self.music_info(song),
        });
        let engine = Arc::clone(&self.engine);
        let source = self.get_source(song);

        tokio::task::spawn_blocking(move || {
            match Self::call_with_retry(&engine, "pic", &source, &info) {
                Ok(result) => extract_cover_response_url(&result).ok_or(FetchError::NotFound),
                Err(error) => Err(FetchError::Other(error)),
            }
        })
        .await
        .unwrap_or_else(|error| Err(FetchError::Other(error.to_string())))
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![
            Quality::Low128,
            Quality::High320,
            Quality::Flac,
            Quality::Flac24,
        ]
    }
}

fn get_str<'a>(val: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        val.get(key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
    })
}

fn normalize_remote_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("//") {
        Some(format!("https:{value}"))
    } else if value.starts_with("http://") || value.starts_with("https://") {
        Some(value.to_string())
    } else {
        None
    }
}

fn extract_named_image_url(value: &serde_json::Value) -> Option<String> {
    const IMAGE_KEYS: &[&str] = &[
        "pic",
        "img",
        "cover",
        "image",
        "picUrl",
        "coverUrl",
        "imageUrl",
        "albumPic",
        "albumPicUrl",
    ];

    match value {
        serde_json::Value::Object(object) => {
            for key in IMAGE_KEYS {
                if let Some(url) = object
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .and_then(normalize_remote_url)
                {
                    return Some(url);
                }
            }
            object.values().find_map(extract_named_image_url)
        }
        serde_json::Value::Array(values) => values.iter().find_map(extract_named_image_url),
        _ => None,
    }
}

fn extract_cover_response_url(value: &serde_json::Value) -> Option<String> {
    if let Some(url) = value.as_str().and_then(normalize_remote_url) {
        return Some(url);
    }
    if let Some(url) = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_remote_url)
    {
        return Some(url);
    }
    if let Some(url) = extract_named_image_url(value) {
        return Some(url);
    }
    ["data", "result", "body"]
        .iter()
        .filter_map(|key| value.get(*key))
        .find_map(extract_cover_response_url)
}

fn get_duration(value: &serde_json::Value) -> Duration {
    if let Some(seconds) = value.as_u64() {
        return Duration::from_secs(seconds);
    }
    let Some(text) = value.as_str() else {
        return Duration::ZERO;
    };
    let mut parts = text.split(':').rev();
    let seconds = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .unwrap_or(0);
    let minutes = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .unwrap_or(0);
    Duration::from_secs(minutes * 60 + seconds)
}

fn parse_js_search_result(result: &serde_json::Value) -> Result<SearchResult, SearchError> {
    let list = result
        .get("list")
        .or_else(|| result.get("data"))
        .or_else(|| result.get("items"))
        .and_then(|value| value.as_array());

    let Some(list) = list else {
        return Ok(SearchResult {
            items: vec![],
            total: 0,
            has_more: false,
        });
    };

    let total = result
        .get("total")
        .and_then(|value| value.as_u64())
        .unwrap_or(list.len() as u64) as u32;
    let limit = result
        .get("limit")
        .and_then(|value| value.as_u64())
        .unwrap_or(30) as u32;
    let mut items = Vec::with_capacity(list.len());

    for item in list {
        let song_id = get_str(item, &["songmid", "id", "songId"]).unwrap_or_default();
        if song_id.is_empty() {
            continue;
        }
        let name = get_str(item, &["songname", "name", "title", "songName"]).unwrap_or_default();
        let singer =
            get_str(item, &["singer", "artist", "author", "singerName"]).unwrap_or_default();
        let source = get_str(item, &["source", "platform"]).unwrap_or("kw");
        let album_name =
            get_str(item, &["albumname", "album", "albumName", "album_name"]).unwrap_or_default();
        let album_id = get_str(item, &["albumid", "albumId", "album_id"]).unwrap_or_default();

        let mut song = SongInfo::new(
            song_id.to_string(),
            SourceId::Local,
            name.to_string(),
            singer.to_string(),
        );
        song.album_name = album_name.to_string();
        song.album_id = album_id.to_string();
        song.duration = get_duration(
            item.get("interval")
                .or_else(|| item.get("duration"))
                .or_else(|| item.get("dt"))
                .unwrap_or(&serde_json::Value::Null),
        );
        song.extra.insert("source".into(), source.to_string());

        if let Some(obj) = item.as_object() {
            for (key, value) in obj {
                match value {
                    serde_json::Value::String(value) => {
                        song.extra.insert(key.clone(), value.clone());
                    }
                    serde_json::Value::Number(value) => {
                        song.extra.insert(key.clone(), value.to_string());
                    }
                    _ => {}
                }
            }
        }
        song.cover_url = extract_named_image_url(item);
        items.push(song);
    }

    let has_more = items.len() >= limit as usize;
    Ok(SearchResult {
        items,
        total,
        has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::{extract_cover_response_url, extract_named_image_url};

    #[test]
    fn extracts_nested_cover_from_music_url_response() {
        let value = serde_json::json!({
            "url": "https://example.com/audio.flac",
            "data": {
                "cover": {
                    "picUrl": "//img.example.com/cover.webp"
                }
            }
        });

        assert_eq!(
            extract_named_image_url(&value).as_deref(),
            Some("https://img.example.com/cover.webp")
        );
    }

    #[test]
    fn extracts_common_pic_action_response_shapes() {
        for value in [
            serde_json::json!("https://img.example.com/a.jpg"),
            serde_json::json!({"url": "https://img.example.com/b.jpg"}),
            serde_json::json!({"data": {"img": "https://img.example.com/c.jpg"}}),
            serde_json::json!({"result": {"coverUrl": "//img.example.com/d.jpg"}}),
        ] {
            assert!(extract_cover_response_url(&value).is_some());
        }
    }
}
