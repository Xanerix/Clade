//! 聚合模式的本地网关：在 clade 进程里起一个 HTTP 服务（127.0.0.1 随机端口），和 claude 同生同死。
//! /v1/models 发布所选各卡的模型（ID 是 cl-claude-<卡id>--<模型>），/v1/messages 按 ID 找到对应的卡，
//! 把模型名换回原名、换上这张卡的真 key 转发，响应原样流式返回。不重试，只设连接超时。

use crate::antigravity::{self, AntigravityCreds};
use crate::convert;
use crate::core::{
    ApiFormat, Auth, Card, CardKind, CardModel, PromptPolicy, CONTEXT_1M, SCRATCHPAD_PROMPT_PREFIX, SCRATCHPAD_PROMPT_SUFFIX,
};
use crate::oauth;
use crate::openai_oauth::{self, OpenAiOAuthCreds};
use anyhow::{bail, Result};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub enum RouteAuth {
    ApiKey(String),
    OpenAiOAuth(Arc<tokio::sync::Mutex<OpenAiOAuthCreds>>),
    Antigravity(Arc<tokio::sync::Mutex<AntigravityCreds>>),
}

pub struct Route {
    pub card: Card,
    pub auth: RouteAuth,
    pub protocol: ApiFormat,
}

impl Route {
    fn api_key(&self) -> &str {
        match &self.auth {
            RouteAuth::ApiKey(k) => k,
            RouteAuth::OpenAiOAuth(_) | RouteAuth::Antigravity(_) => "",
        }
    }
}

pub struct Config {
    pub routes: Vec<Route>,
    /// claude 拿到的随机 token，请求必须带上它
    pub token: String,
    /// 每次启动覆盖
    pub log: Option<PathBuf>,
    /// 访问上游用的代理，和 claude 进程拿到的代理变量一致
    pub proxy: Proxy,
    /// 非空时拦截 auto 模式分类器请求，将 ~14 万字符的默认 system 提示词替换为该文本
    pub classifier_prompt: Option<String>,
}

pub enum Proxy {
    /// 读进程环境的 HTTP(S)_PROXY（reqwest 默认行为）
    Env,
    /// 不走代理
    Off,
    /// 指定 http / https 两个代理，NO_PROXY 照样读环境
    Set { http: String, https: String },
}

/// 网关发布给 claude 的模型 ID。ID 里要有 claude 才进 /model 选择器，又不能以 claude- 开头（会被当成官方模型）。
/// 窗口达到 1M 的加 [1m]：claude 按 1M 算，发请求前会自己去掉这个后缀。
pub fn model_id(card: &str, m: &CardModel) -> String {
    let suffix = if m.context >= CONTEXT_1M { "[1m]" } else { "" };
    format!("cl-claude-{card}--{}{suffix}", m.id)
}

/// /model 里这个模型下面那行说明：卡名，1M 的加标记
pub fn model_description(card: &Card, m: &CardModel) -> String {
    let name = if card.name.is_empty() { &card.id } else { &card.name };
    if m.context >= CONTEXT_1M { format!("{name} · 1M") } else { name.clone() }
}

/// claude 加 [1m] 时会带上这个 Anthropic 专用的 beta 头，转发前去掉
const BETA_1M: &str = "context-1m-2025-08-07";

/// 拼接 OpenAI Chat Completions 上游地址：
/// base_url 去末尾 / 后，已以 /chat/completions 结尾直接用，已以 /v1 结尾补 /chat/completions，否则补 /v1/chat/completions。
pub fn openai_chat_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// 拼接 OpenAI Responses 上游地址：
/// base_url 去末尾 / 后，已以 /responses 结尾直接用，已以 /v1 结尾补 /responses，否则补 /v1/responses。
pub fn openai_responses_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/responses") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/responses")
    } else {
        format!("{base}/v1/responses")
    }
}

/// 拼接 Gemini Native 上游地址：
/// base_url 去末尾 / 后，已以 /v1beta 或 /v1 结尾直接拼 /models/<model>:<action>，否则补 /v1beta。
pub fn gemini_url(base_url: &str, model: &str, stream: bool) -> String {
    let base = base_url.trim_end_matches('/');
    let model = model.strip_prefix("models/").unwrap_or(model);
    let action = if stream { "streamGenerateContent?alt=sse" } else { "generateContent" };
    if base.ends_with("/v1beta") || base.ends_with("/v1") {
        format!("{base}/models/{model}:{action}")
    } else {
        format!("{base}/v1beta/models/{model}:{action}")
    }
}

/// 从完整 URL 提取路径部分（不含 host 和 query），用于日志。
fn url_path(url: &str) -> &str {
    let without_query = url.split('?').next().unwrap_or(url);
    match without_query.find("://") {
        Some(i) => match without_query[i + 3..].find('/') {
            Some(j) => &without_query[i + 3 + j..],
            None => "/",
        },
        None => without_query,
    }
}

/// 这些头不原样转发：认证换成卡的 key，长度和连接由 HTTP 库处理；不转发 accept-encoding，让上游返回未压缩的内容
const SKIP_REQ: &[&str] = &["host", "authorization", "x-api-key", "content-length", "connection", "accept-encoding", "transfer-encoding"];
const SKIP_OPENAI_REQ: &[&str] = &["anthropic-version", "anthropic-beta", "content-type"];
const SKIP_RESP: &[&str] = &["content-length", "connection", "transfer-encoding"];

/// 按代理配置和可选总超时构建 reqwest::Client（网关上游、OAuth 刷新、拉取模型、查额度共用）。
pub fn build_http_client(proxy: &Proxy, timeout: Option<Duration>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(10));
    if let Some(t) = timeout {
        builder = builder.timeout(t);
    }
    builder = match proxy {
        Proxy::Env => builder,
        Proxy::Off => builder.no_proxy(),
        Proxy::Set { http, https } => builder
            .proxy(reqwest::Proxy::http(http)?.no_proxy(reqwest::NoProxy::from_env()))
            .proxy(reqwest::Proxy::https(https)?.no_proxy(reqwest::NoProxy::from_env())),
    };
    Ok(builder.build()?)
}

struct Gateway {
    routes: Vec<Route>,
    /// 去掉 [1m] 的发布 ID -> (第几张卡, 上游模型名)
    models: HashMap<String, (usize, String)>,
    token: String,
    /// 本次会话稳定的轨迹 ID（用于 Antigravity 的 X-Aicode-Trajectory-Id）
    trajectory_id: String,
    /// 转发上游请求用：只设连接超时，不设总时长（流式响应可能持续很久）
    client: reqwest::Client,
    /// OAuth 刷新 token 专用：带 30 秒总超时，避免持锁刷新时卡死整张卡的请求
    oauth_client: reqwest::Client,
    /// 非空时替换 auto 模式分类器请求的 system 提示词
    classifier_prompt: Option<String>,
    log: Option<Mutex<File>>,
}

