//! OAuth 公共设施：凭据管理器读写（支持超长凭据自动分片）、跨进程刷新锁、代理 HTTP 客户端、浏览器 PKCE 回调服务与编解码工具。
//! 不依赖 tauri，GUI 与 clade 网关共用。

use crate::core::Global;
use anyhow::{bail, Context, Result};
use axum::{
    extract::{RawQuery, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

pub const TOKEN_REFRESH_BUFFER_MS: i64 = 60_000;
pub const OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
pub const BROWSER_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Windows 凭据管理器单条 CredentialBlob 上限 2560 字节，留足余量按 2400 字节分片。
const KEYRING_CHUNK_LIMIT: usize = 2400;
const CHUNK_HEADER_PREFIX: &[u8] = b"CHUNKED:";

// ---------- Windows 凭据管理器（OAuth 凭据） ----------

fn oauth_entry(card_id: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new("clade", &format!("oauth:{card_id}"))?)
}

fn oauth_chunk_entry(card_id: &str, idx: usize) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new("clade", &format!("oauth:{card_id}:{idx}"))?)
}

fn delete_entry_ignore_missing(entry: &keyring::Entry) -> Result<()> {
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn parse_chunk_count(raw: &[u8]) -> Option<usize> {
    let rest = raw.strip_prefix(CHUNK_HEADER_PREFIX)?;
    std::str::from_utf8(rest).ok()?.trim().parse::<usize>().ok().filter(|&n| n > 0)
}

/// 从 Windows 凭据管理器读取 OAuth 凭据 JSON；不存在返回 Ok(None)。
pub fn load_creds_json<T: DeserializeOwned>(card_id: &str) -> Result<Option<T>> {
    let raw = match oauth_entry(card_id)?.get_secret() {
        Ok(b) => b,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("读取卡 {card_id} 的 OAuth 凭据失败")),
    };
    let bytes = if let Some(count) = parse_chunk_count(&raw) {
        let mut combined = Vec::with_capacity(count * KEYRING_CHUNK_LIMIT);
        for i in 0..count {
            let chunk = oauth_chunk_entry(card_id, i)?
                .get_secret()
                .with_context(|| format!("读取卡 {card_id} 的 OAuth 凭据分片 {i} 失败"))?;
            combined.extend_from_slice(&chunk);
        }
        combined
    } else {
        raw
    };
    let val = serde_json::from_slice(&bytes).with_context(|| format!("解析卡 {card_id} 的 OAuth 凭据失败"))?;
    Ok(Some(val))
}

/// 将 OAuth 凭据以紧凑 JSON 写入 Windows 凭据管理器；超过单条上限时自动分片存储。
pub fn save_creds_json<T: Serialize>(card_id: &str, val: &T) -> Result<()> {
    let bytes = serde_json::to_vec(val)?;
    let main = oauth_entry(card_id)?;
    let old_chunks = main
        .get_secret()
        .ok()
        .and_then(|b| parse_chunk_count(&b))
        .unwrap_or(0);

    if bytes.len() <= KEYRING_CHUNK_LIMIT {
        main.set_secret(&bytes)
            .with_context(|| format!("保存卡 {card_id} 的 OAuth 凭据失败"))?;
        for i in 0..old_chunks {
            let _ = delete_entry_ignore_missing(&oauth_chunk_entry(card_id, i)?);
        }
    } else {
        let chunks: Vec<&[u8]> = bytes.chunks(KEYRING_CHUNK_LIMIT).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            oauth_chunk_entry(card_id, i)?
                .set_secret(chunk)
                .with_context(|| format!("保存卡 {card_id} 的 OAuth 凭据分片 {i} 失败"))?;
        }
        let header = format!("CHUNKED:{}", chunks.len());
        main.set_secret(header.as_bytes())
            .with_context(|| format!("保存卡 {card_id} 的 OAuth 凭据索引失败"))?;
        for i in chunks.len()..old_chunks {
            let _ = delete_entry_ignore_missing(&oauth_chunk_entry(card_id, i)?);
        }
    }
    Ok(())
}

