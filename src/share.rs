// A one-file, signed-URL localhost downloader for the File Details screen.
// Each ShareHandle owns a Tokio runtime on its own OS thread, HTTP/1 listeners
// on 127.0.0.1:<port> and [::1]:<port>. File bytes are streamed from a
// short-lived `restic dump` subprocess for each download.
//
// The server is bound to exactly one (snap_id, tree_id, name). A URL minted for
// file A cannot be replayed against a later server started for file B.

use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::{Body, Frame};
use hyper::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, HeaderValue, LOCATION};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use sha2::Sha256;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use crate::config::Profile;
use crate::local_server;
use crate::restic;

type HmacSha256 = Hmac<Sha256>;

pub(crate) const SHARE_TTL: Duration = Duration::from_secs(3600);

// What a share server is bound to: a single file inside a snapshot. Grouped
// so callers don't pass five strings positionally.
#[derive(Clone)]
pub(crate) struct ShareTarget {
    pub(crate) snap_id: String,
    pub(crate) tree_id: String,
    pub(crate) name: String,
    pub(crate) display_path: String,
}

pub(crate) struct ShareHandle {
    pub(crate) url: String,
    // Short alias: http://localhost:<port>/s/<short_id>. The server holds
    // <short_id> in-memory and 302-redirects it to the fixed long URL.
    // The short id is freshly generated on every `start` call.
    pub(crate) short_url: String,
    pub(crate) exp_unix: u64,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl ShareHandle {
    pub(crate) fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(jh) = self.join_handle.take() {
            let _ = jh.join();
        }
    }
}

impl Drop for ShareHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

fn random_short_id() -> String {
    hex_encode(&rand::random::<[u8; 8]>())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

fn canonical_message(snap: &str, tree_hex: &str, name: &str, exp: u64) -> Vec<u8> {
    let mut msg = Vec::with_capacity(snap.len() + tree_hex.len() + name.len() + 32);
    msg.extend_from_slice(b"v1\n");
    msg.extend_from_slice(snap.as_bytes());
    msg.push(b'\n');
    msg.extend_from_slice(tree_hex.as_bytes());
    msg.push(b'\n');
    msg.extend_from_slice(name.as_bytes());
    msg.push(b'\n');
    msg.extend_from_slice(exp.to_string().as_bytes());
    msg
}

fn compute_sig(key: &[u8; 32], snap: &str, tree_hex: &str, name: &str, exp: u64) -> String {
    // HMAC::new_from_slice accepts any key length and returns Result solely
    // to be future-compatible; for HMAC-SHA256 with a fixed 32-byte key it
    // never fails.
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&canonical_message(snap, tree_hex, name, exp));
    hex_encode(&mac.finalize().into_bytes())
}

// Returns (absolute_url, relative_path, exp). The absolute form is for human
// display ("here is the URL you can paste into a browser"); the relative
// form (`/dl?...`) is what the short-URL handler emits as the redirect
// `Location` so the browser stays on whatever host:port it dialed in on.
//
// `name` is part of the HMAC message but NOT a URL query parameter — the
// server knows the file's name from its bound state, so putting it in the
// URL would be redundant noise. Cross-file replays are still rejected at
// the HMAC step (different bound name = different sig).
pub(crate) fn build_signed_url(
    port: u16,
    snap_id: &str,
    tree_id: &str,
    name: &str,
    ttl: Duration,
    key: &[u8; 32],
) -> (String, String, u64) {
    let exp = (SystemTime::now() + ttl)
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let sig = compute_sig(key, snap_id, tree_id, name, exp);
    let qs: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("snap", snap_id)
        .append_pair("tree", tree_id)
        .append_pair("exp", &exp.to_string())
        .append_pair("sig", &sig)
        .finish();
    let path = format!("/dl?{qs}");
    // User-facing host is `localhost` (better authenticator/browser
    // compatibility than a bare IP literal); listeners bind both IPv4 and
    // IPv6 loopback below.
    (format!("http://localhost:{port}{path}"), path, exp)
}

