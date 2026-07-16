//! 网易云音乐播放 URL 获取
//!
//! 简化方案：使用公开重定向 URL，reqwest 自动跟随 302
//! 封面从 song.cover_url 取（搜索时已设置）

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};

/// 获取歌曲播放 URL
/// 使用公开重定向 URL，reqwest 默认跟随302
pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let url = format!(
        "https://music.163.com/song/media/outer/url?id={}.mp3",
        song.id
    );

    let qualities: Vec<Quality> = song.qualities.iter().copied().collect();

    Ok(SongUrl {
        url,
        quality,
        duration: song.duration,
        cover_url: song.cover_url.clone(),
        qualities,
    })
}
