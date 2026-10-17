//! macOS 桌面通知：`UNUserNotificationCenter` 只接受有 bundle id、且已在
//! LaunchServices 注册的 .app 进程，voicefox 是终端里直接运行的裸二进制，
//! 因此在数据目录里自建一个 helper bundle：
//!
//! - 主进程首次发通知时把自身可执行文件复制进 `voicefox-notifier.app`，
//!   写入 Info.plist 与图标，ad-hoc 签名后用 `LSRegisterURL` 注册；
//! - 每条通知启动一次 helper，由它申请权限并投递，投递完立即退出；
//! - 用户点击通知时 helper 早已退出，系统会不带参数重新拉起它，
//!   helper 在 delegate 回调里打开通知携带的链接后退出。
//!
//! helper 与主程序是同一个二进制，靠可执行文件名区分，见 [`run_if_helper`]。

use std::collections::VecDeque;
use std::ffi::{CString, c_void};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use lx_core::events::Notification;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, MainThreadMarker, Message, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSWorkspace};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSString, NSURL, ns_string};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNErrorCode, UNErrorDomain, UNMutableNotificationContent,
    UNNotificationAttachment, UNNotificationRequest, UNNotificationResponse,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use super::notification_title;

const BUNDLE_ID: &str = "io.github.emoeem.voicefox.notifier";
const BUNDLE_DIR: &str = "voicefox-notifier.app";
const HELPER_EXECUTABLE: &str = "voicefox-notifier";
/// 记录 bundle 由哪个 voicefox 二进制生成；不能放进 bundle 内部，
/// 否则会破坏签名封印。
const STAMP_FILE: &str = "voicefox-notifier.stamp";
/// 安装过程的跨进程互斥锁。
const LOCK_FILE: &str = "voicefox-notifier.lock";
const ICON_PNG: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../icons/512.png"));

const REPLACE_IDENTIFIER: &str = "voicefox-current";
const URL_KEY: &str = "url";

const EXIT_DENIED: i32 = 2;
const EXIT_NOT_ALLOWED: i32 = 3;
const EXIT_FAILED: i32 = 4;
const EXIT_TIMEOUT: i32 = 5;
const EXIT_AUTHORIZATION_TIMEOUT: i32 = 6;

/// 首次授权需要等用户点弹窗；等不到授权结果时主进程会暂停一段时间再申请。
const POST_TIMEOUT: Duration = Duration::from_secs(60);
/// 因点击被系统拉起时，response 回调通常在启动后立刻到达。
const CLICK_TIMEOUT: Duration = Duration::from_secs(10);
/// 刚注册的 bundle 要过几秒才被通知服务认可，期间返回 NotificationsNotAllowed。
const REGISTRATION_GRACE: Duration = Duration::from_secs(30);
const NOT_ALLOWED_RETRY_DELAY: Duration = Duration::from_secs(2);
const NOT_ALLOWED_MAX_RETRIES: u32 = 5;
/// 安装失败后的重试间隔，每次失败翻倍。
const INSTALL_RETRY_MIN: Duration = Duration::from_secs(30);
const INSTALL_RETRY_MAX: Duration = Duration::from_secs(600);
/// 积压超过这个时长的通知不再补发。
const STALE_AFTER: Duration = Duration::from_secs(10);
/// 用户拒绝后暂停启动 helper；冷却后再试，以便发现用户在系统设置里重新开启。
const DENIED_COOLDOWN: Duration = Duration::from_secs(60);
/// 等待授权结果超时（多半是用户没理弹窗）后暂停更久，避免反复申请打扰用户。
const AUTHORIZATION_TIMEOUT_COOLDOWN: Duration = Duration::from_secs(300);
/// helper 的 stderr 只保留结尾，足够看清失败原因。
const DETAIL_LIMIT: usize = 512;

// 主进程侧

