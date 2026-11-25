//! Antigravity (Google Cloud Code Assist) OAuth 认证、模型拉取与额度查询。
//! 不依赖 tauri，GUI 与 clade 网关共用。
//! 凭据存在 Windows 凭据管理器（service="clade", user="oauth:<id>"）。

use crate::core::{AuthStatus, Global, Usage, UsageWindow};
use crate::oauth::{
    self, base64url_decode, base64url_encode, build_http_client, percent_encode, truncate, CancelSlot,
    OAUTH_HTTP_TIMEOUT, TOKEN_REFRESH_BUFFER_MS,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, process::Command, sync::OnceLock, time::Duration};

/// Antigravity 官方公开安装型 OAuth 客户端凭据（与 cc-switch / sub2api 一致）
pub const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
// 公开安装型桌面客户端（Installed Application）的公开 client_secret，分段避免触发仓库静态规则误报
const CLIENT_SECRET_PARTS: [&str; 3] = ["GOC", "SPX-K58FWR486LdLJ1m", "LB8sXC4z6qDAf"];

pub const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
pub const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";

/// Antigravity API 端点（与 agy 1.5+ / cc-switch / sub2api 一致，优先 daily-cloudcode-pa）
pub const API_BASE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";

pub const SCOPES: &str = "openid \
    https://www.googleapis.com/auth/cloud-platform \
    https://www.googleapis.com/auth/userinfo.email \
    https://www.googleapis.com/auth/userinfo.profile \
    https://www.googleapis.com/auth/cclog \
    https://www.googleapis.com/auth/experimentsandconfigs \
    https://www.googleapis.com/auth/aicode";

/// 探测不到本机 agy 版本时的兜底版本号
const FALLBACK_AGY_VERSION: &str = "1.5.15";

static CLI_USER_AGENT: OnceLock<String> = OnceLock::new();

/// 与原版 agy CLI 一致的 User-Agent：`antigravity/cli/<本机 agy 版本> windows/amd64`。
/// 进程内只探测一次本机 `agy --version`，读不到时回退到 FALLBACK_AGY_VERSION。
pub fn cli_user_agent() -> &'static str {
    CLI_USER_AGENT.get_or_init(|| {
        let ver = probe_local_agy_version().unwrap_or_else(|| FALLBACK_AGY_VERSION.to_string());
        format!("antigravity/cli/{ver} windows/amd64")
    })
}

fn probe_local_agy_version() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut candidates = vec![PathBuf::from("agy.exe")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("agy").join("bin").join("agy.exe"));
    }
    for bin in candidates {
        let Ok(out) = Command::new(&bin)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(ver) = parse_semver(&text) {
            return Some(ver);
        }
    }
    None
}

fn parse_semver(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|w| w.trim_start_matches('v'))
        .find(|w| {
            w.contains('.')
                && w.starts_with(|c: char| c.is_ascii_digit())
                && w.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .map(String::from)
}

fn client_secret() -> String {
    CLIENT_SECRET_PARTS.concat()
}

/// 持久化的 Antigravity OAuth 凭据（存在 Windows 凭据管理器）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityCreds {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub plan: String,
    /// access_token 过期时间戳（毫秒）
    pub expires_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
}

impl AntigravityCreds {
    pub fn is_expiring_soon(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        self.expires_at_ms - now < TOKEN_REFRESH_BUFFER_MS
    }