impl Gateway {
    fn new(cfg: Config) -> Result<Self> {
        let mut models = HashMap::new();
        for (i, r) in cfg.routes.iter().enumerate() {
            for m in &r.card.model_list {
                let id = model_id(&r.card.id, m).trim_end_matches("[1m]").to_string();
                if models.insert(id.clone(), (i, m.id.clone())).is_some() {
                    bail!("网关模型 ID 冲突：{id}");
                }
            }
        }
        let log = match cfg.log {
            Some(p) => Some(Mutex::new(open_log(&p)?)),
            None => None,
        };
        let client = build_http_client(&cfg.proxy, None)?;
        let oauth_client = build_http_client(&cfg.proxy, Some(oauth::OAUTH_HTTP_TIMEOUT))?;
        let trajectory_id = uuid::Uuid::new_v4().to_string();
        let classifier_prompt = cfg
            .classifier_prompt
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(Self {
            routes: cfg.routes,
            models,
            token: cfg.token,
            trajectory_id,
            client,
            oauth_client,
            classifier_prompt,
            log,
        })
    }

    fn log(&self, line: &str) {
        if let Some(f) = &self.log {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(f.lock().unwrap(), "{now} {line}");
        }
    }

    fn authorized(&self, h: &HeaderMap) -> bool {
        let get = |k| h.get(k).and_then(|v: &HeaderValue| v.to_str().ok());
        get(header::AUTHORIZATION).and_then(|v| v.strip_prefix("Bearer ")) == Some(self.token.as_str())
            || get(header::HeaderName::from_static("x-api-key")) == Some(self.token.as_str())
    }

    fn list_models(&self) -> Response {
        let data: Vec<Value> = self
            .routes
            .iter()
            .flat_map(|r| {
                r.card.model_list.iter().map(|m| {
                    json!({
                        "type": "model",
                        "id": model_id(&r.card.id, m),
                        "display_name": m.id,
                        "description": model_description(&r.card, m),
                        "created_at": "1970-01-01T00:00:00Z",
                    })
                })
            })
            .collect();
        Json(json!({ "data": data, "has_more": false })).into_response()
    }
}

/// 打开日志并覆盖旧内容，不让别的进程同时写（读和删可以）。同一个 profile 同时开了多个会话时，
/// 被占用就依次换成 -2、-3……，空出来的编号下次再用，所以文件数不超过同时开的会话数。
fn open_log(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ_DELETE: u32 = 0x1 | 0x4;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 1;
    loop {
        let p = if n == 1 { path.to_path_buf() } else { path.with_file_name(format!("{stem}-{n}{ext}")) };
        match OpenOptions::new().write(true).create(true).truncate(true).share_mode(FILE_SHARE_READ_DELETE).open(&p) {
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => n += 1,
            r => return Ok(r?),
        }
    }
}

/// Anthropic 格式的错误响应，claude 会把 message 显示出来
fn api_error(status: StatusCode, kind: &str, msg: String) -> Response {
    (status, Json(json!({ "type": "error", "error": { "type": kind, "message": msg } }))).into_response()
}

/// 在后台线程起网关，返回端口。线程跟着进程走，claude 退出、clade 退出时一起结束。
pub fn start(cfg: Config) -> Result<u16> {
    let gw = Arc::new(Gateway::new(cfg)?);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    std::thread::spawn(move || {
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("网关监听失败");
            let app = Router::new().fallback(handle).with_state(gw);
            axum::serve(listener, app).await.expect("网关异常退出");
        })
    });
    Ok(port)
}

async fn handle(State(gw): State<Arc<Gateway>>, req: Request) -> Response {
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    if !gw.authorized(req.headers()) {
        gw.log(&format!("{method} {path} 401 token 不对"));
        return api_error(StatusCode::UNAUTHORIZED, "authentication_error", "clade 网关：token 不对".into());
    }
    if method == Method::GET && path == "/v1/models" {
        gw.log("GET /v1/models 200");
        return gw.list_models();
    }
    if method == Method::POST && path.starts_with("/v1/messages") {
        return forward(gw, req).await;
    }
    gw.log(&format!("{method} {path} 404 不支持"));
    api_error(StatusCode::NOT_FOUND, "not_found_error", format!("clade 网关不支持 {method} {path}"))
}

async fn forward(gw: Arc<Gateway>, req: Request) -> Response {
    let start = Instant::now();
    let (parts, body) = req.into_parts();
    let path = parts.uri.path();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", format!("clade 网关读取请求失败：{e}")),
    };
    let mut v: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", format!("clade 网关：请求不是 JSON：{e}")),
    };
    let asked = v["model"].as_str().unwrap_or_default().trim_end_matches("[1m]").to_string();
    let Some((i, upstream_model)) = gw.models.get(&asked) else {
        gw.log(&format!("POST {path} model={asked} 404 未知模型"));
        return api_error(StatusCode::NOT_FOUND, "not_found_error", format!("clade 网关：未知模型 {asked}"));
    };
    let route = &gw.routes[*i];
    v["model"] = Value::String(upstream_model.clone());
    // 先判定并替换 auto 模式分类器提示词（须在主对话 system 改写之前，避免分类器请求被主对话策略覆盖）
    let is_classifier = is_auto_mode_classifier_request(&v);
    if is_classifier {
        if let Some(prompt) = gw.classifier_prompt.as_deref() {
            rewrite_classifier_system_prompt(&mut v, prompt);
        }
    } else {
        let sys_policy = route.card.system_prompt_policy.clone().unwrap_or_else(|| {
            if route.card.should_strip_system_prompt() {
                PromptPolicy::Strip
            } else {
                PromptPolicy::Keep
            }
        });
        let preserve_memory = route.card.preserve_memory_prompt.unwrap_or(true);
        apply_system_prompt_policy(&mut v, &sys_policy, preserve_memory);
        if let Some(rem_policy) = &route.card.system_reminder_policy {
            apply_system_reminder_policy(&mut v, rem_policy);
        }
    }

    if route.card.kind == CardKind::OpenAiOAuth {
        return forward_openai_oauth(&gw, route, &parts, path, upstream_model, &v, start).await;
    }
    if route.card.kind == CardKind::Antigravity {
        return forward_antigravity(gw.clone(), *i, &parts, path, upstream_model, &v, start).await;
    }

    match route.protocol {
        ApiFormat::Anthropic => forward_anthropic(&gw, route, &parts, path, upstream_model, &v, start).await,
        ApiFormat::OpenAiChat => forward_openai_chat(&gw, route, &parts, path, upstream_model, &v, start).await,
        ApiFormat::OpenAiResponses => forward_openai_responses(&gw, route, &parts, path, upstream_model, &v, start).await,
        ApiFormat::GeminiNative => forward_gemini_native(&gw, route, &parts, path, upstream_model, &v, start).await,
    }
}