/// 从 Windows 凭据管理器删除该卡的 OAuth 凭据（含分片）。
pub fn delete_creds(card_id: &str) -> Result<()> {
    let main = oauth_entry(card_id)?;
    if let Ok(raw) = main.get_secret()
        && let Some(count) = parse_chunk_count(&raw)
    {
        for i in 0..count {
            let _ = delete_entry_ignore_missing(&oauth_chunk_entry(card_id, i)?);
        }
    }
    delete_entry_ignore_missing(&main).with_context(|| format!("删除卡 {card_id} 的 OAuth 凭据失败"))
}

// ---------- 跨进程刷新锁 ----------

/// 跨进程独占文件锁：保证同一时间全机只有一个进程刷新同一张卡的 OAuth token。
/// 进程崩溃或句柄 drop 时由操作系统自动释放，不依赖线程亲和性，可安全跨 .await 持有。
pub struct RefreshLock {
    _file: File,
}

fn refresh_lock_path(card_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("clade-oauth-refresh-{card_id}.lock"))
}

pub async fn acquire_refresh_lock(card_id: &str) -> Result<RefreshLock> {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(35);

    let path = refresh_lock_path(card_id);
    let start = Instant::now();
    loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
        {
            Ok(file) => return Ok(RefreshLock { _file: file }),
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                if start.elapsed() >= LOCK_WAIT_TIMEOUT {
                    bail!("等待卡 {card_id} 的 OAuth 刷新锁超时");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e).with_context(|| format!("创建刷新锁 {} 失败", path.display())),
        }
    }
}

// ---------- HTTP 客户端（复用全局代理逻辑） ----------

pub fn build_http_client(global: &Global, timeout: Duration) -> Result<reqwest::Client> {
    let proxy = crate::core::resolve_proxy(global);
    crate::gateway::build_http_client(&proxy, Some(timeout))
}

// ---------- 浏览器 PKCE 登录通用流程 ----------

pub struct CancelSlot(OnceLock<Mutex<Option<oneshot::Sender<()>>>>);

impl Default for CancelSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelSlot {
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn inner(&self) -> &Mutex<Option<oneshot::Sender<()>>> {
        self.0.get_or_init(|| Mutex::new(None))
    }

    pub fn cancel(&self) {
        if let Some(tx) = self.inner().lock().unwrap().take() {
            let _ = tx.send(());
        }
    }

    pub fn set(&self, tx: oneshot::Sender<()>) {
        *self.inner().lock().unwrap() = Some(tx);
    }

    pub fn clear(&self) {
        let _ = self.inner().lock().unwrap().take();
    }
}

/// 打开默认浏览器（用 rundll32 处理含多个 query 参数的长 URL，避免 explorer.exe 截断）
pub fn open_browser(url: &str) -> Result<()> {
    Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn()
        .context("打开浏览器失败")?;
    Ok(())
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    result_tx: Arc<Mutex<Option<oneshot::Sender<Result<String>>>>>,
}

async fn handle_browser_callback(
    State(st): State<CallbackState>,
    RawQuery(query): RawQuery,
) -> impl IntoResponse {
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
    ];
    let parsed = parse_callback_query(query.as_deref().unwrap_or_default(), &st.expected_state);
    match parsed {
        Err(msg) => (
            StatusCode::BAD_REQUEST,
            headers,
            format!("<html><body><h3>登录回调无效</h3><p>{msg}</p></body></html>"),
        ),
        Ok(res) => {
            let ok = res.is_ok();
            if let Some(tx) = st.result_tx.lock().unwrap().take() {
                let _ = tx.send(res);
            }
            if ok {
                (
                    StatusCode::OK,
                    headers,
                    "<html><body><h3>授权成功</h3><p>可以关闭此页面，返回 clade 查看登录状态。</p></body></html>".to_string(),
                )
            } else {
                (
                    StatusCode::OK,
                    headers,
                    "<html><body><h3>授权被拒绝</h3><p>请返回 clade 重试。</p></body></html>".to_string(),
                )
            }
        }
    }
}

