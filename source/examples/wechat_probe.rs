//! QQ 音乐「微信扫码登录」只读探针。
//!
//! 这个 example 走的是**正式的微信登录代码路径**（`tx::login::create_kind`
//! → `poll_wechat` → `exchange_wechat`），但**从不写登录态**：不会碰
//! `session::save_login`，也不会留下任何 cookie。它的用途是在真人扫码之前
//! 先把机器能自证的部分跑通、并把服务端真实响应原样打印出来核对字段。
//!
//! ```text
//! cargo run -p lx-source --example wechat_probe
//! ```
//!
//! 流程：
//! 1. 生成二维码（JPEG）并写到临时文件，打印路径 —— 用微信扫那个文件；
//! 2. 每轮长轮询打印一次状态（服务端约 15 秒才回一次）；
//! 3. 一旦确认，打印 `musicu.fcg` 返回的完整 JSON 与关键字段是否存在；
//! 4. 过期 / 失败 / 超时都会正常退出，退出码 0（网络错误才非 0）。

use std::time::{Duration, Instant};

use base64::Engine;
use lx_core::model::login::QrLoginKind;
use lx_source::tx::login::{self, WxPoll};

/// 探针自身的最长运行时间：比二维码 5 分钟的有效期多留一点余量。
const PROBE_DEADLINE: Duration = Duration::from_secs(330);

fn stamp(start: Instant) -> String {
    format!("t+{:>3}s", start.elapsed().as_secs())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    println!("== QQ 音乐微信扫码登录探针（只读，不写登录态）==");

    let session = login::create_kind(QrLoginKind::WeChat).await?;
    let uuid = session
        .key
        .strip_prefix("wx:")
        .ok_or("微信会话的 key 不带 wx: 前缀，渠道分流可能被改坏")?
        .to_string();
    println!("[{}] uuid = {uuid}", stamp(start));
    println!("[{}] 渠道 = {:?}", stamp(start), session.kind);

    let encoded = session
        .image_png
        .as_deref()
        .ok_or("微信会话没有 image_png（图片二维码必须走 image_png）")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
    let qr_path = std::env::temp_dir().join("voicefox-wechat-qr.jpg");
    std::fs::write(&qr_path, &bytes)?;
    println!(
        "[{}] 二维码 {} 字节，已写到 {} —— 用微信扫它（有效期约 {} 秒）",
        stamp(start),
        bytes.len(),
        qr_path.display(),
        session.expires_in,
    );
    println!("[{}] 开始长轮询（服务端每轮约 15 秒）…", stamp(start));

    while start.elapsed() < PROBE_DEADLINE {
        match login::poll_wechat(&uuid).await? {
            WxPoll::Waiting => println!("[{}] 等待扫码", stamp(start)),
            WxPoll::Scanned => println!("[{}] 已扫码，等待手机确认", stamp(start)),
            WxPoll::Expired => {
                println!("[{}] 二维码已过期，请重新运行探针", stamp(start));
                return Ok(());
            }
            WxPoll::Failed(message) => {
                println!("[{}] 失败：{message}", stamp(start));
                return Ok(());
            }
            WxPoll::Confirmed(code) => {
                println!("[{}] 已确认，wx_code = {code}", stamp(start));
                let response = login::exchange_wechat(&code).await?;
                println!("---- musicu.fcg 原始响应 ----");
                println!("{}", serde_json::to_string_pretty(&response)?);
                println!("---- 关键字段 ----");
                let data = &response["req"]["data"];
                for field in [
                    "musickey",
                    "str_musicid",
                    "musicid",
                    "openid",
                    "unionid",
                    "access_token",
                    "refresh_token",
                    "expired_at",
                ] {
                    println!("  {field} = {:?}", data[field]);
                }
                let outer = response["code"].as_i64();
                let inner = response["req"]["code"].as_i64();
                let has_key = data["musickey"]
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty());
                println!("  code = {outer:?} / req.code = {inner:?}");
                println!(
                    "  结论：{}",
                    if outer == Some(0) && inner == Some(0) && has_key {
                        "拿到 musickey —— 正式登录路径可以落 cookie"
                    } else {
                        "没拿到 musickey —— 按现状会把这次登录判定为失败"
                    }
                );
                println!("（探针到此结束，未写入任何登录态）");
                return Ok(());
            }
        }
    }

    println!("[{}] 探针超时退出", stamp(start));
    Ok(())
}
