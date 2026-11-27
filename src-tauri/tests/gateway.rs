//! 网关测试：本机起一个假上游，经网关转发。不碰真实数据，不需要 #[ignore]。

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::post,
    Router,
};
use clade_lib::antigravity::AntigravityCreds;
use clade_lib::core::{ApiFormat, Auth, Card, CardKind, CardModel};
use clade_lib::gateway::{self, Config, Proxy, Route, RouteAuth};
use clade_lib::openai_oauth::OpenAiOAuthCreds;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<(String, HeaderMap, Value)>>>;

/// 假上游：记录收到的路径、头和请求体。
/// - /api/v1/*：模拟 Anthropic 上游（模型名 bad 返回 400，否则返回一段 SSE）
/// - /openai/*：模拟 OpenAI Chat Completions 上游（非流式、流式、错误）
fn fake_upstream(seen: Seen) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    std::thread::spawn(move || {
        rt.block_on(async move {
            let seen_ant = seen.clone();
            let seen_oa = seen.clone();
            let seen_resp = seen.clone();
            let seen_gem = seen.clone();
            let seen_gem_ca = seen.clone();
            let app = Router::new()
                .route(
                    "/codeassist/{*rest}",
                    post(move |uri: Uri, h: HeaderMap, body: Bytes| {
                        let seen = seen_gem_ca.clone();
                        async move {
                            let v: Value = serde_json::from_slice(&body).unwrap();
                            let path = uri.path().to_string();
                            let stream = path.contains("streamGenerateContent");
                            let model = v["model"].as_str().unwrap_or("").to_string();
                            seen.lock().unwrap().push((path, h, v));
                            if model == "slow-think" {
                                tokio::time::sleep(std::time::Duration::from_millis(2400)).await;
                                let sse = concat!(
                                    "data: {\"response\":{\"responseId\":\"ca-slow\",\"modelVersion\":\"slow-think\",\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"深入思考\"}]}}]}}\n\n",
                                    "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"慢思考完成\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":15,\"candidatesTokenCount\":7}}}\n\n"
                                );
                                return ([("content-type", "text/event-stream")], sse).into_response();
                            }
                            if model == "slow-err" {
                                tokio::time::sleep(std::time::Duration::from_millis(2400)).await;
                                return (
                                    StatusCode::TOO_MANY_REQUESTS,
                                    [("content-type", "application/json")],
                                    r#"{"error":{"message":"slow rate limit","type":"rate_limit_error"}}"#,
                                )
                                    .into_response();
                            }
                            if model == "ws-search" {
                                let sse = concat!(
                                    "data: {\"response\":{\"responseId\":\"ca-ws\",\"modelVersion\":\"ws-search\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"最新 Rust 为 1.100.0\"}]}}]}}\n\n",
                                    "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}]},\"finishReason\":\"STOP\",\"groundingMetadata\":{\"webSearchQueries\":[\"rust latest\"],\"groundingChunks\":[{\"web\":{\"title\":\"rust-lang.org\",\"uri\":\"https://example.com/rust\"}}]}}],\"usageMetadata\":{\"promptTokenCount\":20,\"candidatesTokenCount\":6}}}\n\n"
                                );
                                return ([("content-type", "text/event-stream")], sse).into_response();
                            }
                            if stream {
                                let sse = "data: {\"response\":{\"responseId\":\"ca-s1\",\"modelVersion\":\"gemini-2.5-pro\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"来自CodeAssist\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":4}}}\n\n";
                                return ([("content-type", "text/event-stream")], sse).into_response();
                            }
                            let resp = json!({
                                "response": {
                                    "responseId": "ca-n1",
                                    "modelVersion": "gemini-2.5-pro",
                                    "candidates": [{
                                        "content": {"role": "model", "parts": [{"text": "hello from code assist"}]},
                                        "finishReason": "STOP"
                                    }],
                                    "usageMetadata": {"promptTokenCount": 12, "candidatesTokenCount": 5}
                                }
                            });
                            (StatusCode::OK, [("content-type", "application/json")], resp.to_string()).into_response()
                        }
                    }),
                )
                .route(
                    "/api/v1/{*rest}",
                    post(move |uri: Uri, h: HeaderMap, body: Bytes| {
                        let seen = seen_ant.clone();
                        async move {
                            let v: Value = serde_json::from_slice(&body).unwrap();
                            let bad = v["model"] == "bad";
                            seen.lock().unwrap().push((uri.path().to_string(), h, v));
                            if bad {
                                return (StatusCode::BAD_REQUEST, r#"{"type":"error","error":{"type":"invalid_request_error","message":"nope"}}"#).into_response();
                            }
                            ([("content-type", "text/event-stream")], "event: message_stop\ndata: {}\n\n").into_response()
                        }
                    }),
                )
                .route(
                    "/gemini/{*rest}",
                    post(move |uri: Uri, h: HeaderMap, body: Bytes| {
                        let seen = seen_gem.clone();
                        async move {
                            let v: Value = serde_json::from_slice(&body).unwrap();
                            let path = uri.path().to_string();
                            let stream = path.ends_with(":streamGenerateContent");
                            let is_err = path.contains("err-gem");
                            seen.lock().unwrap().push((path, h, v));
                            if is_err {
                                return (
                                    StatusCode::FORBIDDEN,
                                    [("content-type", "application/json")],
                                    r#"{"error":{"code":403,"message":"api key invalid","status":"PERMISSION_DENIED"}}"#,
                                )
                                    .into_response();
                            }
                            if stream {
                                let sse = concat!(
                                    "data: {\"responseId\":\"gem-s1\",\"modelVersion\":\"gemini-2.5-pro\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"来自Gemini\"}]}}]}\n\n",
                                    "data: {\"responseId\":\"gem-s1\",\"modelVersion\":\"gemini-2.5-pro\",\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"gc_1\",\"name\":\"read\",\"args\":{\"p\":\"a\"}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":14,\"candidatesTokenCount\":6}}\n\n"
                                );
                                return ([("content-type", "text/event-stream")], sse).into_response();
                            }
                            let resp = json!({
                                "responseId": "gem-n1",
                                "modelVersion": "gemini-2.5-pro",
                                "candidates": [{
                                    "content": {"role": "model", "parts": [{"text": "hello from gemini"}]},
                                    "finishReason": "STOP"
                                }],
                                "usageMetadata": {"promptTokenCount": 16, "candidatesTokenCount": 5, "cachedContentTokenCount": 2}
                            });
                            (StatusCode::OK, [("content-type", "application/json")], resp.to_string()).into_response()
                        }
                    }),
                )
                .route(
                    "/openai-resp/{*rest}",
                    post(move |uri: Uri, h: HeaderMap, body: Bytes| {
                        let seen = seen_resp.clone();
                        async move {
                            let v: Value = serde_json::from_slice(&body).unwrap();
                            let model = v["model"].as_str().unwrap_or("").to_string();
                            let stream = v["stream"].as_bool().unwrap_or(false);
                            seen.lock().unwrap().push((uri.path().to_string(), h, v));
                            if model == "err-resp" {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    [("content-type", "application/json")],
                                    r#"{"error":{"message":"invalid input","type":"invalid_request_error"}}"#,
                                )
                                    .into_response();
                            }
                            if model == "ws-resp" {
                                let sse = concat!(
                                    "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_ws\",\"model\":\"ws-resp\"}}\n\n",
                                    "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_call_1\",\"status\":\"completed\",\"action\":{\"type\":\"search\",\"query\":\"rust release\",\"sources\":[{\"url\":\"https://rust-lang.org/\",\"title\":\"Rust\"}]}}}\n\n",
                                    "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Rust 1.100.0\"}\n\n",
                                    "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":4}}}\n\n"
                                );
                                return ([("content-type", "text/event-stream")], sse).into_response();
                            }
                            if stream {
                                let sse = concat!(
                                    "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_s1\",\"model\":\"gpt-5.4\"}}\n\n",
                                    "event: response.content_part.added\ndata: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
                                    "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"来自Responses\"}\n\n",
                                    "event: response.content_part.done\ndata: {\"type\":\"response.content_part.done\",\"part\":{\"type\":\"output_text\",\"text\":\"来自Responses\"}}\n\n",
                                    "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"it_1\",\"call_id\":\"fc_1\",\"name\":\"read\"}}\n\n",
                                    "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\\\"p\\\":\\\"a\\\"}\"}\n\n",
                                    "event: response.function_call_arguments.done\ndata: {\"type\":\"response.function_call_arguments.done\",\"arguments\":\"{\\\"p\\\":\\\"a\\\"}\"}\n\n",
                                    "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":15,\"output_tokens\":7}}}\n\n"
                                );
                                return ([("content-type", "text/event-stream")], sse).into_response();
                            }
                            let resp = json!({
                                "id": "resp_n1",
                                "model": model,
                                "status": "completed",
                                "output": [
                                    {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello from responses"}]}
                                ],
                                "usage": {
                                    "input_tokens": 18,
                                    "output_tokens": 6,
                                    "input_tokens_details": {"cached_tokens": 3}
                                }
                            });
                            (StatusCode::OK, [("content-type", "application/json")], resp.to_string()).into_response()
                        }
                    }),
                )
                .route(
                    "/openai/{*rest}",
                    post(move |uri: Uri, h: HeaderMap, body: Bytes| {
                        let seen = seen_oa.clone();
                        async move {
                            let v: Value = serde_json::from_slice(&body).unwrap();
                            let model = v["model"].as_str().unwrap_or("").to_string();
                            let stream = v["stream"].as_bool().unwrap_or(false);
                            seen.lock().unwrap().push((uri.path().to_string(), h, v));
                            if model == "err-model" {
                                return (
                                    StatusCode::TOO_MANY_REQUESTS,
                                    [("content-type", "application/json")],
                                    r#"{"error":{"message":"quota exceeded","type":"rate_limit_error"}}"#,
                                )
                                    .into_response();
                            }
                            if stream {
                                let sse = concat!(
                                    "data: {\"id\":\"chatcmpl-s1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"你好\"}}]}\n\n",
                                    "data: {\"id\":\"chatcmpl-s1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"ls\",\"arguments\":\"{\\\"p\\\":\\\".\\\"}\"}}]}}]}\n\n",
                                    "data: {\"id\":\"chatcmpl-s1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
                                    "data: {\"id\":\"chatcmpl-s1\",\"model\":\"gpt-4o\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":8}}\n\n",
                                    "data: [DONE]\n\n"
                                );
                                return ([("content-type", "text/event-stream")], sse).into_response();
                            }
                            let resp = json!({
                                "id": "chatcmpl-n1",
                                "model": model,
                                "choices": [{
                                    "index": 0,
                                    "message": {
                                        "role": "assistant",
                                        "content": "hello from openai",
                                        "reasoning_content": "should drop"
                                    },
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 20,
                                    "completion_tokens": 5,
                                    "prompt_tokens_details": {"cached_tokens": 4}
                                }
                            });
                            (StatusCode::OK, [("content-type", "application/json")], resp.to_string()).into_response()
                        }
                    }),
                );
            axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app).await.unwrap();
        })
    });
    port
}