async fn forward_anthropic(
    gw: &Gateway,
    route: &Route,
    parts: &axum::http::request::Parts,
    path: &str,
    upstream_model: &str,
    v: &Value,
    start: Instant,
) -> Response {
    let mut url = format!("{}{path}", route.card.base_url.trim_end_matches('/'));
    if let Some(q) = parts.uri.query() {
        url = format!("{url}?{q}");
    }
    let up_path = url_path(&url);
    let mut headers = HeaderMap::new();
    for (k, val) in &parts.headers {
        if SKIP_REQ.contains(&k.as_str()) {
            continue;
        }
        if k.as_str() == "anthropic-beta" {
            let kept: Vec<&str> = val.to_str().unwrap_or_default().split(',').map(str::trim).filter(|b| *b != BETA_1M && !b.is_empty()).collect();
            if !kept.is_empty() {
                headers.append(k, HeaderValue::from_str(&kept.join(",")).unwrap());
            }
            continue;
        }
        headers.append(k, val.clone());
    }
    let mut up = gw.client.post(&url).headers(headers).body(serde_json::to_vec(v).unwrap());
    up = match route.card.auth {
        Auth::Bearer => up.bearer_auth(route.api_key()),
        Auth::XApiKey => up.header("x-api-key", route.api_key()),
    };
    let tag = format!("POST {path} proto={} upstream={up_path} model={upstream_model} card={}", route.protocol.as_str(), route.card.id);
    let resp = match up.send().await {
        Ok(r) => r,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 连接上游失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关连接 {} 失败：{e}", route.card.id));
        }
    };
    let status = resp.status();
    let mut out = Response::builder().status(status);
    for (k, val) in resp.headers() {
        if !SKIP_RESP.contains(&k.as_str()) {
            out = out.header(k, val);
        }
    }
    let ms = start.elapsed().as_millis();
    if status.is_success() {
        gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
        return out.body(Body::from_stream(resp.bytes_stream())).unwrap();
    }
    // 上游报错：记下错误体，再原样返回给 claude
    let text = resp.bytes().await.unwrap_or_default();
    let shown: String = String::from_utf8_lossy(&text).chars().take(2000).collect();
    gw.log(&format!("{tag} {} {ms}ms {shown}", status.as_u16()));
    out.body(Body::from(text)).unwrap()
}

async fn forward_openai_chat(
    gw: &Gateway,
    route: &Route,
    parts: &axum::http::request::Parts,
    path: &str,
    upstream_model: &str,
    v: &Value,
    start: Instant,
) -> Response {
    if path != "/v1/messages" {
        gw.log(&format!(
            "POST {path} proto={} model={upstream_model} card={} 404 不支持",
            route.protocol.as_str(),
            route.card.id
        ));
        return api_error(StatusCode::NOT_FOUND, "not_found_error", format!("OpenAI Chat 协议不支持 {path}"));
    }

    let url = openai_chat_url(&route.card.base_url);
    let up_path = url_path(&url);
    let tag = format!("POST {path} proto={} upstream={up_path} model={upstream_model} card={}", route.protocol.as_str(), route.card.id);

    let chat_req = match convert::anthropic_to_openai_chat(v) {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 400 {}ms 转换请求失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", format!("clade 网关转换请求失败：{e}"));
        }
    };
    let want_stream = v.get("stream").and_then(Value::as_bool) == Some(true);

    let mut headers = HeaderMap::new();
    for (k, val) in &parts.headers {
        let name = k.as_str();
        if SKIP_REQ.contains(&name) || SKIP_OPENAI_REQ.contains(&name) {
            continue;
        }
        headers.append(k, val.clone());
    }
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let up = gw
        .client
        .post(&url)
        .headers(headers)
        .bearer_auth(route.api_key())
        .body(serde_json::to_vec(&chat_req).unwrap());

    let resp = match up.send().await {
        Ok(r) => r,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 连接上游失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关连接 {} 失败：{e}", route.card.id));
        }
    };
    let status = resp.status();
    let ms = start.elapsed().as_millis();

    if !status.is_success() {
        let text = resp.bytes().await.unwrap_or_default();
        let shown: String = String::from_utf8_lossy(&text).chars().take(2000).collect();
        gw.log(&format!("{tag} {} {ms}ms {shown}", status.as_u16()));
        let err_json = convert::openai_error_to_anthropic(status.as_u16(), &text);
        return (status, Json(err_json)).into_response();
    }

    let is_sse = want_stream
        || resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"));

    if is_sse {
        gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
        let sse = convert::openai_sse_to_anthropic_sse(resp.bytes_stream());
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(sse))
            .unwrap();
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 读取上游响应失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关读取 {} 响应失败：{e}", route.card.id));
        }
    };
    let up_json: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            let shown: String = String::from_utf8_lossy(&bytes).chars().take(500).collect();
            gw.log(&format!("{tag} 502 {}ms 上游响应不是 JSON：{e} {shown}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关：{} 返回的不是 JSON：{e}", route.card.id));
        }
    };
    match convert::openai_chat_to_anthropic(&up_json) {
        Ok(anthropic) => {
            gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
            (status, Json(anthropic)).into_response()
        }
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 转换响应失败：{e}", start.elapsed().as_millis()));
            api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关转换 {} 响应失败：{e}", route.card.id))
        }
    }
}

async fn forward_openai_responses(
    gw: &Gateway,
    route: &Route,
    parts: &axum::http::request::Parts,
    path: &str,
    upstream_model: &str,
    v: &Value,
    start: Instant,
) -> Response {
    if path != "/v1/messages" {
        gw.log(&format!(
            "POST {path} proto={} model={upstream_model} card={} 404 不支持",
            route.protocol.as_str(),
            route.card.id
        ));
        return api_error(StatusCode::NOT_FOUND, "not_found_error", format!("OpenAI Responses 协议不支持 {path}"));
    }

    let url = openai_responses_url(&route.card.base_url);
    let up_path = url_path(&url);
    let tag = format!("POST {path} proto={} upstream={up_path} model={upstream_model} card={}", route.protocol.as_str(), route.card.id);

    let resp_req = match convert::anthropic_to_openai_responses(v) {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 400 {}ms 转换请求失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", format!("clade 网关转换请求失败：{e}"));
        }
    };
    let want_stream = v.get("stream").and_then(Value::as_bool) == Some(true);
    let web_search_name = convert::anthropic_web_search_tool_name(v).map(String::from);

    let mut headers = HeaderMap::new();
    for (k, val) in &parts.headers {
        let name = k.as_str();
        if SKIP_REQ.contains(&name) || SKIP_OPENAI_REQ.contains(&name) {
            continue;
        }
        headers.append(k, val.clone());
    }
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let up = gw
        .client
        .post(&url)
        .headers(headers)
        .bearer_auth(route.api_key())
        .body(serde_json::to_vec(&resp_req).unwrap());

    let resp = match up.send().await {
        Ok(r) => r,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 连接上游失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关连接 {} 失败：{e}", route.card.id));
        }
    };
    let status = resp.status();
    let ms = start.elapsed().as_millis();

    if !status.is_success() {
        let text = resp.bytes().await.unwrap_or_default();
        let shown: String = String::from_utf8_lossy(&text).chars().take(2000).collect();
        gw.log(&format!("{tag} {} {ms}ms {shown}", status.as_u16()));
        let err_json = convert::openai_error_to_anthropic(status.as_u16(), &text);
        return (status, Json(err_json)).into_response();
    }

    let ct = resp.headers().get(header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).unwrap_or("");
    let is_sse = ct.contains("text/event-stream") || (want_stream && !ct.contains("application/json"));

    if is_sse {
        gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
        let sse = convert::openai_responses_sse_to_anthropic_sse_with_web_search(resp.bytes_stream(), web_search_name);
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(sse))
            .unwrap();
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 读取上游响应失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关读取 {} 响应失败：{e}", route.card.id));
        }
    };
    let up_json: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            let shown: String = String::from_utf8_lossy(&bytes).chars().take(500).collect();
            gw.log(&format!("{tag} 502 {}ms 上游响应不是 JSON：{e} {shown}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关：{} 返回的不是 JSON：{e}", route.card.id));
        }
    };
    match convert::openai_responses_to_anthropic_with_web_search(&up_json, web_search_name.as_deref()) {
        Ok(anthropic) => {
            gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
            (status, Json(anthropic)).into_response()
        }
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 转换响应失败：{e}", start.elapsed().as_millis()));
            api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关转换 {} 响应失败：{e}", route.card.id))
        }
    }
}