pub(super) fn run_worker(rx: Receiver<Notification>) {
    let mut installer = Installer::default();
    let mut pending = VecDeque::new();
    let mut paused_until: Option<Instant> = None;
    let mut denied_logged = false;

    loop {
        if pending.is_empty() {
            let Ok(notification) = rx.recv() else {
                break;
            };
            pending.push_back(notification);
        }
        // 投递会阻塞（首次授权要等用户点弹窗），期间到达的通知在这里合并。
        pending.extend(rx.try_iter());
        if paused_until.is_some_and(|until| Instant::now() < until) {
            pending.clear();
            continue;
        }
        paused_until = None;
        coalesce(&mut pending, chrono::Local::now());
        let Some(notification) = pending.pop_front() else {
            continue;
        };

        match deliver(&mut installer, &notification) {
            Ok(Delivery::Delivered) => denied_logged = false,
            Ok(Delivery::Skipped) => {}
            Err(PostError::Denied) => {
                if !denied_logged {
                    tracing::info!(
                        "desktop notifications are disabled for voicefox in System Settings"
                    );
                    denied_logged = true;
                }
                paused_until = Some(Instant::now() + DENIED_COOLDOWN);
            }
            Err(PostError::AuthorizationTimeout) => {
                tracing::info!(
                    "timed out waiting for the notification authorization result, pausing desktop notifications for {}s",
                    AUTHORIZATION_TIMEOUT_COOLDOWN.as_secs()
                );
                paused_until = Some(Instant::now() + AUTHORIZATION_TIMEOUT_COOLDOWN);
            }
            Err(error) => tracing::warn!("desktop notification failed: {error}"),
        }
    }
}

enum Delivery {
    Delivered,
    /// helper 不可用（安装失败或处于退避期），通知未发送。
    Skipped,
}

fn deliver(installer: &mut Installer, notification: &Notification) -> Result<Delivery, PostError> {
    let Some(helper) = installer.helper() else {
        return Ok(Delivery::Skipped);
    };
    let result = helper.post(notification);
    // helper 被删除或替换失败时，重新检查安装后补发一次。
    if !matches!(result, Err(PostError::Spawn(_))) {
        return result.map(|()| Delivery::Delivered);
    }
    installer.invalidate();
    match installer.helper() {
        Some(helper) => helper.post(notification).map(|()| Delivery::Delivered),
        None => result.map(|()| Delivery::Delivered),
    }
}

/// 桌面通知只有即时价值，TUI toast 已经显示过每一条，积压的不必补发：
/// 替换型通知（换曲）只留最新一条，不受时效限制；其余丢弃过时的。
fn coalesce(pending: &mut VecDeque<Notification>, now: chrono::DateTime<chrono::Local>) {
    let latest_replacing = pending
        .iter()
        .rposition(|notification| notification.replace_previous);
    let mut index = 0;
    pending.retain(|notification| {
        let keep = if notification.replace_previous {
            Some(index) == latest_replacing
        } else {
            (now - notification.created_at)
                .to_std()
                .map_or(true, |age| age <= STALE_AFTER)
        };
        index += 1;
        keep
    });
}

/// 缓存已安装的 helper；安装失败按退避间隔重试
#[derive(Default)]
struct Installer {
    helper: Option<HelperBundle>,
    retry_at: Option<Instant>,
    backoff: Duration,
}

impl Installer {
    fn helper(&mut self) -> Option<&HelperBundle> {
        let due = self.retry_at.is_none_or(|at| Instant::now() >= at);
        if self.helper.is_none() && due {
            match HelperBundle::install() {
                Ok(helper) => {
                    self.helper = Some(helper);
                    self.retry_at = None;
                    self.backoff = Duration::ZERO;
                }
                Err(error) => {
                    self.backoff = next_backoff(self.backoff);
                    self.retry_at = Some(Instant::now() + self.backoff);
                    tracing::warn!(
                        "macOS notification helper setup failed, retrying in {}s: {error}",
                        self.backoff.as_secs()
                    );
                }
            }
        }
        self.helper.as_ref()
    }

    fn invalidate(&mut self) {
        self.helper = None;
    }
}

fn next_backoff(previous: Duration) -> Duration {
    (previous * 2).clamp(INSTALL_RETRY_MIN, INSTALL_RETRY_MAX)
}

struct HelperBundle {
    executable: PathBuf,
    registered_at: Instant,
}

