use std::collections::HashMap;

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
    /// 歌单创建者，部分音源（千千、JOOX、Jamendo）会返回。
    #[serde(default)]
    pub creator: Option<String>,
    /// 平台原始链接，用于链接直解与「在网页打开」。
    #[serde(default)]
    pub link: Option<String>,
    /// 音源特有数据（如拉取详情需要的类型标记）。
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

impl Playlist {
    pub fn new(id: impl Into<String>, name: impl Into<String>, source: SourceId) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            source,
            cover_url: None,
            song_count: 0,
            description: None,
            play_count: None,
            creator: None,
            link: None,
            extra: HashMap::new(),
        }
    }
}

/// 歌单分类目录项。
///
/// 对应 music-lib 的 `model.PlaylistCategory`：`group` 用于把分类
/// 归入「语种」「风格」「场景」等分组，`hot` 标记官方推荐的热门分类。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistCategory {
    pub id: String,
    pub name: String,
    pub source: SourceId,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub hot: bool,
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

impl PlaylistCategory {
    pub fn new(id: impl Into<String>, name: impl Into<String>, source: SourceId) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            source,
            group: None,
            count: 0,
            hot: false,
            extra: HashMap::new(),
        }
    }
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
