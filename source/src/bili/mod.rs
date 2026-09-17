//! 哔哩哔哩音源：视频音乐搜索、播放、热门推荐和二维码登录。

mod leaderboard;
mod playlist;
mod search;
mod url;

use std::collections::BTreeMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use md5::Digest;
use serde_json::Value;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::login::{QrLoginResult, QrLoginSession, QrLoginStatus};
use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

use crate::http;
use crate::http::SendWithRetry;
use crate::session::{SessionStore, SourceSession};

pub(crate) fn looks_like_video_reference(input: &str) -> bool {
    search::looks_like_video_reference(input)
}

pub(crate) const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:141.0) Gecko/20100101 Firefox/141.0";
pub(crate) const BILI_REFERER: &str = "https://www.bilibili.com/";

const WBI_MIXIN_KEY_TABLE: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 参与请求的 cookie。站点还会下发埋点、设备指纹等 cookie，它们与接口
/// 无关，带上只会让请求头随会话膨胀，因此这里只保留接口需要的部分。
const BILI_COOKIE_NAMES: &[&str] = &["SESSDATA", "bili_jct", "buvid3", "buvid4", "DedeUserID"];

fn cookie_header(session: &SourceSession) -> Option<String> {
    session.cookie_header_of(BILI_COOKIE_NAMES)
}

/// B 站以 SESSDATA 作为登录凭据，其余 cookie 缺失不影响已登录判定。
fn has_login_cookie(session: &SourceSession) -> bool {
    session.has_cookie("SESSDATA")
}

#[derive(Debug, Clone)]
pub struct BiliUser {
    pub name: String,
    pub id: String,
    pub avatar: String,
}

#[derive(Debug, Clone)]
pub struct BiliQrCode {
    pub url: String,
    pub key: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiliQrStatus {
    Waiting,
    Scanned,
    Expired,
    Success,
}

#[derive(Debug, Clone)]
pub struct BiliQrPoll {
    pub status: BiliQrStatus,
    pub user: Option<BiliUser>,
}

#[derive(Debug, Clone)]
struct WbiKeys {
    img_key: String,
    sub_key: String,
}

pub struct BiliSource {
    store: SessionStore,
    wbi_keys: RwLock<Option<WbiKeys>>,
    session_generation: AtomicU64,
}

impl BiliSource {
    pub fn new() -> Self {
        Self {
            store: SessionStore::load(SourceId::Bili),
            wbi_keys: RwLock::new(None),
            session_generation: AtomicU64::new(0),
        }
    }

    pub fn is_logged_in(&self) -> bool {
        has_login_cookie(&self.store.snapshot())
    }

    pub fn session(&self) -> SourceSession {
        self.store.snapshot()
    }

    pub fn user(&self) -> Option<BiliUser> {
        let session = self.store.snapshot();
        match (
            session.user_name.clone(),
            session.user_id.clone(),
            session.extra("avatar").map(str::to_string),
        ) {
            (Some(name), Some(id), Some(avatar)) => Some(BiliUser { name, id, avatar }),
            _ => None,
        }
    }

    /// 获取一个视频的全部分 P。搜索、榜单和收藏夹接口通常只返回视频级
    /// 条目，播放前通过 view 接口展开，才能让队列逐 P 播放。
    pub async fn video_parts(&self, song: &SongInfo) -> Result<Vec<SongInfo>, FetchError> {
        let bvid = song
            .extra
            .get("bvid")
            .map(String::as_str)
            .unwrap_or(&song.id);
        search::fetch_video_parts(self, bvid)
            .await
            .map(|result| result.items)
            .map_err(|error| FetchError::Other(error.to_string()))
    }

    pub fn logout(&self) -> Result<(), String> {
        self.session_generation.fetch_add(1, Ordering::SeqCst);
        self.store.clear()
    }

    pub async fn login_status(&self) -> Result<Option<BiliUser>, String> {
        let generation = self.session_generation.load(Ordering::SeqCst);
        let json = self
            .get_json("https://api.bilibili.com/x/web-interface/nav", &[], false)
            .await?;
        if self.session_generation.load(Ordering::SeqCst) != generation {
            return Ok(self.is_logged_in().then(|| self.user()).flatten());
        }
        if json["code"].as_i64() != Some(0) {
            return Err(api_error(&json, "检查哔哩哔哩登录状态失败"));
        }
        if json["data"]["isLogin"] != true {
            self.logout()?;
            return Ok(None);
        }
        let user = parse_user(&json)?;
        if self.session_generation.load(Ordering::SeqCst) != generation {
            return Ok(self.is_logged_in().then(|| self.user()).flatten());
        }
        self.store.update(|session| {
            session.user_name = Some(user.name.clone());
            session.user_id = Some(user.id.clone());
            session.set_extra("avatar", &user.avatar);
        })?;
        Ok(Some(user))
    }

