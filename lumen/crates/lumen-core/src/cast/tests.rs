//! A fake renderer in this process: it answers the multicast search, serves its description and
//! records the control calls it receives.

use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

#[derive(Default)]
struct Calls(Mutex<Vec<(String, String)>>);

struct Fake {
    search_address: std::net::SocketAddr,
    calls: Arc<Calls>,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
}

fn description(name: &str, port: u16) -> String {
    format!(
        "<root><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>{name}</friendlyName><UDN>uuid:fake-{port}</UDN><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/AVT/control</controlURL></service></serviceList></device></root>"
    )
}

async fn read_request(
    stream: &mut tokio::net::TcpStream,
) -> Option<(String, std::collections::HashMap<String, String>, String)> {
    let mut data = Vec::new();
    let mut byte = [0u8; 1];
    while !data.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        data.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&data).into_owned();
    let mut lines = head.split("\r\n");
    let request = lines.next()?.to_string();
    let headers: std::collections::HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.ok()?;
    Some((
        request,
        headers,
        String::from_utf8_lossy(&body).into_owned(),
    ))
}

async fn fake(name: &str) -> Fake {
    let calls = Arc::new(Calls::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let description = description(name, port);
    let seen = calls.clone();
    let http = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let description = description.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let Some((request, headers, body)) = read_request(&mut stream).await else {
                    return;
                };
                let reply = if request.starts_with("GET /desc.xml") {
                    description
                } else {
                    let action = headers.get("soapaction").cloned().unwrap_or_default();
                    let name = action
                        .trim_matches('"')
                        .rsplit('#')
                        .next()
                        .unwrap_or("")
                        .to_string();
                    seen.0.lock().unwrap().push((name.clone(), body));
                    match name.as_str() {
                        "GetTransportInfo" => "<r><CurrentTransportState>PLAYING</CurrentTransportState></r>".into(),
                        "GetPositionInfo" => "<r><RelTime>0:01:30</RelTime><TrackDuration>0:20:00</TrackDuration></r>".into(),
                        _ => "<r/>".to_string(),
                    }
                };
                let _ = stream
                    .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes())
                    .await;
            });
        }
    });
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let search_address = udp.local_addr().unwrap();
    let answer = format!(
        "HTTP/1.1 200 OK\r\nLOCATION: http://127.0.0.1:{port}/desc.xml\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\nUSN: uuid:fake-{port}\r\n\r\n"
    );
    let responder = tokio::spawn(async move {
        let mut buffer = [0u8; 2048];
        while let Ok((_, from)) = udp.recv_from(&mut buffer).await {
            let _ = udp.send_to(answer.as_bytes(), from).await;
        }
    });
    Fake {
        search_address,
        calls,
        _tasks: vec![http, responder],
    }
}

fn actions(fake: &Fake) -> Vec<String> {
    fake.calls
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(a, _)| a.clone())
        .collect()
}

#[tokio::test]
async fn a_renderer_on_the_network_is_found_by_name() {
    let tv = fake("Bedroom TV").await;
    let found = discover_at(tv.search_address, Duration::from_millis(400))
        .await
        .unwrap();
    assert_eq!(found.len(), 1, "asked twice, listed once");
    assert_eq!(found[0].name, "Bedroom TV");
    assert_eq!(found[0].address, "127.0.0.1");
    assert!(
        serde_json::to_string(&found[0])
            .unwrap()
            .contains("Bedroom TV")
    );
    assert!(
        !serde_json::to_string(&found[0])
            .unwrap()
            .contains("control"),
        "internals stay out of the page"
    );
}

#[tokio::test]
async fn a_device_that_announces_a_public_address_is_ignored() {
    let public = Url::parse("http://93.184.216.34/desc.xml").unwrap();
    assert!(describe(public).await.is_err());
    let named = Url::parse("http://tv.example/desc.xml").unwrap();
    assert!(
        describe(named).await.is_err(),
        "names are not trusted, only local addresses"
    );
}

#[tokio::test]
async fn an_address_is_sent_and_played_in_order() {
    let tv = fake("TV").await;
    let renderer = discover_at(tv.search_address, Duration::from_millis(300))
        .await
        .unwrap()
        .remove(0);
    let session = start(
        &renderer,
        Source::Url("https://example.com/movie.mkv?a=1&b=2".into()),
        "Movie & Co",
        0.0,
        false,
    )
    .await
    .unwrap();
    assert_eq!(actions(&tv), ["Stop", "SetAVTransportURI", "Play"]);
    let set = tv.calls.0.lock().unwrap()[1].1.clone();
    assert!(set.contains("https://example.com/movie.mkv?a=1&amp;b=2"));
    assert!(set.contains("video/x-matroska"), "{set}");
    assert!(
        set.contains("Movie &amp;amp; Co"),
        "the title is escaped inside the metadata, too"
    );

    session.renderer.pause().await.unwrap();
    session.renderer.seek(125.0).await.unwrap();
    session.renderer.stop().await.unwrap();
    let calls = tv.calls.0.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|(a, b)| a == "Seek" && b.contains("<Target>0:02:05</Target>"))
    );
    drop(calls);
    let status = renderer.status().await.unwrap();
    assert_eq!(
        status,
        Status {
            state: "PLAYING".into(),
            position: 90.0,
            duration: 1200.0
        }
    );
}