async fn forward_gemini_native(
    gw: &Gateway,
    route: &Route,
    parts: &axum::http::request::Parts,
    path: &str,
    upstream_model: &str,
    v: &Value,
    start: Instant,
) -> Response {
    if path != "/v1/messages" {
        gw.log(&format!(
            "POST {path} proto={} model={upstream_model} card={} 404 不支持",
            route.protocol.as_str(),
            route.card.id
        ));
        return api_error(StatusCode::NOT_FOUND, "not_found_error", format!("Gemini Native 协议不支持 {path}"));
    }

    let want_stream = v.get("stream").and_then(Value::as_bool) == Some(true);
    let web_search_name = convert::anthropic_web_search_tool_name(v).map(String::from);
    let url = gemini_url(&route.card.base_url, upstream_model, want_stream);
    let up_path = url_path(&url);
    let tag = format!("POST {path} proto={} upstream={up_path} model={upstream_model} card={}", route.protocol.as_str(), route.card.id);

    let gemini_req = match convert::anthropic_to_gemini(v) {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 400 {}ms 转换请求失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", format!("clade 网关转换请求失败：{e}"));
        }
    };

    let mut headers = HeaderMap::new();
    for (k, val) in &parts.headers {
        let name = k.as_str();
        if SKIP_REQ.contains(&name) || SKIP_OPENAI_REQ.contains(&name) || name == "x-goog-api-key" {
            continue;
        }
        headers.append(k, val.clone());
    }
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let up = gw
        .client
        .post(&url)
        .headers(headers)
        .header("x-goog-api-key", route.api_key())
        .body(serde_json::to_vec(&gemini_req).unwrap());

    let resp = match up.send().await {
        Ok(r) => r,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 连接上游失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关连接 {} 失败：{e}", route.card.id));
        }
    };
    let status = resp.status();
    let ms = start.elapsed().as_millis();

    if !status.is_success() {
        let text = resp.bytes().await.unwrap_or_default();
        let shown: String = String::from_utf8_lossy(&text).chars().take(2000).collect();
        gw.log(&format!("{tag} {} {ms}ms {shown}", status.as_u16()));
        let err_json = convert::openai_error_to_anthropic(status.as_u16(), &text);
        return (status, Json(err_json)).into_response();
    }

    let ct = resp.headers().get(header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).unwrap_or("");
    let is_sse = ct.contains("text/event-stream") || (want_stream && !ct.contains("application/json"));

    if is_sse {
        gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
        let sse = convert::gemini_sse_to_anthropic_sse_with_options(resp.bytes_stream(), None, web_search_name);
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(sse))
            .unwrap();
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 读取上游响应失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关读取 {} 响应失败：{e}", route.card.id));
        }
    };
    let up_json: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            let shown: String = String::from_utf8_lossy(&bytes).chars().take(500).collect();
            gw.log(&format!("{tag} 502 {}ms 上游响应不是 JSON：{e} {shown}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关：{} 返回的不是 JSON：{e}", route.card.id));
        }
    };
    match convert::gemini_to_anthropic_with_web_search(&up_json, web_search_name.as_deref()) {
        Ok(anthropic) => {
            gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
            (status, Json(anthropic)).into_response()
        }
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 转换响应失败：{e}", start.elapsed().as_millis()));
            api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关转换 {} 响应失败：{e}", route.card.id))
        }
    }
}

async fn forward_openai_oauth(
    gw: &Gateway,
    route: &Route,
    parts: &axum::http::request::Parts,
    path: &str,
    upstream_model: &str,
    v: &Value,
    start: Instant,
) -> Response {
    if path != "/v1/messages" {
        gw.log(&format!("POST {path} proto=openai_oauth model={upstream_model} card={} 404 不支持", route.card.id));
        return api_error(StatusCode::NOT_FOUND, "not_found_error", format!("OpenAI 登录卡不支持 {path}"));
    }

    let RouteAuth::OpenAiOAuth(creds_lock) = &route.auth else {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "api_error", "clade 网关：OpenAI 登录卡缺少 OAuth 凭据".into());
    };

    let url = if route.card.base_url.trim().is_empty() {
        openai_oauth::CODEX_RESPONSES_URL.to_string()
    } else {
        openai_responses_url(&route.card.base_url)
    };
    let up_path = url_path(&url);
    let tag = format!("POST {path} proto=openai_oauth upstream={up_path} model={upstream_model} card={}", route.card.id);

    let resp_req = match convert::anthropic_to_codex_oauth_responses(v) {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 400 {}ms 转换请求失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", format!("clade 网关转换请求失败：{e}"));
        }
    };
    let body_bytes = serde_json::to_vec(&resp_req).unwrap();
    let want_stream = v.get("stream").and_then(Value::as_bool) == Some(true);
    let web_search_name = convert::anthropic_web_search_tool_name(v).map(String::from);

    let (mut access_token, mut account_id, can_refresh) = {
        let mut guard = creds_lock.lock().await;
        if let Err(e) = openai_oauth::ensure_valid_creds(&route.card.id, &mut guard, &gw.oauth_client, false).await {
            gw.log(&format!("{tag} 401 {}ms 刷新 OAuth token 失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::UNAUTHORIZED, "authentication_error", format!("clade 网关：{e}"));
        }
        (guard.access_token.clone(), guard.chatgpt_account_id.clone(), !guard.refresh_token.is_empty())
    };

    let build_headers = |account_id: &str| {
        let mut headers = HeaderMap::new();
        for (k, val) in &parts.headers {
            let name = k.as_str();
            if SKIP_REQ.contains(&name)
                || SKIP_OPENAI_REQ.contains(&name)
                || matches!(name, "user-agent" | "originator" | "version" | "chatgpt-account-id" | "accept")
            {
                continue;
            }
            headers.append(k, val.clone());
        }
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(header::USER_AGENT, HeaderValue::from_static(openai_oauth::USER_AGENT));
        headers.insert(header::HeaderName::from_static("originator"), HeaderValue::from_static(openai_oauth::ORIGINATOR));
        headers.insert(header::HeaderName::from_static("version"), HeaderValue::from_static(openai_oauth::CLIENT_VERSION));
        if !account_id.is_empty()
            && let Ok(hv) = HeaderValue::from_str(account_id)
        {
            headers.insert(header::HeaderName::from_static("chatgpt-account-id"), hv);
        }
        headers
    };

    let mut resp = match gw
        .client
        .post(&url)
        .headers(build_headers(&account_id))
        .bearer_auth(&access_token)
        .body(body_bytes.clone())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 连接上游失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关连接 {} 失败：{e}", route.card.id));
        }
    };

    // 收到 401 时强制刷新一次 token 重试
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED && can_refresh {
        let refreshed = {
            let mut guard = creds_lock.lock().await;
            match openai_oauth::ensure_valid_creds(&route.card.id, &mut guard, &gw.oauth_client, true).await {
                Ok(()) => Some((guard.access_token.clone(), guard.chatgpt_account_id.clone())),
                Err(e) => {
                    gw.log(&format!("{tag} 401 {}ms 强制刷新 OAuth token 失败：{e}", start.elapsed().as_millis()));
                    None
                }
            }
        };
        if let Some((new_token, new_account_id)) = refreshed {
            access_token = new_token;
            account_id = new_account_id;
            match gw
                .client
                .post(&url)
                .headers(build_headers(&account_id))
                .bearer_auth(&access_token)
                .body(body_bytes)
                .send()
                .await
            {
                Ok(r) => resp = r,
                Err(e) => {
                    gw.log(&format!("{tag} 502 {}ms 刷新后重试连接上游失败：{e}", start.elapsed().as_millis()));
                    return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关连接 {} 失败：{e}", route.card.id));
                }
            }
        }
    }

    let status = resp.status();
    let ms = start.elapsed().as_millis();

    if !status.is_success() {
        let text = resp.bytes().await.unwrap_or_default();
        let shown: String = String::from_utf8_lossy(&text).chars().take(2000).collect();
        gw.log(&format!("{tag} {} {ms}ms {shown}", status.as_u16()));
        let err_json = convert::openai_error_to_anthropic(status.as_u16(), &text);
        return (status, Json(err_json)).into_response();
    }

    let ct = resp.headers().get(header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).unwrap_or("");
    let is_sse = ct.contains("text/event-stream") || !ct.contains("application/json");

    if is_sse && want_stream {
        gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
        let sse = convert::openai_responses_sse_to_anthropic_sse_with_web_search(resp.bytes_stream(), web_search_name);
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(sse))
            .unwrap();
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 读取上游响应失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关读取 {} 响应失败：{e}", route.card.id));
        }
    };

    if is_sse {
        return match convert::collect_responses_sse_to_anthropic_with_web_search(&bytes, web_search_name.as_deref()) {
            Ok(anthropic) => {
                gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
                (status, Json(anthropic)).into_response()
            }
            Err(e) => {
                gw.log(&format!("{tag} 502 {}ms 转换 SSE 响应失败：{e}", start.elapsed().as_millis()));
                api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关转换 {} 响应失败：{e}", route.card.id))
            }
        };
    }

    let up_json: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            let shown: String = String::from_utf8_lossy(&bytes).chars().take(500).collect();
            gw.log(&format!("{tag} 502 {}ms 上游响应不是 JSON：{e} {shown}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关：{} 返回的不是 JSON：{e}", route.card.id));
        }
    };
    match convert::openai_responses_to_anthropic_with_web_search(&up_json, web_search_name.as_deref()) {
        Ok(anthropic) => {
            gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
            (status, Json(anthropic)).into_response()
        }
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 转换响应失败：{e}", start.elapsed().as_millis()));
            api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关转换 {} 响应失败：{e}", route.card.id))
        }
    }
}

