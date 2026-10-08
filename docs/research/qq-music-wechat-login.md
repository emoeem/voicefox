# QQ 音乐微信扫码登录：调研与实现方案

- 状态：**已实现**（2026-10-08 落地；§1–§7 是当时的调研与方案，与最终实现的差异集中在 §8）
- 对应 issue：[#43 希望 QQ 音乐源登入时支持非 QQ 登录](https://github.com/emoeem/voicefox/issues/43)
- 实测日期：2026-10-08（本机 Arch Linux，全部为真实 HTTP 请求）

## 1. 结论摘要

QQ 音乐的微信登录可以接入，而且走的是 **QQ 音乐自己网页版登录在用的那条链路**，不是凭空逆向出来的私有协议：

1. 用微信开放平台的网站应用 appid **`wx48db31d50e334801`** 向 `open.weixin.qq.com/connect/qrconnect` 要一个微信登录二维码；
2. 长轮询 `lp.open.weixin.qq.com/connect/l/qrconnect` 拿 `wx_code`；
3. 调 **`music.login.LoginServer/Login`**（`comm.tmeLoginType="1"`、`param.strAppid`、`param.code`）换回 `musickey`（微信账号前缀 `W_X_`）与 `musicid`；
4. 把 `musickey` 映射成现有的 `uin` / `qqmusic_key` cookie 即可复用整条已有登录态链路。

**第一方证据**：`https://y.qq.com/portal/wx_redirect.html`（QQ 音乐自己的页面）内联 JS 做的就是第 3 步，参数与本文实测逐字一致，见 §3。

**关键否证**：现有的 `ptlogin2.qq.com` 链路 **没有微信变体** —— 给 `ptqrshow` 加 `login_type=2` / `appid=wx48db31d50e334801` / `ptlang` 全部被忽略，照发 QQ 二维码；`ptqrlogin` 没有合法 `qrsig` 直接 403（TAPISIX）。微信分支必须是**独立代码路径**，不是改参数。

## 2. 实测记录

| # | 端点 | 关键参数 | 结果 | 判定 |
|---|---|---|---|---|
| 1 | `GET https://open.weixin.qq.com/connect/qrconnect` | `appid=wx48db31d50e334801`、`redirect_uri=<encode(https://y.qq.com/portal/wx_redirect.html?login_type=2&surl=https%3A%2F%2Fy.qq.com%2F)>`、`response_type=code`、`scope=snsapi_login`、`state`、桌面 Chrome UA（`Referer: https://y.qq.com/`） | 200，43826 B HTML，标题「微信登录」，内含 `connect/qrcode/061gVkgZ2dwlGa1u` | ✅ 可用（本人复核） |
| 2 | `GET https://open.weixin.qq.com/connect/qrcode/{uuid}` | `Referer: https://open.weixin.qq.com/connect/qrconnect` | 200，`image/jpeg`，470×470 | ✅ 可用 |
| 3 | `GET https://lp.open.weixin.qq.com/connect/l/qrconnect?uuid={uuid}&_={ms}` | 长轮询 | 200（约 15s 后返回）`window.wx_errcode=408;window.wx_code='';` | ✅ 可用 |
| 4 | `GET https://long.open.weixin.qq.com/connect/l/qrconnect` | 同上 | 与 #3 等价（备用域名） | ✅ 可用 |
| 5 | `GET https://y.qq.com/portal/wx_redirect.html?login_type=2&surl=…&code=bogus` | — | 200，内联 JS 调 `music.login.LoginServer/Login`，`strAppid:"wx48db31d50e334801"` | ✅ 第一方证据（本人复核） |
| 6 | `POST https://u.y.qq.com/cgi-bin/musicu.fcg` | `{"comm":{"tmeAppID":"qqmusic","tmeLoginType":"1","g_tk":5381,"platform":"yqq","ct":24,"cv":0},"req":{"module":"music.login.LoginServer","method":"Login","param":{"strAppid":"wx48db31d50e334801","code":"bogus"}}}`，`Cookie: login_type=2` | 200，`{"code":0,…,"req":{"code":1000,"data":{…}}}` | ✅ 端点存在（本人复核） |
| 7 | 同上，去掉 `tmeLoginType` / `strAppid` | — | `req.code=104400` | 这两个字段决定走哪个分支 |
| 8 | 同上，method 换成 `WxLogin` / `LoginByWx` / 随便写 | — | `req.code=40000`（方法不存在） | ❌ **没有** `wx` 命名的独立方法，只能挂在 `Login` 上 |
| 9 | `GET https://ssl.ptlogin2.qq.com/ptqrshow` 加 `login_type=2` / `appid=wx…` / `ptlang` | — | 全 200，照发 `qrsig`，二维码解出 `http://txz.qq.com/p?k=…&f=716027609` | ❌ 参数被忽略，无微信变体 |
| 10 | `GET https://ssl.ptlogin2.qq.com/ptqrlogin`（无合法 `qrsig`） | — | **403**，`Server: TAPISIX/2.2.2` | ❌ 无微信回退 |
| 11 | 解码 #2 的二维码图片 | `zbarimg` | 内容是 `https://open.weixin.qq.com/connect/confirm?uuid=…` | ⚠️ **不能**把文本重编码成二维码（与 QQ 流程不同） |

`szu.y.qq.com` / `shu.y.qq.com` 的同路径同样可用（备用域名回退）。

### 第三方库佐证（未在本机复现，E3）

- [`L-1124/QQMusicApi`](https://github.com/L-1124/QQMusicApi) `qqmusic_api/modules/login.py`：`_get_wx_qr`、`_check_wx_qr`、`_authorize_wx_qr`（`param={"code":code,"strAppid":"wx48db31d50e334801"}`、`comm={"tmeLoginType":1}`，与实测 #6 一致）、`refresh_credential`（`loginMode:2`）；`qqmusic_api/models/login.py` 的 `QRCodeLoginEvents`：`DONE=(0,405)`、`SCAN=(66,408)`、`CONF=(67,404)`、`TIMEOUT=(65,402)`、`REFUSE=(68,403)`；`musickey` 前缀 `Q_H_L_`=QQ、`W_X_`=微信。
- [`guohuiyuan/music-lib`](https://github.com/guohuiyuan/music-lib) `qq/login.go`：`mapQQWXQRStatus`、`fetchQQWXLoginCookies`（要求 `Cookie: login_type=2`）、`normalizeQQMusicCookies`（`uin` ← `…|musicid|userid|wxuin`；`qqmusic_key`/`qm_keyst` ← `musickey`）。
- [`zzstar101/MineRadio-api`](https://github.com/zzstar101/MineRadio-api) `src/qr_login/wechat.rs`（Rust）：轮询超时 16s；**见到 404 后下一轮必须带 `&last=404`**；405 后走 `wx_redirect.html?…&code=` 并跟随跳转收 cookie。

## 3. 第一方证据（`wx_redirect.html` 内联 JS 节选）

```js
s({comm:{tmeAppID:"qqmusic",tmeLoginType:"1"},
   req:{module:"music.login.LoginServer",method:"Login",
        param:{strAppid:"wx48db31d50e334801",code:(n={code:c}).code,onlyopenid:n.onlyopenid}}},
  function(e){ t("login_type",i,"qq.com"); setTimeout(function(){top.location.replace(u)},50) })
```

同页通用信封 `s()`：POST 到 `//u.y.qq.com/cgi-bin/musicu.fcg`，`e.g_tk=<hash33(qqmusic_key|p_skey|skey|p_lskey|lskey)>`、`e.platform="yqq"`、`e.ct=24`、`e.cv=0`、`withCredentials=true`，`Content-Type: application/x-www-form-urlencoded` 但 body 是 `JSON.stringify(...)`。`onlyopenid` 在该页恒为 `undefined`，**不需要传**。

## 4. 完整流程

1. **取二维码页面**：`GET https://open.weixin.qq.com/connect/qrconnect?appid=wx48db31d50e334801&redirect_uri=<encode(WX_REDIRECT_URI)>&response_type=code&scope=snsapi_login&state=<state>&href=<encode(css)>`，带桌面 Chrome UA 与 `Referer: https://y.qq.com/`。
2. **抽 uuid**：优先 `connect/l/qrconnect\?uuid=([A-Za-z0-9_-]+)`，其次 `/connect/qrcode/([A-Za-z0-9_-]+)`。
3. **取二维码图片**：`GET https://open.weixin.qq.com/connect/qrcode/{uuid}` → 字节（JPEG）→ base64 塞进 `QrLoginSession.image_png`。**`url` 必须留空**：二维码内容是 `connect/confirm?uuid=…`，用文本重编码会扫出无效码。`expires_in` 建议 300。
   - 渲染侧无需改动：`app/src/pages/qr_login.rs:500` 用 `image::load_from_memory`，自动识别 JPEG；只有 `image_png` 为 `None` 时才会走 `render_qr_terminal(&session.url, …)`（`app/src/pages/qr_login.rs:99-101`），所以微信分支不能留 `url` 兜底。
4. **轮询**：`GET https://lp.open.weixin.qq.com/connect/l/qrconnect?uuid={uuid}&_={ms}`，HTTP 超时 ≥35s（服务端约 15s 主动返回一次）。解析 `window\.wx_errcode=(\d+);window\.wx_code='([^']*)'`；**第一次见到 `404` 之后，后续每轮都追加 `&last=404`**。
   - 状态映射：`408→QrLoginStatus::Waiting`、`404→Scanned`、`402→Expired`、`403→Failed`、`405→Success`（`405` 时 `wx_code` 非空）。
5. **兑换凭据**：`POST https://u.y.qq.com/cgi-bin/musicu.fcg`（失败回退 `szu.` / `shu.`），body 见 §2 #6，首次 `g_tk` 固定 `5381`，带 `Cookie: login_type=2`、`Origin: https://y.qq.com`。成功判定：外层 `code==0` 且 `req.code==0` 且 `req.data.musickey` 非空。返回字段（实测失败响应里已看到完整 schema）：`musickey`、`musicid`、`str_musicid`、`openid`、`unionid`、`access_token`、`refresh_token`、`expired_at`、`sessionKey`。
6. **落 cookie**（直接喂 `source/src/tx/session.rs:73 save_login`）：
   - `uin` = `str_musicid`（回退 `musicid` 的十进制字符串）、`qqmusic_key` = `qm_keyst` = `musickey`、`login_type` = `2`、`wxuin` = 同 `uin`；
   - 另外**必须**存 `wxopenid` / `wxunionid` / `wxrefresh_token` / `wxaccess_token`，否则无法续期；
   - 存完之后现有 `is_logged_in()`（`source/src/tx/session.rs:46`，要求 `uin` + `qqmusic_key`）自然为真，`with_cookie()` 会把 cookie 带进所有 `musicu.fcg` / vkey 请求。
7. **续期（建议同期做）**：同 module/method，`param` 加 `loginMode:2` 与 `openid` / `unionid` / `refresh_token` / `musickey` / `access_token` / `str_musicid`，`comm.tmeLoginType` 保持 `"1"`。

## 5. 代码改动清单（行号为 2026-10-08 工作区状态，已核对）

**A. `core/src/model/login.rs`**
- `:53` `pub struct QrLoginSession` 加 `#[serde(default)] pub kind: QrLoginKind`；`image_png` 注释改为「服务端返回的二维码图片字节（QQ=PNG / 微信=JPEG）」。
- 新增 `#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)] pub enum QrLoginKind { #[default] Qq, WeChat }`。
- `:71` `QrLoginResult` / `:81` `new(status, message)` 不变。

**B. `core/src/traits/source.rs`**
- `:60` `pub qr_login: bool` 之后加 `pub wechat_login: bool`（**不要**加成 `&'static [QrLoginKind]`：`SourceCapabilities`（`:42`）需要保持 `Copy` + `Default`）。
- `:267` `async fn create_qr_login(&self)` 默认实现不动；新增 `async fn create_qr_login_kind(&self, _kind: QrLoginKind) -> Result<QrLoginSession, FetchError> { self.create_qr_login().await }`，这样 bilibili / 网易云 / 酷狗 等源零改动。
- `:271` `check_qr_login(&self, _key: &str)` **签名不动**：分支信息编码在 key 前缀里（`wx:{uuid}`）。

**C. `source/src/tx/login.rs`（核心，全部新增）**
- 常量区新增 `WX_APP_ID = "wx48db31d50e334801"`、`WX_QR_CONNECT_API`、`WX_QRCODE_API`、`WX_QR_CHECK_API = "https://lp.open.weixin.qq.com/connect/l/qrconnect"`、`WX_REDIRECT_URI`、`MUSICU_API`。
- `:120` `pub async fn create()` 保持不变（内部转调新函数）；新增 `pub async fn create_kind(kind: QrLoginKind)`、`async fn create_wechat()`（`key = format!("wx:{uuid}")`、`url = String::new()`、`image_png = Some(base64(jpeg))`、`expires_in = 300`）。
- `:165` `pub async fn check(key: &str)` 首行加 `if let Some(uuid) = key.strip_prefix("wx:") { return check_wechat(uuid).await; }`；新增 `check_wechat`、`parse_wx_poll`、`status_from_wx_code`、`wx_login_body`、`wx_credentials`。
- `:36` `hash33` 不动（种值为 0，与 §3 里 JS 的 `g_tk=5381` 不同；首次请求写死 5381 即可）。

**D. `source/src/tx/session.rs`**
- `:14` `const LOGIN_COOKIES: [&str; 2] = ["uin", "qqmusic_key"]` 拆成 `LOGIN_ID_COOKIES = ["uin", "wxuin"]` + `LOGIN_KEY_COOKIES = ["qqmusic_key", "qm_keyst"]`；`:46 is_logged_in()` 改成两组各 `any(...)`；`:100-102` 的测试同步。
- `:74` `cookies.get("uin")` → `cookies.get("uin").or_else(|| cookies.get("wxuin"))`。

**E. `source/src/tx/mod.rs`**
- `:61 capabilities()` 加 `wechat_login: true`；`:77 create_qr_login` 之后加 `create_qr_login_kind` 覆写。

**F. `source/src/manager.rs`**
- `:645 create_qr_login` 旁加 `pub async fn create_qr_login_kind(&self, source: SourceId, kind: QrLoginKind)`。

**G. UI（唯一真正需要新写的交互）**
- `core/src/events.rs:136`：`QrLogin(SourceId)` 之外加 `QrLoginWithKind(SourceId, QrLoginKind)`。
- `app/src/main.rs:1994`（事件分支）、`:2001`（`QrLoginPage::new`）、`:5143`（`manager.create_qr_login`）、`:3530`/`:5834`（穷尽匹配处）跟着改：有 kind 时用 `create_qr_login_kind`，页面用 `QrLoginPage::with_kind`。
- `app/src/pages/qr_login.rs`：`:69 QrLoginPage` 加 `pub kind: QrLoginKind`；`:84 new()` 保留（默认 `QrLoginKind::Qq`），新增 `pub fn with_kind(...)`。`:97 set_qr`、`:244 begin_poll`、`:494 render_png_qr` 都不用改。
- `app/src/pages/settings.rs`：`:2637 qr_login_toggle` 与 `:3091 render_qr_login_panel` 在 `capabilities(source).wechat_login` 为真时渲染两个入口（QQ 扫码 / 微信扫码）。

## 6. 风险与未验证项

1. **第三方 appid 的寿命**：`wx48db31d50e334801` 是 QQ 音乐在微信开放平台的网站应用，QQ 音乐换 appid 或下线该页即整体失效。
2. **风控**：`ptqrshow` / `ptqrlogin` 已经有 TAPISIX 403；`musicu.fcg` 高频异常会返回 `1000` / `104400`。只走微信长轮询（服务端自带约 15s 阻塞），不要高频重试。
3. **必须真人扫码**：自动化只能测到「生成二维码 → 轮询返回 408」。
4. **私有语义靠持续维护**：`strAppid`、`onlyopenid`、`login_type=2`、`g_tk=5381`、`last=404`、`loginMode:2` 都非公开文档。
5. **合规**：复用官方网页登录流程、非公开 OpenAPI；`developer.y.qq.com` 面向企业合作方，个人项目拿不到授权，README 里应说明。
6. **未验证（务必如实告知）**：
   - 真人扫码后真 `wx_code` 兑换回来的 `req.data` 是否真的带 `musickey` / `musicid`（失败响应只验到 `req.code=1000`）；
   - 该 RPC 是否还会通过 `Set-Cookie` 下发网页版 cookie（失败响应里没有任何 `Set-Cookie`，看起来只能自己用 JSON 里的 `musickey` 造 cookie）；
   - 自造 cookie 后 vkey / 播放地址能否解锁 VIP / 无损 —— `source/src/tx/url.rs:57` 与 `:60` 把 `"uin"` / `comm.uin` 写死为 `"0"` / `0`，微信账号是否需要传 `musicid` 未验证；
   - 微信账号 `musicid` 与 `openid` 的绑定关系、`login_type=2` 是否所有接口都必需。

## 7. 建议的落地顺序

1. **先做只读探针**（一次性补齐 §6.6）：一个 `voicefox qq-login --wechat`（或隐藏 example）只生成二维码 + 打印轮询状态，扫码成功后把 `req.data` 原样打印、**先不落盘**。确认字段齐全再定 cookie 映射。
2. 再打通 C + D（可加「生成二维码 → uuid 非空 → 轮询 Waiting」的断言测试，CI 不需要真人）。
3. 然后 A / B / E / F（能力位与通用接口）。
4. 最后 G（设置页两个入口 + 真机扫码）。
5. **等价替代**：设置页保留「手动粘贴 Cookie」入口（`SourceCapabilities::login` 已为真），用户从浏览器 F12 复制 `uin` / `qqmusic_key` / `login_type=2` 即可，与扫码链路共用 `save_login`。

> 不在方案内：走「账号绑定」（需要账号所有者操作，播放器做不到）。

## 8. 实现落地记录（2026-10-08）

方案按 §7 的顺序落地，**唯一没做的是「真人扫码」那一步**（见 §8.4）。

### 8.1 与本文方案的三处偏离（都是有意的）

1. **渠道枚举叫 `QrLoginKind::{Standard, WeChat}`**，不是方案里的 `{Qq, WeChat}`：网易云 / 哔哩哔哩 / 酷狗走的是同一条「音源自带渠道」分支，叫 `Qq` 对它们是错的。
2. **事件是 `AppAction::QrLogin(SourceId, QrLoginKind)`**，没有新增 `QrLoginWithKind`：只改一个变体，`main.rs` / `settings.rs` / `favorites.rs` 的匹配点是同一处，不产生两个几乎一样的分支。
3. **渠道创建走 `MusicSource::create_qr_login_kind`**（有默认实现，其余音源零改动）；轮询仍复用 `check_qr_login(&self, key)`，按 `WX_KEY_PREFIX = "wx:"` 前缀分流，`SourceCapabilities` 也没有多出「渠道」概念。

### 8.2 实际改动文件

- `core/src/model/login.rs`：`QrLoginKind` + `QrLoginSession.kind`（`#[serde(default)]`）。
- `core/src/traits/source.rs`：`SourceCapabilities.wechat_login`、`MusicSource::create_qr_login_kind` 默认实现。
- `core/src/events.rs`：`QrLogin(SourceId, QrLoginKind)`。
- `source/src/tx/login.rs`：`create_kind` / `create_qq` / `create_wechat` / `poll_wechat` / `exchange_wechat` / `wechat_cookies` / `check_wechat` + 纯函数单测（uuid 抽取、`wx_errcode` 解析、状态映射、`last=404` 回带、cookie 映射）。
- `source/src/tx/session.rs`：凭据判定拆成 `LOGIN_ID_COOKIES = ["uin","wxuin"]` × `LOGIN_KEY_COOKIES = ["qqmusic_key","qm_keyst"]`（各命中一个即可），`save_login` 的 `user_id` 回退到 `wxuin`。
- `source/src/tx/mod.rs`、`source/src/manager.rs`：capability 位与 `create_qr_login_kind`。
- `app/src/pages/qr_login.rs`：`QrLoginPage::with_kind` + 标题渠道角标（微信）；`set_qr` 以会话里的 `kind` 为准。
- `app/src/pages/settings.rs`：扫码入口列表改成 `(SourceId, QrLoginKind)`，QQ 音乐多出一行微信入口；「支持扫码 / 已登录」计数按音源去重。
- `app/src/main.rs`：`QrLogin(source, kind)` → `QrLoginPage::with_kind`；`spawn_qr_generate` 用 `create_qr_login_kind`，因此二维码过期自动重建仍是同一条渠道。
- `source/examples/wechat_probe.rs`（新增）：只读探针，即 §7.1 那一项。

### 8.3 本次补强的实测证据（§2 之外）

- `qrconnect` 页面里两种 marker 都能拿到 uuid（`connect/l/qrconnect?uuid=` 与 `/connect/qrcode/`；实测 uuid `061quKBr3h35Ga1r`）。
- 二维码 `GET /connect/qrcode/{uuid}` → `image/jpeg`、470×470、46419 B（本轮复测）。
- **长轮询一轮实耗 15.211s**，响应体正是 `window.wx_errcode=408;window.wx_code='';`。这就是全局 HTTP 客户端 15s 超时不够用的原因，实现里对该请求单独设 `WX_POLL_TIMEOUT = 35s`（其余请求仍用全局超时）。

### 8.4 仍未验证（必须真人扫码，同 §6.6）

探针 `cargo run -p lx-source --example wechat_probe` 已跑通「生成二维码 → 写临时文件 → 轮询到 Waiting」。**真人扫码之后的字段仍未验证**：真 `wx_code` 兑换回来的 `req.data` 是否带 `musickey`、该 RPC 是否还会 Set-Cookie、自造 cookie 能否解锁 VIP / 无损。探针在 `Confirmed` 时会原样打印响应 JSON 且**不落盘**，就是为这一步准备的。

### 8.5 顺带说明

§5 里的行号是改动前的工作区快照，实现后已整体位移，仅作历史参考；「`ptlogin2.qq.com` 没有微信变体」这一否证仍然成立（微信是独立代码路径，不是改参数）。