struct Ctx {
    profile: Profile,
    key: [u8; 32],
    snap_id: String,
    tree_hex: String,
    name: String,
    display_path: String,
    short_id: String,
    // Path+query that the short-URL redirect points at. Path-only (no
    // scheme/host/port) so the redirect stays same-origin. Fixed for the
    // server's lifetime — there's no regenerate.
    long_path: String,
}

pub(crate) fn start(
    port: u16,
    profile: Profile,
    key: [u8; 32],
    target: ShareTarget,
    ttl: Duration,
) -> Result<ShareHandle> {
    let ShareTarget {
        snap_id,
        tree_id,
        name,
        display_path,
    } = target;
    if snap_id.len() != 64 || !snap_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected a full 64-char hex snapshot id, got `{snap_id}`");
    }
    let tree_hex = tree_id;
    if tree_hex.len() != 64 || !tree_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid TreeId hex: `{tree_hex}`");
    }

    // Bind synchronously so EADDRINUSE/permission errors surface before we
    // spin up a runtime. Hand the listeners to tokio after.
    let listeners_std = local_server::bind_localhost(port)?;

    let (url, path, exp) =
        build_signed_url(port, &snap_id, &tree_hex, &name, ttl, &key);
    let short_id = random_short_id();
    let short_url = format!("http://localhost:{port}/s/{short_id}");

    let ctx = Arc::new(Ctx {
        profile,
        key,
        snap_id,
        tree_hex,
        name,
        display_path,
        short_id,
        long_path: path,
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let thread_ctx = ctx.clone();
    let join = thread::Builder::new()
        .name(format!("wrustic-share-{port}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => return,
            };
            rt.block_on(async move {
                let mut listeners = Vec::with_capacity(listeners_std.len());
                for listener_std in listeners_std {
                    let listener = match TcpListener::from_std(listener_std) {
                        Ok(l) => l,
                        Err(_) => return,
                    };
                    listeners.push(listener);
                }
                accept_loop(listeners, thread_ctx, shutdown_rx).await;
            });
        })
        .map_err(|e| anyhow!("spawning share thread: {e}"))?;

    Ok(ShareHandle {
        url,
        short_url,
        exp_unix: exp,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join),
    })
}

async fn accept_loop(
    listeners: Vec<TcpListener>,
    ctx: Arc<Ctx>,
    shutdown_rx: oneshot::Receiver<()>,
) {
    for listener in listeners {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            accept_one(listener, ctx).await;
        });
    }

    let _ = shutdown_rx.await;
}

async fn accept_one(listener: TcpListener, ctx: Arc<Ctx>) {
    loop {
        let stream = match listener.accept().await {
            Ok((s, _)) => s,
            Err(_) => continue,
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |req| handle(req, ctx.clone()));
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .await;
        });
    }
}

type RespBody = BoxBody<Bytes, io::Error>;

