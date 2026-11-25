//! OpenAI (ChatGPT Plus/Pro Codex) OAuth 认证、模型拉取与额度查询。
//! 不依赖 tauri，GUI 与 clade 网关共用。
//! 凭据存在 Windows 凭据管理器（service="clade", user="oauth:<id>"）。

use crate::core::{AuthStatus, Global, Usage, UsageWindow};
use crate::oauth::{
    self, base64url_decode, base64url_encode, build_http_client, percent_encode, truncate, CancelSlot,
    OAUTH_HTTP_TIMEOUT, TOKEN_REFRESH_BUFFER_MS,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

/// OpenAI Codex CLI 官方 OAuth Client ID（与 cc-switch / sub2api 一致）
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub const DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
pub const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
pub const BROWSER_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const BROWSER_CALLBACK_PORT: u16 = 1455;

pub const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const CODEX_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// 出站身份三元组（originator / version / User-Agent 同源自洽，参考 cc-switch 与 sub2api）
pub const ORIGINATOR: &str = "codex_cli_rs";
pub const CLIENT_VERSION: &str = "0.169.0";
pub const USER_AGENT: &str = "codex_cli_rs/0.169.0 (Windows 10.0; x86_64) xterm-256color";

/// 持久化的 OpenAI OAuth 凭据（存在 Windows 凭据管理器）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAiOAuthCreds {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub chatgpt_account_id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub plan_type: String,
    /// access_token 过期时间戳（毫秒）
    pub expires_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
}

impl OpenAiOAuthCreds {
    pub fn is_expiring_soon(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        self.expires_at_ms - now < TOKEN_REFRESH_BUFFER_MS
    }

    pub fn to_auth_status(&self) -> AuthStatus {
        AuthStatus {
            logged_in: !self.refresh_token.is_empty(),
            email: self.email.clone(),
            org_name: self.chatgpt_account_id.clone(),
            subscription_type: plan_label(&self.plan_type),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFlowStart {
    pub device_auth_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(default)]
    interval: Option<Value>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DevicePollSuccess {
    authorization_code: String,
    code_verifier: String,
}

// ---------- 凭据读写（Windows 凭据管理器） ----------

pub fn load_creds(card_id: &str) -> Result<Option<OpenAiOAuthCreds>> {
    oauth::load_creds_json(card_id)
}

pub fn save_creds(card_id: &str, creds: &OpenAiOAuthCreds) -> Result<()> {
    oauth::save_creds_json(card_id, creds)
}

pub fn logout(card_id: &str) -> Result<()> {
    oauth::delete_creds(card_id)
}

pub fn auth_status(card_id: &str) -> Result<AuthStatus> {
    match load_creds(card_id)? {
        Some(c) => Ok(c.to_auth_status()),
        None => Ok(AuthStatus::default()),
    }
}

// ---------- 浏览器 PKCE 登录 ----------

static BROWSER_CANCEL: CancelSlot = CancelSlot::new();

/// 取消正在等待回调的浏览器登录
pub fn cancel_browser_login() {
    BROWSER_CANCEL.cancel();
}

pub fn build_authorize_url(verifier: &str, state: &str) -> String {
    let challenge = base64url_encode(&Sha256::digest(verifier.as_bytes()));
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", BROWSER_REDIRECT_URI),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", ORIGINATOR),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{AUTHORIZE_URL}?{query}")
}

/// 启动本地 1455 端口回调服务，打开浏览器完成 OpenAI OAuth PKCE 登录。
pub async fn browser_login(card_id: &str, global: &Global) -> Result<AuthStatus> {
    // 取消上一个未完成的登录流，释放 1455 端口
    cancel_browser_login();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", BROWSER_CALLBACK_PORT))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "无法监听 localhost:{BROWSER_CALLBACK_PORT}（端口可能被其他 Codex 登录占用），请先关闭它或改用「设备码登录」"
            )
        })?;

    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let state = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let url = build_authorize_url(&verifier, &state);

    let code = oauth::wait_browser_callback(listener, "/auth/callback", &url, state, &BROWSER_CANCEL).await?;
    let client = build_http_client(global, OAUTH_HTTP_TIMEOUT)?;
    let tokens = exchange_code_for_tokens(&client, &code, &verifier, BROWSER_REDIRECT_URI).await?;
    let creds = creds_from_token_response(tokens, None)?;
    save_creds(card_id, &creds)?;
    Ok(creds.to_auth_status())
}

