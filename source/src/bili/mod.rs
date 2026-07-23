//! 哔哩哔哩音源：视频音乐搜索、播放、热门推荐和二维码登录。

mod leaderboard;
mod playlist;
mod search;
mod url;

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use md5::Digest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource, SearchError, SearchResult, SongUrl};

use crate::http;

pub(crate) const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:141.0) Gecko/20100101 Firefox/141.0";
pub(crate) const BILI_REFERER: &str = "https://www.bilibili.com/";

const WBI_MIXIN_KEY_TABLE: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct BiliSession {
    pub sessdata: Option<String>,
    pub bili_jct: Option<String>,
    pub buvid3: Option<String>,
    pub buvid4: Option<String>,
    pub dede_user_id: Option<String>,
    pub access_key: Option<String>,
    pub refresh_token: Option<String>,
    pub user_name: Option<String>,
    pub user_id: Option<String>,
    pub avatar: Option<String>,
}

impl BiliSession {
    fn cookie_header(&self) -> Option<String> {
        let mut cookies = Vec::new();
        for (name, value) in [
            ("SESSDATA", self.sessdata.as_deref()),
            ("bili_jct", self.bili_jct.as_deref()),
            ("buvid3", self.buvid3.as_deref()),
            ("buvid4", self.buvid4.as_deref()),
            ("DedeUserID", self.dede_user_id.as_deref()),
        ] {
            if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                cookies.push(format!("{name}={value}"));
            }
        }
        (!cookies.is_empty()).then_some(cookies.join("; "))
    }

    fn has_login_cookie(&self) -> bool {
        self.sessdata
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    }
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
    session: RwLock<BiliSession>,
    wbi_keys: RwLock<Option<WbiKeys>>,
    session_generation: AtomicU64,
}

impl BiliSource {
    pub fn new() -> Self {
        Self {
            session: RwLock::new(load_session()),
            wbi_keys: RwLock::new(None),
            session_generation: AtomicU64::new(0),
        }
    }

    pub fn is_logged_in(&self) -> bool {
        self.session.read().unwrap().has_login_cookie()
    }

    pub fn session(&self) -> BiliSession {
        self.session.read().unwrap().clone()
    }

    pub fn user(&self) -> Option<BiliUser> {
        let session = self.session.read().unwrap();
        match (
            session.user_name.clone(),
            session.user_id.clone(),
            session.avatar.clone(),
        ) {
            (Some(name), Some(id), Some(avatar)) => Some(BiliUser { name, id, avatar }),
            _ => None,
        }
    }

    pub fn logout(&self) -> Result<(), String> {
        self.session_generation.fetch_add(1, Ordering::SeqCst);
        *self.session.write().unwrap() = BiliSession::default();
        remove_session_file()
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
        {
            let mut session = self.session.write().unwrap();
            if self.session_generation.load(Ordering::SeqCst) != generation {
                drop(session);
                return Ok(self.is_logged_in().then(|| self.user()).flatten());
            }
            session.user_name = Some(user.name.clone());
            session.user_id = Some(user.id.clone());
            session.avatar = Some(user.avatar.clone());
            save_session(&session)?;
        }
        Ok(Some(user))
    }

