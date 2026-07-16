use serde::{Deserialize, Serialize};

/// 音源标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceId {
    #[serde(rename = "kw")]
    Kw,
    #[serde(rename = "kg")]
    Kg,
    #[serde(rename = "tx")]
    Tx,
    #[serde(rename = "wy")]
    Wy,
    #[serde(rename = "mg")]
    Mg,
    #[serde(rename = "local")]
    Local,
}

impl SourceId {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceId::Kw => "kw",
            SourceId::Kg => "kg",
            SourceId::Tx => "tx",
            SourceId::Wy => "wy",
            SourceId::Mg => "mg",
            SourceId::Local => "local",
        }
    }

    pub fn all_online() -> &'static [SourceId] {
        &[SourceId::Kw, SourceId::Kg, SourceId::Tx, SourceId::Wy, SourceId::Mg]
    }
}

/// 音质
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Quality {
    #[serde(rename = "128k")]
    Low128,
    #[serde(rename = "320k")]
    High320,
    #[serde(rename = "flac")]
    Flac,
    #[serde(rename = "flac24bit")]
    Flac24,
}

/// 音质尝试顺序（高→低）
pub const QUALITY_ORDER: &[Quality] = &[
    Quality::Flac24,
    Quality::Flac,
    Quality::High320,
    Quality::Low128,
];

/// 播放器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Idle,
    Loading,
    Playing,
    Paused,
    Stopped,
}