// ---------- 设备码登录 ----------

pub async fn start_device_flow(global: &Global) -> Result<DeviceFlowStart> {
    let client = build_http_client(global, OAUTH_HTTP_TIMEOUT)?;
    let resp = client
        .post(DEVICE_USERCODE_URL)
        .header("Content-Type", "application/json")
        .header("User-Agent", USER_AGENT)
        .header("originator", ORIGINATOR)
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .context("请求设备码失败")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("获取设备码失败（HTTP {status}）：{}", truncate(&text, 300));
    }

    let device: DeviceCodeResponse = resp.json().await.context("解析设备码响应失败")?;
    let interval = parse_interval(device.interval.as_ref());
    let expires_in = device.expires_in.unwrap_or(900);
    Ok(DeviceFlowStart {
        device_auth_id: device.device_auth_id,
        user_code: device.user_code,
        verification_uri: DEVICE_VERIFICATION_URL.to_string(),
        expires_in,
        interval,
    })
}

/// 轮询一次设备码状态。Ok(None) 表示等待授权，Ok(Some(status)) 表示登录成功并已保存凭据。
pub async fn poll_device_flow(
    card_id: &str,
    device_auth_id: &str,
    user_code: &str,
    global: &Global,
) -> Result<Option<AuthStatus>> {
    let client = build_http_client(global, OAUTH_HTTP_TIMEOUT)?;
    let resp = client
        .post(DEVICE_TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("User-Agent", USER_AGENT)
        .header("originator", ORIGINATOR)
        .json(&serde_json::json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code,
        }))
        .send()
        .await
        .context("轮询设备码状态失败")?;

    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if status == reqwest::StatusCode::GONE {
        bail!("设备码已过期，请重新发起登录");
    }
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("轮询设备码失败（HTTP {status}）：{}", truncate(&text, 300));
    }

    let poll: DevicePollSuccess = resp.json().await.context("解析设备码授权响应失败")?;
    let tokens =
        exchange_code_for_tokens(&client, &poll.authorization_code, &poll.code_verifier, DEVICE_REDIRECT_URI)
            .await?;
    let creds = creds_from_token_response(tokens, None)?;
    save_creds(card_id, &creds)?;
    Ok(Some(creds.to_auth_status()))
}

// ---------- Token 交换与刷新 ----------

async fn exchange_code_for_tokens(
    client: &reqwest::Client,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthTokenResponse> {
    let resp = client
        .post(TOKEN_URL)
        .header("User-Agent", USER_AGENT)
        .header("originator", ORIGINATOR)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .context("换取 OAuth Token 失败")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("换取 OAuth Token 失败（HTTP {status}）：{}", truncate(&text, 300));
    }

    resp.json().await.context("解析 OAuth Token 响应失败")
}

pub async fn refresh_tokens(
    client: &reqwest::Client,
    refresh_token: &str,
    previous: Option<&OpenAiOAuthCreds>,
) -> Result<OpenAiOAuthCreds> {
    let resp = client
        .post(TOKEN_URL)
        .header("User-Agent", USER_AGENT)
        .header("originator", ORIGINATOR)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
            ("scope", "openid profile email"),
        ])
        .send()
        .await
        .context("刷新 OpenAI OAuth Token 失败")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
            bail!("OpenAI 登录已失效（HTTP {status}），请重新登录：{}", truncate(&text, 200));
        }
        bail!("刷新 OpenAI Token 失败（HTTP {status}）：{}", truncate(&text, 300));
    }

    let tokens: OAuthTokenResponse = resp.json().await.context("解析刷新 Token 响应失败")?;
    creds_from_token_response(tokens, previous)
}