    pub async fn generate_qr_code(&self) -> Result<BiliQrCode, String> {
        let json = http::client()
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")
            .header("User-Agent", USER_AGENT)
            .header("Referer", BILI_REFERER)
            .send()
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
            .send()
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
        let mut session = self.session.write().unwrap().clone();
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
            if let Ok(value) = value.to_str() {
                parse_cookie_pair(value, &mut session);
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
                    set_cookie(&mut session, name, &value);
                }
            }
        }
        if let Some(value) = data["refresh_token"].as_str()
            && !value.is_empty()
        {
            session.refresh_token = Some(value.to_string());
        }
        if !session.has_login_cookie() {
            return Err("哔哩哔哩登录成功，但没有收到会话 cookie".to_string());
        }
        save_session(&session)?;
        *self.session.write().unwrap() = session;
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
        if let Some(cookie) = self.session.read().unwrap().cookie_header() {
            request = request.header("Cookie", cookie);
        }
        request
            .send()
            .await
            .map_err(|error| error.to_string())?
            .json::<Value>()
            .await
            .map_err(|error| error.to_string())
    }

    async fn wbi_keys(&self) -> Result<WbiKeys, String> {
        if let Some(keys) = self.wbi_keys.read().unwrap().clone() {
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
        *self.wbi_keys.write().unwrap() = Some(keys.clone());
        Ok(keys)
    }

    async fn ensure_buvid(&self) -> Result<(), String> {
        if self
            .session
            .read()
            .unwrap()
            .buvid3
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
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
        let mut session = self.session.write().unwrap();
        session.buvid3 = b3;
        session.buvid4 = b4;
        save_session(&session)?;
        Ok(())
    }

    pub(crate) async fn signed_get(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<Value, String> {
        self.ensure_buvid().await?;
        let json = self.get_json(endpoint, params, true).await?;
        if json["code"].as_i64() != Some(-403) {
            return Ok(json);
        }

        *self.wbi_keys.write().unwrap() = None;
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

fn session_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("voicefox")
        .join("bilibili.json")
}

fn load_session() -> BiliSession {
    std::fs::read_to_string(session_path())
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn save_session(session: &BiliSession) -> Result<(), String> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let content = serde_json::to_string_pretty(session).map_err(|error| error.to_string())?;
    save_session_file(&path, content.as_bytes())
}

fn save_session_file(path: &std::path::Path, content: &[u8]) -> Result<(), String> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = path.with_extension(format!("json.tmp-{}-{suffix}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let result = (|| {
        let mut file = options
            .open(&temp_path)
            .map_err(|error| error.to_string())?;
        file.write_all(content).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);

        #[cfg(unix)]
        {
            std::fs::rename(&temp_path, path).map_err(|error| error.to_string())?;
        }
        #[cfg(windows)]
        {
            if path.exists() {
                std::fs::remove_file(path).map_err(|error| error.to_string())?;
            }
            std::fs::rename(&temp_path, path).map_err(|error| error.to_string())?;
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(temp_path);
    }
    result
}

fn remove_session_file() -> Result<(), String> {
    match std::fs::remove_file(session_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("删除哔哩哔哩会话文件失败: {error}")),
    }
}

fn parse_cookie_pair(value: &str, session: &mut BiliSession) {
    if let Some(pair) = value.split(';').next()
        && let Some((name, value)) = pair.split_once('=')
    {
        set_cookie(session, name.trim(), value.trim());
    }
}

fn set_cookie(session: &mut BiliSession, name: &str, value: &str) {
    match name {
        "SESSDATA" => session.sessdata = Some(value.to_string()),
        "bili_jct" => session.bili_jct = Some(value.to_string()),
        "buvid3" => session.buvid3 = Some(value.to_string()),
        "buvid4" => session.buvid4 = Some(value.to_string()),
        "DedeUserID" => session.dede_user_id = Some(value.to_string()),
        "access_key" | "access_token" => session.access_key = Some(value.to_string()),
        "refresh_token" => session.refresh_token = Some(value.to_string()),
        _ => {}
    }
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
    use super::{BiliSession, WbiKeys, encode_wbi_query, save_session_file};

    #[test]
    fn cookie_header_contains_only_supported_cookie_values() {
        let session = BiliSession {
            sessdata: Some("session".into()),
            bili_jct: Some("csrf".into()),
            ..BiliSession::default()
        };
        assert_eq!(
            session.cookie_header().as_deref(),
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

    #[cfg(unix)]
    #[test]
    fn session_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "voicefox-bili-session-test-{}-{}.json",
            std::process::id(),
            super::unix_timestamp()
        ));
        save_session_file(&path, br#"{"sessdata":"secret"}"#).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(path);
    }
}