#[derive(Debug, thiserror::Error)]
enum PostError {
    #[error("notifications denied by the user")]
    Denied,
    #[error("timed out waiting for the authorization result")]
    AuthorizationTimeout,
    #[error("helper is not yet allowed to post notifications: {0}")]
    NotAllowed(String),
    #[error("helper exited with {status}: {detail}")]
    Failed {
        status: std::process::ExitStatus,
        detail: String,
    },
    #[error("failed to run helper: {0}")]
    Spawn(#[from] io::Error),
}

impl HelperBundle {
    fn install() -> io::Result<Self> {
        let source = std::env::current_exe()?;
        let data_dir = crate::storage::default_data_dir();
        fs::create_dir_all(&data_dir)?;
        let bundle = data_dir.join(BUNDLE_DIR);
        let executable = bundle.join("Contents/MacOS").join(HELPER_EXECUTABLE);
        let stamp_path = data_dir.join(STAMP_FILE);
        let stamp = source_stamp(&source)?;

        // 多个 voicefox 实例可能同时首次发通知或升级：检查、重建、写 stamp
        // 与注册必须串行，锁随文件句柄释放。
        let lock = fs::File::create(data_dir.join(LOCK_FILE))?;
        lock.lock()?;

        let up_to_date = executable.is_file()
            && fs::read_to_string(&stamp_path).is_ok_and(|value| value == stamp);
        if !up_to_date {
            remove_stale_staging(&data_dir);
            build_bundle(&source, &data_dir, &bundle)?;
            write_atomically(&stamp_path, &stamp)?;
        }
        register(&bundle)?;

        Ok(Self {
            executable,
            registered_at: Instant::now(),
        })
    }

    fn post(&self, notification: &Notification) -> Result<(), PostError> {
        let args = helper_args(notification);
        let mut attempt = 0;
        loop {
            match self.run(&args) {
                Err(PostError::NotAllowed(_))
                    if attempt < NOT_ALLOWED_MAX_RETRIES
                        && self.registered_at.elapsed() < REGISTRATION_GRACE =>
                {
                    attempt += 1;
                    std::thread::sleep(NOT_ALLOWED_RETRY_DELAY);
                }
                result => return result,
            }
        }
    }

    fn run(&self, args: &[String]) -> Result<(), PostError> {
        let output = Command::new(&self.executable)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        let detail = || stderr_tail(&output.stderr);
        match output.status.code() {
            Some(0) => Ok(()),
            Some(EXIT_DENIED) => Err(PostError::Denied),
            Some(EXIT_NOT_ALLOWED) => Err(PostError::NotAllowed(detail())),
            Some(EXIT_AUTHORIZATION_TIMEOUT) => Err(PostError::AuthorizationTimeout),
            _ => Err(PostError::Failed {
                status: output.status,
                detail: detail(),
            }),
        }
    }
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let start = text
        .char_indices()
        .rev()
        .nth(DETAIL_LIMIT.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    text[start..].to_string()
}

/// 源二进制换了（升级、重新编译、换位置）就重建 bundle。
fn source_stamp(source: &Path) -> io::Result<String> {
    let metadata = fs::metadata(source)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(format!(
        "{}\n{}\n{}\n{modified}\n",
        env!("CARGO_PKG_VERSION"),
        source.display(),
        metadata.len(),
    ))
}

fn staging_prefix() -> String {
    format!("{BUNDLE_DIR}.partial-")
}

/// 持锁时不可能有其他实例在组装，残留的临时目录都来自中断的安装。
fn remove_stale_staging(data_dir: &Path) {
    let Ok(entries) = fs::read_dir(data_dir) else {
        return;
    };
    let prefix = staging_prefix();
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn build_bundle(source: &Path, data_dir: &Path, bundle: &Path) -> io::Result<()> {
    // 先在临时目录里组装并签名，再与旧 bundle 原子交换：任何时刻
    // bundle 路径上都是一个完整的 helper，点击处理不会出现空窗。
    let staging = data_dir.join(format!("{}{}", staging_prefix(), std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = (|| {
        let contents = staging.join("Contents");
        fs::create_dir_all(contents.join("MacOS"))?;
        fs::create_dir_all(contents.join("Resources"))?;
        fs::copy(source, contents.join("MacOS").join(HELPER_EXECUTABLE))?;
        fs::write(contents.join("Info.plist"), info_plist())?;
        fs::write(contents.join("Resources/voicefox.icns"), icns(ICON_PNG))?;

        let output = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&staging)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "codesign exited with {}: {}",
                output.status,
                stderr_tail(&output.stderr)
            )));
        }

        replace_bundle(&staging, bundle)
    })();
    // 成功时 staging 里是被换下来的旧 bundle，失败时是半成品，都要清掉。
    let _ = fs::remove_dir_all(&staging);
    result
}