/// 确保凭据里的 access_token 有效；即将过期或 force=true 时通过跨进程锁串行刷新并写回凭据管理器。
/// 拿到锁后会先重读凭据管理器：若其他进程已经刷新出更新且未过期的 token，直接采用，避免重复消费一次性 refresh_token。
pub async fn ensure_valid_creds(
    card_id: &str,
    creds: &mut OpenAiOAuthCreds,
    client: &reqwest::Client,
    force: bool,
) -> Result<()> {
    if !force && !creds.access_token.is_empty() && !creds.is_expiring_soon() {
        return Ok(());
    }

    let _lock = oauth::acquire_refresh_lock(card_id).await?;

    if let Ok(Some(stored)) = load_creds(card_id) {
        let stored_usable = !stored.access_token.is_empty() && !stored.is_expiring_soon();
        let stored_changed = stored.access_token != creds.access_token
            || stored.refresh_token != creds.refresh_token
            || stored.updated_at_ms > creds.updated_at_ms;
        if stored_usable && (!force || stored.access_token != creds.access_token) {
            *creds = stored;
            return Ok(());
        }
        if stored_changed {
            *creds = stored;
        }
    }

    if creds.refresh_token.is_empty() {
        bail!("OpenAI 登录卡 {card_id} 缺少 refresh_token，请重新登录");
    }
    let updated = refresh_tokens(client, &creds.refresh_token, Some(creds)).await?;
    *creds = updated;
    save_creds(card_id, creds)?;
    Ok(())
}

// ---------- 模型列表拉取 ----------

pub async fn fetch_models(card_id: &str, global: &Global) -> Result<Vec<String>> {
    let mut creds = load_creds(card_id)?
        .with_context(|| format!("OpenAI 登录卡 {card_id} 还没有登录"))?;
    let client = build_http_client(global, Duration::from_secs(15))?;
    ensure_valid_creds(card_id, &mut creds, &client, false).await?;

    let resp = send_models_req(&client, &creds).await?;
    let resp = if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        ensure_valid_creds(card_id, &mut creds, &client, true).await?;
        send_models_req(&client, &creds).await?
    } else {
        resp
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("拉取 OpenAI 模型列表失败（HTTP {status}）：{}", truncate(&text, 300));
    }
    let v: Value = serde_json::from_str(&text).context("OpenAI 模型列表返回的不是 JSON")?;
    Ok(parse_codex_models(&v))
}

async fn send_models_req(client: &reqwest::Client, creds: &OpenAiOAuthCreds) -> Result<reqwest::Response> {
    let mut req = client
        .get(CODEX_MODELS_URL)
        .query(&[("client_version", CLIENT_VERSION)])
        .bearer_auth(&creds.access_token)
        .header("originator", ORIGINATOR)
        .header("version", CLIENT_VERSION)
        .header("User-Agent", USER_AGENT);
    if !creds.chatgpt_account_id.is_empty() {
        req = req.header("chatgpt-account-id", &creds.chatgpt_account_id);
    }
    req.send().await.context("请求 OpenAI 模型列表失败")
}

/// 解析 chatgpt.com/backend-api/codex/models 返回的模型 ID 列表（兼容 models/data/items 数组或 map）。
pub fn parse_codex_models(v: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let entries = v
        .get("models")
        .and_then(Value::as_array)
        .or_else(|| v.get("data").and_then(Value::as_array))
        .or_else(|| v.get("items").and_then(Value::as_array))
        .or_else(|| v.as_array());

    if let Some(arr) = entries {
        for item in arr {
            if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                ids.push(s.to_string());
            } else if let Some(obj) = item.as_object() {
                let id = ["slug", "id", "model", "name"]
                    .iter()
                    .find_map(|k| obj.get(*k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()));
                if let Some(id) = id {
                    ids.push(id.to_string());
                }
            }
        }
    }

    if let Some(map) = v.get("models").and_then(Value::as_object) {
        for (k, item) in map {
            let id = item
                .as_object()
                .and_then(|obj| {
                    ["slug", "id", "model", "name"]
                        .iter()
                        .find_map(|f| obj.get(*f).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()))
                })
                .unwrap_or(k.trim());
            if !id.is_empty() {
                ids.push(id.to_string());
            }
        }
    }

    ids.sort();
    ids.dedup();
    ids
}

// ---------- 额度查询（wham/usage） ----------

pub fn fetch_usage_blocking(card_id: &str, global: &Global) -> Result<Usage> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(fetch_usage(card_id, global))
}