/// Antigravity 流式请求等待上游首包响应头的宽限时间：
/// 超过该时间仍未收到响应头时，抢先开启 SSE 并进入 thinking 块，避免 Claude Code 10 秒后判定卡死。
const ANTIGRAVITY_EARLY_THINKING_DELAY: Duration = Duration::from_secs(2);
/// Antigravity 流式思考静默期的 SSE 心跳间隔：
/// Claude Code 字节流看门狗默认 20 秒无字节即弹「Waiting for API response · will retry in ...」，
/// 内层 OMs 心跳检查周期为 10 秒，8 秒发一次标准 `event: ping` 可稳定刷新字节到达时间而不污染内容块。
const ANTIGRAVITY_SSE_KEEPALIVE: Duration = Duration::from_secs(8);

async fn forward_antigravity(
    gw: Arc<Gateway>,
    route_idx: usize,
    _parts: &axum::http::request::Parts,
    path: &str,
    upstream_model: &str,
    v: &Value,
    start: Instant,
) -> Response {
    let route = &gw.routes[route_idx];
    if path != "/v1/messages" {
        gw.log(&format!("POST {path} proto=antigravity model={upstream_model} card={} 404 不支持", route.card.id));
        return api_error(StatusCode::NOT_FOUND, "not_found_error", format!("Antigravity 登录卡不支持 {path}"));
    }

    let RouteAuth::Antigravity(creds_lock) = &route.auth else {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "api_error", "clade 网关：Antigravity 登录卡缺少 OAuth 凭据".into());
    };
    let creds_lock = creds_lock.clone();
    let card_id = route.card.id.clone();

    let want_stream = v.get("stream").and_then(Value::as_bool) == Some(true);
    let thinking_disabled = v.pointer("/thinking/type").and_then(Value::as_str) == Some("disabled");
    let web_search_name = convert::anthropic_web_search_tool_name(v).map(String::from);
    let base = if route.card.base_url.trim().is_empty() {
        antigravity::API_BASE_URL
    } else {
        route.card.base_url.trim_end_matches('/')
    };
    // Antigravity 的 effort 写在模型名后缀上（如 gemini-4-neon-low / -medium / -high / -xhigh）：
    // 若 Claude Code 传了 output_config.effort（/effort 或 --effort），按档位动态切换模型后缀。
    let effective_model = apply_antigravity_effort(upstream_model, convert::anthropic_effort(v));
    let url = format!("{base}/v1internal:streamGenerateContent?alt=sse");
    let up_path = url_path(&url);
    let tag = format!("POST {path} proto=antigravity upstream={up_path} model={effective_model} card={card_id}");

    let mut inner_req = match convert::anthropic_to_gemini(v) {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 400 {}ms 转换请求失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", format!("clade 网关转换请求失败：{e}"));
        }
    };
    let include_thoughts = !thinking_disabled
        && (v.get("thinking").is_some()
            || ["-xhigh", "-high", "-medium", "-low"]
                .iter()
                .any(|s| effective_model.ends_with(s)));
    prepare_antigravity_inner_request(&mut inner_req, include_thoughts);

    let (mut access_token, mut project_id, can_refresh) = {
        let mut guard = creds_lock.lock().await;
        if let Err(e) = antigravity::ensure_valid_creds(&card_id, &mut guard, &gw.oauth_client, false).await {
            gw.log(&format!("{tag} 401 {}ms 刷新 OAuth token 失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::UNAUTHORIZED, "authentication_error", format!("clade 网关：{e}"));
        }
        (guard.access_token.clone(), guard.project_id.clone(), !guard.refresh_token.is_empty())
    };

    let req_uuid = uuid::Uuid::new_v4().to_string();
    let gw_req = gw.clone();
    let card_id_req = card_id.clone();
    let tag_req = tag.clone();
    let effective_model_req = effective_model.clone();
    let mut send_fut = Box::pin(async move {
        let build_body = |project: &str| {
            let mut wrapped = serde_json::Map::new();
            if !project.is_empty() {
                wrapped.insert("project".into(), json!(project));
            }
            wrapped.insert("requestId".into(), json!(format!("agent-{req_uuid}")));
            wrapped.insert("userAgent".into(), json!("antigravity"));
            wrapped.insert("requestType".into(), json!("agent"));
            wrapped.insert("model".into(), json!(&effective_model_req));
            wrapped.insert("request".into(), inner_req.clone());
            serde_json::to_vec(&Value::Object(wrapped)).unwrap()
        };

        // 模拟原版 agy CLI 出站头：动态探测本机 agy 版本 UA + 轨迹/请求 ID，不透传 Claude Code 头
        let build_headers = || {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(header::ACCEPT, HeaderValue::from_static("text/event-stream"));
            if let Ok(ua) = HeaderValue::from_str(antigravity::cli_user_agent()) {
                headers.insert(header::USER_AGENT, ua);
            }
            if let Ok(tid) = HeaderValue::from_str(&gw_req.trajectory_id) {
                headers.insert(header::HeaderName::from_static("x-aicode-trajectory-id"), tid);
            }
            if let Ok(rid) = HeaderValue::from_str(&req_uuid) {
                headers.insert(header::HeaderName::from_static("x-aicode-request-id"), rid);
            }
            headers
        };

        let mut resp = gw_req
            .client
            .post(&url)
            .headers(build_headers())
            .bearer_auth(&access_token)
            .body(build_body(&project_id))
            .send()
            .await
            .map_err(|e| format!("连接上游失败：{e}"))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && can_refresh {
            let refreshed = {
                let mut guard = creds_lock.lock().await;
                match antigravity::ensure_valid_creds(&card_id_req, &mut guard, &gw_req.oauth_client, true).await {
                    Ok(()) => Some((guard.access_token.clone(), guard.project_id.clone())),
                    Err(e) => {
                        gw_req.log(&format!("{tag_req} 401 {}ms 强制刷新 OAuth token 失败：{e}", start.elapsed().as_millis()));
                        None
                    }
                }
            };
            if let Some((new_token, new_project)) = refreshed {
                access_token = new_token;
                project_id = new_project;
                resp = gw_req
                    .client
                    .post(&url)
                    .headers(build_headers())
                    .bearer_auth(&access_token)
                    .body(build_body(&project_id))
                    .send()
                    .await
                    .map_err(|e| format!("刷新后重试连接上游失败：{e}"))?;
            }
        }
        Ok::<reqwest::Response, String>(resp)
    });

    // 流式且未禁用思考时：宽限期内没等到响应头，抢先返回 200 SSE + 空 thinking 块，让 Claude Code 进入 thinking 态
    let resp_result = if want_stream && !thinking_disabled {
        tokio::select! {
            res = &mut send_fut => res,
            _ = tokio::time::sleep(ANTIGRAVITY_EARLY_THINKING_DELAY) => {
                let ws_name_early = web_search_name.clone();
                let sse = async_stream::stream! {
                    let mut conv = convert::GeminiSseConverter::with_web_search_name(ws_name_early);
                    for ev in conv.start_early_thinking(&effective_model) {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(ev));
                    }
                    let resp = loop {
                        tokio::select! {
                            res = &mut send_fut => match res {
                                Ok(r) => break r,
                                Err(e) => {
                                    gw.log(&format!("{tag} 502 {}ms {e}", start.elapsed().as_millis()));
                                    yield Ok(Bytes::from(conv.error_event(format!("clade 网关连接 {card_id} 失败：{e}"))));
                                    return;
                                }
                            },
                            _ = tokio::time::sleep(ANTIGRAVITY_SSE_KEEPALIVE) => {
                                yield Ok(Bytes::from_static(convert::GeminiSseConverter::ping_event().as_bytes()));
                            }
                        }
                    };
                    let status = resp.status();
                    let ms = start.elapsed().as_millis();
                    if !status.is_success() {
                        let text = resp.bytes().await.unwrap_or_default();
                        let shown: String = String::from_utf8_lossy(&text).chars().take(2000).collect();
                        gw.log(&format!("{tag} {} {ms}ms {shown}", status.as_u16()));
                        let err_json = convert::openai_error_to_anthropic(status.as_u16(), &text);
                        yield Ok(Bytes::from(conv.error_event_from_payload(&err_json)));
                        return;
                    }
                    gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
                    let mut upstream = resp.bytes_stream();
                    loop {
                        let next_item = tokio::select! {
                            item = upstream.next() => Some(item),
                            _ = tokio::time::sleep(ANTIGRAVITY_SSE_KEEPALIVE) => None,
                        };
                        let Some(item) = next_item else {
                            yield Ok(Bytes::from_static(convert::GeminiSseConverter::ping_event().as_bytes()));
                            continue;
                        };
                        let Some(item) = item else {
                            break;
                        };
                        match item {
                            Ok(bytes) => {
                                for ev in conv.push_bytes(&bytes) {
                                    yield Ok(Bytes::from(ev));
                                }
                                if conv.is_finished() {
                                    return;
                                }
                            }
                            Err(e) => {
                                yield Ok(Bytes::from(conv.error_event(format!("上游流读取失败：{e}"))));
                                return;
                            }
                        }
                    }
                    for ev in conv.finish() {
                        yield Ok(Bytes::from(ev));
                    }
                };
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
                    .header(header::CACHE_CONTROL, "no-cache")
                    .body(Body::from_stream(sse))
                    .unwrap();
            }
        }
    } else {
        send_fut.await
    };

    let resp = match resp_result {
        Ok(r) => r,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms {e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关连接 {card_id} 失败：{e}"));
        }
    };

    let status = resp.status();
    let ms = start.elapsed().as_millis();

    if !status.is_success() {
        let text = resp.bytes().await.unwrap_or_default();
        let shown: String = String::from_utf8_lossy(&text).chars().take(2000).collect();
        gw.log(&format!("{tag} {} {ms}ms {shown}", status.as_u16()));
        let err_json = convert::openai_error_to_anthropic(status.as_u16(), &text);
        return (status, Json(err_json)).into_response();
    }

    let ct = resp.headers().get(header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).unwrap_or("");
    let is_sse = ct.contains("text/event-stream") || !ct.contains("application/json");

    if is_sse && want_stream {
        gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
        let sse = convert::gemini_sse_to_anthropic_sse_with_options(
            resp.bytes_stream(),
            Some(ANTIGRAVITY_SSE_KEEPALIVE),
            web_search_name,
        );
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(sse))
            .unwrap();
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 读取上游响应失败：{e}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关读取 {card_id} 响应失败：{e}"));
        }
    };
    if is_sse {
        return match convert::collect_gemini_sse_to_anthropic_with_web_search(&bytes, web_search_name.as_deref()) {
            Ok(anthropic) => {
                gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
                (status, Json(anthropic)).into_response()
            }
            Err(e) => {
                gw.log(&format!("{tag} 502 {}ms 转换 SSE 响应失败：{e}", start.elapsed().as_millis()));
                api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关转换 {card_id} 响应失败：{e}"))
            }
        };
    }
    let up_json: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            let shown: String = String::from_utf8_lossy(&bytes).chars().take(500).collect();
            gw.log(&format!("{tag} 502 {}ms 上游响应不是 JSON：{e} {shown}", start.elapsed().as_millis()));
            return api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关：{card_id} 返回的不是 JSON：{e}"));
        }
    };
    match convert::gemini_to_anthropic_with_web_search(&up_json, web_search_name.as_deref()) {
        Ok(anthropic) => {
            gw.log(&format!("{tag} {} {ms}ms", status.as_u16()));
            (status, Json(anthropic)).into_response()
        }
        Err(e) => {
            gw.log(&format!("{tag} 502 {}ms 转换响应失败：{e}", start.elapsed().as_millis()));
            api_error(StatusCode::BAD_GATEWAY, "api_error", format!("clade 网关转换 {card_id} 响应失败：{e}"))
        }
    }
}

