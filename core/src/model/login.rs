//! 音源登录相关的会话模型。
//!
//! 对齐 music-lib 的 `model.QRLoginSession` / `model.QRLoginResult`：
//! 各平台的扫码登录流程不同，但都可以拆成「创建二维码 → 轮询状态 →
//! 成功后回写 cookie」三步，界面因此只需要处理一套状态机。

use serde::{Deserialize, Serialize};

use super::source::SourceId;

/// 扫码状态机的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QrLoginStatus {
    /// 等待扫码。
    Waiting,
    /// 已扫码，等待用户在手机上确认。
    Scanned,
    /// 登录成功，会话 cookie 已写入。
    Success,
    /// 二维码过期，需要重新生成。
    Expired,
    /// 登录失败。
    Failed,
    /// 网络暂时不可用；二维码仍然有效，应继续轮询。
    NetworkError,
    /// 服务端触发风控/验证；二维码仍然有效，应继续轮询。
    RiskControl,
    /// 服务端暂时异常；二维码仍然有效，应继续轮询。
    ServerError,
    /// 本地会话失效，需要重新生成二维码。
    InvalidSession,
}

impl QrLoginStatus {
    pub const fn is_transient(self) -> bool {
        matches!(
            self,
            Self::NetworkError | Self::RiskControl | Self::ServerError
        )
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Success | Self::Expired | Self::Failed | Self::InvalidSession
        )
    }
}

/// 同一个音源可以提供多条扫码渠道。
///
/// 典型的是 QQ 音乐：QQ 账号扫码与微信账号扫码是两套完全不同的协议，
/// 但登录成功后的登录态是同一份，所以渠道只出现在「怎么拿凭据」这一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QrLoginKind {
    /// 音源自带的那条扫码渠道（QQ 音乐=QQ 扫码，网易云/哔哩哔哩/酷狗=各自 App）。
    #[default]
    Standard,
    /// 微信扫码。目前只有 QQ 音乐提供（微信账号登录 QQ 音乐）。
    WeChat,
}

impl QrLoginKind {
    /// 界面上的补充角标；`Standard` 不需要额外标注。
    pub const fn badge(self) -> Option<&'static str> {
        match self {
            Self::Standard => None,
            Self::WeChat => Some("微信"),
        }
    }
}

/// 一次扫码登录会话.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrLoginSession {
    pub source: SourceId,
    /// 本轮扫码用的是哪条渠道。
    #[serde(default)]
    pub kind: QrLoginKind,
    /// 轮询用的票据。
    pub key: String,
    /// 二维码内容，界面据此渲染二维码。走图片二维码的渠道这里留空。
    pub url: String,
    /// 部分平台返回的是二维码图片而不是链接，这里是图片字节的 base64
    /// （QQ=PNG / 微信=JPEG），界面按图片渲染，与 `url` 二选一。
    #[serde(default)]
    pub image_png: Option<String>,
    /// 二维码有效期（秒）。
    pub expires_in: u64,
}

/// 轮询返回值。
///
/// 注意 cookie 不经过这里：登录凭据由音源侧直接写入会话存储，
/// UI 层只拿状态与账号显示名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrLoginResult {
    pub status: QrLoginStatus,
    /// 供界面直接展示的状态说明。
    pub message: String,
    /// 登录成功后的账号显示名。
    #[serde(default)]
    pub user_name: Option<String>,
}

impl QrLoginResult {
    pub fn new(status: QrLoginStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            user_name: None,
        }
    }
}