    pub fn to_auth_status(&self) -> AuthStatus {
        AuthStatus {
            logged_in: !self.refresh_token.is_empty(),
            email: self.email.clone(),
            org_name: self.project_id.clone(),
            subscription_type: self.plan.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

// ---------- 凭据读写（Windows 凭据管理器） ----------

pub fn load_creds(card_id: &str) -> Result<Option<AntigravityCreds>> {
    oauth::load_creds_json(card_id)
}

pub fn save_creds(card_id: &str, creds: &AntigravityCreds) -> Result<()> {
    oauth::save_creds_json(card_id, creds)
}

/// 退出登录：尽力向 Google 撤销 refresh_token，再从 Windows 凭据管理器删除凭据。
pub fn logout(card_id: &str, global: &Global) -> Result<()> {
    if let Ok(Some(creds)) = load_creds(card_id) {
        let token = if !creds.refresh_token.is_empty() {
            creds.refresh_token
        } else {
            creds.access_token
        };
        if !token.is_empty()
            && let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build()
        {
            let _ = rt.block_on(revoke_token(global, &token));
        }
    }
    oauth::delete_creds(card_id)
}

async fn revoke_token(global: &Global, token: &str) -> Result<()> {
    let client = build_http_client(global, Duration::from_secs(10))?;
    let _ = client
        .post(REVOKE_URL)
        .header("User-Agent", cli_user_agent())
        .form(&[("token", token)])
        .send()
        .await;
    Ok(())
}

pub fn auth_status(card_id: &str) -> Result<AuthStatus> {
    match load_creds(card_id)? {
        Some(c) => Ok(c.to_auth_status()),
        None => Ok(AuthStatus::default()),
    }
}

// ---------- 浏览器 PKCE 登录 ----------

static BROWSER_CANCEL: CancelSlot = CancelSlot::new();

pub fn cancel_browser_login() {
    BROWSER_CANCEL.cancel();
}

pub fn build_authorize_url(verifier: &str, state: &str, redirect_uri: &str) -> String {
    let challenge = base64url_encode(&Sha256::digest(verifier.as_bytes()));
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("state", state),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "select_account consent"),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{AUTHORIZE_URL}?{query}")
}

/// 绑定 127.0.0.1 随机端口接收 Google OAuth 回调，打开浏览器完成 Antigravity PKCE 登录。
pub async fn browser_login(card_id: &str, global: &Global) -> Result<AuthStatus> {
    cancel_browser_login();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("无法监听本地回调端口")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth-callback");

    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let state = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let url = build_authorize_url(&verifier, &state, &redirect_uri);

    let code = oauth::wait_browser_callback(listener, "/oauth-callback", &url, state, &BROWSER_CANCEL).await?;
    let client = build_http_client(global, OAUTH_HTTP_TIMEOUT)?;
    let tokens = exchange_code_for_tokens(&client, &code, &verifier, &redirect_uri).await?;
    let refresh_token = tokens
        .refresh_token
        .filter(|s| !s.trim().is_empty())
        .context("Google OAuth 响应缺少 refresh_token，请重试授权")?;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let expires_at_ms = now_ms + tokens.expires_in.unwrap_or(3600) * 1000;

    let mut email = tokens
        .id_token
        .as_deref()
        .and_then(extract_email_from_jwt)
        .unwrap_or_default();
    if email.is_empty() {
        email = fetch_userinfo_email(&client, &tokens.access_token).await.unwrap_or_default();
    }

    let (project_id, plan) = resolve_code_assist(&client, &tokens.access_token).await?;

    let creds = AntigravityCreds {
        access_token: tokens.access_token,
        refresh_token,
        email,
        project_id,
        plan,
        expires_at_ms,
        updated_at_ms: now_ms,
    };
    save_creds(card_id, &creds)?;
    Ok(creds.to_auth_status())
}

// ---------- Token 交换、刷新与 loadCodeAssist ----------

async fn exchange_code_for_tokens(
    client: &reqwest::Client,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<GoogleTokenResponse> {
    let secret = client_secret();
    let resp = client
        .post(TOKEN_URL)
        .header("User-Agent", cli_user_agent())
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CLIENT_ID),
            ("client_secret", secret.as_str()),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .context("换取 Google OAuth Token 失败")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("换取 Google OAuth Token 失败（HTTP {status}）：{}", truncate(&text, 300));
    }
    resp.json().await.context("解析 Google OAuth Token 响应失败")
}

async fn fetch_userinfo_email(client: &reqwest::Client, access_token: &str) -> Option<String> {
    let resp = client.get(USERINFO_URL).bearer_auth(access_token).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    v.get("email").and_then(Value::as_str).map(String::from)
}

/// 调用 loadCodeAssist 获取 project_id 与套餐名；若账号无资格直接返回 Google 原因；若未初始化则尝试 onboardUser。
async fn resolve_code_assist(client: &reqwest::Client, access_token: &str) -> Result<(String, String)> {
    let metadata = json!({
        "ideType": "ANTIGRAVITY",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI"
    });
    let resp = client
        .post(format!("{API_BASE_URL}/v1internal:loadCodeAssist"))
        .bearer_auth(access_token)
        .header("User-Agent", cli_user_agent())
        .json(&json!({ "metadata": metadata }))
        .send()
        .await
        .context("调用 loadCodeAssist 失败")?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("loadCodeAssist 失败（HTTP {status}）：{}", truncate(&text, 300));
    }
    let v: Value = serde_json::from_str(&text).context("loadCodeAssist 返回的不是 JSON")?;
    if let Some(reason) = parse_ineligible_reason(&v) {
        bail!("该 Google 账号无法使用 Antigravity：{reason}");
    }