/// Claude Code 客户端 auto 模式分类器 system 提示词特征标记（2.1.330 实测）。
const CLASSIFIER_SYSTEM_MARKER: &str = "You are a security monitor for autonomous AI coding agents.";
const CLASSIFIER_PERMISSIONS_MARKER: &str = "<cc_automode_permissions>";

/// 判断是否为 Claude Code 客户端 auto 模式分类器请求。
/// 同时校验 system 里的分类器模板标记，以及无 tools 且 messages 中含 `<transcript>` 块，避免误伤普通对话。
pub fn is_auto_mode_classifier_request(v: &Value) -> bool {
    let has_tools = v.get("tools").and_then(Value::as_array).is_some_and(|a| !a.is_empty());
    if has_tools {
        return false;
    }
    let system_matches = match v.get("system") {
        Some(Value::String(s)) => is_classifier_system_text(s),
        Some(Value::Array(blocks)) => blocks.iter().any(|b| {
            b.get("text")
                .and_then(Value::as_str)
                .is_some_and(is_classifier_system_text)
        }),
        _ => false,
    };
    if !system_matches {
        return false;
    }
    has_transcript_message(v)
}

fn is_classifier_system_text(text: &str) -> bool {
    text.contains(CLASSIFIER_SYSTEM_MARKER) && text.contains(CLASSIFIER_PERMISSIONS_MARKER)
}