pub async fn fetch_usage(card_id: &str, global: &Global) -> Result<Usage> {
    let mut creds = load_creds(card_id)?
        .with_context(|| format!("OpenAI 登录卡 {card_id} 还没有登录"))?;
    let client = build_http_client(global, Duration::from_secs(15))?;
    ensure_valid_creds(card_id, &mut creds, &client, false).await?;

    let resp = send_usage_req(&client, &creds).await?;
    let resp = if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        ensure_valid_creds(card_id, &mut creds, &client, true).await?;
        send_usage_req(&client, &creds).await?
    } else {
        resp
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!("OpenAI 登录凭据已过期或被拒绝（HTTP {status}），请重新登录");
    }
    if !status.is_success() {
        bail!("查询 OpenAI 额度失败（HTTP {status}）：{}", truncate(&text, 300));
    }
    let v: Value = serde_json::from_str(&text).context("OpenAI 额度接口返回的不是 JSON")?;
    Ok(parse_codex_usage(&v))
}

async fn send_usage_req(client: &reqwest::Client, creds: &OpenAiOAuthCreds) -> Result<reqwest::Response> {
    let mut req = client
        .get(CODEX_USAGE_URL)
        .bearer_auth(&creds.access_token)
        .header("User-Agent", "codex-cli")
        .header("Accept", "application/json");
    if !creds.chatgpt_account_id.is_empty() {
        req = req.header("ChatGPT-Account-Id", &creds.chatgpt_account_id);
    }
    req.send().await.context("请求 OpenAI 额度接口失败")
}

pub fn parse_codex_usage(v: &Value) -> Usage {
    let mut windows = Vec::new();
    if let Some(rl) = v.get("rate_limit") {
        for key in ["primary_window", "secondary_window"] {
            let Some(w) = rl.get(key).filter(|w| w.is_object()) else {
                continue;
            };
            let Some(percent) = w.get("used_percent").and_then(Value::as_f64) else {
                continue;
            };
            let label = w
                .get("limit_window_seconds")
                .and_then(Value::as_i64)
                .map(window_seconds_label)
                .unwrap_or_else(|| if key == "primary_window" { "主窗口".into() } else { "次窗口".into() });
            // 已用为 0 时隐藏占位重置时间（与 cc-switch 一致）
            let resets_at = if percent == 0.0 {
                String::new()
            } else {
                w.get("reset_at")
                    .and_then(Value::as_i64)
                    .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default()
            };
            windows.push(UsageWindow { label, percent, resets_at });
        }
    }
    Usage {
        windows,
        fetched_at_ms: chrono::Utc::now().timestamp_millis(),
        credential_expires_at: String::new(),
    }
}

fn window_seconds_label(secs: i64) -> String {
    match secs {
        18_000 => "当前会话（5 小时）".into(),
        604_800 => "本周（7 天）".into(),
        s if s >= 86_400 && s % 86_400 == 0 => format!("{} 天窗口", s / 86_400),
        s if s >= 3_600 && s % 3_600 == 0 => format!("{} 小时窗口", s / 3_600),
        s => format!("{s} 秒窗口"),
    }
}

fn plan_label(plan: &str) -> String {
    match plan.trim().to_ascii_lowercase().as_str() {
        "" => String::new(),
        "plus" => "Plus".into(),
        "pro" => "Pro".into(),
        "team" => "Team".into(),
        "enterprise" => "Enterprise".into(),
        "free" => "Free".into(),
        other => other.to_string(),
    }
}

// ---------- JWT & 工具函数 ----------

