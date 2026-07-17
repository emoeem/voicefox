pub mod scanner;
pub mod metadata;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use async_trait::async_trait;

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource, SearchError, SearchResult, SongUrl};

/// 扫描到的本地歌曲（含文件路径）
#[derive(Debug, Clone)]
pub struct LocalSong {
    pub song: SongInfo,
    pub file_path: PathBuf,
}

/// 本地音源
pub struct LocalSource {
    /// 已扫描的歌曲列表（按目录分组）
    songs: RwLock<HashMap<PathBuf, Vec<LocalSong>>>,
    /// 当前加载的目录列表
    loaded_paths: RwLock<Vec<PathBuf>>,
}

impl LocalSource {
    pub fn new() -> Self {
        Self {
            songs: RwLock::new(HashMap::new()),
            loaded_paths: RwLock::new(Vec::new()),
        }
    }

    /// 扫描指定目录并加载歌曲
    pub fn scan(&self, paths: &[String], max_depth: u32) -> Vec<String> {
        let mut all_songs: HashMap<PathBuf, Vec<LocalSong>> = HashMap::new();
        let mut errors = Vec::new();

        for path_str in paths {
            let path = PathBuf::from(path_str);
            if !path.exists() || !path.is_dir() {
                errors.push(format!("目录不存在: {}", path_str));
                continue;
            }

            let songs = scanner::scan_directory(&path, max_depth);
            let count = songs.len();
            all_songs.insert(path.canonicalize().unwrap_or(path), songs);
            if count > 0 {
                tracing::info!("扫描本地音乐: {} 首 ({})", count, path_str);
            }
        }

        *self.songs.write().unwrap() = all_songs;
        *self.loaded_paths.write().unwrap() = paths.iter().map(|p| PathBuf::from(p)).collect();

        if errors.is_empty() {
            let total: usize = self.songs.read().unwrap().values().map(|v| v.len()).sum();
            tracing::info!("本地音乐扫描完成，共 {} 首", total);
        }

        errors
    }

    /// 获取所有本地歌曲
    pub fn all_songs(&self) -> Vec<SongInfo> {
        self.songs
            .read()
            .unwrap()
            .values()
            .flat_map(|songs| songs.iter().map(|s| s.song.clone()))
            .collect()
    }

    /// 根据路径查找歌曲
    pub fn find_by_path(&self, path: &PathBuf) -> Option<SongInfo> {
        for songs in self.songs.read().unwrap().values() {
            if let Some(s) = songs.iter().find(|s| &s.file_path == path) {
                return Some(s.song.clone());
            }
        }
        None
    }

    /// 获取已加载的目录
    pub fn loaded_paths(&self) -> Vec<PathBuf> {
        self.loaded_paths.read().unwrap().clone()
    }
}

#[async_trait]
impl MusicSource for LocalSource {
    fn id(&self) -> SourceId {
        SourceId::Local
    }

    fn name(&self) -> &str {
        "本地音乐"
    }

    async fn search(
        &self,
        keyword: &str,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let all = self.all_songs();
        let keyword = keyword.to_lowercase();

        let items: Vec<SongInfo> = if keyword.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|s| {
                    s.name.to_lowercase().contains(&keyword)
                        || s.singer.to_lowercase().contains(&keyword)
                        || s.album_name.to_lowercase().contains(&keyword)
                })
                .collect()
        };

        Ok(SearchResult {
            total: items.len() as u32,
            has_more: false,
            items,
        })
    }

    async fn get_song_url(
        &self,
        song: &SongInfo,
        _quality: Quality,
    ) -> Result<SongUrl, FetchError> {
        let path = match song.file_path.as_ref() {
            Some(p) => p.clone(),
            None => {
                let p = PathBuf::from(&song.id);
                if p.exists() { p } else { return Err(FetchError::NotFound); }
            }
        };

        Ok(SongUrl {
            url: format!("file://{}", path.display()),
            quality: Quality::High320,
            duration: song.duration,
            cover_url: song.cover_url.clone(),
            qualities: vec![Quality::High320],
        })
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        if let Some(file_path) = &song.file_path {
            let lrc_path = file_path.with_extension("lrc");
            if lrc_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&lrc_path) {
                    return Ok(LyricData { lyric: content, ..Default::default() });
                }
            }
        }
        // 也尝试 id 路径
        let p = PathBuf::from(&song.id).with_extension("lrc");
        if p.exists() {
            if let Ok(content) = std::fs::read_to_string(&p) {
                return Ok(LyricData { lyric: content, ..Default::default() });
            }
        }
        Ok(LyricData::default())
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        if let Some(cover) = &song.cover_url {
            return Ok(cover.clone());
        }
        Err(FetchError::NotFound)
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![Quality::High320]
    }
}