/// 用 staging 替换 bundle。已有 bundle 时用 `RENAME_SWAP` 原子交换，
/// 失败则旧 bundle 原样保留。
fn replace_bundle(staging: &Path, bundle: &Path) -> io::Result<()> {
    if !bundle.exists() {
        return fs::rename(staging, bundle);
    }
    let from = path_cstring(staging)?;
    let to = path_cstring(bundle)?;
    // SAFETY: both pointers are valid NUL-terminated paths for the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn path_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::other(format!("path contains NUL: {path:?}")))
}

fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".tmp-{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, contents)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>{HELPER_EXECUTABLE}</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleName</key>
	<string>voicefox</string>
	<key>CFBundleDisplayName</key>
	<string>voicefox</string>
	<key>CFBundleIconFile</key>
	<string>voicefox</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSMinimumSystemVersion</key>
	<string>10.14</string>
	<key>LSUIElement</key>
	<true/>
</dict>
</plist>
"#,
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// 只含一张 512×512 PNG（`ic09`）的 icns，够用。
fn icns(png: &[u8]) -> Vec<u8> {
    const HEADER_LEN: usize = 8;
    let entry_len = HEADER_LEN + png.len();
    let total_len = HEADER_LEN + entry_len;
    let mut data = Vec::with_capacity(total_len);
    data.extend_from_slice(b"icns");
    data.extend_from_slice(&(total_len as u32).to_be_bytes());
    data.extend_from_slice(b"ic09");
    data.extend_from_slice(&(entry_len as u32).to_be_bytes());
    data.extend_from_slice(png);
    data
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSRegisterURL(url: *const c_void, update: u8) -> i32;
}

fn register(bundle: &Path) -> io::Result<()> {
    let url = NSURL::from_file_path(bundle)
        .ok_or_else(|| io::Error::other(format!("invalid bundle path {bundle:?}")))?;
    // SAFETY: NSURL is toll-free bridged to CFURLRef and outlives the call.
    let status = unsafe { LSRegisterURL(Retained::as_ptr(&url).cast(), 1) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!("LSRegisterURL failed: {status}")))
    }
}

fn helper_args(notification: &Notification) -> Vec<String> {
    let mut args = vec![
        "--title".to_string(),
        notification_title(notification),
        "--body".to_string(),
        notification.message.clone(),
    ];
    if let Some(icon) = notification
        .icon
        .as_ref()
        .filter(|icon| Path::new(icon).is_file())
    {
        args.extend(["--image".to_string(), icon.clone()]);
    }
    if let Some(url) = &notification.action_url {
        args.extend(["--url".to_string(), url.clone()]);
    }
    if notification.replace_previous {
        args.push("--replace".to_string());
    }
    args
}

// helper 进程侧

/// 当前进程是 helper 时接管并退出，不会返回；否则立即返回。
/// 必须在 `main` 最开头调用：被系统因点击拉起时没有任何参数，
/// 不能交给 CLI 解析。
pub fn run_if_helper() {
    let is_helper = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(|name| name == HELPER_EXECUTABLE))
        .unwrap_or(false);
    if is_helper {
        helper_main();
    }
}

#[derive(Debug, Default, PartialEq)]
struct PostRequest {
    title: String,
    body: String,
    image: Option<String>,
    url: Option<String>,
    replace: bool,
}

fn parse_helper_args(args: impl IntoIterator<Item = String>) -> Option<PostRequest> {
    let mut request = PostRequest::default();
    let mut has_body = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--title" => request.title = args.next()?,
            "--body" => {
                request.body = args.next()?;
                has_body = true;
            }
            "--image" => request.image = Some(args.next()?),
            "--url" => request.url = Some(args.next()?),
            "--replace" => request.replace = true,
            // LaunchServices 拉起时可能附带 -psn_ 等系统参数。
            _ => {}
        }
    }
    has_body.then_some(request)
}

