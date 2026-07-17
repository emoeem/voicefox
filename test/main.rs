//! 音源 + 播放器独立验证工具
//! cargo run --bin lx-test

use lx_core::model::source::Quality;
use lx_core::traits::source::MusicSource;
use lx_source::kw::KwSource;
use lx_source::kg::KgSource;
use lx_source::mg::MgSource;
use lx_source::tx::TxSource;
use lx_source::wy::WySource;

#[tokio::main]
async fn main() {
    println!("=== lx-tui 音源验证 ===\n");

    // 1. 测试 mpv
    print!("[1/7] mpv 可用性 ... ");
    match std::process::Command::new("mpv").arg("--version")
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status()
    {
        Ok(s) if s.success() => println!("✅ mpv 已安装"),
        _ => println!("❌ mpv 未安装 - 播放功能无法使用"),
    }

    // 2. 测试 kw
    test_source("kw", &KwSource::new()).await;

    // 3. 测试 kg
    test_source("kg", &KgSource::new()).await;

    // 4. 测试 mg
    test_source("mg", &MgSource::new()).await;

    // 5. 测试 tx
    test_source("tx", &TxSource::new()).await;

    // 6. 测试 wy
    test_source("wy", &WySource::new()).await;

    // 7. JS 源可用性
    print!("[7/7] Node.js 可用性 ... ");
    match std::process::Command::new("node").arg("--version")
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status()
    {
        Ok(s) if s.success() => println!("✅ Node.js 已安装"),
        _ => println!("⚠ Node.js 未安装 - JS 音源功能无法使用"),
    }

    // 8. 本地音源扫描测试
    print!("\n[8/8] 本地音源扫描测试 ... ");
    let local_source = lx_source::local::LocalSource::new();
    let paths = vec!["/home/emo/Downloads/go-musicfox".to_string()];
    let errors = local_source.scan(&paths, 0);
    let songs = local_source.all_songs();
    if !errors.is_empty() {
        println!("❌ 错误: {}", errors.join("; "));
    } else {
        println!("✅ 扫描完成，共 {} 首", songs.len());
        for s in songs.iter().take(5) {
            println!("   {} - {} ({}:{:02})",
                s.name, s.singer,
                s.duration.as_secs() / 60,
                s.duration.as_secs() % 60);
        }
        if songs.len() > 5 {
            println!("   ... 还有 {} 首", songs.len() - 5);
        }
    }

    println!("\n=== 验证完成 ===");
}

async fn test_source(name: &str, source: &dyn MusicSource) {
    print!("      {} 搜索'晴天' ... ", name);
    match source.search("晴天", 1, 3).await {
        Ok(result) => {
            if result.items.is_empty() {
                println!("❌ 返回 0 条结果");
            } else {
                let s = &result.items[0];
                println!("✅ {} 条, 第一首: {} - {} (id={})",
                    result.total, s.name, s.singer, s.id);
                
                // 测试获取 URL
                print!("        获取播放URL ... ");
                match source.get_song_url(s, Quality::High320).await {
                    Ok(url) => {
                        if url.url.is_empty() {
                            println!("❌ URL 为空");
                        } else {
                            let short = if url.url.len() > 60 {
                                format!("{}...", &url.url[..57])
                            } else { url.url.clone() };
                            println!("✅ {}", short);
                        }
                    }
                    Err(e) => println!("❌ {}", e),
                }

                // 测试歌词
                print!("        获取歌词 ... ");
                match source.get_lyric(s).await {
                    Ok(lyric) => {
                        if lyric.lyric.is_empty() {
                            println!("❌ 歌词为空");
                        } else {
                            let preview: String = lyric.lyric.chars().take(40).collect();
                            println!("✅ {}", preview.replace('\n', "\\n"));
                        }
                    }
                    Err(e) => println!("❌ {}", e),
                }
            }
        }
        Err(e) => println!("❌ {}", e),
    }
}