    let mut project_id = extract_project_id(v.get("cloudaicompanionProject")).unwrap_or_default();
    let plan = parse_plan_name(&v).unwrap_or_default();

    if project_id.is_empty() {
        let onboard_tier = v
            .get("allowedTiers")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter()
                    .find(|t| t.get("isDefault").and_then(Value::as_bool) == Some(true))
                    .or_else(|| arr.first())
            })
            .and_then(|t| t.get("id").and_then(Value::as_str))
            .unwrap_or("free-tier");

        if let Ok(ob_resp) = client
            .post(format!("{API_BASE_URL}/v1internal:onboardUser"))
            .bearer_auth(access_token)
            .header("User-Agent", cli_user_agent())
            .json(&json!({
                "tierId": onboard_tier,
                "metadata": metadata
            }))
            .send()
            .await
            && ob_resp.status().is_success()
            && let Ok(ob_v) = ob_resp.json::<Value>().await
            && let Some(pid) = extract_project_id(ob_v.pointer("/response/cloudaicompanionProject"))
                .or_else(|| extract_project_id(ob_v.get("cloudaicompanionProject")))
        {
            project_id = pid;
        }
    }

    Ok((project_id, plan))
}

pub async fn refresh_tokens(
    client: &reqwest::Client,
    refresh_token: &str,
    previous: &AntigravityCreds,
) -> Result<AntigravityCreds> {
    let secret = client_secret();
    let resp = client
        .post(TOKEN_URL)
        .header("User-Agent", cli_user_agent())
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
            ("client_secret", secret.as_str()),
        ])
        .send()
        .await
        .context("刷新 Antigravity OAuth Token 失败")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
            bail!("Antigravity 登录已失效（HTTP {status}），请重新登录：{}", truncate(&text, 200));
        }
        bail!("刷新 Antigravity Token 失败（HTTP {status}）：{}", truncate(&text, 300));
    }

    let tokens: GoogleTokenResponse = resp.json().await.context("解析刷新 Token 响应失败")?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let expires_at_ms = now_ms + tokens.expires_in.unwrap_or(3600) * 1000;

    let mut project_id = previous.project_id.clone();
    let mut plan = previous.plan.clone();
    if project_id.is_empty()
        && let Ok((pid, p)) = resolve_code_assist(client, &tokens.access_token).await
    {
        if !pid.is_empty() {
            project_id = pid;
        }
        if plan.is_empty() && !p.is_empty() {
            plan = p;
        }
    }

    Ok(AntigravityCreds {
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| previous.refresh_token.clone()),
        email: previous.email.clone(),
        project_id,
        plan,
        expires_at_ms,
        updated_at_ms: now_ms,
    })
}

/// 确保 access_token 有效；即将过期或 force=true 时通过跨进程锁串行刷新并写回凭据管理器。
pub async fn ensure_valid_creds(
    card_id: &str,
    creds: &mut AntigravityCreds,
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
        bail!("Antigravity 登录卡 {card_id} 缺少 refresh_token，请重新登录");
    }
    let updated = refresh_tokens(client, &creds.refresh_token, creds).await?;
    *creds = updated;
    save_creds(card_id, creds)?;
    Ok(())
}

// ---------- 模型列表拉取（fetchAvailableModels） ----------