fn helper_main() -> ! {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("helper must run on the main thread");
        std::process::exit(EXIT_FAILED);
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);

    let request = parse_helper_args(std::env::args().skip(1));
    let timeout = if request.is_some() {
        POST_TIMEOUT
    } else {
        CLICK_TIMEOUT
    };
    // 生命周期必须在设置 delegate 之前确定，点击回调可能随后立即到达。
    *lifecycle() = match request {
        Some(_) => Lifecycle::posting(),
        None => Lifecycle::click_launch(),
    };

    // delegate 是弱引用，必须在 app.run() 期间保持存活。
    let delegate = ClickDelegate::new();
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    if let Some(request) = request {
        post(&center, request);
    }

    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        let code = lifecycle().timeout_code();
        if code != 0 {
            eprintln!("timed out after {}s", timeout.as_secs());
        }
        std::process::exit(code);
    });

    app.run();
    std::process::exit(0);
}

/// helper 何时退出、以什么退出码退出，统一由这里决定：发送模式要等投递
/// 结果，点击启动模式要等一次点击，正在处理的点击总要处理完。退出码只
/// 反映发送结果，主进程据此判断投递是否成功。
#[derive(Debug, PartialEq)]
struct Lifecycle {
    /// 发送结果；`None` 表示仍在等待。点击启动模式没有发送，固定为 `Some(0)`。
    post_code: Option<i32>,
    authorization_completed: bool,
    awaiting_click: bool,
    clicks_in_flight: u32,
}

impl Lifecycle {
    const fn posting() -> Self {
        Self {
            post_code: None,
            authorization_completed: false,
            awaiting_click: false,
            clicks_in_flight: 0,
        }
    }

    const fn click_launch() -> Self {
        Self {
            post_code: Some(0),
            authorization_completed: true,
            awaiting_click: true,
            clicks_in_flight: 0,
        }
    }

    fn exit_code(&self) -> Option<i32> {
        if self.clicks_in_flight > 0 || self.awaiting_click {
            return None;
        }
        self.post_code
    }

    fn finish_post(&mut self, code: i32) -> Option<i32> {
        self.post_code.get_or_insert(code);
        self.exit_code()
    }

    fn begin_click(&mut self) {
        self.clicks_in_flight += 1;
    }

    fn end_click(&mut self) -> Option<i32> {
        self.clicks_in_flight = self.clicks_in_flight.saturating_sub(1);
        self.awaiting_click = false;
        self.exit_code()
    }

    /// 超时只能说明没等到授权回调（多半是用户没理弹窗，也可能是系统服务
    /// 没响应），并不能证明用户做了什么。
    fn timeout_code(&self) -> i32 {
        match self.post_code {
            Some(code) => code,
            None if self.authorization_completed => EXIT_TIMEOUT,
            None => EXIT_AUTHORIZATION_TIMEOUT,
        }
    }
}

static LIFECYCLE: Mutex<Lifecycle> = Mutex::new(Lifecycle::click_launch());

fn lifecycle() -> std::sync::MutexGuard<'static, Lifecycle> {
    LIFECYCLE.lock().unwrap_or_else(|error| error.into_inner())
}

fn finish_post(code: i32) {
    if let Some(code) = lifecycle().finish_post(code) {
        std::process::exit(code);
    }
}

fn post(center: &UNUserNotificationCenter, request: PostRequest) {
    let poster = center.retain();
    let handler = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        lifecycle().authorization_completed = true;
        if !granted.as_bool() {
            // SAFETY: the framework passes either null or a valid NSError.
            match unsafe { error.as_ref() } {
                Some(error) => fail("requestAuthorization", error),
                None => {
                    eprintln!("authorization not granted");
                    finish_post(EXIT_DENIED);
                }
            }
            return;
        }
        let content = build_content(&request);
        let identifier = if request.replace {
            REPLACE_IDENTIFIER.to_string()
        } else {
            unique_identifier()
        };
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&identifier),
            &content,
            None,
        );
        let done = RcBlock::new(|error: *mut NSError| {
            // SAFETY: the framework passes either null or a valid NSError.
            match unsafe { error.as_ref() } {
                Some(error) => fail("addNotificationRequest", error),
                None => finish_post(0),
            }
        });
        poster.addNotificationRequest_withCompletionHandler(&request, Some(&done));
    });
    center
        .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &handler);
}

/// 授权与投递共用的错误分类：只有 UNErrorDomain 的 NotificationsNotAllowed
/// 代表 bundle 尚未被通知服务认可、值得重试，其余都是系统错误。
fn exit_code_for(error: &NSError) -> i32 {
    // SAFETY: UNErrorDomain is an immutable framework constant.
    let domain = unsafe { UNErrorDomain };
    let is_user_notifications = domain.is_some_and(|domain| *error.domain() == *domain);
    if is_user_notifications && error.code() == UNErrorCode::NotificationsNotAllowed.0 {
        EXIT_NOT_ALLOWED
    } else {
        EXIT_FAILED
    }
}