    pub async fn generate_qr_code(&self) -> Result<BiliQrCode, String> {
        let json = http::client()
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")
            .header("User-Agent", USER_AGENT)
            .header("Referer", BILI_REFERER)
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await
            .map_err(|error| error.to_string())?
            .json::<Value>()
            .await
            .map_err(|error| error.to_string())?;
        if json["code"].as_i64() != Some(0) {
            return Err(api_error(&json, "生成哔哩哔哩二维码失败"));
        }
        let url = json["data"]["url"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "哔哩哔哩二维码地址为空".to_string())?;
        let key = json["data"]["qrcode_key"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "哔哩哔哩二维码 key 为空".to_string())?;
        Ok(BiliQrCode {
            url: url.to_string(),
            key: key.to_string(),
            expires_in: 180,
        })
    }

    pub async fn poll_qr_code(&self, key: &str) -> Result<BiliQrPoll, String> {
        let url = format!(
            "https://passport.bilibili.com/x/passport-login/web/qrcode/poll?qrcode_key={}",
            urlencoding::encode(key)
        );
        let response = http::client()
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", BILI_REFERER)
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await
            .map_err(|error| error.to_string())?;
        let headers = response.headers().clone();
        let json = response
            .json::<Value>()
            .await
            .map_err(|error| error.to_string())?;
        // B 站 web 扫码 API 返回两层 code：json.code 是 HTTP 状态，
        // json.data.code 才是真正的扫码状态码。
        let outer_code = json["code"].as_i64().unwrap_or(-1);
        if outer_code != 0 {
            return Err(api_error(&json, "检查哔哩哔哩二维码失败"));
        }
        let data = &json["data"];
        let code = data["code"].as_i64().unwrap_or(-1);
        let status = match code {
            0 => {
                // 真正的登录成功：data 中有 url / refresh_token，Set-Cookie 中有 SESSDATA
                self.capture_login_session(&headers, data)?;
                let user = self
                    .login_status()
                    .await?
                    .ok_or_else(|| "哔哩哔哩会话校验失败，请重新扫码".to_string())?;
                return Ok(BiliQrPoll {
                    status: BiliQrStatus::Success,
                    user: Some(user),
                });
            }
            86101 => BiliQrStatus::Waiting,
            86090 => BiliQrStatus::Scanned,
            86038 => BiliQrStatus::Expired,
            _ => return Err(qr_api_error(data)),
        };
        Ok(BiliQrPoll { status, user: None })
    }

    fn capture_login_session(
        &self,
        headers: &reqwest::header::HeaderMap,
        data: &Value,
    ) -> Result<(), String> {
        let mut session = self.store.snapshot();
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
            if let Ok(value) = value.to_str() {
                session.apply_set_cookie(value);
            }
        }
        if let Some(url) = data["url"].as_str()
            && !url.is_empty()
        {
            for pair in url.split('?').nth(1).unwrap_or_default().split('&') {
                if let Some((name, value)) = pair.split_once('=') {
                    let value = urlencoding::decode(value)
                        .map(|value| value.into_owned())
                        .unwrap_or_else(|_| value.to_string());
                    session.set_cookie(name, &value);
                }
            }
        }
        if let Some(value) = data["refresh_token"].as_str()
            && !value.is_empty()
        {
            session.set_extra("refresh_token", value);
        }
        if !has_login_cookie(&session) {
            return Err("哔哩哔哩登录成功，但没有收到会话 cookie".to_string());
        }
        self.store.replace(session)?;
        self.session_generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub(crate) async fn get_json(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
        signed: bool,
    ) -> Result<Value, String> {
        let mut params = params.iter().cloned().collect::<BTreeMap<_, _>>();
        if signed {
            let keys = self.wbi_keys().await?;
            params.insert("wts", unix_timestamp().to_string());
            let query = encode_wbi_query(&params, &keys);
            return self.request_json(format!("{endpoint}?{query}")).await;
        }
        let query = params
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    urlencoding::encode(key),
                    urlencoding::encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let url = if query.is_empty() {
            endpoint.to_string()
        } else {
            format!("{endpoint}?{query}")
        };
        self.request_json(url).await
    }

    async fn request_json(&self, url: String) -> Result<Value, String> {
        let mut request = http::client()
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", BILI_REFERER);
        if let Some(cookie) = cookie_header(&self.store.snapshot()) {
            request = request.header("Cookie", cookie);
        }
        request
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await
            .map_err(|error| error.to_string())?
            .json::<Value>()
            .await
            .map_err(|error| error.to_string())
    }

