//! 统一通知服务：TUI toast 由 AppContext 管理，桌面通知在后台发送。

use lx_core::events::Notification;

#[cfg(windows)]
type DesktopSender = std::sync::mpsc::Sender<Notification>;
#[cfg(not(windows))]
type DesktopSender = tokio::sync::mpsc::UnboundedSender<Notification>;

#[derive(Clone)]
pub struct DesktopNotifier {
    tx: DesktopSender,
}

impl DesktopNotifier {
    pub fn new() -> Self {
        #[cfg(windows)]
        let (tx, rx) = std::sync::mpsc::channel();
        #[cfg(not(windows))]
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_desktop_worker(rx);
        Self { tx }
    }

    pub fn send(&self, notification: Notification) {
        let _ = self.tx.send(notification);
    }
}

#[cfg(target_os = "linux")]
fn spawn_desktop_worker(mut rx: tokio::sync::mpsc::UnboundedReceiver<Notification>) {
    tokio::spawn(async move {
        let mut connection = None;
        let mut replaced_id = 0u32;

        while let Some(notification) = rx.recv().await {
            if connection.is_none() {
                match zbus::Connection::session().await {
                    Ok(value) => connection = Some(value),
                    Err(error) => {
                        tracing::debug!("desktop notification D-Bus unavailable: {error}");
                        continue;
                    }
                }
            }

            let Some(conn) = connection.as_ref() else {
                continue;
            };
            match send_linux_notification(conn, &notification, replaced_id).await {
                Ok(id) => {
                    if notification.replace_previous {
                        replaced_id = id;
                    }
                }
                Err(error) => {
                    tracing::warn!("desktop notification failed: {error}");
                    connection = None;
                }
            }
        }
    });
}

#[cfg(windows)]
fn spawn_desktop_worker(rx: std::sync::mpsc::Receiver<Notification>) {
    if let Err(error) = std::thread::Builder::new()
        .name("desktop-notification".to_string())
        .spawn(move || run_windows_worker(rx))
    {
        tracing::warn!("desktop notification worker failed to start: {error}");
    }
}

#[cfg(windows)]
fn run_windows_worker(rx: std::sync::mpsc::Receiver<Notification>) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    let mut notifier = None;
    loop {
        let received = if notifier.is_some() {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(notification) => Some(notification),
                Err(RecvTimeoutError::Timeout) => {
                    notifier = None;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            rx.recv().ok()
        };

        let Some(notification) = received else {
            break;
        };
        if notifier.is_none() {
            match WindowsNotifier::new() {
                Ok(value) => notifier = Some(value),
                Err(error) => {
                    tracing::warn!("desktop notification setup failed: {error}");
                    continue;
                }
            }
        }
        if let Some(value) = notifier.as_mut() {
            if let Err(error) = value.show(&notification) {
                tracing::warn!("desktop notification failed: {error}");
                notifier = None;
            }
        }
    }
}

#[cfg(windows)]
struct WindowsNotifier {
    hwnd: windows_sys::Win32::Foundation::HWND,
    data: windows_sys::Win32::UI::Shell::NOTIFYICONDATAW,
    has_notification: bool,
}

#[cfg(windows)]
impl WindowsNotifier {
    fn new() -> std::io::Result<Self> {
        use std::mem::size_of;
        use std::ptr::{null, null_mut};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::Shell::{
            NIF_ICON, NIF_TIP, NIM_ADD, NOTIFYICONDATAW, Shell_NotifyIconW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, LoadIconW, WS_OVERLAPPED,
        };

        const APP_ICON_RESOURCE_ID: usize = 1;
        const STATIC_CLASS: [u16; 7] = [83, 84, 65, 84, 73, 67, 0];
        // SAFETY: STATIC_CLASS is NUL-terminated and all optional handles are null.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                STATIC_CLASS.as_ptr(),
                null(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            )
        };
        if hwnd.is_null() {
            return Err(std::io::Error::last_os_error());
        }