fn fail(stage: &str, error: &NSError) {
    eprintln!(
        "{stage}: {} ({} {})",
        error.localizedDescription(),
        error.domain(),
        error.code()
    );
    finish_post(exit_code_for(error));
}

fn build_content(request: &PostRequest) -> Retained<UNMutableNotificationContent> {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&request.title));
    content.setBody(&NSString::from_str(&request.body));
    if let Some(url) = &request.url {
        let value = NSString::from_str(url);
        let user_info = NSDictionary::from_slices(&[ns_string!(URL_KEY)], &[&*value]);
        // SAFETY: NSString keys and values are valid as AnyObject; userInfo
        // only needs to be a property-list dictionary.
        unsafe { content.setUserInfo(user_info.cast_unchecked::<AnyObject, AnyObject>()) };
    }
    if let Some(attachment) = request.image.as_deref().and_then(attachment) {
        content.setAttachments(&NSArray::from_retained_slice(&[attachment]));
    }
    content
}

/// 通知服务会把附件文件移进自己的存储区，因此先复制一份；
/// 文件类型由扩展名判断，而封面缓存统一叫 `.jpg`，所以按内容嗅探。
fn attachment(path: &str) -> Option<Retained<UNNotificationAttachment>> {
    let format = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .format()?;
    let extension = format.extensions_str().first()?;
    let copy = std::env::temp_dir().join(format!("{}.{extension}", unique_identifier()));
    fs::copy(path, &copy).ok()?;
    let url = NSURL::from_file_path(&copy)?;
    // SAFETY: options is nil, which the API accepts.
    let result = unsafe {
        UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(
            ns_string!("cover"),
            &url,
            None,
        )
    };
    if result.is_err() {
        let _ = fs::remove_file(&copy);
    }
    result.ok()
}

fn unique_identifier() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("voicefox-{}-{nanos}", std::process::id())
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VoicefoxNotificationClickDelegate"]
    struct ClickDelegate;

    unsafe impl NSObjectProtocol for ClickDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for ClickDelegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            lifecycle().begin_click();
            let user_info = response.notification().request().content().userInfo();
            let url = user_info
                .objectForKey(ns_string!(URL_KEY))
                .and_then(|value| value.downcast::<NSString>().ok())
                .and_then(|value| NSURL::URLWithString(&value));
            if let Some(url) = url {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
            completion_handler.call(());
            if let Some(code) = lifecycle().end_click() {
                std::process::exit(code);
            }
        }
    }
);