fn card(id: &str, port: u16, auth: Auth, models: &[(&str, u64)]) -> Card {
    Card {
        id: id.into(),
        name: format!("卡{id}"),
        kind: CardKind::Api,
        protocol: ApiFormat::Anthropic,
        base_url: format!("http://127.0.0.1:{port}/api/"),
        auth,
        model_list: models.iter().map(|(m, c)| CardModel { id: (*m).into(), context: *c }).collect(),
        strip_system_prompt: None,
        ..Default::default()
    }
}

fn openai_card(id: &str, base_url: String, models: &[(&str, u64)]) -> Card {
    Card {
        id: id.into(),
        name: format!("卡{id}"),
        kind: CardKind::Api,
        protocol: ApiFormat::OpenAiChat,
        base_url,
        auth: Auth::XApiKey, // OpenAI 卡应忽略此项，固定用 Bearer
        model_list: models.iter().map(|(m, c)| CardModel { id: (*m).into(), context: *c }).collect(),
        strip_system_prompt: None,
        ..Default::default()
    }
}

#[test]
fn model_ids() {
    assert_eq!(gateway::model_id("op", &CardModel { id: "glm-5".into(), context: 200_000 }), "cl-claude-op--glm-5");
    assert_eq!(gateway::model_id("op", &CardModel { id: "k2".into(), context: 1_000_000 }), "cl-claude-op--k2[1m]");
    assert_eq!(gateway::model_id("op", &CardModel { id: "k3".into(), context: 400_000 }), "cl-claude-op--k3", "只有 1M 及以上才加 [1m]");
}

