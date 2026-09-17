use serde::{Deserialize, Serialize};

/// 单个音源的最近一次连通性检测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceHealth {
    pub id: SourceId,
    pub name: String,
    pub ok: bool,
    pub latency_ms: u64,
    pub result_count: u32,
    pub detail: String,
}

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
    #[serde(rename = "bili")]
    Bili,
    #[serde(rename = "soda")]
    Soda,
    #[serde(rename = "qianqian")]
    Qianqian,
    #[serde(rename = "joox")]
    Joox,
    #[serde(rename = "jamendo")]
    Jamendo,
    #[serde(rename = "fivesing")]
    Fivesing,
    #[serde(rename = "apple")]
    Apple,
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
            SourceId::Bili => "bili",
            SourceId::Soda => "soda",
            SourceId::Qianqian => "qianqian",
            SourceId::Joox => "joox",
            SourceId::Jamendo => "jamendo",
            SourceId::Fivesing => "fivesing",
            SourceId::Apple => "apple",
            SourceId::Local => "local",
        }
    }

    /// 音源显示名，如 `酷我`、`汽水音乐`。
    ///
    /// 界面多处需要展示中文名，集中在这里可以避免每个页面各写一份
    /// `match`——新增音源时也只需要改这一处。
    pub fn display_name(&self) -> &'static str {
        match self {
            SourceId::Kw => "酷我",
            SourceId::Kg => "酷狗",
            SourceId::Tx => "QQ",
            SourceId::Wy => "网易云",
            SourceId::Mg => "咪咕",
            SourceId::Bili => "哔哩哔哩",
            SourceId::Soda => "汽水音乐",
            SourceId::Qianqian => "千千音乐",
            SourceId::Joox => "JOOX",
            SourceId::Jamendo => "Jamendo",
            SourceId::Fivesing => "5sing",
            SourceId::Apple => "Apple Music",
            SourceId::Local => "本地",
        }
    }

    /// 带平台标识的显示名，如 `酷我 kw`，用于设置页和切换列表。
    pub fn display_label(&self) -> &'static str {
        match self {
            SourceId::Kw => "酷我 kw",
            SourceId::Kg => "酷狗 kg",
            SourceId::Tx => "QQ tx",
            SourceId::Wy => "网易 wy",
            SourceId::Mg => "咪咕 mg",
            SourceId::Bili => "哔哩哔哩 bili",
            SourceId::Soda => "汽水音乐 soda",
            SourceId::Qianqian => "千千音乐 qianqian",
            SourceId::Joox => "JOOX joox",
            SourceId::Jamendo => "Jamendo jamendo",
            SourceId::Fivesing => "5sing fivesing",
            SourceId::Apple => "Apple Music apple",
            SourceId::Local => "本地 local",
        }
    }

    /// 已接入实现的在线音源。
    ///
    /// 只用语默认启用音源与界面遍历：新平台在实现落地前不进入这个列表，
    /// 避免用户默认启用一个还没有实现的音源。未实现的平台通过
    /// [`SourceId::all_known`] 枚举。
    pub fn all_online() -> &'static [SourceId] {
        &[
            SourceId::Kw,
            SourceId::Kg,
            SourceId::Tx,
            SourceId::Wy,
            SourceId::Mg,
            SourceId::Bili,
            SourceId::Qianqian,
            SourceId::Joox,
            SourceId::Fivesing,
            SourceId::Jamendo,
            SourceId::Apple,
            SourceId::Soda,
        ]
    }

    /// 全部已知音源（含尚未接入实现的平台）。
    pub fn all_known() -> &'static [SourceId] {
        &[
            SourceId::Kw,
            SourceId::Kg,
            SourceId::Tx,
            SourceId::Wy,
            SourceId::Mg,
            SourceId::Bili,
            SourceId::Soda,
            SourceId::Qianqian,
            SourceId::Joox,
            SourceId::Jamendo,
            SourceId::Fivesing,
            SourceId::Apple,
            SourceId::Local,
        ]
    }

    /// 默认启用的音源。
    ///
    /// 与 [`SourceId::all_online`] 的区别只有 Apple Music：它只能播放 30 秒
    /// 试听片段，默认开着会让搜索结果里出现"放不完整"的歌，因此需要用户
    /// 在设置页显式打开。其余已接入音源默认全开。
    pub fn default_enabled() -> &'static [SourceId] {
        &[
            SourceId::Kw,
            SourceId::Kg,
            SourceId::Tx,
            SourceId::Wy,
            SourceId::Mg,
            SourceId::Bili,
            SourceId::Qianqian,
            SourceId::Joox,
            SourceId::Fivesing,
            SourceId::Jamendo,
            SourceId::Soda,
        ]
    }

    /// 根据分享链接推断音源。
    ///
    /// 只匹配域名，不校验链接里的 ID：真正的解析失败由音源自己报错，
    /// 这里只负责决定把链接交给谁。顺序敏感——`5sing.kugou.com` 必须排在
    /// `kugou.com` 前面，否则 5sing 的链接会被当成酷狗。
    pub fn detect_from_link(link: &str) -> Option<SourceId> {
        const RULES: &[(&str, SourceId)] = &[
            ("5sing.kugou.com", SourceId::Fivesing),
            ("5sing.com", SourceId::Fivesing),
            ("music.163.com", SourceId::Wy),
            ("163cn.tv", SourceId::Wy),
            ("y.qq.com", SourceId::Tx),
            ("c.y.qq.com", SourceId::Tx),
            ("i.y.qq.com", SourceId::Tx),
            ("qq.com", SourceId::Tx),
            ("kuwo.cn", SourceId::Kw),
            ("kugou.com", SourceId::Kg),
            ("migu.cn", SourceId::Mg),
            ("bilibili.com", SourceId::Bili),
            ("b23.tv", SourceId::Bili),
            ("music.taihe.com", SourceId::Qianqian),
            ("qianqian.com", SourceId::Qianqian),
            // 千千音乐现在的分享域名是 91q.com（原百度音乐）。
            ("music.91q.com", SourceId::Qianqian),
            ("91q.com", SourceId::Qianqian),
            ("joox.com", SourceId::Joox),
            ("jamendo.com", SourceId::Jamendo),
            ("music.apple.com", SourceId::Apple),
            ("qishui.douyin.com", SourceId::Soda),
            ("douyin.com", SourceId::Soda),
        ];
        let host = link_host(link)?;
        RULES
            .iter()
            .find(|(domain, _)| host == *domain || host.ends_with(&format!(".{domain}")))
            .map(|(_, source)| *source)
    }
}

