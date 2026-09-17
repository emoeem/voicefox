//! 音源登录相关的会话模型。
//!
//! 对齐 music-lib 的 `model.QRLoginSession` / `model.QRLoginResult`：
//! 各平台的扫码登录流程不同，但都可以拆成「创建二维码 → 轮询状态 →
//! 成功后回写 cookie」三步，界面因此只需要处理一套状态机。

use std::collections::BTreeMap;

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
}

/// 一次扫码登录会话。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrLoginSession {
    pub source: SourceId,
    /// 轮询用的票据。
    pub key: String,
    /// 二维码内容，界面据此渲染二维码。
    pub url: String,
    /// 部分平台（QQ）返回的是二维码图片而不是链接，这里是 PNG 的 base64。
    #[serde(default)]
    pub image_png: Option<String>,
    /// 二维码有效期（秒）。
    pub expires_in: u64,
}

/// 轮询返回值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrLoginResult {
    pub status: QrLoginStatus,
    /// 供界面直接展示的状态说明。
    pub message: String,
    /// 登录成功后写入本地存储的 cookie。
    #[serde(default)]
    pub cookies: BTreeMap<String, String>,
    /// 登录成功后的账号显示名。
    #[serde(default)]
    pub user_name: Option<String>,
}

impl QrLoginResult {
    pub fn new(status: QrLoginStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            cookies: BTreeMap::new(),
            user_name: None,
        }
    }
}