    async fn wbi_keys(&self) -> Result<WbiKeys, String> {
        if let Some(keys) = self
            .wbi_keys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Ok(keys);
        }
        let json = self
            .request_json("https://api.bilibili.com/x/web-interface/nav".to_string())
            .await?;
        if json["code"].as_i64() != Some(0) {
            return Err(api_error(&json, "获取哔哩哔哩 WBI 密钥失败"));
        }
        let img_key = key_from_url(json["data"]["wbi_img"]["img_url"].as_str());
        let sub_key = key_from_url(json["data"]["wbi_img"]["sub_url"].as_str());
        if img_key.is_empty() || sub_key.is_empty() {
            return Err("哔哩哔哩 WBI 密钥为空".to_string());
        }
        let keys = WbiKeys { img_key, sub_key };
        *self.wbi_keys.write().unwrap_or_else(|e| e.into_inner()) = Some(keys.clone());
        Ok(keys)
    }

    async fn ensure_buvid(&self) -> Result<(), String> {
        if self.store.snapshot().has_cookie("buvid3") {
            return Ok(());
        }
        let json = self
            .get_json("https://api.bilibili.com/x/frontend/finger/spi", &[], false)
            .await?;
        let b3 = json["data"]["b_3"].as_str().map(str::to_string);
        let b4 = json["data"]["b_4"].as_str().map(str::to_string);
        if b3.is_none() && b4.is_none() {
            return Ok(());
        }
        self.store.update(|session| {
            if let Some(value) = b3.as_deref() {
                session.set_cookie("buvid3", value);
            }
            if let Some(value) = b4.as_deref() {
                session.set_cookie("buvid4", value);
            }
        })?;
        Ok(())
    }

    pub(crate) async fn signed_get(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<Value, String> {
        self.ensure_buvid().await?;
        let json = self.get_json(endpoint, params, true).await?;
        // -403 表示 WBI 签名失效；-412 风控也可能是签名过期触发，同样刷新后重试
        if !matches!(json["code"].as_i64(), Some(-403) | Some(-412)) {
            return Ok(json);
        }

        *self.wbi_keys.write().unwrap_or_else(|e| e.into_inner()) = None;
        self.get_json(endpoint, params, true).await
    }
}

impl Default for BiliSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicSource for BiliSource {
    fn id(&self) -> SourceId {
        SourceId::Bili
    }

    fn name(&self) -> &str {
        "哔哩哔哩"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            playlists: true,
            album: true,
            artist: true,
            leaderboard: true,
            link_parse: true,
            login: true,
            qr_login: true,
            ..Default::default()
        }
    }

    /// 通用扫码登录入口：界面只处理 `QrLoginSession` / `QrLoginResult`，
    /// 各平台二维码协议差异由音源自己消化。
    async fn create_qr_login(&self) -> Result<QrLoginSession, FetchError> {
        let qr = self.generate_qr_code().await.map_err(FetchError::Other)?;
        Ok(QrLoginSession {
            source: SourceId::Bili,
            key: qr.key,
            url: qr.url,
            image_png: None,
            expires_in: qr.expires_in,
        })
    }

    async fn check_qr_login(&self, key: &str) -> Result<QrLoginResult, FetchError> {
        let poll = self.poll_qr_code(key).await.map_err(FetchError::Other)?;
        let result = match poll.status {
            BiliQrStatus::Waiting => QrLoginResult::new(QrLoginStatus::Waiting, "等待扫码"),
            BiliQrStatus::Scanned => {
                QrLoginResult::new(QrLoginStatus::Scanned, "已扫码，请在手机上确认")
            }
            BiliQrStatus::Expired => QrLoginResult::new(QrLoginStatus::Expired, "二维码已过期"),
            BiliQrStatus::Success => {
                // `poll_qr_code` 已经把会话写进存储，这里只回传给界面展示。
                let session = self.session();
                let mut result = QrLoginResult::new(QrLoginStatus::Success, "登录成功");
                result.cookies = session.cookies;
                result.user_name = session
                    .user_name
                    .or_else(|| poll.user.as_ref().map(|user| user.name.clone()));
                result
            }
        };
        Ok(result)
    }