fn err_resp(status: StatusCode, msg: &'static str) -> Response<RespBody> {
    let body = Full::new(Bytes::from_static(msg.as_bytes()))
        .map_err(|never: Infallible| match never {})
        .boxed();
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    resp
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    ctx: Arc<Ctx>,
) -> Result<Response<RespBody>, Infallible> {
    if req.method() != Method::GET {
        return Ok(err_resp(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"));
    }

    // Short-URL redirect: /s/<short_id> -> 302 to the current long URL.
    // The short id is fixed for the server's lifetime; the redirect target
    // is read fresh on each request so `regenerate` is picked up
    // immediately.
    if let Some(rest) = req.uri().path().strip_prefix("/s/") {
        if rest == ctx.short_id {
            // Path-relative Location so the browser keeps its current
            // host:port. Avoids surprises if someone reaches the server
            // via a different name (loopback alias, ssh -L tunnel, etc.).
            let mut resp = Response::new(
                Full::new(Bytes::from_static(b""))
                    .map_err(|never: Infallible| match never {})
                    .boxed(),
            );
            *resp.status_mut() = StatusCode::FOUND;
            if let Ok(v) = HeaderValue::from_str(&ctx.long_path) {
                resp.headers_mut().insert(LOCATION, v);
            }
            return Ok(resp);
        }
        return Ok(err_resp(StatusCode::NOT_FOUND, "no such short id"));
    }

    if req.uri().path() != "/dl" {
        return Ok(err_resp(StatusCode::NOT_FOUND, "not found"));
    }

    let q = req.uri().query().unwrap_or("");
    let mut snap = None;
    let mut tree = None;
    let mut exp_s = None;
    let mut sig = None;
    for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
        match k.as_ref() {
            "snap" => snap = Some(v.into_owned()),
            "tree" => tree = Some(v.into_owned()),
            "exp" => exp_s = Some(v.into_owned()),
            "sig" => sig = Some(v.into_owned()),
            _ => {}
        }
    }
    let (Some(snap), Some(tree), Some(exp_s), Some(sig)) = (snap, tree, exp_s, sig) else {
        return Ok(err_resp(StatusCode::BAD_REQUEST, "missing query parameters"));
    };

    // Cross-check the URL's snap/tree against the server's bound values
    // before any crypto work. The file's name isn't in the URL — the
    // server already knows it — but it's part of the HMAC message, so a
    // URL minted for a different file still fails at the verify step
    // below.
    if snap != ctx.snap_id || tree != ctx.tree_hex {
        return Ok(err_resp(StatusCode::NOT_FOUND, "no such file"));
    }

    let Ok(exp) = exp_s.parse::<u64>() else {
        return Ok(err_resp(StatusCode::BAD_REQUEST, "bad exp"));
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX);
    if now > exp {
        return Ok(err_resp(StatusCode::FORBIDDEN, "expired"));
    }

    let Some(sig_bytes) = hex_decode(&sig) else {
        return Ok(err_resp(StatusCode::FORBIDDEN, "invalid signature"));
    };
    let mut mac = HmacSha256::new_from_slice(&ctx.key).expect("any-length");
    mac.update(&canonical_message(&snap, &tree, &ctx.name, exp));
    if mac.verify_slice(&sig_bytes).is_err() {
        return Ok(err_resp(StatusCode::FORBIDDEN, "invalid signature"));
    }

    // `restic dump` is blocking; hand it to the blocking pool and stream its
    // stdout through the bounded channel so HTTP backpressure reaches the
    // child process.
    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(8);
    let profile = ctx.profile.clone();
    let snapshot_id = ctx.snap_id.clone();
    let display_path = ctx.display_path.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = restic::stream_dump(&profile, &snapshot_id, &display_path, &tx) {
            let _ = tx.blocking_send(Err(io::Error::other(format!("{e:#}"))));
        }
    });

    let body = ChannelBody { rx, done: false }.boxed();
    let mut resp = Response::new(body);
    let ct = mime_for(&ctx.display_path);
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(ct));
    // No caching: the URL is single-purpose and short-lived, and we don't
    // want browsers/proxies to hand back stale bytes if the user reuses it.
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Inline so browsers/clients display the file in-place when they can,
    // but still pass through a filename for save-as.
    let filename = sanitize_filename(basename(&ctx.display_path));
    let disp = format!("inline; filename=\"{filename}\"");
    if let Ok(v) = HeaderValue::from_str(&disp) {
        resp.headers_mut().insert(CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

// Custom hyper Body that polls a tokio mpsc receiver. Avoids pulling in
// async-stream or tokio-stream just for this one bridge.
struct ChannelBody {
    rx: mpsc::Receiver<io::Result<Bytes>>,
    done: bool,
}

impl Body for ChannelBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(Ok(Frame::data(b)))),
            Poll::Ready(Some(Err(e))) => {
                self.done = true;
                Poll::Ready(Some(Err(e)))
            }
        }
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

