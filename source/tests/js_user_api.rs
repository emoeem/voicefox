use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::MusicSource;
use lx_source::js::engine::JsEngine;
use lx_source::js::js_source::JsSource;

#[tokio::test]
async fn loads_user_api_v3_source_and_gets_music_url() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("node is unavailable; skipping JS source compatibility test");
        return;
    }

    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/user_api_v3.js");
    let engine = JsEngine::new(fixture).expect("fixture source should initialize");
    assert!(engine.supports_source("kw"));
    assert_eq!(
        engine.supported_qualities("kw"),
        vec!["128k".to_string(), "320k".to_string()]
    );

    let source = JsSource::new("fixture".to_string(), engine, "kw".to_string());
    let mut song = SongInfo::new(
        "song-1".to_string(),
        SourceId::Kw,
        "Test Song".to_string(),
        "Test Singer".to_string(),
    );
    song.qualities = BTreeSet::from([Quality::Low128, Quality::High320]);

    let result = source
        .get_song_url(&song, Quality::High320)
        .await
        .expect("fixture should return a URL");

    assert_eq!(result.url, "https://example.com/kw/320k/song-1.mp3");
    assert_eq!(result.quality, Quality::High320);
}

#[test]
fn source_script_cannot_access_node_require() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("node is unavailable; skipping JS sandbox test");
        return;
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "voicefox-js-sandbox-{}-{unique}.js",
        std::process::id()
    ));
    let script = r#"
let blocked = false;
try {
    require('fs');
} catch (_) {
    blocked = true;
}
if (!blocked) throw new Error('require must not be exposed');
lx.send(lx.EVENT_NAMES.inited, {
    sources: {
        kw: {
            type: 'music',
            actions: ['musicUrl'],
            qualitys: ['128k'],
        },
    },
});
"#;
    std::fs::write(&path, script).unwrap();

    let result = JsEngine::new(path.to_str().unwrap());

    let _ = std::fs::remove_file(path);
    if let Err(error) = result {
        panic!("sandbox source should initialize: {error}");
    }
}

#[test]
fn source_script_can_initialize_over_network_in_permission_mode() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("node is unavailable; skipping JS network permission test");
        return;
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for source network request"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("failed to accept source network request: {error}"),
            }
        };
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        let body =
            r#"{"sources":{"kw":{"type":"music","actions":["musicUrl"],"qualitys":["128k"]}}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "voicefox-js-network-{}-{unique}.js",
        std::process::id()
    ));
    let script = format!(
        r#"
lx.request('http://{address}/init', {{}}, (error, response) => {{
    if (error) throw error;
    lx.send(lx.EVENT_NAMES.inited, response.body);
}});
"#
    );
    std::fs::write(&path, script).unwrap();

    let result = JsEngine::new(path.to_str().unwrap());

    let _ = std::fs::remove_file(path);
    server.join().unwrap();
    if let Err(error) = result {
        panic!("sandbox source should be allowed to initialize over network: {error}");
    }
}
