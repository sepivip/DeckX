use std::{
    fs,
    io::{Cursor, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use axum::http::{HeaderMap, HeaderValue, header};
use base64::{Engine, engine::general_purpose::STANDARD};
use resvg::usvg;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::{
    Instance, PluginHost, load_json,
    package::{self, Plugin},
    server::{Server, local_request, lock},
    token,
    visual::{RenderSpec, rasterize_svg, render, svg_images},
};
use crate::profile::Control;

const MANIFEST: (&str, &[u8]) = ("x.sdPlugin/manifest.json", b"{}");
const RED: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60"><rect width="60" height="60" fill="red"/></svg>"#;

fn manifest(id: &str, actions: &[&str]) -> Vec<u8> {
    let actions = actions
        .iter()
        .map(|uuid| json!({"UUID": uuid, "Name": uuid, "PropertyInspectorPath": "pi.html"}))
        .collect::<Vec<_>>();
    json!({"UUID": id, "Name": id, "Author": "DeckX", "CodePath": "plugin.js", "Actions": actions})
        .to_string()
        .into_bytes()
}

/// Writes a plugin folder with a Node entry point and an inspector page.
fn plugin_folder(parent: &Path, name: &str, manifest: &[u8]) -> Result<PathBuf> {
    let root = parent.join(name);
    fs::create_dir_all(&root)?;
    fs::write(root.join("manifest.json"), manifest)?;
    fs::write(root.join("plugin.js"), "")?;
    fs::write(root.join("pi.html"), "<!doctype html>")?;
    Ok(root)
}

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

fn archive(path: &Path, entries: &[(&str, &[u8])]) -> Result<()> {
    let mut zip = ZipWriter::new(fs::File::create(path)?);
    for (name, bytes) in entries {
        zip.start_file(*name, options())?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(())
}

/// Extracts an archive into a new folder named after it.
fn extract(source: &Path) -> Result<PathBuf> {
    let destination = source.with_extension("out");
    fs::create_dir(&destination)?;
    package::extract(source, &destination)
}

fn offset(bytes: &[u8], signature: &[u8], index: usize) -> usize {
    bytes
        .windows(signature.len())
        .enumerate()
        .filter(|(_, window)| *window == signature)
        .nth(index)
        .expect("zip header")
        .0
}

#[test]
fn packages_extract_inside_their_folder() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("valid.zip");
    archive(&source, &[MANIFEST, ("x.sdPlugin/images/icon.png", b"png")])?;
    let root = extract(&source)?;
    assert!(root.ends_with("x.sdPlugin"));
    assert!(root.join("images").join("icon.png").is_file());

    for (index, name) in [
        "../evil.txt",
        "x.sdPlugin/../../evil.txt",
        "x.sdPlugin/a/../b.txt",
        "/evil.txt",
        r"\evil.txt",
        "C:/evil.txt",
        "C:evil.txt",
        r"\\localhost\share\evil.txt",
        r"\\?\C:\evil.txt",
        "x.sdPlugin/file.txt:hidden",
        "x.sdPlugin/CON",
        "x.sdPlugin/nul.txt",
        "x.sdPlugin/COM1.js",
        "x.sdPlugin/file.",
    ]
    .into_iter()
    .enumerate()
    {
        let source = temporary.path().join(format!("unsafe-{index}.zip"));
        archive(&source, &[MANIFEST, (name, b"x")])?;
        assert!(extract(&source).is_err(), "{name} was extracted");
    }
    assert!(!temporary.path().join("evil.txt").exists());
    Ok(())
}

#[test]
fn package_links_and_sizes_are_checked() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("link.zip");
    let mut zip = ZipWriter::new(fs::File::create(&source)?);
    zip.start_file(MANIFEST.0, options())?;
    zip.write_all(MANIFEST.1)?;
    zip.add_symlink("x.sdPlugin/link", "../../outside", options())?;
    zip.finish()?;
    let error = extract(&source).expect_err("a symbolic link was extracted");
    assert!(format!("{error:#}").contains("symbolic links"), "{error:#}");

    // Declare 10 bytes for an entry that stores 100.
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(MANIFEST.0, options())?;
    zip.write_all(MANIFEST.1)?;
    zip.start_file("x.sdPlugin/big.bin", options())?;
    zip.write_all(&[b'A'; 100])?;
    let mut bytes = zip.finish()?.into_inner();
    let local = offset(&bytes, b"PK\x03\x04", 1);
    let central = offset(&bytes, b"PK\x01\x02", 1);
    bytes[local + 22..local + 26].copy_from_slice(&10u32.to_le_bytes());
    bytes[central + 24..central + 28].copy_from_slice(&10u32.to_le_bytes());
    let source = temporary.path().join("size.zip");
    fs::write(&source, bytes)?;
    let error = extract(&source).expect_err("an oversized entry was extracted");
    assert!(
        format!("{error:#}").contains("Unexpected expanded plugin size"),
        "{error:#}"
    );

    // Windows file names ignore case, so the second entry would replace the first.
    let source = temporary.path().join("case.zip");
    archive(
        &source,
        &[
            MANIFEST,
            ("x.sdPlugin/Readme.txt", b"a"),
            ("x.sdPlugin/README.txt", b"b"),
        ],
    )?;
    assert!(extract(&source).is_err());
    Ok(())
}

#[test]
fn local_files_stay_inside_the_plugin() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("plugin");
    fs::create_dir_all(root.join("sub"))?;
    fs::write(root.join("a.txt"), "a")?;
    fs::write(root.join("sub").join("b.txt"), "b")?;
    fs::write(temporary.path().join("outside.txt"), "x")?;
    let root = fs::canonicalize(root)?;
    for name in ["a.txt", "./a.txt", "sub/b.txt", r"sub\b.txt"] {
        assert!(
            package::local_file(&root, name).is_some_and(|path| path.starts_with(&root)),
            "{name} was not found"
        );
    }
    // Absolute names are rejected even when they point inside the plugin.
    let verbatim = root.join("a.txt").display().to_string();
    let absolute = verbatim.trim_start_matches(r"\\?\").to_owned();
    for name in [
        verbatim.as_str(),
        absolute.as_str(),
        "C:a.txt",
        r"\\localhost\C$\Windows\win.ini",
        r"\\?\UNC\localhost\share\a.txt",
        r"\\.\pipe\deckx",
        "/a.txt",
        r"\a.txt",
        "../outside.txt",
        "sub/../a.txt",
        "a.txt:stream",
        "CON",
        "sub/NUL.txt",
        "",
        "missing.txt",
        "sub",
    ] {
        assert!(
            package::local_file(&root, name).is_none(),
            "{name} was accepted"
        );
    }
    Ok(())
}

#[test]
fn identifiers_are_safe_file_names() {
    for valid in [
        "com.elgato.hello-world",
        "a",
        "com_example.plugin-2",
        "COM10",
        "console.app",
    ] {
        assert!(package::identifier(valid), "{valid} was rejected");
    }
    let long = "x".repeat(241);
    for invalid in [
        "",
        ".",
        "..",
        "CON",
        "con",
        "NUL.plugin",
        "com1",
        "LPT9.x",
        "AUX",
        "trailing.",
        "a/b",
        r"a\b",
        "a:b",
        long.as_str(),
    ] {
        assert!(!package::identifier(invalid), "{invalid} was accepted");
    }
}

#[test]
fn manifests_are_validated() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend(manifest("com.test.bom", &["com.test.bom.act"]));
    let plugin = Plugin::read(&plugin_folder(temporary.path(), "bom", &bom)?, None)?;
    assert_eq!(plugin.id, "com.test.bom");
    assert!(
        plugin
            .action("com.test.bom.act")
            .is_some_and(|action| action.inspector.is_some())
    );
    let unnamed = br#"{"Name":"X","CodePath":"plugin.js","Actions":[{"UUID":"com.test.folder.a","Name":"A"}]}"#;
    let plugin = Plugin::read(
        &plugin_folder(temporary.path(), "com.test.folder.sdPlugin", unnamed)?,
        None,
    )?;
    assert_eq!(plugin.id, "com.test.folder");

    let mut large = manifest("com.test.large", &["com.test.large.act"]);
    large.resize(1_048_577, b' ');
    let invalid: [(&str, &[u8]); 13] = [
        ("syntax", b"{\"UUID\":"),
        ("large", &large),
        ("no-actions", br#"{"UUID":"com.test.x","Name":"X","CodePath":"plugin.js"}"#),
        ("empty-actions", br#"{"UUID":"com.test.x","Name":"X","CodePath":"plugin.js","Actions":[]}"#),
        ("no-code", br#"{"UUID":"com.test.x","Name":"X","Actions":[{"UUID":"com.test.x.a","Name":"A"}]}"#),
        ("absolute-code", br#"{"UUID":"com.test.x","Name":"X","CodePath":"C:\\Windows\\System32\\cmd.exe","Actions":[{"UUID":"com.test.x.a","Name":"A"}]}"#),
        ("unc-code", br#"{"UUID":"com.test.x","Name":"X","CodePath":"\\\\localhost\\share\\plugin.exe","Actions":[{"UUID":"com.test.x.a","Name":"A"}]}"#),
        ("parent-code", br#"{"UUID":"com.test.x","Name":"X","CodePath":"../bom/plugin.js","Actions":[{"UUID":"com.test.x.a","Name":"A"}]}"#),
        ("device-uuid", br#"{"UUID":"CON","Name":"X","CodePath":"plugin.js","Actions":[{"UUID":"com.test.x.a","Name":"A"}]}"#),
        ("dot-uuid", br#"{"UUID":"..","Name":"X","CodePath":"plugin.js","Actions":[{"UUID":"com.test.x.a","Name":"A"}]}"#),
        ("duplicate-actions", br#"{"UUID":"com.test.x","Name":"X","CodePath":"plugin.js","Actions":[{"UUID":"com.test.x.a","Name":"A"},{"UUID":"com.test.x.a","Name":"B"}]}"#),
        ("mac-only", br#"{"UUID":"com.test.x","Name":"X","CodePath":"plugin.js","OS":[{"Platform":"mac"}],"Actions":[{"UUID":"com.test.x.a","Name":"A"}]}"#),
        ("sdk-4", br#"{"UUID":"com.test.x","Name":"X","CodePath":"plugin.js","SDKVersion":4,"Actions":[{"UUID":"com.test.x.a","Name":"A"}]}"#),
    ];
    for (name, manifest) in invalid {
        let root = plugin_folder(temporary.path(), name, manifest)?;
        assert!(Plugin::read(&root, None).is_err(), "{name} was accepted");
    }
    Ok(())
}

#[test]
fn folder_imports_are_copied_updated_and_uninstalled() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let source = plugin_folder(
        temporary.path(),
        "com.test.copy.sdPlugin",
        &manifest("com.test.copy", &["com.test.copy.act"]),
    )?;
    let mut host = PluginHost::at(temporary.path().join("PluginHost"))?;
    assert_eq!(host.import(&source)?, "com.test.copy");
    let packages = fs::canonicalize(host.packages())?;
    let first = host.plugins["com.test.copy"].root.clone();
    assert!(first.starts_with(&packages), "the folder was not copied");
    assert_eq!(host.owner("com.test.copy.act"), Some("com.test.copy"));
    assert!(
        host.item("com.test.copy.act")
            .is_some_and(|item| item.inspector)
    );

    host.import(&source)?;
    let second = host.plugins["com.test.copy"].root.clone();
    assert_ne!(first, second);
    assert!(!first.exists(), "the replaced package was kept");
    let installed = host.installed();
    assert_eq!(installed.len(), 1);
    assert!(!installed[0].enabled);

    host.uninstall("com.test.copy")?;
    assert!(host.installed().is_empty());
    assert!(host.owner("com.test.copy.act").is_none());
    assert!(host.catalog().is_empty());
    assert!(!second.exists(), "the package was kept");
    assert!(
        source.join("manifest.json").is_file(),
        "the imported folder changed"
    );
    Ok(())
}

#[test]
fn local_requests_need_a_loopback_host_and_origin() {
    let headers = |host: Option<&str>, origin: Option<&str>| {
        let mut headers = HeaderMap::new();
        for (name, value) in [(header::HOST, host), (header::ORIGIN, origin)] {
            if let Some(value) = value {
                headers.insert(name, HeaderValue::from_str(value).expect("header"));
            }
        }
        headers
    };
    for (host, origin) in [
        ("127.0.0.1:4100", None),
        ("localhost:4100", None),
        ("127.0.0.1:4100", Some("http://127.0.0.1:4100")),
        ("localhost:4100", Some("http://localhost:4100")),
    ] {
        assert!(
            local_request(&headers(Some(host), origin), 4100),
            "{host} {origin:?} was rejected"
        );
    }
    for (host, origin) in [
        (None, None),
        (Some("127.0.0.1:4101"), None),
        (Some("127.0.0.1"), None),
        (Some("deckx.example:4100"), None),
        (Some("127.0.0.1:4100"), Some("http://deckx.example")),
        (Some("127.0.0.1:4100"), Some("null")),
        (Some("127.0.0.1:4100"), Some("https://127.0.0.1:4100")),
        (Some("127.0.0.1:4100"), Some("http://127.0.0.1:4101")),
    ] {
        assert!(
            !local_request(&headers(host, origin), 4100),
            "{host:?} {origin:?} was accepted"
        );
    }
}

/// A hub with plugins A and B running and one visible binding for each.
struct Fixture {
    server: Server,
    a: Instance,
    b: Instance,
    token: String,
    _temporary: tempfile::TempDir,
}

fn hub() -> Result<Fixture> {
    let temporary = tempfile::tempdir()?;
    let server = Server::start(temporary.path().join("settings.json"))?;
    let read = |id: &str| -> Result<Plugin> {
        let action = format!("{id}.act");
        Plugin::read(
            &plugin_folder(temporary.path(), id, &manifest(id, &[action.as_str()]))?,
            None,
        )
    };
    let binding = |id: &str, index| Instance {
        context: format!("context-{id}"),
        action: format!("{id}.act"),
        control: Control::Button(index),
    };
    let (a, b) = (binding("com.test.a", 0), binding("com.test.b", 1));
    let token = token();
    {
        let mut hub = lock(&server.hub);
        hub.begin(read("com.test.a")?, token.clone());
        hub.begin(read("com.test.b")?, super::token());
        hub.set_visible(vec![a.clone(), b.clone()]);
    }
    Ok(Fixture {
        server,
        a,
        b,
        token,
        _temporary: temporary,
    })
}

#[test]
fn registration_needs_an_unused_token_and_its_event() -> Result<()> {
    let fixture = hub()?;
    let mut hub = lock(&fixture.server.hub);
    let (tx, _messages) = mpsc::channel(128);
    assert!(!hub.authenticate("unknown", "registerPlugin", tx.clone()));
    assert!(!hub.authenticate(&fixture.token, "registerPropertyInspector", tx.clone()));
    assert!(hub.authenticate(&fixture.token, "registerPlugin", tx.clone()));
    assert!(!hub.authenticate(&fixture.token, "registerPlugin", tx));
    assert!(hub.running("com.test.a"));
    Ok(())
}

#[test]
fn plugins_and_inspectors_reach_only_their_bindings() -> Result<()> {
    let fixture = hub()?;
    let mut hub = lock(&fixture.server.hub);
    let (tx, _plugin) = mpsc::channel(128);
    assert!(hub.authenticate(&fixture.token, "registerPlugin", tx));
    let title =
        |context: &str| json!({"event":"setTitle","context":context,"payload":{"title":"Hi"}});

    hub.handle(&fixture.token, &title(&fixture.b.context));
    assert_eq!(
        hub.version(&fixture.b.context, true).0,
        0,
        "A titled B's key"
    );
    hub.handle(
        &fixture.token,
        &json!({"event":"setSettings","context":fixture.b.context,"payload":{"x":1}}),
    );
    assert_eq!(
        hub.configure("com.test.b", Some(&fixture.b), None)?,
        json!({})
    );
    hub.handle(&fixture.token, &title(&fixture.a.context));
    let version = hub.version(&fixture.a.context, true).0;
    assert!(version > 0, "A could not title its own key");

    let panel = url::Url::parse(&hub.open_panel(fixture.a.clone())?)?;
    let inspector = panel
        .path_segments()
        .and_then(|mut segments| segments.nth(1))
        .context("Missing inspector token")?
        .to_owned();
    let (tx, mut messages) = mpsc::channel(128);
    assert!(hub.authenticate(&inspector, "registerPropertyInspector", tx));
    hub.handle(&inspector, &title(&fixture.a.context));
    assert_eq!(
        hub.version(&fixture.a.context, true).0,
        version,
        "an inspector set a title"
    );
    hub.handle(
        &inspector,
        &json!({"event":"getSettings","context":fixture.b.context,"id":1}),
    );
    assert!(
        messages.try_recv().is_err(),
        "an inspector read B's settings"
    );
    hub.handle(
        &inspector,
        &json!({"event":"getSettings","context":fixture.a.context,"id":1}),
    );
    let reply = messages.try_recv()?;
    assert_eq!(reply["event"], "didReceiveSettings");
    assert_eq!(reply["id"], 1);
    Ok(())
}

#[test]
fn incoming_events_keep_their_wire_format() -> Result<()> {
    let fixture = hub()?;
    let mut hub = lock(&fixture.server.hub);
    let (tx, mut messages) = mpsc::channel(128);
    assert!(hub.authenticate(&fixture.token, "registerPlugin", tx));
    while messages.try_recv().is_ok() {}

    // A present id is echoed, even when it is null.
    hub.handle(
        &fixture.token,
        &json!({"event":"getGlobalSettings","id":null}),
    );
    assert_eq!(
        messages.try_recv()?,
        json!({"event":"didReceiveGlobalSettings","payload":{"settings":{}},"id":null})
    );
    hub.handle(&fixture.token, &json!({"event":"getGlobalSettings"}));
    assert!(messages.try_recv()?.get("id").is_none());

    for ignored in [
        json!({"event":"getSettings"}),
        json!({"event":"getSettings","context":5}),
        json!({"event":"GetSettings","context":fixture.a.context}),
        json!({"event":7}),
        json!({"context":fixture.a.context}),
        json!("getSettings"),
    ] {
        assert_eq!(hub.handle(&fixture.token, &ignored), None);
        assert!(messages.try_recv().is_err(), "{ignored} was answered");
    }

    // A mistyped state is treated as missing rather than rejecting the message.
    hub.handle(
        &fixture.token,
        &json!({"event":"setTitle","context":fixture.a.context,"payload":{"title":"Hi","state":"1"}}),
    );
    assert!(hub.version(&fixture.a.context, true).0 > 0);
    hub.handle(
        &fixture.token,
        &json!({"event":"getResources","context":fixture.a.context,"id":"r"}),
    );
    let reply = messages.try_recv()?;
    assert_eq!(reply["event"], "didReceiveResources");
    assert_eq!(reply["id"], "r");
    Ok(())
}

#[test]
fn open_url_is_rate_limited() -> Result<()> {
    let fixture = hub()?;
    let mut hub = lock(&fixture.server.hub);
    let (tx, _messages) = mpsc::channel(128);
    assert!(hub.authenticate(&fixture.token, "registerPlugin", tx));
    let open = json!({"event":"openUrl","payload":{"url":"https://example.com/"}});
    assert_eq!(
        hub.handle(&fixture.token, &open).as_deref(),
        Some("https://example.com/")
    );
    assert_eq!(hub.handle(&fixture.token, &open), None);
    Ok(())
}

fn svg_tile(svg: &str) -> image::RgbImage {
    let spec = RenderSpec {
        root: PathBuf::new(),
        image: Some(Arc::from(format!(
            "data:image/svg+xml;base64,{}",
            STANDARD.encode(svg)
        ))),
        title: None,
        flash: None,
    };
    render(&spec, [0, 0, 0], "")
}

fn red(tile: &image::RgbImage) -> bool {
    tile.pixels().any(|pixel| pixel[0] > 200 && pixel[1] < 60)
}

#[test]
fn svg_images_resolve_only_inline_svg() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let inner = temporary.path().join("inner.svg");
    fs::write(&inner, RED)?;
    let outer = |href: &str| {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="60" height="60"><image width="60" height="60" xlink:href="{href}"/></svg>"#
        )
    };
    assert!(red(&svg_tile(RED)), "the test image did not render");
    let inline = format!("data:image/svg+xml;base64,{}", STANDARD.encode(RED));
    assert!(red(&svg_tile(&outer(&inline))), "inline SVG was not drawn");
    let path = inner.display().to_string();
    let file = url::Url::from_file_path(&inner)
        .map_err(|()| anyhow::anyhow!("Invalid test path"))?
        .to_string();
    for href in [path.as_str(), file.as_str(), r"\\localhost\share\inner.svg"] {
        assert!(!red(&svg_tile(&outer(href))), "{href} was loaded");
    }
    let resolver = svg_images();
    let options = usvg::Options::default();
    assert!((resolver.resolve_string)(&path, &options).is_none());
    let png = Arc::new(b"\x89PNG\r\n\x1a\n".to_vec());
    assert!((resolver.resolve_data)("image/png", png, &options).is_none());
    assert!(
        rasterize_svg(&[0x1f, 0x8b, 8, 0])
            .is_err_and(|error| error.to_string().contains("Compressed SVG"))
    );
    Ok(())
}

fn wait_for(message: &str, mut check: impl FnMut() -> bool) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(12) {
        if check() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    anyhow::bail!("{message}");
}

#[test]
#[ignore = "Requires Elgato's unmodified Hello World sample package and Node.js 20+"]
fn elgato_package_handshake_and_settings() -> Result<()> {
    let sample = std::env::var_os("DECKX_TEST_PLUGIN")
        .context("Set DECKX_TEST_PLUGIN to the Hello World package")?;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("PluginHost");
    let mut host = PluginHost::at(root.clone())?;
    host.import(&PathBuf::from(sample))?;
    let action = host
        .catalog()
        .into_iter()
        .next()
        .context("No actions in the package")?;
    ensure!(
        action.key && action.inspector,
        "The Counter action should support a key and an inspector"
    );
    let instance = Instance {
        context: token(),
        action: action.uuid,
        control: Control::Button(0),
    };
    let hub = host.hub();
    lock(&hub).device(true);
    lock(&hub).set_visible(vec![instance.clone()]);
    host.set_enabled("com.elgato.hello-world", true)?;
    if let Err(error) = wait_for("The plugin did not register", || {
        lock(&hub).running("com.elgato.hello-world")
    }) {
        host.poll();
        let log = std::fs::read_to_string(root.join("Logs/com.elgato.hello-world.log"))
            .unwrap_or_default();
        anyhow::bail!("{error}: {:?}\n{log}", host.installed());
    }
    wait_for("willAppear did not produce a key title", || {
        lock(&hub).version(&instance.context, true).0 > 0
    })?;

    let previous = lock(&hub).version(&instance.context, true).0;
    ensure!(
        lock(&hub).input(&instance, "keyDown", json!({})),
        "Could not send keyDown"
    );
    ensure!(
        lock(&hub).input(&instance, "keyUp", json!({})),
        "Could not send keyUp"
    );
    wait_for("keyDown did not produce a title update", || {
        lock(&hub).version(&instance.context, true).0 > previous
    })?;
    let spec = lock(&hub)
        .render_spec(&instance, true)
        .context("No plugin artwork")?;
    let tile = super::render(&spec, [34, 34, 34], "");
    ensure!(
        tile.pixels().any(|p| p.0 != [34, 34, 34]),
        "Plugin title/artwork did not render"
    );

    let panel = url::Url::parse(&lock(&hub).open_panel(instance.clone())?)?;
    let port = panel.port().context("No inspector port")?;
    let mut client = TcpStream::connect(("127.0.0.1", port))?;
    client.set_read_timeout(Some(Duration::from_secs(4)))?;
    write!(
        client,
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n",
        panel.path()
    )?;
    let mut page = String::new();
    client.read_to_string(&mut page)?;
    ensure!(
        page.starts_with("HTTP/1.1 200") && page.contains("_deckx.js"),
        "Inspector bootstrap is missing"
    );
    let panel_token = panel
        .path_segments()
        .and_then(|mut segments| segments.nth(1))
        .context("Missing inspector token")?;

    let settings_client = r#"
        const [port, token, context] = process.argv.slice(1);
        const ws = new WebSocket('ws://127.0.0.1:' + port);
        const timer = setTimeout(() => process.exit(2), 5000);
        ws.addEventListener('open', () => {
            ws.send(JSON.stringify({event:'registerPropertyInspector', uuid:token}));
            ws.send(JSON.stringify({event:'setSettings', context, payload:{language:'de', titleKey:'clickme'}}));
            ws.send(JSON.stringify({event:'getSettings', context, id:'request-1'}));
        });
        ws.addEventListener('message', e => {
            const message = JSON.parse(e.data);
            if (message.event === 'didReceiveSettings' && message.id === 'request-1'
                && message.payload.settings.language === 'de') {
                clearTimeout(timer);
                ws.close();
            }
        });
    "#;
    let status = Command::new("node.exe")
        .args([
            "-e",
            settings_client,
            &port.to_string(),
            panel_token,
            &instance.context,
        ])
        .status()?;
    ensure!(status.success(), "The inspector settings round trip failed");
    wait_for("Plugin settings were not persisted", || {
        load_json::<Value>(&root.join("settings.json")).is_ok_and(|data| {
            data["settings"]["com.elgato.hello-world"][&instance.context]["language"] == "de"
        })
    })?;
    lock(&hub).device(false);
    host.set_enabled("com.elgato.hello-world", false)?;
    ensure!(
        !lock(&hub).running("com.elgato.hello-world"),
        "Plugin did not stop"
    );
    Ok(())
}

#[test]
#[ignore = "Requires a Marketplace browser plugin package and Microsoft Edge"]
fn marketplace_browser_package_registers() -> Result<()> {
    let sample =
        std::env::var_os("DECKX_TEST_BROWSER_PLUGIN").context("Set DECKX_TEST_BROWSER_PLUGIN")?;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("PluginHost");
    let mut host = PluginHost::at(root.clone())?;
    host.import(&PathBuf::from(sample))?;
    let id = host
        .installed()
        .into_iter()
        .next()
        .context("No imported plugin")?
        .id;
    ensure!(!host.catalog().is_empty(), "The package has no actions");
    host.set_enabled(&id, true)?;
    let hub = host.hub();
    if let Err(error) = wait_for("The Marketplace plugin did not register", || {
        host.poll();
        lock(&hub).running(&id)
    }) {
        host.poll();
        let log = std::fs::read_to_string(root.join(format!("Logs/{id}.log"))).unwrap_or_default();
        anyhow::bail!("{error}: {:?}\n{log}", host.installed());
    }
    host.set_enabled(&id, false)?;
    Ok(())
}
