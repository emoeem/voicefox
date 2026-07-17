use super::source::SourceId;

/// 单个音源提供的排行榜入口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderboardInfo {
    pub id: String,
    pub name: String,
    pub source: SourceId,
    pub update: Option<String>,
}

impl LeaderboardInfo {
    pub fn new(id: String, name: String, source: SourceId) -> Self {
        Self {
            id,
            name,
            source,
            update: None,
        }
    }
}
