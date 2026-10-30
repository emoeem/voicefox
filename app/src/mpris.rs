//! Linux MPRIS 服务，供 Waybar、桌面媒体键和播放器控件使用。
//!
//! 快照 / 命令的数据结构在 [`crate::media_session`]（与 Windows SMTC 共用），
//! 这里只做 zbus 协议翻译：把 [`MediaSnapshot`] 映射到 org.mpris.MediaPlayer2
//! 属性，把桌面发来的 D-Bus 调用转成 [`MediaCommand`]。

use std::collections::HashMap;
use std::time::Duration;

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::media_session::{MediaCommand, MediaHandle, MediaSnapshot};

const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";

pub async fn start() -> anyhow::Result<(
    MediaHandle,
    tokio::sync::mpsc::UnboundedReceiver<MediaCommand>,
)> {
    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (handle, update_rx) = crate::media_session::channel();
    let initial = MediaSnapshot::default();
    let root = MediaPlayer2 {
        command_tx: command_tx.clone(),
    };
    let player = MprisPlayer {
        command_tx,
        state: initial,
    };
    let bus_name = format!(
        "org.mpris.MediaPlayer2.voicefox.instance{}",
        std::process::id()
    );
    let connection = zbus::connection::Builder::session()?
        .serve_at(MPRIS_PATH, root)?
        .serve_at(MPRIS_PATH, player)?
        .name(bus_name)?
        .build()
        .await?;

    let update_task = tokio::spawn(async move {
        if let Err(error) = run_updates(connection, update_rx).await {
            tracing::warn!("MPRIS update service stopped: {error}");
        }
    });
    // panic 不能像 Err 一样走上面的日志分支：单独 await JoinHandle 记一笔，
    // 否则媒体键静默失灵只剩全局 hook 的一条日志。
    tokio::spawn(async move {
        if let Err(join_error) = update_task.await
            && join_error.is_panic()
        {
            tracing::warn!("MPRIS update service panicked: {join_error}");
        }
    });

    Ok((handle, command_rx))
}