    /// 哔哩哔哩的「链接直解」复用搜索：搜索入口本身就能识别 BV/av 号、
    /// 长链接与 b23.tv 短链，这里取第一条结果即可。
    async fn parse_link(&self, link: &str) -> Result<ParsedLink, FetchError> {
        let result = self
            .search(link, 1, 5)
            .await
            .map_err(|error| FetchError::Other(error.to_string()))?;
        let song = result
            .items
            .into_iter()
            .next()
            .ok_or(FetchError::NotFound)?;
        Ok(ParsedLink::Song(Box::new(song)))
    }

    fn logout(&self) -> Result<(), FetchError> {
        BiliSource::logout(self).map_err(FetchError::Other)
    }

    fn is_logged_in(&self) -> bool {
        BiliSource::is_logged_in(self)
    }

    async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        search::search(self, keyword, page, limit).await
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        url::get_song_url(self, song, quality).await
    }

    async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
        Err(FetchError::NotFound)
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        song.cover_url.clone().ok_or(FetchError::NotFound)
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![Quality::Low128, Quality::High320]
    }

    async fn get_playlists(&self, tag_id: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
        playlist::get_playlists(self, tag_id, page).await
    }

    async fn get_playlist_detail(
        &self,
        playlist_id: &str,
        page: u32,
    ) -> Result<Vec<SongInfo>, FetchError> {
        playlist::get_playlist_detail(self, playlist_id, page).await
    }

    async fn get_leaderboard_boards(&self) -> Result<Vec<LeaderboardInfo>, SearchError> {
        leaderboard::get_boards()
    }

    async fn get_leaderboard(
        &self,
        id: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        leaderboard::get_list(self, id, page, limit).await
    }
}

fn parse_user(json: &Value) -> Result<BiliUser, String> {
    let id = value_string(&json["data"]["mid"]);
    if id.is_empty() {
        return Err("哔哩哔哩用户 ID 为空".to_string());
    }
    Ok(BiliUser {
        name: json["data"]["uname"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "哔哩哔哩用户名称为空".to_string())?
            .to_string(),
        id,
        avatar: json["data"]["face"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    })
}

fn api_error(json: &Value, fallback: &str) -> String {
    let code = json["code"].as_i64().unwrap_or(-1);
    // -412（风控/限流）与 -352（风控校验失败）的原始 message 对用户不友好，
    // 转成明确提示并保留原始 code 便于排查
    if matches!(code, -412 | -352) {
        return format!("B 站风控拦截，请稍后再试或重新登录 (code={code})");
    }
    let message = json["message"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    format!("{message} (code={code})")
}

fn qr_api_error(data: &Value) -> String {
    let code = data["code"].as_i64().unwrap_or(-1);
    let message = data["message"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or("检查哔哩哔哩二维码失败");
    format!("{message} (code={code})")
}

fn key_from_url(value: Option<&str>) -> String {
    value
        .and_then(|value| value.rsplit('/').next())
        .and_then(|value| value.split('.').next())
        .unwrap_or_default()
        .to_string()
}

fn encode_wbi_query(params: &BTreeMap<&str, String>, keys: &WbiKeys) -> String {
    let mixed = format!("{}{}", keys.img_key, keys.sub_key);
    let mixin_key: String = WBI_MIXIN_KEY_TABLE
        .iter()
        .filter_map(|index| mixed.chars().nth(*index))
        .take(32)
        .collect();
    let encoded = params
        .iter()
        .map(|(key, value)| {
            let value = value
                .chars()
                .filter(|character| !matches!(character, '!' | '\'' | '(' | ')' | '*'))
                .collect::<String>();
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(&value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let digest = md5::Md5::digest(format!("{encoded}{mixin_key}").as_bytes());
    format!("{encoded}&w_rid={digest:x}")
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{WbiKeys, cookie_header, encode_wbi_query};
    use crate::session::SourceSession;

    #[test]
    fn cookie_header_contains_only_supported_cookie_values() {
        let mut session = SourceSession::default();
        session.set_cookie("SESSDATA", "session");
        session.set_cookie("bili_jct", "csrf");
        // 站点埋点 cookie 与接口无关，不应出现在请求头里。
        session.set_cookie("b_nut", "1234567890");
        assert_eq!(
            cookie_header(&session).as_deref(),
            Some("SESSDATA=session; bili_jct=csrf")
        );
    }

    #[test]
    fn wbi_query_is_sorted_and_signed() {
        let keys = WbiKeys {
            img_key: "a".repeat(32),
            sub_key: "b".repeat(32),
        };
        let query = encode_wbi_query(
            &[("keyword", "晴天".to_string()), ("page", "1".to_string())]
                .into_iter()
                .collect(),
            &keys,
        );
        assert!(query.starts_with("keyword="));
        assert!(query.contains("&page=1"));
        assert!(query.contains("&w_rid="));
    }
}