/// 从链接中取出小写主机名；非 http(s) 链接返回 `None`。
fn link_host(link: &str) -> Option<String> {
    let link = link.trim();
    let rest = link
        .strip_prefix("http://")
        .or_else(|| link.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?.trim();
    // 去掉 `user:pass@` 与端口，只留主机名。
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = authority.split(':').next()?.trim();
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// 音质
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub enum Quality {
    #[serde(rename = "128k")]
    #[default]
    Low128,
    #[serde(rename = "320k")]
    High320,
    #[serde(rename = "flac")]
    Flac,
    #[serde(rename = "flac24bit")]
    Flac24,
}

impl Quality {
    /// 档位标签
    pub fn label(self) -> &'static str {
        match self {
            Quality::Low128 => "128K",
            Quality::High320 => "320K",
            Quality::Flac => "FLAC",
            Quality::Flac24 => "Hi-Res",
        }
    }
}

/// 音质尝试顺序（高→低）
pub const QUALITY_ORDER: &[Quality] = &[
    Quality::Flac24,
    Quality::Flac,
    Quality::High320,
    Quality::Low128,
];

/// 音频文件的实际编码参数
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioProperties {
    /// 音频流码率（kb/s）
    pub bitrate: Option<u32>,
    /// 采样率（Hz）
    pub sample_rate: Option<u32>,
    /// 位深（bit）
    pub bit_depth: Option<u8>,
    /// 是否为无损编码
    pub lossless: bool,
}

impl AudioProperties {
    /// 实测规格标签
    ///
    /// 无损编码的码率不表示规格，取位深与采样率。有损编码的码率即规格
    pub fn label(&self) -> Option<String> {
        if self.lossless {
            return match (self.bit_depth, self.sample_rate) {
                (Some(depth), Some(rate)) => Some(format!("{}/{}", depth, format_khz(rate))),
                (None, Some(rate)) => Some(format!("{}kHz", format_khz(rate))),
                _ => None,
            };
        }
        self.bitrate.map(|bitrate| format!("{}K", bitrate))
    }
}

/// 采样率转 kHz 显示，整千值省去小数部分
fn format_khz(hz: u32) -> String {
    if hz.is_multiple_of(1000) {
        (hz / 1000).to_string()
    } else {
        format!("{:.1}", hz as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::SourceId;

    #[test]
    fn detects_sources_from_share_links() {
        assert_eq!(
            SourceId::detect_from_link("https://music.163.com/#/playlist?id=123"),
            Some(SourceId::Wy)
        );
        assert_eq!(
            SourceId::detect_from_link("http://y.qq.com/n/ryqq/songDetail/abc"),
            Some(SourceId::Tx)
        );
        assert_eq!(
            SourceId::detect_from_link("https://www.kuwo.cn/play_detail/123"),
            Some(SourceId::Kw)
        );
        assert_eq!(
            SourceId::detect_from_link("https://www.bilibili.com/video/BV1xx411c7mD"),
            Some(SourceId::Bili)
        );
    }

    #[test]
    fn five_sing_links_are_not_mistaken_for_kugou() {
        // 5sing 域名以 kugou.com 结尾，规则顺序必须把更具体的放前面。
        assert_eq!(
            SourceId::detect_from_link("https://5sing.kugou.com/yc/12345678.html"),
            Some(SourceId::Fivesing)
        );
        assert_eq!(
            SourceId::detect_from_link("https://www.kugou.com/song/#hash=abc"),
            Some(SourceId::Kg)
        );
    }

    #[test]
    fn link_detection_ignores_ports_credentials_and_non_http_input() {
        assert_eq!(
            SourceId::detect_from_link("https://user:pass@music.163.com:443/song/1"),
            Some(SourceId::Wy)
        );
        assert_eq!(SourceId::detect_from_link("周杰伦 晴天"), None);
        assert_eq!(SourceId::detect_from_link("ftp://music.163.com/x"), None);
        assert_eq!(SourceId::detect_from_link("https://example.com/x"), None);
    }
}

/// 播放器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Idle,
    Loading,
    Playing,
    Paused,
    Stopped,
}