fn creds_from_token_response(
    tokens: OAuthTokenResponse,
    previous: Option<&OpenAiOAuthCreds>,
) -> Result<OpenAiOAuthCreds> {
    let refresh_token = tokens
        .refresh_token
        .filter(|s| !s.trim().is_empty())
        .or_else(|| previous.map(|p| p.refresh_token.clone()).filter(|s| !s.is_empty()))
        .context("OAuth 响应缺少 refresh_token")?;

    let id_claims = tokens
        .id_token
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(extract_jwt_claims)
        .unwrap_or_default();
    let access_claims = extract_jwt_claims(&tokens.access_token);

    let chatgpt_account_id = id_claims
        .chatgpt_account_id
        .or(access_claims.chatgpt_account_id)
        .or_else(|| previous.map(|p| p.chatgpt_account_id.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_default();

    let email = id_claims
        .email
        .or(access_claims.email)
        .or_else(|| previous.map(|p| p.email.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_default();

    let plan_type = id_claims
        .plan_type
        .or(access_claims.plan_type)
        .or_else(|| previous.map(|p| p.plan_type.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_default();

    let now_ms = chrono::Utc::now().timestamp_millis();
    let expires_at_ms = tokens
        .expires_in
        .map(|s| now_ms + s * 1000)
        .or_else(|| access_claims.exp.map(|e| e * 1000))
        .unwrap_or(now_ms + 3600 * 1000);

    Ok(OpenAiOAuthCreds {
        access_token: tokens.access_token,
        refresh_token,
        chatgpt_account_id,
        email,
        plan_type,
        expires_at_ms,
        updated_at_ms: now_ms,
    })
}

#[derive(Default, Debug, PartialEq)]
pub struct JwtMeta {
    pub chatgpt_account_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub exp: Option<i64>,
}

pub fn extract_jwt_claims(jwt: &str) -> JwtMeta {
    let Some(payload_b64) = jwt.split('.').nth(1) else {
        return JwtMeta::default();
    };
    let Some(bytes) = base64url_decode(payload_b64) else {
        return JwtMeta::default();
    };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return JwtMeta::default();
    };
    let auth_obj = v.get("https://api.openai.com/auth");
    let profile_obj = v.get("https://api.openai.com/profile");

    let non_empty = |val: Option<&Value>| {
        val.and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };

    JwtMeta {
        chatgpt_account_id: non_empty(v.get("chatgpt_account_id"))
            .or_else(|| non_empty(auth_obj.and_then(|a| a.get("chatgpt_account_id")))),
        email: non_empty(v.get("email"))
            .or_else(|| non_empty(profile_obj.and_then(|p| p.get("email")))),
        plan_type: non_empty(auth_obj.and_then(|a| a.get("chatgpt_plan_type")))
            .or_else(|| non_empty(v.get("chatgpt_plan_type"))),
        exp: v.get("exp").and_then(Value::as_i64),
    }
}

fn parse_interval(v: Option<&Value>) -> u64 {
    let raw = match v {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
        _ => None,
    };
    raw.unwrap_or(5).max(5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jwt_claims_extracts_nested_openai_auth() {
        let payload = json!({
            "email": "user@example.com",
            "exp": 1900000000,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc_123",
                "chatgpt_plan_type": "plus"
            }
        });
        let jwt = format!(
            "{}.{}.sig",
            base64url_encode(b"{\"alg\":\"RS256\"}"),
            base64url_encode(payload.to_string().as_bytes())
        );
        let meta = extract_jwt_claims(&jwt);
        assert_eq!(meta.chatgpt_account_id.as_deref(), Some("acc_123"));
        assert_eq!(meta.email.as_deref(), Some("user@example.com"));
        assert_eq!(meta.plan_type.as_deref(), Some("plus"));
        assert_eq!(meta.exp, Some(1900000000));
    }

    #[test]
    fn parse_codex_models_handles_slug_and_id() {
        let v = json!({
            "models": [
                { "slug": "gpt-5.4" },
                { "id": "gpt-5.3-codex" },
                "gpt-5.4"
            ]
        });
        assert_eq!(parse_codex_models(&v), vec!["gpt-5.3-codex", "gpt-5.4"]);
    }

    #[test]
    fn parse_codex_usage_maps_windows() {
        let v = json!({
            "rate_limit": {
                "primary_window": { "used_percent": 42.5, "limit_window_seconds": 18000, "reset_at": 1700000000 },
                "secondary_window": { "used_percent": 0.0, "limit_window_seconds": 604800, "reset_at": 1700500000 }
            }
        });
        let u = parse_codex_usage(&v);
        assert_eq!(u.windows.len(), 2);
        assert_eq!(u.windows[0].label, "当前会话（5 小时）");
        assert_eq!(u.windows[0].percent, 42.5);
        assert!(!u.windows[0].resets_at.is_empty());
        assert_eq!(u.windows[1].label, "本周（7 天）");
        assert_eq!(u.windows[1].percent, 0.0);
        assert!(u.windows[1].resets_at.is_empty());
    }
}