impl ClickDelegate {
    fn new() -> Retained<Self> {
        // SAFETY: plain NSObject subclass without ivars.
        unsafe { msg_send![Self::class(), new] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn helper_args_round_trip() {
        let notification = Notification::info("正文")
            .with_title("标题")
            .replacing_previous();
        let request = parse_helper_args(helper_args(&notification)).unwrap();
        assert_eq!(
            request,
            PostRequest {
                title: "标题".to_string(),
                body: "正文".to_string(),
                image: None,
                url: None,
                replace: true,
            }
        );
    }

    #[test]
    fn launch_without_body_is_a_click_relaunch() {
        assert_eq!(parse_helper_args(Vec::new()), None);
        assert_eq!(parse_helper_args(args(&["-psn_0_12345"])), None);
        assert_eq!(parse_helper_args(args(&["--title"])), None);
    }

    #[test]
    fn icns_wraps_png_as_ic09() {
        let data = icns(b"PNG");
        assert_eq!(&data[..4], b"icns");
        assert_eq!(u32::from_be_bytes(data[4..8].try_into().unwrap()), 19);
        assert_eq!(&data[8..12], b"ic09");
        assert_eq!(u32::from_be_bytes(data[12..16].try_into().unwrap()), 11);
        assert_eq!(&data[16..], b"PNG");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-{name}", unique_identifier()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn marked_dir(parent: &Path, name: &str, marker: &str) -> PathBuf {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("marker"), marker).unwrap();
        dir
    }

    #[test]
    fn only_not_allowed_in_user_notifications_domain_is_retryable() {
        let domain = unsafe { UNErrorDomain }.unwrap();
        let not_allowed = NSError::new(UNErrorCode::NotificationsNotAllowed.0, domain);
        let invalid_attachment = NSError::new(UNErrorCode::AttachmentInvalidURL.0, domain);
        let foreign = NSError::new(
            UNErrorCode::NotificationsNotAllowed.0,
            ns_string!("NSCocoaErrorDomain"),
        );
        assert_eq!(exit_code_for(&not_allowed), EXIT_NOT_ALLOWED);
        assert_eq!(exit_code_for(&invalid_attachment), EXIT_FAILED);
        assert_eq!(exit_code_for(&foreign), EXIT_FAILED);
    }

    fn aged(
        notification: Notification,
        now: chrono::DateTime<chrono::Local>,
        secs: i64,
    ) -> Notification {
        Notification {
            created_at: now - chrono::TimeDelta::seconds(secs),
            ..notification
        }
    }

    fn messages(pending: &VecDeque<Notification>) -> Vec<&str> {
        pending
            .iter()
            .map(|notification| notification.message.as_str())
            .collect()
    }

    #[test]
    fn coalesce_keeps_latest_track_change_and_fresh_messages() {
        let now = chrono::Local::now();
        let mut pending = VecDeque::from([
            aged(Notification::info("曲 1").replacing_previous(), now, 50),
            aged(Notification::info("过时"), now, 40),
            aged(Notification::info("曲 2").replacing_previous(), now, 30),
            aged(Notification::info("新鲜"), now, 3),
            aged(Notification::info("曲 3").replacing_previous(), now, 20),
            aged(Notification::info("时钟回拨"), now, -5),
        ]);
        coalesce(&mut pending, now);
        // 最新的换曲通知即使已超过时效也保留：它代表当前正在播放的歌曲。
        assert_eq!(messages(&pending), ["新鲜", "曲 3", "时钟回拨"]);
    }

    #[test]
    fn post_and_clicks_wait_for_each_other() {
        let mut state = Lifecycle::posting();
        // 发送期间处理完的点击不能结束进程。
        state.begin_click();
        assert_eq!(state.end_click(), None);
        // 发送先完成时要等正在处理的点击；退出码取第一次的发送结果。
        state.begin_click();
        assert_eq!(state.finish_post(EXIT_DENIED), None);
        assert_eq!(state.finish_post(0), None);
        assert_eq!(state.end_click(), Some(EXIT_DENIED));
    }

    #[test]
    fn click_launch_exits_after_the_click() {
        let mut state = Lifecycle::click_launch();
        assert_eq!(state.exit_code(), None);
        state.begin_click();
        assert_eq!(state.end_click(), Some(0));
    }

    #[test]
    fn timeout_distinguishes_authorization_wait() {
        let mut state = Lifecycle::posting();
        assert_eq!(state.timeout_code(), EXIT_AUTHORIZATION_TIMEOUT);
        state.authorization_completed = true;
        assert_eq!(state.timeout_code(), EXIT_TIMEOUT);
        assert_eq!(Lifecycle::click_launch().timeout_code(), 0);
    }

    #[test]
    fn replace_bundle_swaps_existing_and_installs_fresh() {
        let dir = temp_dir("replace");
        let bundle = dir.join(BUNDLE_DIR);

        let staging = marked_dir(&dir, "first", "v1");
        replace_bundle(&staging, &bundle).unwrap();
        assert_eq!(fs::read_to_string(bundle.join("marker")).unwrap(), "v1");
        assert!(!staging.exists());

        let staging = marked_dir(&dir, "second", "v2");
        replace_bundle(&staging, &bundle).unwrap();
        assert_eq!(fs::read_to_string(bundle.join("marker")).unwrap(), "v2");
        // 旧 bundle 被换到 staging 路径上，由调用方清理。
        assert_eq!(fs::read_to_string(staging.join("marker")).unwrap(), "v1");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stderr_tail_keeps_the_end_on_char_boundaries() {
        assert_eq!(stderr_tail(b"  short\n"), "short");
        let long = format!("{}错误原因", "前".repeat(DETAIL_LIMIT));
        let tail = stderr_tail(long.as_bytes());
        assert_eq!(tail.chars().count(), DETAIL_LIMIT);
        assert!(tail.ends_with("错误原因"));
    }
}