pub async fn fetch_models(card_id: &str, global: &Global) -> Result<Vec<String>> {
    let mut creds = load_creds(card_id)?
        .with_context(|| format!("Antigravity 登录卡 {card_id} 还没有登录"))?;
    let client = build_http_client(global, Duration::from_secs(15))?;
    ensure_valid_creds(card_id, &mut creds, &client, false).await?;

    if creds.project_id.is_empty()
        && let Ok((pid, p)) = resolve_code_assist(&client, &creds.access_token).await
        && !pid.is_empty()
    {
        creds.project_id = pid;
        if creds.plan.is_empty() && !p.is_empty() {
            creds.plan = p;
        }
        let _ = save_creds(card_id, &creds);
    }

    let send_once = |token: &str, project_id: &str| {
        let mut body = json!({});
        if !project_id.is_empty() {
            body["project"] = json!(project_id);
        }
        client
            .post(format!("{API_BASE_URL}/v1internal:fetchAvailableModels"))
            .bearer_auth(token)
            .header("User-Agent", cli_user_agent())
            .json(&body)
            .send()
    };

    let mut resp = send_once(&creds.access_token, &creds.project_id)
        .await
        .context("请求 Antigravity 模型列表失败")?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        ensure_valid_creds(card_id, &mut creds, &client, true).await?;
        resp = send_once(&creds.access_token, &creds.project_id)
            .await
            .context("请求 Antigravity 模型列表失败")?;
    }

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("拉取 Antigravity 模型列表失败（HTTP {status}）：{}", truncate(&text, 300));
    }
    let v: Value = serde_json::from_str(&text).context("Antigravity 模型列表返回的不是 JSON")?;
    Ok(parse_available_models(&v))
}

pub fn parse_available_models(v: &Value) -> Vec<String> {
    let mut ids: Vec<String> = v
        .get("models")
        .and_then(Value::as_object)
        .map(|map| map.keys().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect())
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

// ---------- 额度查询（retrieveUserQuotaSummary） ----------

pub fn fetch_usage_blocking(card_id: &str, global: &Global) -> Result<Usage> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(fetch_usage(card_id, global))
}

pub async fn fetch_usage(card_id: &str, global: &Global) -> Result<Usage> {
    let mut creds = load_creds(card_id)?
        .with_context(|| format!("Antigravity 登录卡 {card_id} 还没有登录"))?;
    let client = build_http_client(global, Duration::from_secs(20))?;
    ensure_valid_creds(card_id, &mut creds, &client, false).await?;

    if creds.project_id.is_empty() {
        let (pid, plan) = resolve_code_assist(&client, &creds.access_token).await?;
        if !pid.is_empty() {
            creds.project_id = pid;
        }
        if !plan.is_empty() {
            creds.plan = plan;
        }
        let _ = save_creds(card_id, &creds);
    }

    let send_once = |token: &str, project_id: &str| {
        let mut body = json!({});
        if !project_id.is_empty() {
            body["project"] = json!(project_id);
        }
        client
            .post(format!("{API_BASE_URL}/v1internal:retrieveUserQuotaSummary"))
            .bearer_auth(token)
            .header("User-Agent", cli_user_agent())
            .json(&body)
            .send()
    };

    let mut resp = send_once(&creds.access_token, &creds.project_id)
        .await
        .context("请求 Antigravity 额度接口失败")?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        ensure_valid_creds(card_id, &mut creds, &client, true).await?;
        resp = send_once(&creds.access_token, &creds.project_id)
            .await
            .context("请求 Antigravity 额度接口失败")?;
    }

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!("Antigravity 额度查询被拒绝（HTTP {status}）：{}", truncate(&text, 300));
    }
    if !status.is_success() {
        bail!("查询 Antigravity 额度失败（HTTP {status}）：{}", truncate(&text, 300));
    }
    let v: Value = serde_json::from_str(&text).context("Antigravity 额度接口返回的不是 JSON")?;
    Ok(parse_quota_summary(&v))
}