        // SAFETY: null requests the module containing the current executable.
        let module = unsafe { GetModuleHandleW(null()) };
        if module.is_null() {
            let error = std::io::Error::last_os_error();
            // SAFETY: hwnd was created by this thread and is still valid.
            unsafe { DestroyWindow(hwnd) };
            return Err(error);
        }

        // SAFETY: resource 1 is the application icon embedded at build time.
        let icon = unsafe { LoadIconW(module, APP_ICON_RESOURCE_ID as _) };
        if icon.is_null() {
            let error = std::io::Error::last_os_error();
            // SAFETY: hwnd was created by this thread and is still valid.
            unsafe { DestroyWindow(hwnd) };
            return Err(error);
        }

        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_TIP,
            hIcon: icon,
            hBalloonIcon: icon,
            ..Default::default()
        };
        data.szTip = utf16_array("voicefox");
        // SAFETY: data contains a valid window, shared icon and terminated strings.
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } == 0 {
            // SAFETY: hwnd was created by this thread and is still valid.
            unsafe { DestroyWindow(hwnd) };
            return Err(std::io::Error::other("Shell_NotifyIconW(NIM_ADD) failed"));
        }

        Ok(Self {
            hwnd,
            data,
            has_notification: false,
        })
    }

    fn show(&mut self, notification: &Notification) -> std::io::Result<()> {
        use windows_sys::Win32::UI::Shell::{NIF_INFO, NIM_MODIFY, Shell_NotifyIconW};

        if self.has_notification {
            self.hide()?;
        }
        self.data.uFlags = NIF_INFO;
        self.data.dwInfoFlags = windows_info_flags();
        self.data.szInfoTitle = utf16_array(&notification_title(notification));
        self.data.szInfo = utf16_array(&notification.message);
        // SAFETY: data belongs to the notification icon registered by this instance.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data) } == 0 {
            return Err(std::io::Error::other(
                "Shell_NotifyIconW(NIM_MODIFY) failed",
            ));
        }
        self.has_notification = true;
        Ok(())
    }

    fn hide(&mut self) -> std::io::Result<()> {
        use windows_sys::Win32::UI::Shell::{NIF_INFO, NIM_MODIFY, Shell_NotifyIconW};

        self.data.uFlags = NIF_INFO;
        self.data.szInfoTitle = [0; 64];
        self.data.szInfo = [0; 256];
        // SAFETY: data belongs to the notification icon registered by this instance.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data) } == 0 {
            return Err(std::io::Error::other(
                "Shell_NotifyIconW(NIM_MODIFY) failed to hide notification",
            ));
        }
        self.has_notification = false;
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsNotifier {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::Shell::{NIM_DELETE, Shell_NotifyIconW};
        use windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow;

        // SAFETY: both resources were created and registered by this instance.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.data);
            DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(windows)]
fn windows_info_flags() -> u32 {
    use windows_sys::Win32::UI::Shell::{NIIF_LARGE_ICON, NIIF_RESPECT_QUIET_TIME, NIIF_USER};

    NIIF_USER | NIIF_LARGE_ICON | NIIF_RESPECT_QUIET_TIME
}

#[cfg(all(not(target_os = "linux"), not(windows)))]
fn spawn_desktop_worker(mut rx: tokio::sync::mpsc::UnboundedReceiver<Notification>) {
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
}