#[test]
fn forward() {
    // 环境里设了 http_proxy 时，网关访问本机的假上游也会走代理；只有这个测试建 HTTP 客户端，在它们创建之前设好
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1") };
    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let log = std::env::temp_dir().join(format!("clade-gateway-test-{}.log", std::process::id()));
    let port = gateway::start(Config {
        routes: vec![
            Route { card: card("a", up, Auth::Bearer, &[("m1", 200_000), ("big", 1_000_000), ("bad", 200_000)]), auth: RouteAuth::ApiKey("key-a".into()), protocol: ApiFormat::Anthropic },
            Route { card: card("b", up, Auth::XApiKey, &[("m2", 128_000)]), auth: RouteAuth::ApiKey("key-b".into()), protocol: ApiFormat::Anthropic },
        ],
        token: "tok-9f3a".into(),
        log: Some(log.clone()),
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::new();

    // token 不对
    let r = http.get(format!("{base}/v1/models")).bearer_auth("wrong").send().unwrap();
    assert_eq!(r.status(), 401);

    // 模型列表
    let v: Value = http.get(format!("{base}/v1/models?limit=1000")).bearer_auth("tok-9f3a").send().unwrap().json().unwrap();
    let ids: Vec<&str> = v["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["cl-claude-a--m1", "cl-claude-a--big[1m]", "cl-claude-a--bad", "cl-claude-b--m2"]);
    assert_eq!(v["data"][1]["display_name"], "big");
    assert_eq!(v["data"][1]["description"], "卡a · 1M");
    assert_eq!(v["data"][3]["description"], "卡b");

    // 转发到卡 a：换回原名、换 Bearer key、去掉 1M beta、保留其余头和 query，流式内容原样返回
    let r = http
        .post(format!("{base}/v1/messages?beta=true"))
        .bearer_auth("tok-9f3a")
        .header("anthropic-beta", "claude-code-20250219,context-1m-2025-08-07,effort-2025-11-24")
        .header("anthropic-version", "2023-06-01")
        .json(&json!({ "model": "cl-claude-a--big", "stream": true }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().unwrap(), "event: message_stop\ndata: {}\n\n");
    {
        let s = seen.lock().unwrap();
        let (_, h, body) = s.last().unwrap();
        assert_eq!(body["model"], "big");
        assert_eq!(body["stream"], true);
        assert_eq!(h["authorization"], "Bearer key-a");
        assert!(h.get("x-api-key").is_none());
        assert_eq!(h["anthropic-beta"], "claude-code-20250219,effort-2025-11-24");
        assert_eq!(h["anthropic-version"], "2023-06-01");
    }

    // 卡 b 用 x-api-key；claude 用 x-api-key 带 token 也认；带 [1m] 的 ID 也能找到
    let r = http
        .post(format!("{base}/v1/messages/count_tokens"))
        .header("x-api-key", "tok-9f3a")
        .json(&json!({ "model": "cl-claude-b--m2[1m]" }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    {
        let s = seen.lock().unwrap();
        let (_, h, body) = s.last().unwrap();
        assert_eq!(body["model"], "m2");
        assert_eq!(h["x-api-key"], "key-b");
        assert!(h.get("authorization").is_none());
    }

    // 未知模型
    let r = http.post(format!("{base}/v1/messages")).bearer_auth("tok-9f3a").json(&json!({ "model": "claude-sonnet-5" })).send().unwrap();
    assert_eq!(r.status(), 404);
    let v: Value = r.json().unwrap();
    assert_eq!(v["type"], "error");

    // 上游报错原样返回，并记进日志
    let r = http.post(format!("{base}/v1/messages")).bearer_auth("tok-9f3a").json(&json!({ "model": "cl-claude-a--bad" })).send().unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.text().unwrap().contains("nope"));
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("proto=anthropic") && text.contains("model=big card=a 200"), "{text}");
    assert!(text.contains("model=bad card=a 400") && text.contains("nope"), "{text}");
    assert!(!text.contains("key-a") && !text.contains("tok-9f3a"), "日志里不能有 key 和 token：{text}");

    // 上游连不上
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let port2 = gateway::start(Config {
        routes: vec![Route { card: card("c", dead, Auth::Bearer, &[("m", 200_000)]), auth: RouteAuth::ApiKey("k".into()), protocol: ApiFormat::Anthropic }],
        token: "tok-9f3a".into(),
        log: None,
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();
    let r = http.post(format!("http://127.0.0.1:{port2}/v1/messages")).bearer_auth("tok-9f3a").json(&json!({ "model": "cl-claude-c--m" })).send().unwrap();
    assert_eq!(r.status(), 502);
    let _ = std::fs::remove_file(&log);
}

#[test]
fn openai_chat_forward() {
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1") };
    assert_eq!(gateway::openai_chat_url("https://api.openai.com"), "https://api.openai.com/v1/chat/completions");
    assert_eq!(gateway::openai_chat_url("https://api.openai.com/v1/"), "https://api.openai.com/v1/chat/completions");
    assert_eq!(
        gateway::openai_chat_url("https://api.openai.com/v1/chat/completions/"),
        "https://api.openai.com/v1/chat/completions"
    );

    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let log = std::env::temp_dir().join(format!("clade-gateway-oa-{}.log", std::process::id()));
    let port = gateway::start(Config {
        routes: vec![
            Route {
                card: openai_card("oa1", format!("http://127.0.0.1:{up}/openai"), &[("gpt-4o", 200_000), ("err-model", 200_000)]),
                auth: RouteAuth::ApiKey("sk-oa-secret".into()),
                protocol: ApiFormat::OpenAiChat,
            },
            Route {
                card: openai_card("oa2", format!("http://127.0.0.1:{up}/openai/v1"), &[("o3-mini", 200_000)]),
                auth: RouteAuth::ApiKey("sk-oa2".into()),
                protocol: ApiFormat::OpenAiChat,
            },
        ],
        token: "tok-oa".into(),
        log: Some(log.clone()),
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();

    // 1. 非流式：路径补 /v1/chat/completions，认证固定 Bearer，去掉 anthropic-* 头，响应转为 Anthropic JSON
    let r = http
        .post(format!("{base}/v1/messages?beta=true"))
        .bearer_auth("tok-oa")
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "interleaved-thinking-2025-05-14")
        .json(&json!({
            "model": "cl-claude-oa1--gpt-4o",
            "system": "sys prompt",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().unwrap();
    assert_eq!(v["type"], "message");
    assert_eq!(v["id"], "msg_chatcmpl-n1");
    assert_eq!(v["model"], "gpt-4o");
    assert_eq!(v["stop_reason"], "end_turn");
    assert_eq!(v["content"], json!([{"type": "text", "text": "hello from openai"}]));
    assert_eq!(v["usage"], json!({"input_tokens": 20, "output_tokens": 5, "cache_read_input_tokens": 4}));
    {
        let s = seen.lock().unwrap();
        let (path, h, body) = s.last().unwrap();
        assert_eq!(path, "/openai/v1/chat/completions");
        assert_eq!(h["authorization"], "Bearer sk-oa-secret");
        assert!(h.get("x-api-key").is_none());
        assert!(h.get("anthropic-version").is_none());
        assert!(h.get("anthropic-beta").is_none());
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"][0], json!({"role": "system", "content": "sys prompt"}));
    }

    // 2. base_url 以 /v1 结尾时不重复补 /v1；o3-mini 用 max_completion_tokens
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-oa")
        .json(&json!({
            "model": "cl-claude-oa2--o3-mini",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 300
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    {
        let s = seen.lock().unwrap();
        let (path, h, body) = s.last().unwrap();
        assert_eq!(path, "/openai/v1/chat/completions");
        assert_eq!(h["authorization"], "Bearer sk-oa2");
        assert_eq!(body["max_completion_tokens"], 300);
        assert!(body.get("max_tokens").is_none());
    }

    // 3. 流式：带 stream_options.include_usage，返回 Anthropic SSE 事件序列
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-oa")
        .json(&json!({
            "model": "cl-claude-oa1--gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().contains("text/event-stream"));
    let sse_text = r.text().unwrap();
    assert!(sse_text.contains("event: message_start"), "{sse_text}");
    assert!(sse_text.contains("\"text\":\"你好\""), "{sse_text}");
    assert!(sse_text.contains("\"type\":\"tool_use\""), "{sse_text}");
    assert!(sse_text.contains("\"type\":\"input_json_delta\""), "{sse_text}");
    assert!(sse_text.contains("event: message_delta"), "{sse_text}");
    assert!(sse_text.contains("\"stop_reason\":\"tool_use\""), "{sse_text}");
    assert!(sse_text.contains("event: message_stop"), "{sse_text}");
    {
        let s = seen.lock().unwrap();
        let (_, _, body) = s.last().unwrap();
        assert_eq!(body["stream_options"], json!({"include_usage": true}));
    }

    // 4. 上游 HTTP 错误：状态码保留，错误体转为 Anthropic 格式
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-oa")
        .json(&json!({
            "model": "cl-claude-oa1--err-model",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 429);
    let err_v: Value = r.json().unwrap();
    assert_eq!(err_v, json!({"type": "error", "error": {"type": "rate_limit_error", "message": "quota exceeded"}}));

    // 日志包含协议和上游路径，不含 key 和 token
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("proto=openai_chat upstream=/openai/v1/chat/completions model=gpt-4o card=oa1 200"), "{text}");
    assert!(text.contains("model=err-model card=oa1 429"), "{text}");
    assert!(!text.contains("sk-oa-secret") && !text.contains("tok-oa"), "日志里不能有 key 和 token：{text}");
    let _ = std::fs::remove_file(&log);
}

#[test]
fn openai_responses_forward() {
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1") };
    assert_eq!(gateway::openai_responses_url("https://api.openai.com"), "https://api.openai.com/v1/responses");
    assert_eq!(gateway::openai_responses_url("https://api.openai.com/v1/"), "https://api.openai.com/v1/responses");
    assert_eq!(gateway::openai_responses_url("https://api.openai.com/v1/responses/"), "https://api.openai.com/v1/responses");

    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let log = std::env::temp_dir().join(format!("clade-gateway-resp-{}.log", std::process::id()));
    let mut rcard = openai_card("resp1", format!("http://127.0.0.1:{up}/openai-resp"), &[("gpt-5.4", 200_000), ("err-resp", 200_000)]);
    rcard.protocol = ApiFormat::OpenAiResponses;
    let port = gateway::start(Config {
        routes: vec![Route { card: rcard, auth: RouteAuth::ApiKey("sk-resp-secret".into()), protocol: ApiFormat::OpenAiResponses }],
        token: "tok-resp".into(),
        log: Some(log.clone()),
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();

    // 1. 非流式
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-resp")
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "cl-claude-resp1--gpt-5.4",
            "system": "be concise",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().unwrap();
    assert_eq!(v["id"], "msg_resp_n1");
    assert_eq!(v["content"], json!([{"type": "text", "text": "hello from responses"}]));
    assert_eq!(v["usage"], json!({"input_tokens": 18, "output_tokens": 6, "cache_read_input_tokens": 3}));
    {
        let s = seen.lock().unwrap();
        let (path, h, body) = s.last().unwrap();
        assert_eq!(path, "/openai-resp/v1/responses");
        assert_eq!(h["authorization"], "Bearer sk-resp-secret");
        assert!(h.get("anthropic-version").is_none());
        assert_eq!(body["instructions"], "be concise");
        assert_eq!(body["max_output_tokens"], 100);
    }

    // 2. 流式 SSE
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-resp")
        .json(&json!({
            "model": "cl-claude-resp1--gpt-5.4",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let sse_text = r.text().unwrap();
    assert!(sse_text.contains("event: message_start"), "{sse_text}");
    assert!(sse_text.contains("\"text\":\"来自Responses\""), "{sse_text}");
    assert!(sse_text.contains("\"type\":\"tool_use\""), "{sse_text}");
    assert!(sse_text.contains("\"stop_reason\":\"tool_use\""), "{sse_text}");
    assert!(sse_text.contains("event: message_stop"), "{sse_text}");

    // 3. 上游错误
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-resp")
        .json(&json!({
            "model": "cl-claude-resp1--err-resp",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 400);
    let err_v: Value = r.json().unwrap();
    assert_eq!(err_v["error"]["message"], "invalid input");

    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("proto=openai_responses upstream=/openai-resp/v1/responses model=gpt-5.4 card=resp1 200"), "{text}");
    assert!(!text.contains("sk-resp-secret") && !text.contains("tok-resp"), "日志里不能有 key 和 token：{text}");
    let _ = std::fs::remove_file(&log);
}

#[test]
fn gemini_native_forward() {
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1") };
    assert_eq!(
        gateway::gemini_url("https://generativelanguage.googleapis.com", "gemini-2.5-pro", false),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
    );
    assert_eq!(
        gateway::gemini_url("https://generativelanguage.googleapis.com/v1beta/", "models/gemini-2.5-pro", true),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
    );

    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let log = std::env::temp_dir().join(format!("clade-gateway-gem-{}.log", std::process::id()));
    let mut gcard = openai_card("gem1", format!("http://127.0.0.1:{up}/gemini"), &[("gemini-2.5-pro", 200_000), ("err-gem", 200_000)]);
    gcard.protocol = ApiFormat::GeminiNative;
    let port = gateway::start(Config {
        routes: vec![Route { card: gcard, auth: RouteAuth::ApiKey("AIza-secret".into()), protocol: ApiFormat::GeminiNative }],
        token: "tok-gem".into(),
        log: Some(log.clone()),
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();

    // 1. 非流式
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-gem")
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "cl-claude-gem1--gemini-2.5-pro",
            "system": "be brief",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 128
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().unwrap();
    assert_eq!(v["id"], "msg_gem-n1");
    assert_eq!(v["content"], json!([{"type": "text", "text": "hello from gemini"}]));
    assert_eq!(v["usage"], json!({"input_tokens": 16, "output_tokens": 5, "cache_read_input_tokens": 2}));
    {
        let s = seen.lock().unwrap();
        let (path, h, body) = s.last().unwrap();
        assert_eq!(path, "/gemini/v1beta/models/gemini-2.5-pro:generateContent");
        assert_eq!(h["x-goog-api-key"], "AIza-secret");
        assert!(h.get("authorization").is_none());
        assert!(h.get("anthropic-version").is_none());
        assert_eq!(body["systemInstruction"], json!({"parts": [{"text": "be brief"}]}));
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 128);
    }

    // 2. 流式 SSE
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-gem")
        .json(&json!({
            "model": "cl-claude-gem1--gemini-2.5-pro",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let sse_text = r.text().unwrap();
    assert!(sse_text.contains("event: message_start"), "{sse_text}");
    assert!(sse_text.contains("\"text\":\"来自Gemini\""), "{sse_text}");
    assert!(sse_text.contains("\"type\":\"tool_use\""), "{sse_text}");
    assert!(sse_text.contains("\"stop_reason\":\"tool_use\""), "{sse_text}");
    assert!(sse_text.contains("event: message_stop"), "{sse_text}");

    // 3. 上游错误
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-gem")
        .json(&json!({
            "model": "cl-claude-gem1--err-gem",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 403);
    let err_v: Value = r.json().unwrap();
    assert_eq!(err_v["error"]["message"], "api key invalid");

    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("proto=gemini_native upstream=/gemini/v1beta/models/gemini-2.5-pro:generateContent model=gemini-2.5-pro card=gem1 200"), "{text}");
    assert!(!text.contains("AIza-secret") && !text.contains("tok-gem"), "日志里不能有 key 和 token：{text}");
    let _ = std::fs::remove_file(&log);
}

#[test]
fn log_in_use() {
    // 同一个 profile 开两个会话：第二个网关的日志换成 -2，不和第一个写到同一个文件里
    let dir = std::env::temp_dir();
    let log = dir.join(format!("clade-gateway-dup-{}.log", std::process::id()));
    let log2 = dir.join(format!("clade-gateway-dup-{}-2.log", std::process::id()));
    let cfg = || Config {
        routes: vec![Route { card: card("a", 1, Auth::Bearer, &[("m", 200_000)]), auth: RouteAuth::ApiKey("k".into()), protocol: ApiFormat::Anthropic }],
        token: "t".into(),
        log: Some(log.clone()),
        proxy: Proxy::Env,
        classifier_prompt: None,
    };
    let _p1 = gateway::start(cfg()).unwrap();
    let p2 = gateway::start(cfg()).unwrap();
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();
    let r = http.get(format!("http://127.0.0.1:{p2}/v1/models")).bearer_auth("wrong").send().unwrap();
    assert_eq!(r.status(), 401);
    assert!(std::fs::read_to_string(&log2).unwrap().contains("401"));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
    // 日志打开期间也能删
    std::fs::remove_file(&log).unwrap();
    std::fs::remove_file(&log2).unwrap();
}

#[test]
fn id_collision() {
    // 卡 a 的 b--x 和卡 a--b 的 x 发布出来是同一个 ID
    let r = gateway::start(Config {
        routes: vec![
            Route { card: card("a", 1, Auth::Bearer, &[("b--x", 200_000)]), auth: RouteAuth::ApiKey("k".into()), protocol: ApiFormat::Anthropic },
            Route { card: card("a--b", 1, Auth::Bearer, &[("x", 200_000)]), auth: RouteAuth::ApiKey("k".into()), protocol: ApiFormat::Anthropic },
        ],
        token: "t".into(),
        log: None,
        proxy: Proxy::Env,
        classifier_prompt: None,
    });
    assert!(r.is_err());
}

#[test]
fn openai_oauth_card_forwards_codex_responses() {
    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let log = std::env::temp_dir().join(format!("clade-gateway-oauth-{}.log", std::process::id()));
    let ocard = Card {
        id: "chatgpt".into(),
        name: "ChatGPT Pro".into(),
        kind: CardKind::OpenAiOAuth,
        protocol: ApiFormat::OpenAiResponses,
        base_url: format!("http://127.0.0.1:{up}/openai-resp/v1/responses"),
        auth: Auth::Bearer,
        model_list: vec![CardModel { id: "gpt-5.4".into(), context: 200_000 }],
        strip_system_prompt: None,
        ..Default::default()
    };
    let creds = OpenAiOAuthCreds {
        access_token: "oa-access-secret".into(),
        refresh_token: "oa-refresh-secret".into(),
        chatgpt_account_id: "acct_xyz".into(),
        email: "dev@example.com".into(),
        plan_type: "plus".into(),
        expires_at_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    let port = gateway::start(Config {
        routes: vec![Route {
            card: ocard,
            auth: RouteAuth::OpenAiOAuth(Arc::new(tokio::sync::Mutex::new(creds))),
            protocol: ApiFormat::OpenAiResponses,
        }],
        token: "tok-oauth".into(),
        log: Some(log.clone()),
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();

    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();

    // 1. 流式请求：验证 Codex OAuth 专用头与请求体约束（store=false, stream=true, 去掉 max_output_tokens/temperature）
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-oauth")
        .json(&json!({
            "model": "cl-claude-chatgpt--gpt-5.4",
            "max_tokens": 128,
            "temperature": 0.5,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let sse = r.text().unwrap();
    assert!(sse.contains("event: message_start"), "{sse}");
    assert!(sse.contains("event: message_stop"), "{sse}");

    // 2. 非流式请求：上游仍收到 stream=true，网关把 SSE 聚合为 Anthropic JSON 返回
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-oauth")
        .json(&json!({
            "model": "cl-claude-chatgpt--gpt-5.4",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hi non-stream"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");

    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 2);
    for (path, headers, req_body) in calls.iter() {
        assert_eq!(path, "/openai-resp/v1/responses");
        assert_eq!(headers.get("authorization").unwrap(), "Bearer oa-access-secret");
        assert_eq!(headers.get("chatgpt-account-id").unwrap(), "acct_xyz");
        assert_eq!(headers.get("originator").unwrap(), "codex_cli_rs");
        assert!(headers.get("version").is_some());
        assert_eq!(req_body["store"], false);
        assert_eq!(req_body["stream"], true);
        assert!(req_body.get("max_output_tokens").is_none());
        assert!(req_body.get("temperature").is_none());
        assert!(req_body.get("instructions").is_some());
        assert!(req_body.get("tools").is_some());
        assert_eq!(req_body["parallel_tool_calls"], true);
        assert!(req_body["include"].as_array().unwrap().iter().any(|v| v == "reasoning.encrypted_content"));
    }

    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("proto=openai_oauth"), "{text}");
    assert!(!text.contains("oa-access-secret") && !text.contains("tok-oauth"), "日志里不能有 token：{text}");
    let _ = std::fs::remove_file(&log);
}

#[test]
fn antigravity_card_forwards_code_assist() {
    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let log = std::env::temp_dir().join(format!("clade-gateway-agy-{}.log", std::process::id()));
    let acard = Card {
        id: "agy-login".into(),
        name: "Antigravity".into(),
        kind: CardKind::Antigravity,
        protocol: ApiFormat::GeminiNative,
        base_url: format!("http://127.0.0.1:{up}/codeassist"),
        auth: Auth::Bearer,
        model_list: vec![CardModel { id: "gemini-2.5-pro".into(), context: 1_000_000 }],
        strip_system_prompt: None,
        ..Default::default()
    };
    let creds = AntigravityCreds {
        access_token: "agy-access-secret".into(),
        refresh_token: "agy-refresh-secret".into(),
        email: "dev@gmail.com".into(),
        project_id: "proj-abc".into(),
        plan: "AI Pro".into(),
        expires_at_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    let port = gateway::start(Config {
        routes: vec![Route {
            card: acard,
            auth: RouteAuth::Antigravity(Arc::new(tokio::sync::Mutex::new(creds))),
            protocol: ApiFormat::GeminiNative,
        }],
        token: "tok-agy".into(),
        log: Some(log.clone()),
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();

    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();

    // 1. 流式请求
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-agy")
        .json(&json!({
            "model": "cl-claude-agy-login--gemini-2.5-pro[1m]",
            "max_tokens": 128,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let sse = r.text().unwrap();
    assert!(sse.contains("来自CodeAssist"), "{sse}");
    assert!(sse.contains("event: message_stop"), "{sse}");

    // 2. 非流式请求（上游统一走 streamGenerateContent，由网关聚合 SSE 返回 Anthropic JSON）
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-agy")
        .json(&json!({
            "model": "cl-claude-agy-login--gemini-2.5-pro",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hi non-stream"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().unwrap();
    assert_eq!(body["content"][0]["text"], "来自CodeAssist");

    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "/codeassist/v1internal:streamGenerateContent");
    assert_eq!(calls[1].0, "/codeassist/v1internal:streamGenerateContent");
    assert_eq!(
        calls[0].1.get("x-aicode-trajectory-id").unwrap(),
        calls[1].1.get("x-aicode-trajectory-id").unwrap(),
        "同一会话应复用稳定的 X-Aicode-Trajectory-Id"
    );
    for (_, headers, req_body) in calls.iter() {
        assert_eq!(headers.get("authorization").unwrap(), "Bearer agy-access-secret");
        assert!(headers.get("user-agent").unwrap().to_str().unwrap().starts_with("antigravity/cli/"));
        assert!(headers.get("x-aicode-request-id").is_some());
        assert_eq!(req_body["model"], "gemini-2.5-pro");
        assert_eq!(req_body["project"], "proj-abc");
        assert_eq!(req_body["userAgent"], "antigravity");
        assert_eq!(req_body["requestType"], "agent");
        assert!(req_body["requestId"].as_str().unwrap().starts_with("agent-"));
        assert!(req_body["request"]["sessionId"].as_str().unwrap().starts_with('-'));
        assert!(req_body["request"]["contents"].is_array());
        assert_eq!(req_body["request"]["toolConfig"]["functionCallingConfig"]["mode"], "VALIDATED");
    }

    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("proto=antigravity"), "{text}");
    assert!(!text.contains("agy-access-secret") && !text.contains("tok-agy"), "日志里不能有 token：{text}");
    let _ = std::fs::remove_file(&log);
}

#[test]
fn antigravity_slow_stream_opens_early_thinking() {
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1") };
    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let acard = Card {
        id: "agy-slow".into(),
        name: "Antigravity".into(),
        kind: CardKind::Antigravity,
        protocol: ApiFormat::GeminiNative,
        base_url: format!("http://127.0.0.1:{up}/codeassist"),
        auth: Auth::Bearer,
        model_list: vec![
            CardModel { id: "slow-think".into(), context: 1_000_000 },
            CardModel { id: "slow-err".into(), context: 1_000_000 },
        ],
        strip_system_prompt: None,
        ..Default::default()
    };
    let creds = AntigravityCreds {
        access_token: "agy-access-secret".into(),
        refresh_token: "agy-refresh-secret".into(),
        email: "dev@gmail.com".into(),
        project_id: "proj-abc".into(),
        plan: "AI Pro".into(),
        expires_at_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    let port = gateway::start(Config {
        routes: vec![Route {
            card: acard,
            auth: RouteAuth::Antigravity(Arc::new(tokio::sync::Mutex::new(creds))),
            protocol: ApiFormat::GeminiNative,
        }],
        token: "tok-slow".into(),
        log: None,
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();

    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();

    // 1. 上游延迟 > 2s：客户端应在 ~2s 收到 200 SSE + thinking block，后续无缝接上 thought 与 text
    let t0 = std::time::Instant::now();
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-slow")
        .json(&json!({
            "model": "cl-claude-agy-slow--slow-think[1m]",
            "max_tokens": 128,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .unwrap();
    let headers_ms = t0.elapsed().as_millis();
    assert_eq!(r.status(), 200);
    assert!(headers_ms < 2350, "应在抢先阈值附近返回响应头，实际耗时 {headers_ms}ms");
    let sse = r.text().unwrap();
    assert_eq!(sse.matches("event: message_start").count(), 1, "不能重复发 message_start：{sse}");
    assert_eq!(sse.matches("\"type\":\"thinking\"").count(), 1, "只应开一个 thinking 块：{sse}");
    assert!(sse.contains("\"type\":\"thinking_delta\""), "{sse}");
    assert!(sse.contains("深入思考"), "{sse}");
    assert!(sse.contains("慢思考完成"), "{sse}");
    assert!(sse.contains("event: message_stop"), "{sse}");

    // 2. 抢先开启 SSE 后上游报错：通过 SSE error 事件透出错误信息
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-slow")
        .json(&json!({
            "model": "cl-claude-agy-slow--slow-err[1m]",
            "max_tokens": 128,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let sse = r.text().unwrap();
    assert!(sse.contains("event: error"), "{sse}");
    assert!(sse.contains("rate_limit_error"), "{sse}");
    assert!(sse.contains("slow rate limit"), "{sse}");
}

#[test]
fn antigravity_effort_rewrites_tiered_model_suffix() {
    assert_eq!(gateway::apply_antigravity_effort("gemini-4-neon-low", Some("high")), "gemini-4-neon-high");
    assert_eq!(gateway::apply_antigravity_effort("gemini-4-neon-low", Some("max")), "gemini-4-neon-xhigh");
    assert_eq!(gateway::apply_antigravity_effort("claude-opus-5-5-medium", Some("max")), "claude-opus-5-5-high");
    assert_eq!(gateway::apply_antigravity_effort("gemini-3.8-flash-high", Some("low")), "gemini-3.8-flash-low");
    assert_eq!(gateway::apply_antigravity_effort("gemini-2.5-pro", Some("high")), "gemini-2.5-pro");
    assert_eq!(gateway::apply_antigravity_effort("gemini-4-neon-low", None), "gemini-4-neon-low");
}

#[test]
fn strip_system_prompt_keeps_only_scratchpad() {
    let scratchpad = "Scratchpad directory: C:\\Temp\\claude\\abc\\sess\\scratchpad\n\
        Always use this directory for temporary files (intermediate results, scripts, outputs that don't belong in the project) \
        instead of /tmp or other system temp directories. It is session-specific and isolated from the project. \
        Only use /tmp if the user explicitly asks.";

    // 1. 数组格式（Claude Code 主提示词块 + scratchpad 块）
    let mut v = json!({
        "model": "x",
        "system": [
            { "type": "text", "text": "You are Claude Code, Anthropic's official CLI." },
            { "type": "text", "text": scratchpad }
        ]
    });
    gateway::strip_system_prompt_keep_scratchpad(&mut v);
    assert_eq!(v["system"], scratchpad);

    // 2. 字符串格式（主提示词与 scratchpad 拼在同一个字符串里）
    let mut v2 = json!({
        "model": "x",
        "system": format!("You are Claude Code.\n\n{scratchpad}\n\nExtra trailing stuff")
    });
    gateway::strip_system_prompt_keep_scratchpad(&mut v2);
    assert_eq!(v2["system"], scratchpad);

    // 3. 不含 scratchpad 时完全移除 system
    let mut v3 = json!({
        "model": "x",
        "system": [{ "type": "text", "text": "You are Claude Code." }]
    });
    gateway::strip_system_prompt_keep_scratchpad(&mut v3);
    assert!(v3.get("system").is_none());
}

#[test]
fn classifier_prompt_rewrite_unit_and_e2e() {
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1") };

    // 1. 单元测试：识别分类器请求、保留计费头和 cache_control、丢弃 14 万字符默认模板和附加块
    let mut cls_req = json!({
        "model": "m",
        "max_tokens": 64,
        "stop_sequences": ["</block>"],
        "system": [
            { "type": "text", "text": "x-anthropic-billing-header: cc_version=2.1.330;" },
            {
                "type": "text",
                "text": "You are a security monitor for autonomous AI coding agents.\n<cc_automode_permissions>\n...huge...\n</cc_automode_permissions>",
                "cache_control": { "type": "ephemeral", "ttl": "1h" }
            },
            { "type": "text", "text": "\n\n## Session Context\n- extra" }
        ],
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "<transcript>\nuser: ls\n</transcript>\n" }] }
        ]
    });
    assert!(gateway::is_auto_mode_classifier_request(&cls_req));
    assert!(gateway::rewrite_classifier_system_prompt(&mut cls_req, "SHORT CLASSIFIER PROMPT"));
    assert_eq!(
        cls_req["system"],
        json!([
            { "type": "text", "text": "x-anthropic-billing-header: cc_version=2.1.330;" },
            {
                "type": "text",
                "text": "SHORT CLASSIFIER PROMPT",
                "cache_control": { "type": "ephemeral", "ttl": "1h" }
            }
        ])
    );

    // 2. 普通主对话（带 tools，或无分类器标记）不误伤
    let mut normal_with_tools = json!({
        "model": "m",
        "tools": [{ "name": "Bash", "input_schema": { "type": "object" } }],
        "system": [{ "type": "text", "text": "You are a security monitor for autonomous AI coding agents. <cc_automode_permissions>" }],
        "messages": [{ "role": "user", "content": "<transcript>hi</transcript>" }]
    });
    assert!(!gateway::is_auto_mode_classifier_request(&normal_with_tools));
    assert!(!gateway::rewrite_classifier_system_prompt(&mut normal_with_tools, "SHORT"));

    let mut normal_chat = json!({
        "model": "m",
        "system": [{ "type": "text", "text": "You are Claude Code." }],
        "messages": [{ "role": "user", "content": "hello" }]
    });
    assert!(!gateway::rewrite_classifier_system_prompt(&mut normal_chat, "SHORT"));

    // 3. 端到端：即使卡开启了 strip_system_prompt（如 Antigravity 默认行为），分类器自定义提示词仍应保留并送达上游
    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let mut c = card("a", up, Auth::Bearer, &[("m1", 200_000)]);
    c.strip_system_prompt = Some(true);
    let port = gateway::start(Config {
        routes: vec![Route { card: c, auth: RouteAuth::ApiKey("key-a".into()), protocol: ApiFormat::Anthropic }],
        token: "tok-cls".into(),
        log: None,
        proxy: Proxy::Env,
        classifier_prompt: Some("CUSTOM MINI CLASSIFIER".into()),
    })
    .unwrap();
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    // 分类器请求 -> 替换为自定义提示词，不被 strip_system_prompt 剥掉
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-cls")
        .json(&json!({
            "model": "cl-claude-a--m1",
            "system": [
                { "type": "text", "text": "You are a security monitor for autonomous AI coding agents.\n<cc_automode_permissions>rules</cc_automode_permissions>" }
            ],
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "<transcript>\nrun cargo test\n</transcript>" }] }]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    {
        let s = seen.lock().unwrap();
        let (_, _, body) = s.last().unwrap();
        assert_eq!(body["system"], json!([{ "type": "text", "text": "CUSTOM MINI CLASSIFIER" }]));
    }

    // 普通对话 -> 仍按卡的 strip_system_prompt 剥离
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-cls")
        .json(&json!({
            "model": "cl-claude-a--m1",
            "system": [{ "type": "text", "text": "You are Claude Code." }],
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    {
        let s = seen.lock().unwrap();
        let (_, _, body) = s.last().unwrap();
        assert!(body.get("system").is_none());
    }
}

#[test]
fn system_prompt_and_reminder_policies() {
    use clade_lib::core::PromptPolicy;

    let scratchpad = "Scratchpad directory: C:\\Temp\\claude\\abc\\sess\\scratchpad\n\
        Always use this directory for temporary files (intermediate results, scripts, outputs that don't belong in the project) \
        instead of /tmp or other system temp directories. It is session-specific and isolated from the project. \
        Only use /tmp if the user explicitly asks.";
    let full_sys = format!(
        "You are Claude Code.\n\n# Harness\n- rule\n\n# Memory\n\nYou have a persistent file-based memory at `C:\\mem`.\nSave facts here.\n\n# Environment\n- Claude 5\n\n{scratchpad}"
    );

    // 1. Strip + preserve_memory=true => 保留 # Memory 与 scratchpad
    let mut v1 = json!({ "model": "m", "system": [{ "type": "text", "text": full_sys }] });
    gateway::apply_system_prompt_policy(&mut v1, &PromptPolicy::Strip, true);
    let s1 = v1["system"].as_str().unwrap();
    assert!(s1.starts_with("# Memory\n\nYou have a persistent file-based memory"));
    assert!(!s1.contains("# Environment"));
    assert!(!s1.contains("You are Claude Code."));
    assert!(s1.ends_with(scratchpad));

    // 2. Custom + preserve_memory=true => 自定义文本 + # Memory + scratchpad
    let mut v2 = json!({ "model": "m", "system": [{ "type": "text", "text": full_sys }] });
    gateway::apply_system_prompt_policy(&mut v2, &PromptPolicy::Custom { text: "Be concise.".into() }, true);
    let s2 = v2["system"].as_str().unwrap();
    assert!(s2.starts_with("Be concise.\n\n# Memory\n"));
    assert!(s2.ends_with(scratchpad));

    // 3. Custom + preserve_memory=false => 自定义文本 + scratchpad（不含 # Memory）
    let mut v3 = json!({ "model": "m", "system": [{ "type": "text", "text": full_sys }] });
    gateway::apply_system_prompt_policy(&mut v3, &PromptPolicy::Custom { text: "Only custom.".into() }, false);
    assert_eq!(v3["system"], format!("Only custom.\n\n{scratchpad}"));

    // 4. system-reminder 策略：Strip 移除 <system-reminder> 块和 role=system 消息，保留用户原文
    let sample_msgs = || {
        json!({
            "model": "m",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "<system-reminder>\nAttribution...\n</system-reminder>" },
                        { "type": "text", "text": "prefix <system-reminder>inline</system-reminder> real question" }
                    ]
                },
                {
                    "role": "system",
                    "content": [{ "type": "text", "text": "# Environment\nDeferred tools..." }]
                },
                {
                    "role": "assistant",
                    "content": [{ "type": "text", "text": "answer" }]
                }
            ]
        })
    };

    let mut m_strip = sample_msgs();
    gateway::apply_system_reminder_policy(&mut m_strip, &PromptPolicy::Strip);
    assert_eq!(
        m_strip["messages"],
        json!([
            { "role": "user", "content": [{ "type": "text", "text": "prefix  real question" }] },
            { "role": "assistant", "content": [{ "type": "text", "text": "answer" }] }
        ])
    );

    // 5. system-reminder 策略：Custom 剥离原有块后在首条 user 消息前注入自定义 <system-reminder>
    let mut m_custom = sample_msgs();
    gateway::apply_system_reminder_policy(&mut m_custom, &PromptPolicy::Custom { text: "Always reply in Chinese.".into() });
    assert_eq!(
        m_custom["messages"],
        json!([
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "<system-reminder>\nAlways reply in Chinese.\n</system-reminder>" },
                    { "type": "text", "text": "prefix  real question" }
                ]
            },
            { "role": "assistant", "content": [{ "type": "text", "text": "answer" }] }
        ])
    );

    // 6. 端到端：验证自定义 system 与自定义 system-reminder 经网关转发到上游，且分类器请求不受主对话策略影响
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1") };
    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());
    let mut c = card("p", up, Auth::Bearer, &[("m1", 200_000)]);
    c.system_prompt_policy = Some(PromptPolicy::Custom { text: "MY SYS".into() });
    c.preserve_memory_prompt = Some(true);
    c.system_reminder_policy = Some(PromptPolicy::Strip);
    let port = gateway::start(Config {
        routes: vec![Route { card: c, auth: RouteAuth::ApiKey("k".into()), protocol: ApiFormat::Anthropic }],
        token: "tok-pol".into(),
        log: None,
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    // 主对话：应用 system 与 reminder 策略
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-pol")
        .json(&json!({
            "model": "cl-claude-p--m1",
            "system": [{ "type": "text", "text": "You are Claude Code.\n\n# Memory\nmem body\n\n# Environment\nenv" }],
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "<system-reminder>git</system-reminder>" }, { "type": "text", "text": "hello" }] },
                { "role": "system", "content": [{ "type": "text", "text": "# Environment\ntools" }] }
            ]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    {
        let s = seen.lock().unwrap();
        let (_, _, body) = s.last().unwrap();
        assert_eq!(body["system"], "MY SYS\n\n# Memory\nmem body");
        assert_eq!(body["messages"], json!([{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }]));
    }

    // 分类器请求（未配置自定义 classifier_prompt）：不被主对话的 MY SYS 覆盖
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-pol")
        .json(&json!({
            "model": "cl-claude-p--m1",
            "system": [{ "type": "text", "text": "You are a security monitor for autonomous AI coding agents.\n<cc_automode_permissions>rules</cc_automode_permissions>" }],
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "<transcript>\nrun ls\n</transcript>" }] }]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    {
        let s = seen.lock().unwrap();
        let (_, _, body) = s.last().unwrap();
        assert!(body["system"][0]["text"].as_str().unwrap().contains("<cc_automode_permissions>"));
    }
}

#[test]
fn web_search_e2e_antigravity_and_codex_oauth() {
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1") };
    let seen: Seen = Default::default();
    let up = fake_upstream(seen.clone());

    let agy_card = Card {
        id: "agy-ws".into(),
        name: "AgyWS".into(),
        kind: CardKind::Antigravity,
        protocol: ApiFormat::Anthropic,
        base_url: format!("http://127.0.0.1:{up}/codeassist"),
        auth: Auth::Bearer,
        model_list: vec![CardModel { id: "ws-search".into(), context: 1_000_000 }],
        ..Default::default()
    };
    let agy_creds = AntigravityCreds {
        access_token: "ya29.ws".into(),
        refresh_token: String::new(),
        email: "u@example.com".into(),
        project_id: "proj-ws".into(),
        plan: "PRO".into(),
        expires_at_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
        updated_at_ms: 0,
    };

    let oa_card = Card {
        id: "oa-ws".into(),
        name: "OaWS".into(),
        kind: CardKind::OpenAiOAuth,
        protocol: ApiFormat::Anthropic,
        base_url: format!("http://127.0.0.1:{up}/openai-resp/v1/responses"),
        auth: Auth::Bearer,
        model_list: vec![CardModel { id: "ws-resp".into(), context: 200_000 }],
        ..Default::default()
    };
    let oa_creds = OpenAiOAuthCreds {
        access_token: "at-ws".into(),
        refresh_token: String::new(),
        chatgpt_account_id: "acct-ws".into(),
        email: "u@example.com".into(),
        plan_type: "plus".into(),
        expires_at_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
        updated_at_ms: 0,
    };

    let port = gateway::start(Config {
        routes: vec![
            Route {
                card: agy_card,
                auth: RouteAuth::Antigravity(Arc::new(tokio::sync::Mutex::new(agy_creds))),
                protocol: ApiFormat::Anthropic,
            },
            Route {
                card: oa_card,
                auth: RouteAuth::OpenAiOAuth(Arc::new(tokio::sync::Mutex::new(oa_creds))),
                protocol: ApiFormat::Anthropic,
            },
        ],
        token: "tok-ws".into(),
        log: None,
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();

    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");

    // 1. Antigravity 流式 WebSearch：发 googleSearch，收回 server_tool_use + web_search_tool_result
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-ws")
        .json(&json!({
            "model": "cl-claude-agy-ws--ws-search[1m]",
            "max_tokens": 128,
            "stream": true,
            "tools": [{"type": "web_search_20250305", "name": "web_search"}],
            "tool_choice": {"type": "tool", "name": "web_search"},
            "messages": [{"role": "user", "content": "latest rust"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let sse = r.text().unwrap();
    assert!(sse.contains("\"type\":\"server_tool_use\""), "{sse}");
    assert!(sse.contains("\"type\":\"web_search_tool_result\""), "{sse}");
    assert!(sse.contains("https://example.com/rust"), "{sse}");
    assert!(sse.contains("最新 Rust 为 1.100.0"), "{sse}");
    assert!(sse.contains("\"web_search_requests\":1"), "{sse}");

    // 2. OpenAI Codex OAuth 流式 WebSearch：发 web_search + external_web_access + include，收回结果块
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("tok-ws")
        .json(&json!({
            "model": "cl-claude-oa-ws--ws-resp",
            "max_tokens": 128,
            "stream": true,
            "tools": [{"type": "web_search_20250305", "name": "web_search"}],
            "tool_choice": {"type": "tool", "name": "web_search"},
            "messages": [{"role": "user", "content": "latest rust"}]
        }))
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    let sse = r.text().unwrap();
    assert!(sse.contains("\"type\":\"server_tool_use\""), "{sse}");
    assert!(sse.contains("\"type\":\"web_search_tool_result\""), "{sse}");
    assert!(sse.contains("https://rust-lang.org/"), "{sse}");
    assert!(sse.contains("Rust 1.100.0"), "{sse}");

    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].2["request"]["tools"], json!([{ "googleSearch": {} }]));
    assert_eq!(calls[1].2["tools"][0]["type"], "web_search");
    assert_eq!(calls[1].2["tools"][0]["external_web_access"], true);
    assert_eq!(calls[1].2["tool_choice"], "required");
}

/// 实机联调：使用 ~/.clade 里已登录的 OpenAI 登录卡测试状态、拉取模型、查额度、流式/非流式/工具调用转发。
#[test]
#[ignore]
fn live_openai_oauth_e2e() {
    let store = clade_lib::core::load().unwrap();
    let card = store
        .cards
        .iter()
        .find(|c| c.kind == CardKind::OpenAiOAuth)
        .expect("clade.json 里没有 OpenAI 登录卡")
        .clone();
    let model = card
        .model_list
        .first()
        .map(|m| m.id.clone())
        .unwrap_or_else(|| "gpt-5.4".into());

    // 1. 登录状态
    let st = clade_lib::core::card_auth_status(&card.id).unwrap();
    println!("[status] logged_in={} email={} org={} plan={}", st.logged_in, st.email, st.org_name, st.subscription_type);
    assert!(st.logged_in, "OpenAI 登录卡 {} 未登录", card.id);

    // 2. 拉取模型列表
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let models = rt
        .block_on(clade_lib::openai_oauth::fetch_models(&card.id, &store.global))
        .unwrap();
    println!("[models] count={} sample={:?}", models.len(), &models[..models.len().min(8)]);
    assert!(!models.is_empty(), "没有拉到模型");

    // 3. 查询额度
    let usage = clade_lib::core::usage(&card.id).unwrap();
    println!("[usage] windows={:?}", usage.windows);
    assert!(!usage.windows.is_empty(), "没有查到额度");

    // 4. 启动本地网关，走真实 ChatGPT Codex 后端
    let creds = clade_lib::openai_oauth::load_creds(&card.id).unwrap().unwrap();
    let mut test_card = card.clone();
    if test_card.model_list.is_empty() {
        test_card.model_list.push(CardModel { id: model.clone(), context: 200_000 });
    }
    let published = gateway::model_id(&test_card.id, &test_card.model_list[0]);
    let port = gateway::start(Config {
        routes: vec![Route {
            card: test_card,
            auth: RouteAuth::OpenAiOAuth(Arc::new(tokio::sync::Mutex::new(creds))),
            protocol: ApiFormat::OpenAiResponses,
        }],
        token: "live-tok".into(),
        log: None,
        proxy: Proxy::Env,
        classifier_prompt: None,
    })
    .unwrap();

    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();

    // 4a. 流式对话
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("live-tok")
        .json(&json!({
            "model": published,
            "max_tokens": 64,
            "stream": true,
            "system": "You are a concise assistant.",
            "messages": [{"role": "user", "content": "Reply with exactly: OK"}]
        }))
        .send()
        .unwrap();
    let status = r.status();
    let sse = r.text().unwrap();
    println!("[stream] status={status} len={}", sse.len());
    assert_eq!(status, 200, "stream failed: {sse}");
    assert!(sse.contains("event: message_start"), "{sse}");
    assert!(sse.contains("event: content_block_delta"), "{sse}");
    assert!(sse.contains("event: message_stop"), "{sse}");

    // 4b. 非流式对话（验证 SSE 聚合为 Anthropic JSON）
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("live-tok")
        .json(&json!({
            "model": published,
            "max_tokens": 64,
            "stream": false,
            "system": "You are a concise assistant.",
            "messages": [{"role": "user", "content": "Reply with exactly: PONG"}]
        }))
        .send()
        .unwrap();
    let status = r.status();
    let body_text = r.text().unwrap();
    println!("[non-stream] status={status} body={body_text}");
    assert_eq!(status, 200, "non-stream failed: {body_text}");
    let body: Value = serde_json::from_str(&body_text).unwrap();
    assert_eq!(body["type"], "message");
    assert!(!body["content"].as_array().unwrap().is_empty());

    // 4c. 工具调用
    let r = http
        .post(format!("{base}/v1/messages"))
        .bearer_auth("live-tok")
        .json(&json!({
            "model": published,
            "max_tokens": 128,
            "stream": true,
            "system": "Always call the get_weather tool when asked about weather.",
            "tools": [{
                "name": "get_weather",
                "description": "Get weather for a city",
                "input_schema": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }
            }],
            "messages": [{"role": "user", "content": "What is the weather in Tokyo?"}]
        }))
        .send()
        .unwrap();
    let status = r.status();
    let tool_sse = r.text().unwrap();
    println!("[tool] status={status} has_tool_use={}", tool_sse.contains("\"type\":\"tool_use\""));
    assert_eq!(status, 200, "tool failed: {tool_sse}");
    assert!(tool_sse.contains("\"type\":\"tool_use\""), "{tool_sse}");
    assert!(tool_sse.contains("get_weather"), "{tool_sse}");
}