struct MediaPlayer2 {
    command_tx: tokio::sync::mpsc::UnboundedSender<MediaCommand>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl MediaPlayer2 {
    fn raise(&self) {}

    fn quit(&self) {
        let _ = self.command_tx.send(MediaCommand::Quit);
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> &'static str {
        "voicefox"
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> &'static str {
        "voicefox"
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<&'static str> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

struct MprisPlayer {
    command_tx: tokio::sync::mpsc::UnboundedSender<MediaCommand>,
    state: MediaSnapshot,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl MprisPlayer {
    fn next(&self) {
        let _ = self.command_tx.send(MediaCommand::Next);
    }

    fn previous(&self) {
        let _ = self.command_tx.send(MediaCommand::Previous);
    }

    fn pause(&self) {
        let _ = self.command_tx.send(MediaCommand::Pause);
    }

    fn play_pause(&self) {
        let _ = self.command_tx.send(MediaCommand::Toggle);
    }

    fn stop(&self) {
        let _ = self.command_tx.send(MediaCommand::Stop);
    }

    fn play(&self) {
        let _ = self.command_tx.send(MediaCommand::Play);
    }

    fn seek(&self, offset: i64) {
        let _ = self.command_tx.send(MediaCommand::SeekBy(offset));
    }

    fn set_position(&self, track_id: OwnedObjectPath, position: i64) {
        if track_id.as_str() != self.state.track_path || position < 0 {
            return;
        }
        if self.state.duration_micros > 0 && position > self.state.duration_micros {
            return;
        }
        let _ = self
            .command_tx
            .send(MediaCommand::SetPosition(Duration::from_micros(
                position as u64,
            )));
    }

    fn open_uri(&self, _uri: &str) {}

    #[zbus(property)]
    fn playback_status(&self) -> &'static str {
        self.state.playback_status
    }

    #[zbus(property)]
    fn loop_status(&self) -> &'static str {
        self.state.loop_status
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.state.shuffle
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, Value<'static>> {
        let mut values = HashMap::new();
        // "/" 是 MPRIS 规范里的合法 trackid 占位；展开常量避免在 zbus 错误
        // 路径里再埋一个 unwrap。
        let track_path =
            OwnedObjectPath::try_from(self.state.track_path.clone()).unwrap_or_else(|_| {
                OwnedObjectPath::try_from("/").expect("static \"/\" is a valid object path")
            });
        values.insert("mpris:trackid".to_string(), Value::new(track_path));
        if self.state.duration_micros > 0 {
            values.insert(
                "mpris:length".to_string(),
                Value::new(self.state.duration_micros),
            );
        }
        if !self.state.art_url.is_empty() {
            values.insert(
                "mpris:artUrl".to_string(),
                Value::new(self.state.art_url.clone()),
            );
        }
        if !self.state.title.is_empty() {
            values.insert(
                "xesam:title".to_string(),
                Value::new(self.state.title.clone()),
            );
        }
        if !self.state.artist.is_empty() {
            values.insert(
                "xesam:artist".to_string(),
                Value::new(vec![self.state.artist.clone()]),
            );
        }
        if !self.state.album.is_empty() {
            values.insert(
                "xesam:album".to_string(),
                Value::new(self.state.album.clone()),
            );
        }
        if !self.state.source_url.is_empty() {
            values.insert(
                "xesam:url".to_string(),
                Value::new(self.state.source_url.clone()),
            );
        }
        values
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.state.volume
    }

    #[zbus(property)]
    fn set_volume(&self, volume: f64) {
        let _ = self
            .command_tx
            .send(MediaCommand::SetVolume(volume.clamp(0.0, 1.0)));
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.state.position_micros
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.state.can_go_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.state.can_go_previous
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.state.can_play()
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.state.can_play()
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.state.can_seek()
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;
}

async fn run_updates(
    connection: zbus::Connection,
    mut update_rx: tokio::sync::mpsc::UnboundedReceiver<MediaSnapshot>,
) -> zbus::Result<()> {
    while let Some(snapshot) = update_rx.recv().await {
        // 单次更新失败（如 D-Bus 瞬断）只记录日志并继续，避免更新循环
        // 永久退出导致桌面控件状态冻结。
        if let Err(error) = apply_snapshot(&connection, snapshot).await {
            tracing::warn!("MPRIS snapshot update failed: {error}");
        }
    }
    Ok(())
}

async fn apply_snapshot(
    connection: &zbus::Connection,
    snapshot: MediaSnapshot,
) -> zbus::Result<()> {
    {
        let interface_ref = connection
            .object_server()
            .interface::<_, MprisPlayer>(MPRIS_PATH)
            .await?;
        let emitter = interface_ref.signal_emitter().clone();
        let mut interface = interface_ref.get_mut().await;
        let previous = interface.state.clone();
        interface.state = snapshot;

        if previous.position_epoch != interface.state.position_epoch {
            MprisPlayer::seeked(&emitter, interface.state.position_micros).await?;
        }
        if previous.playback_status != interface.state.playback_status {
            interface.playback_status_changed(&emitter).await?;
        }
        if previous.metadata_changed(&interface.state) {
            interface.metadata_changed(&emitter).await?;
        }
        if previous.volume != interface.state.volume {
            interface.volume_changed(&emitter).await?;
        }
        if previous.loop_status != interface.state.loop_status {
            interface.loop_status_changed(&emitter).await?;
        }
        if previous.shuffle != interface.state.shuffle {
            interface.shuffle_changed(&emitter).await?;
        }
        if previous.can_go_next != interface.state.can_go_next {
            interface.can_go_next_changed(&emitter).await?;
        }
        if previous.can_go_previous != interface.state.can_go_previous {
            interface.can_go_previous_changed(&emitter).await?;
        }
        if previous.can_play() != interface.state.can_play() {
            interface.can_play_changed(&emitter).await?;
            interface.can_pause_changed(&emitter).await?;
        }
        if previous.can_seek() != interface.state.can_seek() {
            interface.can_seek_changed(&emitter).await?;
        }
    }
    Ok(())
}