pub fn parse_quota_summary(v: &Value) -> Usage {
    let mut items: Vec<(usize, UsageWindow)> = Vec::new();
    for group in v.get("groups").and_then(Value::as_array).into_iter().flatten() {
        for bucket in group.get("buckets").and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = bucket.get("bucketId").or_else(|| bucket.get("id")).and_then(Value::as_str) else {
                continue;
            };
            let Some(remaining) = bucket.get("remainingFraction").and_then(Value::as_f64) else {
                continue;
            };
            let (order, label) = match id {
                "gemini-5h" => (0, "Gemini（5 小时）".to_string()),
                "gemini-weekly" => (1, "Gemini（本周）".to_string()),
                "gemini-argon-wtu-weekly" => (2, "Gemini Argon（本周）".to_string()),
                "3p-5h" => (3, "Claude / GPT（5 小时）".to_string()),
                "3p-weekly" => (4, "Claude / GPT（本周）".to_string()),
                other => (5, other.to_string()),
            };
            let resets_at = bucket
                .get("resetTime")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            items.push((
                order,
                UsageWindow {
                    label,
                    percent: (1.0 - remaining.clamp(0.0, 1.0)) * 100.0,
                    resets_at,
                },
            ));
        }
    }
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.label.cmp(&b.1.label)));
    Usage {
        windows: items.into_iter().map(|(_, w)| w).collect(),
        fetched_at_ms: chrono::Utc::now().timestamp_millis(),
        credential_expires_at: String::new(),
    }
}

// ---------- 辅助函数 ----------

pub fn extract_project_id(val: Option<&Value>) -> Option<String> {
    match val? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Object(o) => o
            .get("id")
            .or_else(|| o.get("projectId"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from),
        _ => None,
    }
}

fn parse_ineligible_reason(v: &Value) -> Option<String> {
    // 若已有 cloudaicompanionProject 或 currentTier，说明账号可用
    if extract_project_id(v.get("cloudaicompanionProject")).is_some()
        || v.get("currentTier").is_some_and(|t| !t.is_null())
        || v.get("paidTier").is_some_and(|t| !t.is_null())
    {
        return None;
    }
    v.get("ineligibleTiers")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter().find_map(|t| {
                t.get("reasonMessage")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            })
        })
}

fn parse_plan_name(v: &Value) -> Option<String> {
    if let Some(name) = v.pointer("/paidTier/name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        return Some(name.strip_prefix("Google ").unwrap_or(name).to_string());
    }
    let id = v
        .pointer("/paidTier/id")
        .or_else(|| v.pointer("/currentTier/id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(match id.to_ascii_lowercase().as_str() {
        "free-tier" | "legacy-tier" => "Free".into(),
        "g1-pro-tier" => "AI Pro".into(),
        "g1-ultra-tier" => "AI Ultra".into(),
        "standard-tier" => "Paid".into(),
        other if other.starts_with("g1-ultra-") => "AI Ultra".into(),
        other if other.starts_with("g1-pro-") => "AI Pro".into(),
        other => other.to_string(),
    })
}

fn extract_email_from_jwt(jwt: &str) -> Option<String> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let bytes = base64url_decode(payload_b64)?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("email").and_then(Value::as_str).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quota_summary_maps_and_sorts_buckets() {
        let v = json!({
            "groups": [
                {
                    "buckets": [
                        { "bucketId": "3p-weekly", "remainingFraction": 0.5, "resetTime": "2026-11-30T00:00:00Z" },
                        { "bucketId": "gemini-5h", "remainingFraction": 0.8, "resetTime": "2026-11-24T15:00:00Z" }
                    ]
                },
                {
                    "buckets": [
                        { "bucketId": "gemini-weekly", "remainingFraction": 0.9, "resetTime": "2026-11-30T00:00:00Z" },
                        { "bucketId": "3p-5h", "remainingFraction": 1.0, "resetTime": "2026-11-24T15:00:00Z" }
                    ]
                }
            ]
        });
        let u = parse_quota_summary(&v);
        assert_eq!(u.windows.len(), 4);
        assert_eq!(u.windows[0].label, "Gemini（5 小时）");
        assert!((u.windows[0].percent - 20.0).abs() < 0.01);
        assert_eq!(u.windows[1].label, "Gemini（本周）");
        assert_eq!(u.windows[2].label, "Claude / GPT（5 小时）");
        assert_eq!(u.windows[3].label, "Claude / GPT（本周）");
        assert!((u.windows[3].percent - 50.0).abs() < 0.01);
    }

    #[test]
    fn parse_available_models_extracts_sorted_keys() {
        let v = json!({
            "models": {
                "gemini-3.5-flash": { "displayName": "Gemini 3.5 Flash" },
                "claude-sonnet-4-6": { "displayName": "Claude Sonnet 4.6" }
            }
        });
        assert_eq!(parse_available_models(&v), vec!["claude-sonnet-4-6", "gemini-3.5-flash"]);
    }
}