fn has_transcript_message(v: &Value) -> bool {
    let Some(messages) = v.get("messages").and_then(Value::as_array) else {
        return false;
    };
    messages.iter().any(|m| match m.get("content") {
        Some(Value::String(s)) => s.contains("<transcript>"),
        Some(Value::Array(blocks)) => blocks.iter().any(|b| {
            b.get("text")
                .and_then(Value::as_str)
                .is_some_and(|t| t.contains("<transcript>"))
        }),
        _ => false,
    })
}

/// 若请求为 auto 模式分类器请求，将 system 里的 ~14 万字符默认模板替换为 `custom_prompt`，
/// 保留主块原有的 `cache_control`（若有）及 Claude Code 的计费头块（`x-anthropic-billing-header:`）。
/// 返回是否发生了替换。
pub fn rewrite_classifier_system_prompt(v: &mut Value, custom_prompt: &str) -> bool {
    let custom = custom_prompt.trim();
    if custom.is_empty() || !is_auto_mode_classifier_request(v) {
        return false;
    }
    let Some(obj) = v.as_object_mut() else {
        return false;
    };
    let Some(sys) = obj.get("system") else {
        return false;
    };
    let next = match sys {
        Value::String(_) => Value::String(custom.to_string()),
        Value::Array(blocks) => {
            let mut out = Vec::new();
            let mut replaced = false;
            for b in blocks {
                let text = b.get("text").and_then(Value::as_str).unwrap_or_default();
                if text.starts_with("x-anthropic-billing-header:") {
                    out.push(b.clone());
                } else if !replaced && is_classifier_system_text(text) {
                    let mut new_block = json!({ "type": "text", "text": custom });
                    if let Some(cc) = b.get("cache_control") {
                        new_block["cache_control"] = cc.clone();
                    }
                    out.push(new_block);
                    replaced = true;
                }
            }
            if !replaced {
                out.push(json!({ "type": "text", "text": custom }));
            }
            Value::Array(out)
        }
        _ => return false,
    };
    obj.insert("system".into(), next);
    true
}

/// 按策略处理主对话请求顶层的 `system` 字段：
/// - `Keep`：保持原样
/// - `Strip`：剥离默认系统提示词，保留 scratchpad（以及可选的 `# Memory` 段落）
/// - `Custom { text }`：用自定义文本替换默认系统提示词，并追加原请求里的 scratchpad（以及可选的 `# Memory` 段落）
pub fn apply_system_prompt_policy(v: &mut Value, policy: &PromptPolicy, preserve_memory: bool) {
    if matches!(policy, PromptPolicy::Keep) {
        return;
    }
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    let scratchpad = obj.get("system").and_then(extract_scratchpad_from_system);
    let memory = if preserve_memory {
        obj.get("system").and_then(extract_memory_from_system)
    } else {
        None
    };

    let mut parts: Vec<String> = Vec::new();
    if let PromptPolicy::Custom { text } = policy {
        let t = text.trim();
        if !t.is_empty() {
            parts.push(t.to_string());
        }
    }
    if let Some(mem) = memory {
        parts.push(mem);
    }
    if let Some(sp) = scratchpad {
        parts.push(sp);
    }

    if parts.is_empty() {
        obj.remove("system");
    } else {
        obj.insert("system".into(), Value::String(parts.join("\n\n")));
    }
}

/// 剥离 Claude Code 系统提示词，但保留 clade 通过 --append-system-prompt 注入的 scratchpad 路径提示。
/// 若请求中不含 scratchpad 提示，则直接移除 system 字段。
pub fn strip_system_prompt_keep_scratchpad(v: &mut Value) {
    apply_system_prompt_policy(v, &PromptPolicy::Strip, false);
}

fn extract_scratchpad_snippet(text: &str) -> Option<String> {
    let start = text.find(SCRATCHPAD_PROMPT_PREFIX)?;
    let rest = &text[start..];
    let end = rest.find(SCRATCHPAD_PROMPT_SUFFIX).map(|i| i + SCRATCHPAD_PROMPT_SUFFIX.len())?;
    Some(rest[..end].to_string())
}

fn extract_scratchpad_from_system(system: &Value) -> Option<String> {
    match system {
        Value::String(s) => extract_scratchpad_snippet(s),
        Value::Array(blocks) => blocks.iter().find_map(|b| {
            b.get("text")
                .and_then(Value::as_str)
                .and_then(extract_scratchpad_snippet)
        }),
        _ => None,
    }
}

/// 从主系统提示词里提取 `# Memory` 段落（以 `# Memory` 行开头，到下一个 `# ` 一级标题前结束）。
fn extract_memory_section(text: &str) -> Option<String> {
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if let Some(s_idx) = start {
            if trimmed.starts_with("# ") {
                let s = text[s_idx..offset].trim();
                return (!s.is_empty()).then(|| s.to_string());
            }
        } else if trimmed == "# Memory" {
            start = Some(offset);
        }
        offset += line.len();
    }
    let s = text[start?..].trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn extract_memory_from_system(system: &Value) -> Option<String> {
    match system {
        Value::String(s) => extract_memory_section(s),
        Value::Array(blocks) => blocks.iter().find_map(|b| {
            b.get("text")
                .and_then(Value::as_str)
                .and_then(extract_memory_section)
        }),
        _ => None,
    }
}

