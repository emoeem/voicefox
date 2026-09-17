//! 联网冒烟测试：逐个音源跑「搜索 → 解析播放地址 → 取歌词」。
//!
//! 默认忽略，需要联网时手动执行：
//! `cargo test -p lx-source --test network_smoke -- --ignored --nocapture`
//!
//! 不做断言，只打印每个音源的返回情况——沙箱里无法联网，这层验证必须手动跑。

use std::sync::Arc;

use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{ParsedLink, SourceCapabilities};
use lx_source::manager::SourceManager;

/// 搜索关键词，可用 `SMOKE_KEYWORD` 覆盖。
///
/// 默认「周杰伦」用于验证搜索与解析链路；但他在国内平台几乎全是 VIP 曲目，
/// 测播放地址时应换成免费曲库关键词（如 `儿歌`、`纯音乐`）。
fn keyword() -> String {
    std::env::var("SMOKE_KEYWORD").unwrap_or_else(|_| "周杰伦".to_string())
}

fn describe(capabilities: SourceCapabilities) -> String {
    let mut flags = Vec::new();
    if capabilities.link_parse {
        flags.push("直解");
    }
    if capabilities.playlist_categories {
        flags.push("分类");
    }
    if capabilities.playlist_search {
        flags.push("歌单搜索");
    }
    if capabilities.qr_login {
        flags.push("扫码");
    }
    if capabilities.user_playlists {
        flags.push("我的歌单");
    }
    flags.join("/")
}

#[tokio::test]
#[ignore = "需要联网，手动执行"]
async fn every_source_can_search_and_resolve() {
    let manager = Arc::new(SourceManager::new(SourceId::Kw, SourceId::all_online()));
    for source_id in SourceId::all_online() {
        let Some(source) = manager.get(*source_id) else {
            println!("== {} 未注册 ==", source_id.as_str());
            continue;
        };
        println!(
            "\n===== {} ({}) [{}] =====",
            source_id.display_name(),
            source_id.as_str(),
            describe(source.capabilities())
        );

        let keyword = keyword();
        let search = match source.search(&keyword, 1, 10).await {
            Ok(result) => result,
            Err(error) => {
                println!("搜索失败: {error}");
                continue;
            }
        };
        println!(
            "搜索命中 {} 首，has_more={}",
            search.items.len(),
            search.has_more
        );
        let Some(song) = search.items.first() else {
            println!("没有结果，跳过后续检查");
            continue;
        };
        println!(
            "首条: {} - {} | 专辑 {} | 时长 {}s",
            song.name,
            song.singer,
            song.album_name,
            song.duration.as_secs()
        );

        // 前三首都试一遍：VIP / 版权受限曲目拿不到地址是正常现象，
        // 只试第一首会把「这一首不可播」误判成「音源坏了」。
        let mut resolved = false;
        for candidate in search.items.iter().take(3) {
            // 走 SourceManager 而不是直接调音源：应用里播放/下载用的是这条路径，
            // 它会带上换源与解析器回退，直接调音源会低估实际成功率。
            match manager.get_song_url(candidate, Quality::High320).await {
                Ok(url) => {
                    println!(
                        "播放地址: {} | 档位 {} | 大小 {:?}（曲目: {}）",
                        url.url.chars().take(80).collect::<String>(),
                        url.quality.label(),
                        url.size,
                        candidate.name
                    );
                    resolved = true;
                    break;
                }
                Err(error) => println!("  「{}」取地址失败: {error}", candidate.name),
            }
        }
        if !resolved {
            println!("前三首都没拿到播放地址");
        }

        match source.get_lyric(song).await {
            Ok(lyric) if !lyric.lyric.trim().is_empty() => {
                println!("歌词: {} 字节", lyric.lyric.len())
            }
            Ok(_) => println!("歌词: 空"),
            Err(error) => println!("歌词失败: {error}"),
        }
    }
}