// Small lookup table keyed on the lowercased file extension. Falls back to
// application/octet-stream when nothing matches. The browser uses this with
// `Content-Disposition: inline` to decide whether to render in-place — so
// the goal is to cover the common previewable types (images, text, PDF,
// audio/video), not to be exhaustive.
fn mime_for(display_path: &str) -> &'static str {
    let name = basename(display_path);
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let ext_lower = ext.to_ascii_lowercase();
    match ext_lower.as_str() {
        // text
        "txt" | "log" => "text/plain; charset=utf-8",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        // images
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        // documents
        "pdf" => "application/pdf",
        // audio
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        // video
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        // archives
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        _ => "application/octet-stream",
    }
}

// Keep only printable ASCII without quotes or control bytes — anything else
// gets dropped so the Content-Disposition header can't be broken or used to
// smuggle CRLF. Browsers will fall back to the URL/UA defaults if filename is
// empty.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control() && *c != '"' && *c != '\\')
        .collect();
    if cleaned.is_empty() {
        "download".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = [0u8, 1, 2, 0xff, 0x10, 0xab];
        let hex = hex_encode(&bytes);
        assert_eq!(hex, "000102ff10ab");
        assert_eq!(hex_decode(&hex).unwrap(), bytes);
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_none());
    }

    #[test]
    fn hex_decode_rejects_non_hex() {
        assert!(hex_decode("zz").is_none());
    }

    #[test]
    fn sig_changes_per_inputs() {
        let k = key();
        let a = compute_sig(&k, "snap-a", "tree-a", "file-a", 123);
        let b = compute_sig(&k, "snap-a", "tree-a", "file-b", 123);
        let c = compute_sig(&k, "snap-a", "tree-b", "file-a", 123);
        let d = compute_sig(&k, "snap-b", "tree-a", "file-a", 123);
        let e = compute_sig(&k, "snap-a", "tree-a", "file-a", 124);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_ne!(a, e);
    }

    #[test]
    fn sig_verify_round_trip() {
        let k = key();
        let exp = 1_700_000_000u64;
        let sig = compute_sig(&k, "snap", "tree", "name", exp);
        let sig_bytes = hex_decode(&sig).unwrap();
        let mut mac = HmacSha256::new_from_slice(&k).unwrap();
        mac.update(&canonical_message("snap", "tree", "name", exp));
        assert!(mac.verify_slice(&sig_bytes).is_ok());
    }

    #[test]
    fn sig_verify_rejects_tampered_byte() {
        let k = key();
        let exp = 1_700_000_000u64;
        let sig = compute_sig(&k, "snap", "tree", "name", exp);
        let mut bytes = hex_decode(&sig).unwrap();
        bytes[0] ^= 0x01;
        let mut mac = HmacSha256::new_from_slice(&k).unwrap();
        mac.update(&canonical_message("snap", "tree", "name", exp));
        assert!(mac.verify_slice(&bytes).is_err());
    }

    #[test]
    fn sig_verify_rejects_wrong_name() {
        let k = key();
        let exp = 1_700_000_000u64;
        // URL minted with name=A but server bound to name=B verifies B's message.
        let sig = compute_sig(&k, "snap", "tree", "nameA", exp);
        let sig_bytes = hex_decode(&sig).unwrap();
        let mut mac = HmacSha256::new_from_slice(&k).unwrap();
        mac.update(&canonical_message("snap", "tree", "nameB", exp));
        assert!(mac.verify_slice(&sig_bytes).is_err());
    }

    #[test]
    fn sanitize_filename_strips_unsafe() {
        assert_eq!(sanitize_filename("ok.txt"), "ok.txt");
        assert_eq!(sanitize_filename("bad\"name\r\n.txt"), "badname.txt");
        assert_eq!(sanitize_filename(""), "download");
        // Non-ASCII gets stripped; if nothing survives we fall back.
        assert_eq!(sanitize_filename("日本語"), "download");
    }

    #[test]
    fn basename_picks_tail() {
        assert_eq!(basename("foo/bar/baz.txt"), "baz.txt");
        assert_eq!(basename("just-a-name"), "just-a-name");
        assert_eq!(basename(""), "");
    }

    #[test]
    fn mime_for_known_extensions() {
        assert_eq!(mime_for("/x/y.png"), "image/png");
        assert_eq!(mime_for("Photo.JPG"), "image/jpeg");
        assert_eq!(mime_for("a/b/c.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("data.json"), "application/json; charset=utf-8");
        assert_eq!(mime_for("doc.pdf"), "application/pdf");
        assert_eq!(mime_for("clip.MP4"), "video/mp4");
        assert_eq!(mime_for("archive.tar.gz"), "application/gzip");
    }

    #[test]
    fn mime_for_unknown_falls_back() {
        assert_eq!(mime_for("Makefile"), "application/octet-stream");
        assert_eq!(mime_for("no-extension"), "application/octet-stream");
        assert_eq!(mime_for("weird.qqqq"), "application/octet-stream");
    }

    // End-to-end smoke test against a disposable repository under ./tmp.
    #[test]
    #[ignore]
    fn e2e_serves_and_validates_signed_url() {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::path::PathBuf;

        let root = PathBuf::from("tmp").join(format!("share-it-{}", std::process::id()));
        // Remove the fixture directory on both normal completion and panic.
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).ok();
            }
        }
        let _cleanup = Cleanup(root.clone());
        let repository = root.join("repository");
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("greeting.txt"), b"hello signed-url world\n").unwrap();
        std::fs::write(source.join("blob.bin"), b"other file\n").unwrap();
        let profile = Profile::Local {
            password: "sandbox".into(),
            local_path: repository.to_string_lossy().into_owned(),
        };
        restic::run(&profile, &["init"]).expect("init");
        restic::run(&profile, &["backup", source.to_str().unwrap()]).expect("backup");

        let repo = crate::repo::open_indexed(&profile).expect("open repo");
        let snaps = crate::repo::load_snapshots(&profile).expect("list snapshots");
        let snap = snaps.first().expect("at least one snapshot");
        let snap_id = snap.id.clone();

        let root_tree =
            crate::repo::snapshot_root_tree(&repo, &snap_id).expect("root tree id");
        fn find_file(
            repo: &crate::repo::RepoSession,
            tree: &str,
            prefix: &str,
        ) -> Option<(String, String)> {
            for row in crate::repo::list_tree(repo, tree).ok()? {
                let path = format!("{prefix}/{}", row.name);
                if row.name == "greeting.txt" {
                    return Some((tree.to_string(), path));
                }
                if let Some(subtree) = row.subtree
                    && let Some(found) = find_file(repo, &subtree, &path)
                {
                    return Some(found);
                }
            }
            None
        }
        let (source_tree, display_path) =
            find_file(&repo, &root_tree, "").expect("find greeting.txt");

        let target = ShareTarget {
            snap_id: snap_id.clone(),
            tree_id: source_tree.clone(),
            name: "greeting.txt".into(),
            display_path,
        };

        // Bind to an ephemeral port instead of 7834 so this test can run
        // alongside a real wrustic instance.
        let listeners = crate::local_server::bind_localhost(0).unwrap();
        let port = listeners[0].local_addr().unwrap().port();
        drop(listeners);

        let key = [0x77u8; 32];

        let handle = start(port, profile, key, target, SHARE_TTL).expect("start server");

        // Issue a raw HTTP/1.0 GET (no need to pull in a client) and pull
        // the body until EOF.
        let url = handle.url.clone();
        let qs = url.split_once('?').unwrap().1;
        let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        write!(sock, "GET /dl?{qs} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        sock.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.0 200") || resp_str.starts_with("HTTP/1.1 200"),
            "got: {resp_str}"
        );
        let body_start = resp.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let body = &resp[body_start..];
        assert_eq!(body, b"hello signed-url world\n");

        // Tamper the sig: flip the first hex char. Must come back 403.
        let mut bad_qs = qs.to_string();
        let sig_pos = bad_qs.find("sig=").unwrap() + 4;
        let ch = bad_qs.as_bytes()[sig_pos];
        let flipped = if ch == b'0' { '1' } else { '0' };
        bad_qs.replace_range(sig_pos..sig_pos + 1, &flipped.to_string());
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(sock, "GET /dl?{bad_qs} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        sock.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.0 403") || resp_str.starts_with("HTTP/1.1 403"),
            "got: {resp_str}"
        );

        // Cross-replay check: forge a URL for the OTHER file in the same
        // tree (`blob.bin`). The HMAC binds the bound name into the sig,
        // so a URL legitimately signed for `blob.bin` should be rejected
        // (403 invalid signature) by a server bound to `greeting.txt`.
        let exp_for_other: u64 = qs
            .split('&')
            .find_map(|kv| kv.strip_prefix("exp="))
            .unwrap()
            .parse()
            .unwrap();
        let other_sig = compute_sig(&key, &snap_id, &source_tree, "blob.bin", exp_for_other);
        let other_qs: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("snap", &snap_id)
            .append_pair("tree", &source_tree)
            .append_pair("exp", &exp_for_other.to_string())
            .append_pair("sig", &other_sig)
            .finish();
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(sock, "GET /dl?{other_qs} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        sock.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.0 403") || resp_str.starts_with("HTTP/1.1 403"),
            "got: {resp_str}"
        );

        // Expired URL: build one signed correctly but with exp 60s in the
        // past. HMAC verifies (correct key + correct message), so the only
        // thing that can reject it is the wall-clock check. Distinct code
        // path from the tampered-sig case above.
        let past_exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(60);
        let past_sig =
            compute_sig(&key, &snap_id, &source_tree, "greeting.txt", past_exp);
        let past_qs: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("snap", &snap_id)
            .append_pair("tree", &source_tree)
            .append_pair("exp", &past_exp.to_string())
            .append_pair("sig", &past_sig)
            .finish();
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(sock, "GET /dl?{past_qs} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        sock.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.0 403") || resp_str.starts_with("HTTP/1.1 403"),
            "got: {resp_str}"
        );
        // Reject by the *expired* branch, not the signature branch.
        assert!(
            resp_str.contains("expired"),
            "expected `expired` body, got: {resp_str}"
        );

        // Short URL: GET /s/<id> should 302 with Location = the long URL.
        let short_path = handle.short_url.rsplit_once('/').unwrap().1.to_string();
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            sock,
            "GET /s/{short_path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n"
        )
        .unwrap();
        let mut resp = Vec::new();
        sock.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.0 302") || resp_str.starts_with("HTTP/1.1 302"),
            "got: {resp_str}"
        );
        let loc = resp_str
            .lines()
            .find_map(|l| l.strip_prefix("location: "))
            .or_else(|| resp_str.lines().find_map(|l| l.strip_prefix("Location: ")))
            .expect("Location header");
        // Path-relative redirect: no scheme/host/port in the Location.
        assert!(
            loc.trim().starts_with("/dl?"),
            "Location should be path-relative, got: {loc}"
        );
        assert!(handle.url.ends_with(loc.trim()), "long URL should end with the redirect path");

        // Unknown short id: 404.
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(sock, "GET /s/deadbeef HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        sock.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.0 404") || resp_str.starts_with("HTTP/1.1 404"),
            "got: {resp_str}"
        );

        handle.stop();

        // After stop, the port should be unbound. Connect attempts fail.
        let res = TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            std::time::Duration::from_millis(500),
        );
        assert!(res.is_err(), "server should be stopped");
    }
}