/// 移除字符串中所有的 `<system-reminder>...</system-reminder>` 块，若无标签返回 None。
fn strip_system_reminder_tags(text: &str) -> Option<String> {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    if !text.contains(OPEN) {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(OPEN) {
        out.push_str(&rest[..i]);
        let after = &rest[i + OPEN.len()..];
        match after.find(CLOSE) {
            Some(j) => rest = &after[j + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    Some(out.trim().to_string())
}

/// 按策略处理 `messages` 中的 `<system-reminder>...</system-reminder>` 块和 `role: "system"` 消息：
/// - `Keep`：保持原样
/// - `Strip`：移除所有 `<system-reminder>` 块与 `role: "system"` 消息
/// - `Custom { text }`：移除原有 `<system-reminder>` 与 `role: "system"` 消息，并在首条 `user` 消息前插入一条自定义 `<system-reminder>` 文本块
pub fn apply_system_reminder_policy(v: &mut Value, policy: &PromptPolicy) {
    if matches!(policy, PromptPolicy::Keep) {
        return;
    }
    let Some(msgs) = v.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    let mut next_msgs = Vec::with_capacity(msgs.len());
    for mut msg in msgs.drain(..) {
        if msg.get("role").and_then(Value::as_str) == Some("system") {
            continue;
        }
        let mut keep_msg = true;
        if let Some(content) = msg.get_mut("content") {
            match content {
                Value::String(s) => {
                    if let Some(cleaned) = strip_system_reminder_tags(s) {
                        if cleaned.is_empty() {
                            keep_msg = false;
                        } else {
                            *s = cleaned;
                        }
                    }
                }
                Value::Array(blocks) => {
                    let mut new_blocks = Vec::with_capacity(blocks.len());
                    for mut b in blocks.drain(..) {
                        if b.get("type").and_then(Value::as_str) == Some("text")
                            && let Some(t) = b.get("text").and_then(Value::as_str)
                            && let Some(cleaned) = strip_system_reminder_tags(t)
                        {
                            if cleaned.is_empty() {
                                continue;
                            }
                            b["text"] = Value::String(cleaned);
                        }
                        new_blocks.push(b);
                    }
                    if new_blocks.is_empty() {
                        keep_msg = false;
                    } else {
                        *blocks = new_blocks;
                    }
                }
                _ => {}
            }
        }
        if keep_msg {
            next_msgs.push(msg);
        }
    }

    if let PromptPolicy::Custom { text } = policy {
        let custom = text.trim();
        if !custom.is_empty() {
            let wrapped = if custom.contains("<system-reminder>") {
                custom.to_string()
            } else {
                format!("<system-reminder>\n{custom}\n</system-reminder>")
            };
            if let Some(first_user) = next_msgs
                .iter_mut()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
            {
                match first_user.get_mut("content") {
                    Some(Value::String(s)) => {
                        let prev = std::mem::take(s);
                        first_user["content"] = json!([
                            { "type": "text", "text": wrapped },
                            { "type": "text", "text": prev },
                        ]);
                    }
                    Some(Value::Array(blocks)) => {
                        blocks.insert(0, json!({ "type": "text", "text": wrapped }));
                    }
                    _ => {}
                }
            } else {
                next_msgs.insert(0, json!({ "role": "user", "content": wrapped }));
            }
        }
    }

    *msgs = next_msgs;
}

/// 将 Claude Code 内置 effort（low / medium / high / xhigh / max）映射到 Antigravity 带档位后缀的模型名。
/// 不带 `-low` / `-medium` / `-high` / `-xhigh` 后缀的模型保持原样。
pub fn apply_antigravity_effort(model: &str, effort: Option<&str>) -> String {
    let Some(effort) = effort else {
        return model.to_string();
    };
    let base = ["-xhigh", "-high", "-medium", "-low"]
        .iter()
        .find_map(|s| model.strip_suffix(s));
    let Some(base) = base else {
        return model.to_string();
    };
    let supports_xhigh = matches!(base, "gemini-4-neon" | "gemini-4-hydrogen" | "gemini-4-hydrogen-tiered");
    let suffix = match effort {
        "low" => "low",
        "medium" => "medium",
        "xhigh" | "max" if supports_xhigh => "xhigh",
        _ => "high",
    };
    format!("{base}-{suffix}")
}

/// 基于首条用户消息生成稳定的负 63 位 sessionId（与 sub2api 一致），有助于上游提示词缓存命中。
fn stable_antigravity_session_id(inner: &Value) -> String {
    use sha2::{Digest, Sha256};
    if let Some(contents) = inner.get("contents").and_then(Value::as_array) {
        for c in contents {
            if c.get("role").and_then(Value::as_str) == Some("user")
                && let Some(text) = c
                    .get("parts")
                    .and_then(Value::as_array)
                    .and_then(|ps| ps.iter().find_map(|p| p.get("text").and_then(Value::as_str)))
                    .filter(|t| !t.is_empty())
            {
                let hash = Sha256::digest(text.as_bytes());
                let n = u64::from_be_bytes(hash[..8].try_into().unwrap()) & 0x7FFF_FFFF_FFFF_FFFF;
                return format!("-{n}");
            }
        }
    }
    let id = uuid::Uuid::new_v4();
    let n = u64::from_be_bytes(id.as_bytes()[..8].try_into().unwrap()) & 0x7FFF_FFFF_FFFF_FFFF;
    format!("-{n}")
}

/// 适配 Antigravity v1internal 约束（模拟原版 agy CLI）：
/// 1. 注入 Antigravity identity 提示块（当卡片开启「剥离系统提示词」时，v["system"] 已在转发前移除，这里只保留干净的 identity）
/// 2. 补齐 toolConfig 与稳定的 sessionId
/// 3. 按需开启 generationConfig.thinkingConfig.includeThoughts，让上游实时流式返回思考摘要
fn prepare_antigravity_inner_request(inner: &mut Value, include_thoughts: bool) {
    const AGY_IDENTITY: &str = "<identity>\n\
You are Antigravity, a powerful agentic AI coding assistant designed by the Google Deepmind team working on Advanced Agentic Coding.\n\
You are pair programming with a USER to solve their coding task.\n\
</identity>";

    let session_id = stable_antigravity_session_id(inner);
    let Some(obj) = inner.as_object_mut() else {
        return;
    };
    obj.entry("sessionId".to_string()).or_insert_with(|| json!(session_id));
    obj.entry("toolConfig".to_string())
        .or_insert_with(|| json!({ "functionCallingConfig": { "mode": "VALIDATED" } }));
    if include_thoughts {
        let gen_cfg = obj.entry("generationConfig".to_string()).or_insert_with(|| json!({}));
        if let Some(gc) = gen_cfg.as_object_mut() {
            let tc = gc.entry("thinkingConfig".to_string()).or_insert_with(|| json!({}));
            if let Some(tco) = tc.as_object_mut() {
                tco.entry("includeThoughts".to_string()).or_insert_with(|| json!(true));
            }
        }
    }

    let mut has_agy_identity = false;
    let mut new_parts: Vec<Value> = Vec::new();
    if let Some(parts) = obj.get("systemInstruction").and_then(|s| s.get("parts")).and_then(Value::as_array) {
        for p in parts {
            if let Some(text) = p.get("text").and_then(Value::as_str) {
                if text.contains("You are Antigravity") {
                    has_agy_identity = true;
                }
                if !text.trim().is_empty() {
                    new_parts.push(json!({ "text": text }));
                }
            } else {
                new_parts.push(p.clone());
            }
        }
    }
    if !has_agy_identity {
        new_parts.insert(0, json!({ "text": AGY_IDENTITY }));
    }
    obj.insert("systemInstruction".into(), json!({ "role": "user", "parts": new_parts }));
}