#[cfg(target_os = "linux")]
async fn send_linux_notification(
    connection: &zbus::Connection,
    notification: &Notification,
    replaced_id: u32,
) -> zbus::Result<u32> {
    use std::collections::HashMap;

    use zbus::zvariant::OwnedValue;

    let proxy = zbus::Proxy::new(
        connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await?;
    let title = notification_title(notification);
    let icon = notification.icon.clone().unwrap_or_else(default_app_icon);
    let replace_id = if notification.replace_previous {
        replaced_id
    } else {
        0
    };
    let actions: Vec<String> = Vec::new();
    let hints: HashMap<String, OwnedValue> = HashMap::new();

    proxy
        .call(
            "Notify",
            &(
                "voicefox",
                replace_id,
                icon,
                title,
                notification.message.as_str(),
                actions,
                hints,
                5_000i32,
            ),
        )
        .await
}

#[cfg(any(target_os = "linux", windows, test))]
fn notification_title(notification: &Notification) -> String {
    notification
        .title
        .clone()
        .unwrap_or_else(|| level_title(notification))
}

#[cfg(any(target_os = "linux", windows, test))]
fn level_title(notification: &Notification) -> String {
    use lx_core::events::NotificationLevel;

    match &notification.level {
        NotificationLevel::Info => "voicefox".to_string(),
        NotificationLevel::Success => "voicefox · 成功".to_string(),
        NotificationLevel::Warn => "voicefox · 警告".to_string(),
        NotificationLevel::Error => "voicefox · 错误".to_string(),
    }
}

#[cfg(target_os = "linux")]
fn default_app_icon() -> String {
    let mut candidates = Vec::new();
    if let Some(data_home) = dirs::data_dir() {
        candidates.push(
            data_home
                .join("icons/hicolor/512x512/apps/voicefox.png")
                .to_string_lossy()
                .into_owned(),
        );
    }
    candidates.extend([
        "/usr/local/share/icons/hicolor/512x512/apps/voicefox.png".to_string(),
        "/usr/share/icons/hicolor/512x512/apps/voicefox.png".to_string(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/../icons/512.png").to_string(),
    ]);

    candidates
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .unwrap_or_else(|| "voicefox".to_string())
}

#[cfg(any(windows, test))]
fn utf16_array<const N: usize>(text: &str) -> [u16; N] {
    let mut destination = [0; N];
    let capacity = N.saturating_sub(1);
    let mut written = 0;
    for character in text.chars() {
        let mut encoded = [0; 2];
        let units = character.encode_utf16(&mut encoded);
        if written + units.len() > capacity {
            break;
        }
        destination[written..written + units.len()].copy_from_slice(units);
        written += units.len();
    }
    destination
}

#[cfg(test)]
mod tests {
    use super::{notification_title, utf16_array};
    use lx_core::events::Notification;

    fn decoded<const N: usize>(value: &[u16; N]) -> String {
        let end = value.iter().position(|unit| *unit == 0).unwrap_or(N);
        String::from_utf16(&value[..end]).unwrap()
    }

    #[test]
    fn utf16_writer_handles_empty_and_ascii_text() {
        let empty: [u16; 4] = utf16_array("");
        assert_eq!(empty, [0; 4]);

        let ascii: [u16; 5] = utf16_array("test");
        assert_eq!(decoded(&ascii), "test");
        assert_eq!(ascii[4], 0);
    }

    #[test]
    fn utf16_writer_truncates_on_character_boundaries() {
        let chinese: [u16; 4] = utf16_array("中文测试");
        assert_eq!(decoded(&chinese), "中文测");

        let surrogate: [u16; 4] = utf16_array("A😀B");
        assert_eq!(decoded(&surrogate), "A😀");
        assert_eq!(surrogate[3], 0);
    }

    #[test]
    fn notification_title_uses_explicit_title_or_level() {
        let explicit = Notification::info("body").with_title("title");
        assert_eq!(notification_title(&explicit), "title");
        assert_eq!(
            notification_title(&Notification::warning("body")),
            "voicefox · 警告"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_notifications_use_the_application_icon() {
        use super::windows_info_flags;
        use windows_sys::Win32::UI::Shell::{NIIF_LARGE_ICON, NIIF_RESPECT_QUIET_TIME, NIIF_USER};

        assert_eq!(
            windows_info_flags(),
            NIIF_USER | NIIF_LARGE_ICON | NIIF_RESPECT_QUIET_TIME
        );
    }
}
