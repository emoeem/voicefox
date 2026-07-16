use serde::{Deserialize, Serialize};

use super::source::SourceId;

/// 歌单/专辑/歌手等集合元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub source: SourceId,
    pub cover_url: Option<String>,
    pub song_count: u32,
    pub description: Option<String>,
    pub play_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub source: SourceId,
    pub cover_url: Option<String>,
    pub artist: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub source: SourceId,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Tag {
    pub id: String,
    pub name: String,
}