#[tokio::test]
async fn starting_partway_in_seeks_after_play() {
    let tv = fake("TV").await;
    let renderer = discover_at(tv.search_address, Duration::from_millis(300))
        .await
        .unwrap()
        .remove(0);
    start(
        &renderer,
        Source::Url("https://example.com/a.mp4".into()),
        "A",
        300.0,
        false,
    )
    .await
    .unwrap();
    assert_eq!(actions(&tv), ["Stop", "SetAVTransportURI", "Play", "Seek"]);
}

#[tokio::test]
async fn private_sources_need_the_local_network_setting() {
    let tv = fake("TV").await;
    let renderer = discover_at(tv.search_address, Duration::from_millis(300))
        .await
        .unwrap()
        .remove(0);
    for blocked in [
        "http://169.254.169.254/latest",
        "file:///etc/passwd",
        "http://192.168.1.5/a.mp4",
    ] {
        assert!(
            start(&renderer, Source::Url(blocked.into()), "x", 0.0, false)
                .await
                .is_err(),
            "{blocked}"
        );
    }
    assert!(actions(&tv).is_empty(), "nothing reached the device");
    start(
        &renderer,
        Source::Url("http://192.168.1.5/a.mp4".into()),
        "x",
        0.0,
        true,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_local_file_is_served_to_the_device_only() {
    let tv = fake("TV").await;
    let renderer = discover_at(tv.search_address, Duration::from_millis(300))
        .await
        .unwrap()
        .remove(0);
    let dir = std::env::temp_dir().join(format!("lumen-cast-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("clip.mp4");
    let content: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
    std::fs::write(&file, &content).unwrap();

    let session = start(&renderer, Source::File(file), "Clip", 0.0, false)
        .await
        .unwrap();
    let set = tv.calls.0.lock().unwrap()[1].1.clone();
    let start_at = set
        .find("http://127.0.0.1:")
        .expect("the device is told our address");
    let address: String = set[start_at..].chars().take_while(|c| *c != '<').collect();
    assert!(
        address.contains("/m/") && address.ends_with("/media.mp4"),
        "{address}"
    );

    // The device's own address: whole file, then a range.
    let device = reqwest::Client::builder().no_proxy().build().unwrap();
    let whole = device.get(&address).send().await.unwrap();
    assert_eq!(whole.status(), 200);
    assert_eq!(whole.headers()["content-type"], "video/mp4");
    assert_eq!(whole.bytes().await.unwrap().as_ref(), content.as_slice());
    let part = device
        .get(&address)
        .header("Range", "bytes=1000-1999")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(part.headers()["content-range"], "bytes 1000-1999/100000");
    assert_eq!(part.bytes().await.unwrap().as_ref(), &content[1000..2000]);
    let beyond = device
        .get(&address)
        .header("Range", "bytes=200000-")
        .send()
        .await
        .unwrap();
    assert_eq!(beyond.status(), 416);
    let head = device.head(&address).send().await.unwrap();
    assert_eq!(head.headers()["content-length"], "100000");

    // Only media files go out: a document or a key file is refused before anything is served.
    let secret = dir.join("notes.txt");
    std::fs::write(&secret, "secret").unwrap();
    assert!(
        start(&renderer, Source::File(secret), "x", 0.0, false)
            .await
            .is_err()
    );

    // Other paths and other tokens are not served.
    let base = &address[..address.find("/m/").unwrap()];
    assert_eq!(
        device
            .get(format!("{base}/m/00000000/media.mp4"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        device
            .get(format!("{base}/etc/passwd"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    // Another computer on the network gets nothing: here, a different source address.
    let stranger = reqwest::Client::builder()
        .no_proxy()
        .local_address("127.0.0.2".parse::<IpAddr>().unwrap())
        .timeout(Duration::from_millis(800))
        .build()
        .unwrap();
    assert!(
        stranger.get(&address).send().await.is_err(),
        "only the device the file was sent to can fetch it"
    );

    // Once the session ends, the file is no longer served.
    drop(session);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(device.get(&address).send().await.is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_refusal_from_the_device_is_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                if read_request(&mut stream).await.is_some() {
                    let body = "<s:Fault><detail><UPnPError><errorCode>701</errorCode><errorDescription>Transition not available</errorDescription></UPnPError></detail></s:Fault>";
                    let _ = stream.write_all(format!("HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                }
            });
        }
    });
    let renderer = Renderer {
        id: "x".into(),
        name: "Picky TV".into(),
        address: "127.0.0.1".into(),
        control_url: Url::parse(&format!("http://127.0.0.1:{port}/c")).unwrap(),
        service_type: "urn:schemas-upnp-org:service:AVTransport:1".into(),
    };
    let error = String::from(renderer.pause().await.unwrap_err());
    assert!(
        error.contains("Picky TV") && error.contains("Transition not available"),
        "{error}"
    );
}