#[tokio::test]
#[ignore = "需要联网，手动执行"]
async fn playlist_categories_and_search_work() {
    let manager = Arc::new(SourceManager::new(SourceId::Kw, SourceId::all_online()));
    for source_id in SourceId::all_online() {
        let Some(source) = manager.get(*source_id) else {
            continue;
        };
        let capabilities = source.capabilities();
        if !capabilities.playlist_search && !capabilities.playlist_categories {
            continue;
        }
        println!("\n===== {} =====", source_id.display_name());
        if capabilities.playlist_search {
            match source.search_playlists(&keyword(), 1).await {
                Ok(items) => println!("歌单搜索: {} 个", items.len()),
                Err(error) => println!("歌单搜索失败: {error}"),
            }
        }
        if capabilities.playlist_categories {
            match source.get_playlist_categories().await {
                Ok(categories) => {
                    println!("歌单分类: {} 个", categories.len());
                    let names = categories
                        .iter()
                        .take(3)
                        .map(|item| item.name.clone())
                        .collect::<Vec<_>>();
                    println!("  示例: {}", names.join(" / "));
                    // 用第一个真实分类拉一次歌单，验证分类 ID 能被下游接口接受。
                    if let Some(category) = categories.iter().find(|item| !item.id.is_empty()) {
                        match source.get_playlists(&category.id, 1).await {
                            Ok(items) => {
                                println!("  「{}」下 {} 个歌单", category.name, items.len())
                            }
                            Err(error) => println!("  分类歌单失败: {error}"),
                        }
                    }
                }
                Err(error) => println!("歌单分类失败: {error}"),
            }
        }
        if capabilities.user_playlists {
            match source.get_user_playlists(1, 10).await {
                Ok(items) => println!("我的歌单: {} 个（已登录）", items.len()),
                Err(error) => println!("我的歌单: {error}（未登录时属于预期）"),
            }
        }
    }
}

#[tokio::test]
#[ignore = "需要联网，手动执行"]
async fn qr_login_sessions_can_be_created() {
    let manager = Arc::new(SourceManager::new(SourceId::Kw, SourceId::all_online()));
    for source_id in SourceId::all_online() {
        let Some(source) = manager.get(*source_id) else {
            continue;
        };
        if !source.capabilities().qr_login {
            continue;
        }
        match source.create_qr_login().await {
            Ok(session) => println!(
                "{}: 二维码已生成（key 长度 {}，图片 {} 字符，有效期 {}s）",
                source_id.display_name(),
                session.key.len(),
                session.image_png.as_deref().map(str::len).unwrap_or(0),
                session.expires_in
            ),
            Err(error) => println!("{}: 生成二维码失败: {error}", source_id.display_name()),
        }
    }
}

#[tokio::test]
#[ignore = "需要联网，手动执行"]
async fn link_parsing_works() {
    let manager = Arc::new(SourceManager::new(SourceId::Kw, SourceId::all_online()));
    let links = [
        "https://music.163.com/#/song?id=186016",
        "https://www.kuwo.cn/play_detail/3049327",
        "https://y.qq.com/n/ryqq/songDetail/003aAYrm3GE0Ac",
        "https://www.jamendo.com/track/1214935",
        "https://music.91q.com/song/T10045678",
    ];
    for link in links {
        match manager.parse_link(link).await {
            Ok((source, parsed)) => {
                let summary = match parsed {
                    ParsedLink::Song(song) => format!("单曲 {} - {}", song.name, song.singer),
                    ParsedLink::Playlist { playlist, songs } => {
                        format!("歌单「{}」{} 首", playlist.name, songs.len())
                    }
                    ParsedLink::Album { playlist, songs } => {
                        format!("专辑「{}」{} 首", playlist.name, songs.len())
                    }
                };
                println!("{} → {}: {summary}", source.as_str(), link);
            }
            Err(error) => println!("{link}: {error}"),
        }
    }
}