/// 在已绑定的本地 TcpListener 上启动临时回调服务、打开浏览器并等待授权码。
pub async fn wait_browser_callback(
    listener: tokio::net::TcpListener,
    callback_path: &str,
    authorize_url: &str,
    expected_state: String,
    cancel: &CancelSlot,
) -> Result<String> {
    let (result_tx, result_rx) = oneshot::channel::<Result<String>>();
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    cancel.set(cancel_tx);

    let cb_state = CallbackState {
        expected_state,
        result_tx: Arc::new(Mutex::new(Some(result_tx))),
    };
    let app = Router::new()
        .route(callback_path, get(handle_browser_callback))
        .with_state(cb_state);

    let server_handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    if let Err(e) = open_browser(authorize_url) {
        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
        cancel.clear();
        return Err(e);
    }

    let code_res = tokio::select! {
        res = result_rx => match res {
            Ok(inner) => inner,
            Err(_) => Err(anyhow::anyhow!("登录回调通道异常关闭")),
        },
        _ = &mut cancel_rx => Err(anyhow::anyhow!("已取消登录")),
        _ = tokio::time::sleep(BROWSER_LOGIN_TIMEOUT) => Err(anyhow::anyhow!("登录超时（5 分钟未完成授权）")),
    };

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server_handle).await;
    cancel.clear();

    code_res
}

// ---------- 编解码与辅助函数 ----------

pub fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

pub fn parse_callback_query(query: &str, expected_state: &str) -> Result<Result<String>, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let k = percent_decode(k).map_err(|_| "查询参数编码错误".to_string())?;
        let v = percent_decode(v).map_err(|_| "查询参数编码错误".to_string())?;
        if map.insert(k, v).is_some() {
            return Err("回调参数重复".to_string());
        }
    }
    if map.get("state").map(String::as_str) != Some(expected_state) {
        return Err("state 校验失败，请重新发起登录".to_string());
    }
    match (map.get("code"), map.get("error")) {
        (Some(code), None) if !code.is_empty() => Ok(Ok(code.clone())),
        (None, Some(err)) => Ok(Err(anyhow::anyhow!("浏览器授权被拒绝：{err}"))),
        _ => Err("缺少授权码".to_string()),
    }
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Result<String, ()> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_val(bytes[i + 1]).ok_or(())?;
                let lo = hex_val(bytes[i + 2]).ok_or(())?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b'%' => return Err(()),
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn base64url_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((input.len() * 4).div_ceil(3));
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        }
    }
    out
}

pub fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let val = |b: u8| -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some((b - b'A') as u32),
            b'a'..=b'z' => Some((b - b'a' + 26) as u32),
            b'0'..=b'9' => Some((b - b'0' + 52) as u32),
            b'-' | b'+' => Some(62),
            b'_' | b'/' => Some(63),
            _ => None,
        }
    };
    let clean: Vec<u8> = input.bytes().filter(|&b| b != b'=' && !b.is_ascii_whitespace()).collect();
    if clean.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity((clean.len() * 3) / 4);
    for chunk in clean.chunks(4) {
        let v0 = val(chunk[0])?;
        let v1 = val(chunk[1])?;
        let v2 = if chunk.len() > 2 { val(chunk[2])? } else { 0 };
        let v3 = if chunk.len() > 3 { val(chunk[3])? } else { 0 };
        let n = (v0 << 18) | (v1 << 12) | (v2 << 6) | v3;
        out.push(((n >> 16) & 0xff) as u8);
        if chunk.len() > 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if chunk.len() > 3 {
            out.push((n & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_roundtrip() {
        for data in [b"".as_slice(), b"a", b"ab", b"abc", b"hello world!"] {
            let enc = base64url_encode(data);
            assert!(!enc.contains('='));
            assert_eq!(base64url_decode(&enc).unwrap(), data);
        }
    }

    #[test]
    fn callback_query_validates_state_and_code() {
        assert!(parse_callback_query("code=abc", "s1").is_err());
        assert!(parse_callback_query("state=wrong&code=abc", "s1").is_err());
        assert!(parse_callback_query("state=s1&state=s1&code=abc", "s1").is_err());
        assert_eq!(parse_callback_query("state=s1&code=a%2Bb", "s1").unwrap().unwrap(), "a+b");
        assert!(parse_callback_query("state=s1&error=access_denied", "s1").unwrap().is_err());
    }
}
