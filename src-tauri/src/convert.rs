//! Anthropic Messages ↔ OpenAI Chat Completions / OpenAI Responses / Gemini Native 协议转换模块。
//!
//! 只负责结构与流式状态机转换，不涉及网络与认证。

use anyhow::{bail, Result};
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// 判断是否为 Anthropic 托管的 WebSearch 服务端工具（如 `web_search_20250305`）。
pub fn is_anthropic_web_search_tool(tool: &Value) -> bool {
    tool.get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t == "web_search" || t.starts_with("web_search_"))
}

/// 从 Anthropic 请求体中提取托管 WebSearch 工具的名称（优先匹配被 `tool_choice` 指定的名称）。
pub fn anthropic_web_search_tool_name(body: &Value) -> Option<&str> {
    let tools = body.get("tools").and_then(Value::as_array)?;
    let forced = body
        .get("tool_choice")
        .and_then(Value::as_object)
        .filter(|tc| tc.get("type").and_then(Value::as_str) == Some("tool"))
        .and_then(|tc| tc.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if let Some(name) = forced
        && tools
            .iter()
            .any(|t| is_anthropic_web_search_tool(t) && t.get("name").and_then(Value::as_str) == Some(name))
    {
        return Some(name);
    }
    tools
        .iter()
        .find(|t| is_anthropic_web_search_tool(t))
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn make_web_search_result(url: &str, title: Option<&str>, page_age: Option<&Value>) -> Value {
    let title = title.filter(|t| !t.is_empty()).unwrap_or(url);
    let page_age = page_age.filter(|v| v.is_string()).cloned().unwrap_or(Value::Null);
    json!({
        "type": "web_search_result",
        "url": url,
        "title": title,
        "encrypted_content": "",
        "page_age": page_age,
    })
}

fn merge_web_search_results(target: &mut Vec<Value>, incoming: Vec<Value>) {
    for item in incoming {
        let Some(url) = item.get("url").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            continue;
        };
        if let Some(existing) = target
            .iter_mut()
            .find(|r| r.get("url").and_then(Value::as_str) == Some(url))
        {
            let cur_title = existing.get("title").and_then(Value::as_str).unwrap_or("");
            if (cur_title.is_empty() || cur_title == url)
                && let Some(new_title) = item.get("title").and_then(Value::as_str).filter(|t| !t.is_empty() && *t != url)
            {
                existing["title"] = json!(new_title);
            }
            if existing.get("page_age").is_none_or(Value::is_null)
                && let Some(age) = item.get("page_age").filter(|v| v.is_string())
            {
                existing["page_age"] = age.clone();
            }
        } else {
            target.push(item);
        }
    }
}

fn attach_web_search_usage(usage: &mut Value, web_search_requests: u64) {
    if web_search_requests > 0
        && let Some(obj) = usage.as_object_mut()
    {
        obj.insert(
            "server_tool_use".into(),
            json!({ "web_search_requests": web_search_requests }),
        );
    }
}

/// 将历史消息中的 `web_search_tool_result` 块转成简洁文本，供不支持原生回放的上游协议使用。
fn summarize_web_search_result_block(block: &Value) -> String {
    match block.get("content") {
        Some(Value::Array(items)) => {
            let lines: Vec<String> = items
                .iter()
                .filter_map(|it| {
                    let url = it.get("url").and_then(Value::as_str)?;
                    let title = it.get("title").and_then(Value::as_str).unwrap_or(url);
                    Some(format!("- {title}: {url}"))
                })
                .collect();
            if lines.is_empty() {
                String::new()
            } else {
                format!("[Web search results]\n{}", lines.join("\n"))
            }
        }
        Some(Value::Object(err)) => {
            let code = err.get("error_code").and_then(Value::as_str).unwrap_or("error");
            format!("[Web search error: {code}]")
        }
        _ => String::new(),
    }
}

/// 判断模型名是否属于 OpenAI o1/o3/o4 推理系列（需使用 max_completion_tokens）。
fn is_o_series(model: &str) -> bool {
    let name = model.rsplit('/').next().unwrap_or(model).to_ascii_lowercase();
    ["o1", "o3", "o4"].iter().any(|p| {
        name == *p || name.strip_prefix(p).is_some_and(|rest| rest.starts_with('-') || rest.starts_with('.'))
    })
}

/// 规范化 Anthropic 消息 ID，确保以 msg_ 开头。
fn ensure_msg_id(raw: &str) -> String {
    if raw.is_empty() {
        format!("msg_{}", uuid::Uuid::new_v4().simple())
    } else if raw.starts_with("msg_") {
        raw.to_string()
    } else {
        format!("msg_{raw}")
    }
}

/// 映射 OpenAI finish_reason → Anthropic stop_reason。
pub fn map_stop_reason(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("stop") => "end_turn",
        Some("length") => "max_tokens",
        Some("tool_calls" | "function_call") => "tool_use",
        Some("content_filter") => "end_turn",
        _ => "end_turn",
    }
}

/// 映射 OpenAI usage 对象 → Anthropic usage 对象。
/// OpenAI 的 `prompt_tokens` 包含 `cached_tokens`，而 Anthropic 的 `input_tokens` 不含 `cache_read_input_tokens`。
fn map_usage(usage: Option<&Value>) -> Value {
    let prompt_tokens = usage.and_then(|u| u.get("prompt_tokens")).and_then(Value::as_u64).unwrap_or(0);
    let output_tokens = usage.and_then(|u| u.get("completion_tokens")).and_then(Value::as_u64).unwrap_or(0);
    let cache_read = usage
        .and_then(|u| u.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": prompt_tokens.saturating_sub(cache_read),
        "output_tokens": output_tokens,
        "cache_read_input_tokens": cache_read,
    })
}

/// 从 Anthropic 请求体提取 Claude Code 内置的 effort 档位（`output_config.effort` 或 `thinking.budget_tokens`）。
/// `thinking.type == "disabled"` 时返回 None。
pub fn anthropic_effort(body: &Value) -> Option<&'static str> {
    if body.pointer("/thinking/type").and_then(Value::as_str) == Some("disabled") {
        return None;
    }
    if let Some(e) = body.pointer("/output_config/effort").and_then(Value::as_str) {
        return match e.trim().to_ascii_lowercase().as_str() {
            "low" => Some("low"),
            "medium" => Some("medium"),
            "high" => Some("high"),
            "xhigh" => Some("xhigh"),
            "max" => Some("max"),
            _ => None,
        };
    }
    if body.pointer("/thinking/type").and_then(Value::as_str) == Some("enabled")
        && let Some(budget) = body.pointer("/thinking/budget_tokens").and_then(Value::as_u64)
    {
        return Some(if budget <= 2048 {
            "low"
        } else if budget <= 8192 {
            "medium"
        } else {
            "high"
        });
    }
    None
}

fn supports_openai_reasoning_effort(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model).trim().to_ascii_lowercase();
    is_o_series(&m) || m.starts_with("gpt-5") || m.starts_with("gpt-6") || m.contains("codex")
}

/// OpenAI Chat Completions 用的 reasoning_effort（标准三档 low / medium / high）
fn openai_chat_effort(effort: &str) -> &'static str {
    match effort {
        "low" => "low",
        "medium" => "medium",
        _ => "high",
    }
}

/// OpenAI Responses / Codex OAuth 用的 reasoning.effort（GPT-5+/6+ 支持 xhigh）
fn openai_responses_effort(model: &str, effort: &str) -> &'static str {
    let m = model.rsplit('/').next().unwrap_or(model).trim().to_ascii_lowercase();
    let supports_xhigh = m.starts_with("gpt-5") || m.starts_with("gpt-6") || m.contains("codex");
    match effort {
        "low" => "low",
        "medium" => "medium",
        "xhigh" | "max" if supports_xhigh => "xhigh",
        _ => "high",
    }
}

/// 清理工具参数的 JSON Schema：根上补 "type": "object"，去掉 $schema，递归去掉 cache_control。
fn clean_schema(mut schema: Value, is_root: bool) -> Value {
    match &mut schema {
        Value::Object(obj) => {
            obj.remove("$schema");
            obj.remove("cache_control");
            if is_root && !obj.contains_key("type") {
                obj.insert("type".into(), json!("object"));
            }
            for v in obj.values_mut() {
                let taken = std::mem::replace(v, Value::Null);
                *v = clean_schema(taken, false);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                let taken = std::mem::replace(v, Value::Null);
                *v = clean_schema(taken, false);
            }
        }
        _ => {}
    }
    schema
}

/// 转换 Anthropic image 内容块 → OpenAI image_url 内容块。
fn convert_image_block(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let source_type = source.get("type").and_then(Value::as_str)?;
    let url = match source_type {
        "base64" => {
            let media_type = source.get("media_type").and_then(Value::as_str).unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            format!("data:{media_type};base64,{data}")
        }
        "url" => source.get("url").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    Some(json!({
        "type": "image_url",
        "image_url": { "url": url }
    }))
}

/// 将 tool_result 的 content 字段转为文本或 JSON 字符串。
fn tool_result_content_to_string(content: Option<&Value>) -> String {
    match content {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            if !arr.is_empty() && arr.iter().all(|b| b.get("type").and_then(Value::as_str) == Some("text")) {
                arr.iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                serde_json::to_string(arr).unwrap_or_default()
            }
        }
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// 拆分 tool_result 的 content：遇到 Anthropic image 块时提取出来由调用方转成对应协议的图片结构，
/// 避免整段 base64 被当成纯文本 JSON 序列化撑爆上下文。无图片时返回原文本和空图片列表。
fn split_tool_result_images<F>(content: Option<&Value>, mut convert_img: F) -> (String, Vec<Value>)
where
    F: FnMut(&Value) -> Option<Value>,
{
    let Some(Value::Array(arr)) = content else {
        return (tool_result_content_to_string(content), Vec::new());
    };
    if !arr.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("image")) {
        return (tool_result_content_to_string(content), Vec::new());
    }

    let mut texts = Vec::new();
    let mut images = Vec::new();
    for block in arr {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    texts.push(t.to_string());
                }
            }
            Some("image") => {
                if let Some(img) = convert_img(block) {
                    images.push(img);
                }
            }
            _ => {
                if let Ok(s) = serde_json::to_string(block) {
                    texts.push(s);
                }
            }
        }
    }
    if images.is_empty() {
        return (tool_result_content_to_string(content), Vec::new());
    }
    let text = if texts.is_empty() {
        "[tool result image attached]".to_string()
    } else {
        texts.join("\n")
    };
    (text, images)
}

/// 转换单条 Anthropic 消息为一条或多条 OpenAI Chat 消息。
fn convert_message(msg: &Value) -> Vec<Value> {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
    let Some(content) = msg.get("content") else {
        return vec![json!({"role": role, "content": ""})];
    };

    if let Some(text) = content.as_str() {
        return vec![json!({"role": role, "content": text})];
    }

    let Some(blocks) = content.as_array() else {
        return vec![json!({"role": role, "content": content})];
    };

    let mut out = Vec::new();
    let mut parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_media_parts = Vec::new();

    for block in blocks {
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(json!({"type": "text", "text": text}));
                }
            }
            "image" => {
                if let Some(img) = convert_image_block(block) {
                    parts.push(img);
                }
            }
            "tool_use" => {
                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".into());
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments,
                    }
                }));
            }
            "tool_result" => {
                let tool_call_id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                let (content_str, images) = split_tool_result_images(block.get("content"), convert_image_block);
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content_str,
                }));
                if !images.is_empty() {
                    tool_media_parts.push(json!({
                        "type": "text",
                        "text": format!("[media output of tool call {tool_call_id}]"),
                    }));
                    tool_media_parts.extend(images);
                }
            }
            // thinking / redacted_thinking 在请求里直接丢弃；历史 web_search 结果保留为简洁文本
            "thinking" | "redacted_thinking" | "server_tool_use" => {}
            "web_search_tool_result" => {
                let summary = summarize_web_search_result_block(block);
                if !summary.is_empty() {
                    parts.push(json!({"type": "text", "text": summary}));
                }
            }
            _ => {}
        }
    }

    // OpenAI Chat 的 role: tool 不能携带图片，在全部并列 tool 消息后追加一条 user 消息承载提取出的图片
    if !tool_media_parts.is_empty() {
        out.push(json!({
            "role": "user",
            "content": tool_media_parts,
        }));
    }

    if !parts.is_empty() || !tool_calls.is_empty() {
        let mut m = Map::new();
        m.insert("role".into(), json!(role));
        if parts.is_empty() {
            m.insert("content".into(), Value::Null);
        } else if parts.len() == 1 && parts[0].get("type").and_then(Value::as_str) == Some("text") {
            m.insert("content".into(), parts[0]["text"].clone());
        } else {
            m.insert("content".into(), Value::Array(parts));
        }
        if !tool_calls.is_empty() {
            m.insert("tool_calls".into(), Value::Array(tool_calls));
        }
        out.push(Value::Object(m));
    }

    out
}

/// 映射 Anthropic tool_choice → OpenAI Chat tool_choice。
fn map_tool_choice(tc: &Value) -> Option<Value> {
    match tc {
        Value::String(s) => match s.as_str() {
            "auto" => Some(json!("auto")),
            "any" => Some(json!("required")),
            "none" => Some(json!("none")),
            _ => None,
        },
        Value::Object(obj) => match obj.get("type").and_then(Value::as_str) {
            Some("auto") => Some(json!("auto")),
            Some("any") => Some(json!("required")),
            Some("none") => Some(json!("none")),
            Some("tool") => {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
                Some(json!({
                    "type": "function",
                    "function": { "name": name }
                }))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Anthropic Messages 请求体 → OpenAI Chat Completions 请求体。
///
/// 只输出 Chat Completions 认识的字段，不透传 metadata、thinking、cache_control 等 Anthropic 专有字段。
pub fn anthropic_to_openai_chat(body: &Value) -> Result<Value> {
    let mut result = Map::new();

    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    if !model.is_empty() {
        result.insert("model".into(), json!(model));
    }

    let mut messages = Vec::new();

    // system：字符串或 text 块数组，合成一条 system 消息；空内容不发
    if let Some(system) = body.get("system") {
        if let Some(text) = system.as_str() {
            if !text.is_empty() {
                messages.push(json!({"role": "system", "content": text}));
            }
        } else if let Some(arr) = system.as_array() {
            let parts: Vec<&str> = arr
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .filter(|t| !t.is_empty())
                .collect();
            if !parts.is_empty() {
                messages.push(json!({"role": "system", "content": parts.join("\n")}));
            }
        }
    }

    // messages
    if let Some(msgs) = body.get("messages").and_then(Value::as_array) {
        for msg in msgs {
            messages.extend(convert_message(msg));
        }
    }
    result.insert("messages".into(), Value::Array(messages));

    // max_tokens：o1/o3/o4 系列使用 max_completion_tokens
    if let Some(v) = body.get("max_tokens").filter(|v| !v.is_null()) {
        if is_o_series(model) {
            result.insert("max_completion_tokens".into(), v.clone());
        } else {
            result.insert("max_tokens".into(), v.clone());
        }
    }

    if let Some(v) = body.get("temperature").filter(|v| !v.is_null()) {
        result.insert("temperature".into(), v.clone());
    }
    if let Some(v) = body.get("top_p").filter(|v| !v.is_null()) {
        result.insert("top_p".into(), v.clone());
    }
    if let Some(v) = body.get("stop_sequences").filter(|v| !v.is_null()) {
        result.insert("stop".into(), v.clone());
    }
    if let Some(v) = body.get("stream").filter(|v| !v.is_null()) {
        result.insert("stream".into(), v.clone());
        if v.as_bool() == Some(true) {
            result.insert("stream_options".into(), json!({"include_usage": true}));
        }
    }

    // tools（Chat Completions 不支持 Anthropic 托管 web_search，直接过滤避免生成空函数）
    let mut dropped_web_search_names: Vec<&str> = Vec::new();
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let openai_tools: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(Value::as_str)?;
                if is_anthropic_web_search_tool(t) {
                    dropped_web_search_names.push(name);
                    return None;
                }
                let mut func = Map::new();
                func.insert("name".into(), json!(name));
                if let Some(desc) = t.get("description").and_then(Value::as_str)
                    && !desc.is_empty()
                {
                    func.insert("description".into(), json!(desc));
                }
                let schema = t.get("input_schema").cloned().unwrap_or_else(|| json!({}));
                func.insert("parameters".into(), clean_schema(schema, true));
                Some(json!({
                    "type": "function",
                    "function": Value::Object(func)
                }))
            })
            .collect();
        if !openai_tools.is_empty() {
            result.insert("tools".into(), Value::Array(openai_tools));
        }
    }

    // tool_choice（若指向已过滤掉的 web_search，则不发送）
    let tc_points_to_dropped = body
        .get("tool_choice")
        .and_then(Value::as_object)
        .filter(|o| o.get("type").and_then(Value::as_str) == Some("tool"))
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|n| dropped_web_search_names.contains(&n));
    if !tc_points_to_dropped && let Some(tc) = body.get("tool_choice").and_then(map_tool_choice) {
        result.insert("tool_choice".into(), tc);
    }

    if let Some(effort) = anthropic_effort(body)
        && supports_openai_reasoning_effort(model)
    {
        result.insert("reasoning_effort".into(), json!(openai_chat_effort(effort)));
    }

    Ok(Value::Object(result))
}

/// OpenAI Chat Completions 非流式响应 → Anthropic Messages 响应。
pub fn openai_chat_to_anthropic(body: &Value) -> Result<Value> {
    let choices = body
        .get("choices")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty());
    let Some(choice) = choices.and_then(|c| c.first()) else {
        bail!("OpenAI 响应里没有 choices");
    };
    let Some(message) = choice.get("message") else {
        bail!("OpenAI 响应里没有 message");
    };

    let mut content = Vec::new();

    // reasoning_content 直接丢弃；提取 message.content 文本
    if let Some(msg_content) = message.get("content") {
        if let Some(text) = msg_content.as_str() {
            if !text.is_empty() {
                content.push(json!({"type": "text", "text": text}));
            }
        } else if let Some(parts) = msg_content.as_array() {
            for part in parts {
                if matches!(part.get("type").and_then(Value::as_str), Some("text" | "output_text"))
                    && let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
        }
    }

    // tool_calls → tool_use 块
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (idx, tc) in tool_calls.iter().enumerate() {
            let id = tc
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| format!("call_{idx}"));
            let func = tc.get("function");
            let name = func.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("");
            let input = match func.and_then(|f| f.get("arguments")) {
                Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
                    Ok(v @ Value::Object(_)) => v,
                    _ => json!({}),
                },
                Some(v @ Value::Object(_)) => v.clone(),
                _ => json!({}),
            };
            content.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }));
        }
    }

    let id = ensure_msg_id(body.get("id").and_then(Value::as_str).unwrap_or(""));
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let stop_reason = map_stop_reason(choice.get("finish_reason").and_then(Value::as_str));
    let usage = map_usage(body.get("usage"));

    Ok(json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage,
    }))
}

/// 将上游 HTTP 错误响应体转为 Anthropic 错误格式。
pub fn openai_error_to_anthropic(status: u16, body: &[u8]) -> Value {
    let default_type = match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        _ => "api_error",
    };

    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        if let Some(err) = v.get("error") {
            if let Some(obj) = err.as_object() {
                let err_type = obj.get("type").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(default_type);
                let msg = obj
                    .get("message")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| serde_json::to_string(err).unwrap_or_default());
                return json!({
                    "type": "error",
                    "error": { "type": err_type, "message": msg }
                });
            }
            if let Some(s) = err.as_str() {
                return json!({
                    "type": "error",
                    "error": { "type": default_type, "message": s }
                });
            }
        }
        if let Some(msg) = v.get("message").and_then(Value::as_str) {
            return json!({
                "type": "error",
                "error": { "type": default_type, "message": msg }
            });
        }
    }

    let raw = String::from_utf8_lossy(body).trim().to_string();
    let msg = if raw.is_empty() { format!("上游返回 HTTP {status}") } else { raw };
    json!({
        "type": "error",
        "error": { "type": default_type, "message": msg }
    })
}

// ---------- 流式响应转换（OpenAI SSE → Anthropic SSE） ----------

fn format_sse(event_type: &str, payload: &Value) -> String {
    format!("event: {event_type}\ndata: {}\n\n", serde_json::to_string(payload).unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveBlock {
    Text(u32),
    Tool { upstream_index: usize, anthropic_index: u32 },
}

#[derive(Default)]
struct ToolState {
    anthropic_index: Option<u32>,
    id: String,
    name: String,
    started: bool,
    pending_args: String,
}

/// 增量 OpenAI SSE → Anthropic SSE 状态机，支持任意 TCP 字节分块。
pub struct SseConverter {
    buf: Vec<u8>,
    message_id: String,
    model: String,
    sent_message_start: bool,
    next_block_index: u32,
    active_block: Option<ActiveBlock>,
    tools: HashMap<usize, ToolState>,
    stop_reason: Option<&'static str>,
    latest_usage: Option<Value>,
    finished: bool,
    ended_with_error: bool,
}

impl Default for SseConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl SseConverter {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            message_id: String::new(),
            model: String::new(),
            sent_message_start: false,
            next_block_index: 0,
            active_block: None,
            tools: HashMap::new(),
            stop_reason: None,
            latest_usage: None,
            finished: false,
            ended_with_error: false,
        }
    }

    /// 喂入上游字节块，返回可立即发送给下游的 Anthropic SSE 事件文本列表。
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();

        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line_bytes = self.buf.drain(..=pos).collect::<Vec<u8>>();
            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            self.process_line(&line, &mut out);
            if self.finished {
                self.buf.clear();
                break;
            }
        }
        out
    }

    /// 上游流结束时调用：处理缓冲里残留的最后一行，并在未收到 [DONE] 时补发结束事件。
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if !self.buf.is_empty() {
            let mut line_bytes = std::mem::take(&mut self.buf);
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            self.process_line(&line, &mut out);
        }
        if !self.finished && !self.ended_with_error {
            self.emit_final(&mut out);
        }
        out
    }

    /// 底层网络流出错时生成 Anthropic error 事件并终止状态机。
    pub fn error_event(&mut self, msg: String) -> String {
        self.finished = true;
        self.ended_with_error = true;
        format_sse(
            "error",
            &json!({
                "type": "error",
                "error": { "type": "api_error", "message": msg }
            }),
        )
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<String>) {
        let Some(rest) = line.strip_prefix("data:") else {
            return;
        };
        let data = rest.trim();
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.emit_final(out);
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return;
        };

        // 上游在 SSE 内返回错误对象
        if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
            self.ended_with_error = true;
            self.finished = true;
            let (err_type, err_msg) = if let Some(obj) = err.as_object() {
                (
                    obj.get("type").and_then(Value::as_str).unwrap_or("api_error").to_string(),
                    obj.get("message")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .unwrap_or_else(|| serde_json::to_string(err).unwrap_or_default()),
                )
            } else if let Some(s) = err.as_str() {
                ("api_error".to_string(), s.to_string())
            } else {
                ("api_error".to_string(), serde_json::to_string(err).unwrap_or_default())
            };
            out.push(format_sse(
                "error",
                &json!({
                    "type": "error",
                    "error": { "type": err_type, "message": err_msg }
                }),
            ));
            return;
        }

        if self.message_id.is_empty()
            && let Some(id) = chunk.get("id").and_then(Value::as_str).filter(|s| !s.is_empty())
        {
            self.message_id = ensure_msg_id(id);
        }
        if self.model.is_empty()
            && let Some(m) = chunk.get("model").and_then(Value::as_str).filter(|s| !s.is_empty())
        {
            self.model = m.to_string();
        }

        // 记录 usage（可能是首个 chunk，也可能是末尾独立的 usage chunk）
        if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
            self.latest_usage = Some(map_usage(Some(u)));
        }

        // 只在第一个有效 chunk 发 message_start
        if !self.sent_message_start {
            self.emit_message_start(chunk.get("usage").filter(|u| u.is_object()), out);
        }

        let Some(choice) = chunk.get("choices").and_then(Value::as_array).and_then(|c| c.first()) else {
            return;
        };

        if let Some(delta) = choice.get("delta") {
            // 文本增量（空串不产生空块；reasoning_content 直接忽略）
            if let Some(text) = delta.get("content").and_then(Value::as_str)
                && !text.is_empty()
            {
                self.emit_text_delta(text, out);
            }

            // 工具调用增量
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tool_calls {
                    self.emit_tool_delta(tc, out);
                }
            }
        }

        // finish_reason 到达时只记 stop_reason，等 [DONE] 拿到最后 usage 再发 message_delta
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str).filter(|s| !s.is_empty())
            && self.stop_reason.is_none()
        {
            self.stop_reason = Some(map_stop_reason(Some(fr)));
        }
    }

    fn emit_message_start(&mut self, first_usage: Option<&Value>, out: &mut Vec<String>) {
        if self.sent_message_start {
            return;
        }
        self.sent_message_start = true;
        if self.message_id.is_empty() {
            self.message_id = ensure_msg_id("");
        }
        let u = map_usage(first_usage);
        out.push(format_sse(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": u["input_tokens"],
                        "output_tokens": 0,
                        "cache_read_input_tokens": u["cache_read_input_tokens"],
                    }
                }
            }),
        ));
    }

    fn close_active_block(&mut self, out: &mut Vec<String>) {
        if let Some(active) = self.active_block.take() {
            let index = match active {
                ActiveBlock::Text(i) | ActiveBlock::Tool { anthropic_index: i, .. } => i,
            };
            out.push(format_sse(
                "content_block_stop",
                &json!({
                    "type": "content_block_stop",
                    "index": index,
                }),
            ));
        }
    }

    fn emit_text_delta(&mut self, text: &str, out: &mut Vec<String>) {
        let index = match self.active_block {
            Some(ActiveBlock::Text(i)) => i,
            _ => {
                self.close_active_block(out);
                let i = self.next_block_index;
                self.next_block_index += 1;
                out.push(format_sse(
                    "content_block_start",
                    &json!({
                        "type": "content_block_start",
                        "index": i,
                        "content_block": { "type": "text", "text": "" }
                    }),
                ));
                self.active_block = Some(ActiveBlock::Text(i));
                i
            }
        };
        out.push(format_sse(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            }),
        ));
    }

    fn emit_tool_delta(&mut self, tc: &Value, out: &mut Vec<String>) {
        let upstream_index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;

        // 切换工具块或从文本块切换过来前，先停上一个块
        match self.active_block {
            Some(ActiveBlock::Text(_)) => self.close_active_block(out),
            Some(ActiveBlock::Tool { upstream_index: cur, .. }) if cur != upstream_index => {
                self.close_active_block(out);
            }
            _ => {}
        }

        let id_delta = tc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty());
        let name_delta = tc.pointer("/function/name").and_then(Value::as_str).filter(|s| !s.is_empty());
        let args_delta = tc.pointer("/function/arguments").and_then(Value::as_str).filter(|s| !s.is_empty());

        let state = self.tools.entry(upstream_index).or_default();
        if let Some(id) = id_delta {
            state.id = id.to_string();
        }
        if let Some(name) = name_delta {
            state.name = name.to_string();
        }

        if !state.started && !state.name.is_empty() {
            let anthropic_index = self.next_block_index;
            self.next_block_index += 1;
            if state.id.is_empty() {
                state.id = format!("call_{upstream_index}");
            }
            state.anthropic_index = Some(anthropic_index);
            state.started = true;
            out.push(format_sse(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": anthropic_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": state.id,
                        "name": state.name,
                        "input": {}
                    }
                }),
            ));
            self.active_block = Some(ActiveBlock::Tool { upstream_index, anthropic_index });

            if !state.pending_args.is_empty() {
                let pending = std::mem::take(&mut state.pending_args);
                out.push(format_sse(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": anthropic_index,
                        "delta": { "type": "input_json_delta", "partial_json": pending }
                    }),
                ));
            }
        }

        if let Some(args) = args_delta {
            if state.started {
                let anthropic_index = state.anthropic_index.unwrap();
                out.push(format_sse(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": anthropic_index,
                        "delta": { "type": "input_json_delta", "partial_json": args }
                    }),
                ));
            } else {
                state.pending_args.push_str(args);
            }
        }
    }

    fn emit_final(&mut self, out: &mut Vec<String>) {
        if self.finished {
            return;
        }
        self.finished = true;

        // 一直没内容的流也要发 message_start / message_delta / message_stop
        if !self.sent_message_start {
            self.emit_message_start(None, out);
        }

        self.close_active_block(out);

        // 兜底：若有收到 arguments 但一直缺 name 的未启动工具块，补齐 start/delta/stop
        let mut late: Vec<(usize, String, String, String)> = self
            .tools
            .iter_mut()
            .filter(|(_, s)| !s.started && (!s.pending_args.is_empty() || !s.id.is_empty()))
            .map(|(u_idx, s)| {
                s.started = true;
                let id = if s.id.is_empty() { format!("call_{u_idx}") } else { s.id.clone() };
                let name = if s.name.is_empty() { "unknown_tool".to_string() } else { s.name.clone() };
                (*u_idx, id, name, std::mem::take(&mut s.pending_args))
            })
            .collect();
        late.sort_unstable_by_key(|(u_idx, _, _, _)| *u_idx);
        for (_, id, name, pending) in late {
            let idx = self.next_block_index;
            self.next_block_index += 1;
            out.push(format_sse(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} }
                }),
            ));
            if !pending.is_empty() {
                out.push(format_sse(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "input_json_delta", "partial_json": pending }
                    }),
                ));
            }
            out.push(format_sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": idx }),
            ));
        }

        let usage = self.latest_usage.clone().unwrap_or_else(|| map_usage(None));
        let stop_reason = self.stop_reason.unwrap_or("end_turn");
        out.push(format_sse(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": stop_reason,
                    "stop_sequence": null
                },
                "usage": usage
            }),
        ));
        out.push(format_sse("message_stop", &json!({"type": "message_stop"})));
    }
}

/// 将上游 OpenAI SSE 字节流实时转为 Anthropic SSE 字节流（边收边发）。
pub fn openai_sse_to_anthropic_sse<E: std::fmt::Display + Send + 'static>(
    upstream: impl Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut conv = SseConverter::new();
        tokio::pin!(upstream);

        while let Some(item) = upstream.next().await {
            match item {
                Ok(bytes) => {
                    for ev in conv.push_bytes(&bytes) {
                        yield Ok(Bytes::from(ev));
                    }
                    if conv.finished {
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
    }
}

// ---------- OpenAI Responses API 转换 ----------

/// OpenAI Responses API 拒绝小于 16 的 max_output_tokens；小预算探测请求抬到 16。
const RESPONSES_MIN_MAX_OUTPUT_TOKENS: u64 = 16;

fn convert_image_block_responses(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let source_type = source.get("type").and_then(Value::as_str)?;
    let url = match source_type {
        "base64" => {
            let media_type = source.get("media_type").and_then(Value::as_str).unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            format!("data:{media_type};base64,{data}")
        }
        "url" => source.get("url").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    Some(json!({ "type": "input_image", "image_url": url }))
}

/// 将 tool_result 的 content 转为 Responses API 的 function_call_output.output：
/// 含图片时返回 input_text / input_image 数组，无图片时沿用纯字符串。
fn tool_result_to_responses_output(content: Option<&Value>) -> Value {
    let Some(Value::Array(arr)) = content else {
        return json!(tool_result_content_to_string(content));
    };
    if !arr.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("image")) {
        return json!(tool_result_content_to_string(content));
    }
    let mut items = Vec::new();
    let mut has_image = false;
    for block in arr {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    items.push(json!({ "type": "input_text", "text": t }));
                }
            }
            Some("image") => {
                if let Some(img) = convert_image_block_responses(block) {
                    has_image = true;
                    items.push(img);
                }
            }
            _ => {
                if let Ok(s) = serde_json::to_string(block) {
                    items.push(json!({ "type": "input_text", "text": s }));
                }
            }
        }
    }
    if has_image {
        Value::Array(items)
    } else {
        json!(tool_result_content_to_string(content))
    }
}

fn map_tool_choice_responses(tc: &Value, web_search_names: &[&str], is_codex_oauth: bool) -> Option<Value> {
    match tc {
        Value::String(s) => match s.as_str() {
            "auto" => Some(json!("auto")),
            "any" => Some(json!("required")),
            "none" => Some(json!("none")),
            _ => None,
        },
        Value::Object(obj) => match obj.get("type").and_then(Value::as_str) {
            Some("auto") => Some(json!("auto")),
            Some("any") => Some(json!("required")),
            Some("none") => Some(json!("none")),
            Some("tool") => {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
                if web_search_names.contains(&name) {
                    if is_codex_oauth {
                        Some(json!("required"))
                    } else {
                        Some(json!({ "type": "web_search" }))
                    }
                } else {
                    Some(json!({ "type": "function", "name": name }))
                }
            }
            _ => None,
        },
        _ => None,
    }
}

fn build_responses_web_search_tool(tool: &Value, is_codex_oauth: bool) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), json!("web_search"));
    if is_codex_oauth {
        obj.insert("external_web_access".into(), json!(true));
    }
    let mut filters = Map::new();
    if let Some(allowed) = tool
        .get("allowed_domains")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        filters.insert("allowed_domains".into(), Value::Array(allowed.clone()));
    }
    if !is_codex_oauth
        && let Some(blocked) = tool
            .get("blocked_domains")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
    {
        filters.insert("blocked_domains".into(), Value::Array(blocked.clone()));
    }
    if !filters.is_empty() {
        obj.insert("filters".into(), Value::Object(filters));
    }
    if let Some(loc) = tool.get("user_location").filter(|v| v.is_object()) {
        obj.insert("user_location".into(), loc.clone());
    }
    Value::Object(obj)
}

fn responses_web_search_action_input(item: &Value) -> Value {
    if let Some(query) = item.pointer("/action/query").and_then(Value::as_str) {
        return json!({ "query": query });
    }
    if let Some(queries) = item.pointer("/action/queries").and_then(Value::as_array)
        && let Some(first) = queries.iter().find_map(Value::as_str)
    {
        return json!({ "query": first });
    }
    if let Some(url) = item.pointer("/action/url").and_then(Value::as_str) {
        return json!({ "query": url });
    }
    json!({})
}

fn responses_web_search_results_from_action(item: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(sources) = item.pointer("/action/sources").and_then(Value::as_array) else {
        return out;
    };
    for s in sources {
        if let Some(url) = s.get("url").and_then(Value::as_str).filter(|u| !u.is_empty()) {
            let title = s.get("title").and_then(Value::as_str);
            merge_web_search_results(&mut out, vec![make_web_search_result(url, title, s.get("page_age"))]);
        }
    }
    out
}

fn responses_web_search_results_from_annotations(annotations: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for ann in annotations {
        if ann.get("type").and_then(Value::as_str) != Some("url_citation") {
            continue;
        }
        if let Some(url) = ann.get("url").and_then(Value::as_str).filter(|u| !u.is_empty()) {
            let title = ann.get("title").and_then(Value::as_str);
            merge_web_search_results(&mut out, vec![make_web_search_result(url, title, None)]);
        }
    }
    out
}

fn responses_web_search_error(item: &Value) -> Option<Value> {
    let failed = item.get("status").and_then(Value::as_str) == Some("failed")
        || item.get("error").is_some_and(|e| !e.is_null());
    if !failed {
        return None;
    }
    let code = item
        .pointer("/error/code")
        .or_else(|| item.pointer("/error/type"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("unavailable");
    Some(json!({
        "type": "web_search_tool_result_error",
        "error_code": code,
    }))
}

fn map_responses_usage(usage: Option<&Value>) -> Value {
    let raw_input = usage
        .and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens")))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens").or_else(|| u.get("completion_tokens")))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // OpenAI Responses 的 input_tokens/prompt_tokens 包含 cached_tokens；若上游已经直接给 Anthropic 风格的 cache_read_input_tokens 则不扣减。
    let cached_in_total = usage
        .and_then(|u| {
            u.pointer("/input_tokens_details/cached_tokens")
                .or_else(|| u.pointer("/prompt_tokens_details/cached_tokens"))
        })
        .and_then(Value::as_u64);
    let (input_tokens, cache_read) = match cached_in_total {
        Some(cached) => (raw_input.saturating_sub(cached), cached),
        None => {
            let direct = usage
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (raw_input, direct)
        }
    };
    json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_read_input_tokens": cache_read,
    })
}

fn map_responses_stop_reason(status: Option<&str>, has_tool_use: bool, incomplete_reason: Option<&str>) -> &'static str {
    match status {
        Some("completed") if has_tool_use => "tool_use",
        Some("incomplete")
            if matches!(incomplete_reason, Some("max_output_tokens" | "max_tokens") | None) =>
        {
            "max_tokens"
        }
        _ if has_tool_use => "tool_use",
        _ => "end_turn",
    }
}

/// Anthropic Messages 请求体 → OpenAI Responses 请求体。
pub fn anthropic_to_openai_responses(body: &Value) -> Result<Value> {
    anthropic_to_openai_responses_inner(body, false)
}

fn anthropic_to_openai_responses_inner(body: &Value, is_codex_oauth: bool) -> Result<Value> {
    let mut result = Map::new();

    if let Some(model) = body.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        result.insert("model".into(), json!(model));
    }

    // system → instructions
    if let Some(system) = body.get("system") {
        let instructions = if let Some(text) = system.as_str() {
            text.to_string()
        } else if let Some(arr) = system.as_array() {
            arr.iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            String::new()
        };
        if !instructions.is_empty() {
            result.insert("instructions".into(), json!(instructions));
        }
    }

    // messages → input（tool_use/tool_result 提升为独立顶层 item）
    let mut input = Vec::new();
    if let Some(msgs) = body.get("messages").and_then(Value::as_array) {
        for msg in msgs {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            let text_type = if role == "assistant" { "output_text" } else { "input_text" };
            match msg.get("content") {
                Some(Value::String(text)) => {
                    input.push(json!({
                        "role": role,
                        "content": [{ "type": text_type, "text": text }]
                    }));
                }
                Some(Value::Array(blocks)) => {
                    let mut parts = Vec::new();
                    for block in blocks {
                        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(Value::as_str) {
                                    parts.push(json!({ "type": text_type, "text": text }));
                                }
                            }
                            "image" => {
                                if let Some(img) = convert_image_block_responses(block) {
                                    parts.push(img);
                                }
                            }
                            "tool_use" => {
                                if !parts.is_empty() {
                                    input.push(json!({ "role": role, "content": std::mem::take(&mut parts) }));
                                }
                                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                                let args = block.get("input").cloned().unwrap_or_else(|| json!({}));
                                input.push(json!({
                                    "type": "function_call",
                                    "call_id": id,
                                    "name": name,
                                    "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
                                }));
                            }
                            "tool_result" => {
                                if !parts.is_empty() {
                                    input.push(json!({ "role": role, "content": std::mem::take(&mut parts) }));
                                }
                                let call_id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                                let output = tool_result_to_responses_output(block.get("content"));
                                input.push(json!({
                                    "type": "function_call_output",
                                    "call_id": call_id,
                                    "output": output,
                                }));
                            }
                            "thinking" | "redacted_thinking" | "server_tool_use" => {}
                            "web_search_tool_result" => {
                                let summary = summarize_web_search_result_block(block);
                                if !summary.is_empty() {
                                    parts.push(json!({ "type": text_type, "text": summary }));
                                }
                            }
                            _ => {}
                        }
                    }
                    if !parts.is_empty() {
                        input.push(json!({ "role": role, "content": parts }));
                    }
                }
                _ => {}
            }
        }
    }
    result.insert("input".into(), Value::Array(input));

    // max_tokens → max_output_tokens（低于 16 的正整数抬到 16）
    if let Some(v) = body.get("max_tokens").filter(|v| !v.is_null()) {
        let mapped = match v.as_u64() {
            Some(n) if (1..RESPONSES_MIN_MAX_OUTPUT_TOKENS).contains(&n) => json!(RESPONSES_MIN_MAX_OUTPUT_TOKENS),
            _ => v.clone(),
        };
        result.insert("max_output_tokens".into(), mapped);
    }

    if let Some(v) = body.get("temperature").filter(|v| !v.is_null()) {
        result.insert("temperature".into(), v.clone());
    }
    if let Some(v) = body.get("top_p").filter(|v| !v.is_null()) {
        result.insert("top_p".into(), v.clone());
    }
    if let Some(v) = body.get("stream").filter(|v| !v.is_null()) {
        result.insert("stream".into(), v.clone());
    }

    // tools（普通函数扁平结构 + Anthropic 托管 web_search → Responses web_search）
    let mut web_search_names: Vec<&str> = Vec::new();
    let mut has_web_search = false;
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mut resp_tools: Vec<Value> = Vec::new();
        for t in tools {
            if is_anthropic_web_search_tool(t) {
                if let Some(name) = t.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    web_search_names.push(name);
                }
                if !has_web_search {
                    has_web_search = true;
                    resp_tools.push(build_responses_web_search_tool(t, is_codex_oauth));
                }
                continue;
            }
            let Some(name) = t.get("name").and_then(Value::as_str) else {
                continue;
            };
            let mut tool = Map::new();
            tool.insert("type".into(), json!("function"));
            tool.insert("name".into(), json!(name));
            tool.insert("strict".into(), json!(false));
            if let Some(desc) = t.get("description").and_then(Value::as_str)
                && !desc.is_empty()
            {
                tool.insert("description".into(), json!(desc));
            }
            let schema = t.get("input_schema").cloned().unwrap_or_else(|| json!({}));
            tool.insert("parameters".into(), clean_schema(schema, true));
            resp_tools.push(Value::Object(tool));
        }
        // Codex OAuth 强制指定 web_search 时，tool_choice 需降级为 "required"，同时过滤无关工具避免误调
        let forced_ws = is_codex_oauth
            && body
                .get("tool_choice")
                .and_then(Value::as_object)
                .filter(|o| o.get("type").and_then(Value::as_str) == Some("tool"))
                .and_then(|o| o.get("name"))
                .and_then(Value::as_str)
                .is_some_and(|n| web_search_names.contains(&n));
        if forced_ws {
            resp_tools.retain(|t| t.get("type").and_then(Value::as_str) == Some("web_search"));
        }
        if !resp_tools.is_empty() {
            result.insert("tools".into(), Value::Array(resp_tools));
        }
    }

    if has_web_search {
        let mut includes: Vec<Value> = body
            .get("include")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !includes.iter().any(|v| v.as_str() == Some("web_search_call.action.sources")) {
            includes.push(json!("web_search_call.action.sources"));
        }
        result.insert("include".into(), Value::Array(includes));
    }

    if let Some(tc) = body
        .get("tool_choice")
        .and_then(|tc| map_tool_choice_responses(tc, &web_search_names, is_codex_oauth))
    {
        result.insert("tool_choice".into(), tc);
    }

    if let Some(effort) = anthropic_effort(body) {
        let model = body.get("model").and_then(Value::as_str).unwrap_or("");
        if supports_openai_reasoning_effort(model) {
            result.insert("reasoning".into(), json!({ "effort": openai_responses_effort(model, effort) }));
        }
    }

    Ok(Value::Object(result))
}

/// Anthropic Messages 请求体 → ChatGPT Codex OAuth (`chatgpt.com/backend-api/codex/responses`) 请求体。
/// 遵循 ChatGPT 消费级后端约束（参考 cc-switch 与 sub2api）：
/// - `store: false`、`stream: true`、`include` 含 `reasoning.encrypted_content`
/// - 删除上游拒绝的参数（`max_output_tokens` / `temperature` / `top_p` 等）
/// - 兜底必填字段 `instructions` / `tools` / `parallel_tool_calls`
pub fn anthropic_to_codex_oauth_responses(body: &Value) -> Result<Value> {
    let mut req = anthropic_to_openai_responses_inner(body, true)?;
    if let Some(obj) = req.as_object_mut() {
        obj.insert("store".into(), json!(false));
        obj.insert("stream".into(), json!(true));

        let mut includes: Vec<Value> = obj
            .get("include")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| body.get("include").and_then(Value::as_array).cloned())
            .unwrap_or_default();
        if !includes.iter().any(|v| v.as_str() == Some("reasoning.encrypted_content")) {
            includes.push(json!("reasoning.encrypted_content"));
        }
        obj.insert("include".into(), Value::Array(includes));

        for unsupported in [
            "max_output_tokens",
            "max_completion_tokens",
            "temperature",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
            "stream_options",
            "truncation",
            "stop_sequences",
            "metadata",
            "user",
        ] {
            obj.remove(unsupported);
        }

        obj.entry("instructions".to_string()).or_insert_with(|| json!(""));
        obj.entry("tools".to_string()).or_insert_with(|| json!([]));
        obj.entry("parallel_tool_calls".to_string()).or_insert_with(|| json!(true));
        if let Some(effort) = anthropic_effort(body) {
            let model = body.get("model").and_then(Value::as_str).unwrap_or("");
            obj.insert("reasoning".into(), json!({ "effort": openai_responses_effort(model, effort) }));
        }
    }
    Ok(req)
}

/// 将 Responses SSE 字节流收集为单个 Anthropic Messages 非流式 JSON 响应。
/// 用于 ChatGPT Codex OAuth 上游强制流式，但客户端（如分类器 / 标题生成）请求非流式的场景。
pub fn collect_responses_sse_to_anthropic(bytes: &[u8]) -> Result<Value> {
    collect_responses_sse_to_anthropic_with_web_search(bytes, None)
}

pub fn collect_responses_sse_to_anthropic_with_web_search(bytes: &[u8], web_search_name: Option<&str>) -> Result<Value> {
    let text = String::from_utf8_lossy(bytes);
    let mut completed: Option<Value> = None;
    let mut current_event = String::new();
    let mut id = String::new();
    let mut model = String::new();
    let mut collected_text = String::new();
    // (call_id, name, arguments)
    let mut tools: Vec<(String, String, String)> = Vec::new();
    let mut web_searches: Vec<Value> = Vec::new();
    let mut annotations: Vec<Value> = Vec::new();
    let mut usage: Option<Value> = None;
    let mut status: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            current_event.clear();
            continue;
        }
        if let Some(ev) = line.strip_prefix("event:") {
            current_event = ev.trim().to_string();
            continue;
        }
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let data = rest.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let ev = v
            .get("type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(current_event.as_str());
        match ev {
            "response.created" => {
                let r = v.get("response").unwrap_or(&v);
                if id.is_empty()
                    && let Some(s) = r.get("id").and_then(Value::as_str)
                {
                    id = s.to_string();
                }
                if model.is_empty()
                    && let Some(s) = r.get("model").and_then(Value::as_str)
                {
                    model = s.to_string();
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(d) = v.get("delta").and_then(Value::as_str) {
                    collected_text.push_str(d);
                }
            }
            "response.output_text.annotation.added" => {
                if let Some(ann) = v.get("annotation") {
                    annotations.push(ann.clone());
                }
            }
            "response.output_item.added" => {
                if let Some(item) = v.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("function_call")
                {
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    tools.push((call_id, name, String::new()));
                }
            }
            "response.output_item.done" => {
                if let Some(item) = v.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("web_search_call")
                {
                    web_searches.push(item.clone());
                }
            }
            "response.function_call_arguments.delta" => {
                if let (Some(d), Some(last)) = (v.get("delta").and_then(Value::as_str), tools.last_mut()) {
                    last.2.push_str(d);
                }
            }
            "response.function_call_arguments.done" => {
                if let (Some(args), Some(last)) = (v.get("arguments").and_then(Value::as_str), tools.last_mut())
                    && last.2.is_empty()
                {
                    last.2.push_str(args);
                }
            }
            "response.completed" | "response.done" | "response.incomplete" | "response.failed" => {
                let r = v.get("response").cloned().unwrap_or(v);
                if let Some(s) = r.get("status").and_then(Value::as_str) {
                    status = Some(s.to_string());
                }
                if let Some(u) = r.get("usage").filter(|u| u.is_object()) {
                    usage = Some(u.clone());
                }
                completed = Some(r);
            }
            "error" => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| v.get("message").and_then(Value::as_str))
                    .unwrap_or("上游 SSE 返回错误");
                bail!("{msg}");
            }
            _ => {}
        }
    }

    if let Some(resp) = completed
        && (resp.get("output").and_then(Value::as_array).is_some()
            || matches!(resp.get("status").and_then(Value::as_str), Some("failed" | "cancelled"))
            || resp.get("error").is_some_and(|e| !e.is_null()))
    {
        return openai_responses_to_anthropic_with_web_search(&resp, web_search_name);
    }

    let ws_name = web_search_name.filter(|s| !s.is_empty()).unwrap_or("web_search");
    let ann_results = responses_web_search_results_from_annotations(&annotations);
    let mut content = Vec::new();
    let ws_count = web_searches.len() as u64;
    for (idx, item) in web_searches.iter().enumerate() {
        let sid = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("srvtoolu_{idx}"));
        let input = responses_web_search_action_input(item);
        let result_content = if let Some(err) = responses_web_search_error(item) {
            err
        } else {
            let mut results = responses_web_search_results_from_action(item);
            if idx + 1 == web_searches.len() {
                merge_web_search_results(&mut results, ann_results.clone());
            }
            Value::Array(results)
        };
        content.push(json!({
            "type": "server_tool_use",
            "id": sid,
            "name": ws_name,
            "input": input,
        }));
        content.push(json!({
            "type": "web_search_tool_result",
            "tool_use_id": sid,
            "content": result_content,
        }));
    }
    if !collected_text.is_empty() {
        content.push(json!({ "type": "text", "text": collected_text }));
    }
    let has_tool_use = !tools.is_empty();
    for (idx, (call_id, name, args)) in tools.into_iter().enumerate() {
        let cid = if call_id.is_empty() { format!("call_{idx}") } else { call_id };
        let input = serde_json::from_str::<Value>(&args)
            .ok()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| json!({}));
        content.push(json!({
            "type": "tool_use",
            "id": cid,
            "name": name,
            "input": input,
        }));
    }
    let stop_reason = map_responses_stop_reason(status.as_deref(), has_tool_use, None);
    let mut out_usage = map_responses_usage(usage.as_ref());
    attach_web_search_usage(&mut out_usage, ws_count);
    Ok(json!({
        "id": ensure_msg_id(&id),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": out_usage,
    }))
}

/// OpenAI Responses 非流式响应 → Anthropic Messages 响应。
pub fn openai_responses_to_anthropic(body: &Value) -> Result<Value> {
    openai_responses_to_anthropic_with_web_search(body, None)
}

pub fn openai_responses_to_anthropic_with_web_search(body: &Value, web_search_name: Option<&str>) -> Result<Value> {
    let status = body.get("status").and_then(Value::as_str);
    if matches!(status, Some("failed" | "cancelled")) || body.get("error").is_some_and(|e| !e.is_null()) {
        let msg = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| body.get("error").and_then(Value::as_str))
            .unwrap_or("Responses 上游返回失败状态");
        bail!("{msg}");
    }

    let Some(output) = body.get("output").and_then(Value::as_array) else {
        bail!("OpenAI Responses 响应里没有 output");
    };

    // 先收集 message 里的 url_citation 注解，补全 web_search_call 缺失的标题或回退来源
    let mut citation_results: Vec<Value> = Vec::new();
    let mut last_ok_ws_idx: Option<usize> = None;
    for (idx, item) in output.iter().enumerate() {
        match item.get("type").and_then(Value::as_str) {
            Some("web_search_call") if responses_web_search_error(item).is_none() => {
                last_ok_ws_idx = Some(idx);
            }
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        if let Some(anns) = part.get("annotations").and_then(Value::as_array) {
                            merge_web_search_results(
                                &mut citation_results,
                                responses_web_search_results_from_annotations(anns),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let ws_name = web_search_name.filter(|s| !s.is_empty()).unwrap_or("web_search");
    let mut content = Vec::new();
    let mut has_tool_use = false;
    let mut web_search_requests: u64 = 0;

    for (idx, item) in output.iter().enumerate() {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        match item_type {
            "web_search_call" => {
                web_search_requests += 1;
                let sid = item
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| format!("srvtoolu_{idx}"));
                let input = responses_web_search_action_input(item);
                let result_content = if let Some(err) = responses_web_search_error(item) {
                    err
                } else {
                    let mut results = responses_web_search_results_from_action(item);
                    if last_ok_ws_idx == Some(idx) {
                        merge_web_search_results(&mut results, citation_results.clone());
                    }
                    Value::Array(results)
                };
                content.push(json!({
                    "type": "server_tool_use",
                    "id": sid,
                    "name": ws_name,
                    "input": input,
                }));
                content.push(json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": sid,
                    "content": result_content,
                }));
            }
            "message" => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text" | "text") => {
                                if let Some(text) = part.get("text").and_then(Value::as_str)
                                    && !text.is_empty()
                                {
                                    content.push(json!({"type": "text", "text": text}));
                                }
                            }
                            Some("refusal") => {
                                if let Some(refusal) = part.get("refusal").and_then(Value::as_str)
                                    && !refusal.is_empty()
                                {
                                    content.push(json!({"type": "text", "text": refusal}));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "function_call" => {
                has_tool_use = true;
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| format!("call_{idx}"));
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let input = match item.get("arguments") {
                    Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
                        Ok(v @ Value::Object(_)) => v,
                        _ => json!({}),
                    },
                    Some(v @ Value::Object(_)) => v.clone(),
                    _ => json!({}),
                };
                content.push(json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": input,
                }));
            }
            // reasoning 丢弃
            _ => {}
        }
    }

    let id = ensure_msg_id(body.get("id").and_then(Value::as_str).unwrap_or(""));
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let incomplete_reason = body.pointer("/incomplete_details/reason").and_then(Value::as_str);
    let stop_reason = map_responses_stop_reason(status, has_tool_use, incomplete_reason);
    let mut usage = map_responses_usage(body.get("usage"));
    attach_web_search_usage(&mut usage, web_search_requests);

    Ok(json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RespActiveBlock {
    Text { index: u32, had_delta: bool },
    Tool { index: u32, had_delta: bool },
}

/// 增量 OpenAI Responses SSE → Anthropic SSE 状态机，支持任意 TCP 字节分块。
pub struct ResponsesSseConverter {
    buf: Vec<u8>,
    current_event: String,
    message_id: String,
    model: String,
    web_search_name: String,
    sent_message_start: bool,
    next_block_index: u32,
    active_block: Option<RespActiveBlock>,
    has_tool_use: bool,
    web_search_count: u64,
    emitted_web_search_ids: Vec<String>,
    pending_web_search_annotations: Vec<Value>,
    stop_reason: Option<&'static str>,
    latest_usage: Option<Value>,
    finished: bool,
    ended_with_error: bool,
}

impl Default for ResponsesSseConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponsesSseConverter {
    pub fn new() -> Self {
        Self::with_web_search_name(None)
    }

    pub fn with_web_search_name(web_search_name: Option<String>) -> Self {
        Self {
            buf: Vec::new(),
            current_event: String::new(),
            message_id: String::new(),
            model: String::new(),
            web_search_name: web_search_name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "web_search".to_string()),
            sent_message_start: false,
            next_block_index: 0,
            active_block: None,
            has_tool_use: false,
            web_search_count: 0,
            emitted_web_search_ids: Vec::new(),
            pending_web_search_annotations: Vec::new(),
            stop_reason: None,
            latest_usage: None,
            finished: false,
            ended_with_error: false,
        }
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();

        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line_bytes = self.buf.drain(..=pos).collect::<Vec<u8>>();
            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            self.process_line(&line, &mut out);
            if self.finished {
                self.buf.clear();
                break;
            }
        }
        out
    }

    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if !self.buf.is_empty() {
            let mut line_bytes = std::mem::take(&mut self.buf);
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            self.process_line(&line, &mut out);
        }
        if !self.finished && !self.ended_with_error {
            self.emit_final(&mut out);
        }
        out
    }

    pub fn error_event(&mut self, msg: String) -> String {
        self.finished = true;
        self.ended_with_error = true;
        format_sse(
            "error",
            &json!({
                "type": "error",
                "error": { "type": "api_error", "message": msg }
            }),
        )
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<String>) {
        if line.is_empty() {
            self.current_event.clear();
            return;
        }
        if let Some(ev) = line.strip_prefix("event:") {
            self.current_event = ev.trim().to_string();
            return;
        }
        let Some(rest) = line.strip_prefix("data:") else {
            return;
        };
        let data = rest.trim();
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.emit_final(out);
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let event_name = v
            .get("type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(self.current_event.as_str())
            .to_string();
        self.handle_event(&event_name, &v, out);
    }

    fn handle_event(&mut self, event_name: &str, data: &Value, out: &mut Vec<String>) {
        match event_name {
            "response.created" => {
                let resp = data.get("response").unwrap_or(data);
                if self.message_id.is_empty()
                    && let Some(id) = resp.get("id").and_then(Value::as_str).filter(|s| !s.is_empty())
                {
                    self.message_id = ensure_msg_id(id);
                }
                if self.model.is_empty()
                    && let Some(m) = resp.get("model").and_then(Value::as_str).filter(|s| !s.is_empty())
                {
                    self.model = m.to_string();
                }
                self.emit_message_start(resp.get("usage").filter(|u| u.is_object()), out);
            }

            "response.content_part.added" => {
                let part_type = data.pointer("/part/type").and_then(Value::as_str);
                if matches!(part_type, Some("output_text" | "refusal")) {
                    self.emit_message_start(None, out);
                    if !matches!(self.active_block, Some(RespActiveBlock::Text { .. })) {
                        self.open_text_block(out);
                    }
                }
            }

            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str)
                    && !delta.is_empty()
                {
                    self.emit_message_start(None, out);
                    let index = match &mut self.active_block {
                        Some(RespActiveBlock::Text { index, had_delta }) => {
                            *had_delta = true;
                            *index
                        }
                        _ => {
                            let i = self.open_text_block(out);
                            if let Some(RespActiveBlock::Text { had_delta, .. }) = &mut self.active_block {
                                *had_delta = true;
                            }
                            i
                        }
                    };
                    out.push(format_sse(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "text_delta", "text": delta }
                        }),
                    ));
                }
            }

            "response.output_text.done" | "response.refusal.done" => {
                let text = data
                    .get("text")
                    .or_else(|| data.get("refusal"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !text.is_empty() {
                    let need_emit = match &self.active_block {
                        Some(RespActiveBlock::Text { had_delta, .. }) => !*had_delta,
                        _ => true,
                    };
                    if need_emit {
                        self.emit_message_start(None, out);
                        let index = match &mut self.active_block {
                            Some(RespActiveBlock::Text { index, had_delta }) => {
                                *had_delta = true;
                                *index
                            }
                            _ => {
                                let i = self.open_text_block(out);
                                if let Some(RespActiveBlock::Text { had_delta, .. }) = &mut self.active_block {
                                    *had_delta = true;
                                }
                                i
                            }
                        };
                        out.push(format_sse(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": index,
                                "delta": { "type": "text_delta", "text": text }
                            }),
                        ));
                    }
                }
            }

            "response.output_text.annotation.added" => {
                if let Some(ann) = data.get("annotation") {
                    merge_web_search_results(
                        &mut self.pending_web_search_annotations,
                        responses_web_search_results_from_annotations(std::slice::from_ref(ann)),
                    );
                }
            }

            "response.content_part.done" => {
                if let Some(anns) = data.pointer("/part/annotations").and_then(Value::as_array) {
                    merge_web_search_results(
                        &mut self.pending_web_search_annotations,
                        responses_web_search_results_from_annotations(anns),
                    );
                }
                if matches!(self.active_block, Some(RespActiveBlock::Text { .. })) {
                    self.close_active_block(out);
                }
            }

            "response.output_item.added" => {
                let Some(item) = data.get("item") else {
                    return;
                };
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    self.has_tool_use = true;
                    self.emit_message_start(None, out);
                    self.close_active_block(out);
                    let idx = self.next_block_index;
                    self.next_block_index += 1;
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .unwrap_or_else(|| format!("call_{idx}"));
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    out.push(format_sse(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "tool_use",
                                "id": call_id,
                                "name": name,
                                "input": {}
                            }
                        }),
                    ));
                    self.active_block = Some(RespActiveBlock::Tool { index: idx, had_delta: false });
                }
            }

            "response.function_call_arguments.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str)
                    && !delta.is_empty()
                    && let Some(RespActiveBlock::Tool { index, had_delta, .. }) = &mut self.active_block
                {
                    *had_delta = true;
                    let idx = *index;
                    out.push(format_sse(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": { "type": "input_json_delta", "partial_json": delta }
                        }),
                    ));
                }
            }

            "response.function_call_arguments.done" => {
                if let Some(RespActiveBlock::Tool { index, had_delta, .. }) = &mut self.active_block {
                    let idx = *index;
                    if !*had_delta
                        && let Some(args) = data.get("arguments").and_then(Value::as_str).filter(|s| !s.is_empty())
                    {
                        *had_delta = true;
                        out.push(format_sse(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": { "type": "input_json_delta", "partial_json": args }
                            }),
                        ));
                    }
                    self.close_active_block(out);
                }
            }

            "response.output_item.done" => {
                let item = data.get("item");
                let item_type = item.and_then(|i| i.get("type")).and_then(Value::as_str);
                if item_type == Some("function_call") {
                    if let Some(RespActiveBlock::Tool { index, had_delta, .. }) = &mut self.active_block {
                        let idx = *index;
                        if !*had_delta
                            && let Some(args) = item
                                .and_then(|i| i.get("arguments"))
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                        {
                            *had_delta = true;
                            out.push(format_sse(
                                "content_block_delta",
                                &json!({
                                    "type": "content_block_delta",
                                    "index": idx,
                                    "delta": { "type": "input_json_delta", "partial_json": args }
                                }),
                            ));
                        }
                        self.close_active_block(out);
                    }
                } else if item_type == Some("web_search_call") {
                    if let Some(item) = item {
                        self.emit_web_search_item(item, None, out);
                    }
                } else if item_type == Some("message") && matches!(self.active_block, Some(RespActiveBlock::Text { .. })) {
                    self.close_active_block(out);
                }
            }

            "response.completed" | "response.incomplete" => {
                let resp = data.get("response").unwrap_or(data);
                if let Some(output) = resp.get("output").and_then(Value::as_array) {
                    // 收集终态 output 中可能补齐的 url_citation，以及未在 item.done 出现的 web_search_call
                    let mut final_citations = std::mem::take(&mut self.pending_web_search_annotations);
                    for item in output {
                        if item.get("type").and_then(Value::as_str) == Some("message")
                            && let Some(parts) = item.get("content").and_then(Value::as_array)
                        {
                            for part in parts {
                                if let Some(anns) = part.get("annotations").and_then(Value::as_array) {
                                    merge_web_search_results(
                                        &mut final_citations,
                                        responses_web_search_results_from_annotations(anns),
                                    );
                                }
                            }
                        }
                    }
                    let unemitted: Vec<&Value> = output
                        .iter()
                        .filter(|it| it.get("type").and_then(Value::as_str) == Some("web_search_call"))
                        .filter(|it| {
                            it.get("id")
                                .and_then(Value::as_str)
                                .is_none_or(|id| !self.emitted_web_search_ids.iter().any(|e| e == id))
                        })
                        .collect();
                    for (i, item) in unemitted.iter().enumerate() {
                        let extra = if i + 1 == unemitted.len() {
                            Some(final_citations.as_slice())
                        } else {
                            None
                        };
                        self.emit_web_search_item(item, extra, out);
                    }
                }
                if let Some(u) = resp.get("usage").filter(|u| u.is_object()) {
                    self.latest_usage = Some(map_responses_usage(Some(u)));
                }
                let status = resp
                    .get("status")
                    .and_then(Value::as_str)
                    .or(if event_name == "response.incomplete" { Some("incomplete") } else { Some("completed") });
                let incomplete_reason = resp.pointer("/incomplete_details/reason").and_then(Value::as_str);
                self.stop_reason = Some(map_responses_stop_reason(status, self.has_tool_use, incomplete_reason));
                self.emit_final(out);
            }

            "response.failed" | "error" => {
                self.ended_with_error = true;
                self.finished = true;
                let resp = data.get("response").unwrap_or(data);
                let err = resp.get("error").unwrap_or(resp);
                let err_type = err
                    .get("type")
                    .or_else(|| err.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("api_error");
                let err_msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| err.as_str())
                    .unwrap_or("上游 Responses 请求失败");
                out.push(format_sse(
                    "error",
                    &json!({
                        "type": "error",
                        "error": { "type": err_type, "message": err_msg }
                    }),
                ));
            }

            _ => {}
        }
    }

    fn emit_message_start(&mut self, first_usage: Option<&Value>, out: &mut Vec<String>) {
        if self.sent_message_start {
            return;
        }
        self.sent_message_start = true;
        if self.message_id.is_empty() {
            self.message_id = ensure_msg_id("");
        }
        let u = map_responses_usage(first_usage);
        out.push(format_sse(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": u["input_tokens"],
                        "output_tokens": 0,
                        "cache_read_input_tokens": u["cache_read_input_tokens"],
                    }
                }
            }),
        ));
    }

    fn open_text_block(&mut self, out: &mut Vec<String>) -> u32 {
        self.close_active_block(out);
        let idx = self.next_block_index;
        self.next_block_index += 1;
        out.push(format_sse(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": { "type": "text", "text": "" }
            }),
        ));
        self.active_block = Some(RespActiveBlock::Text { index: idx, had_delta: false });
        idx
    }

    fn close_active_block(&mut self, out: &mut Vec<String>) {
        if let Some(active) = self.active_block.take() {
            let idx = match active {
                RespActiveBlock::Text { index, .. } | RespActiveBlock::Tool { index, .. } => index,
            };
            out.push(format_sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": idx }),
            ));
        }
    }

    fn emit_web_search_item(&mut self, item: &Value, extra_results: Option<&[Value]>, out: &mut Vec<String>) {
        let sid = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("srvtoolu_{}", self.web_search_count));
        if self.emitted_web_search_ids.iter().any(|id| id == &sid) {
            return;
        }
        self.emitted_web_search_ids.push(sid.clone());
        self.web_search_count += 1;

        self.emit_message_start(None, out);
        self.close_active_block(out);

        let input = responses_web_search_action_input(item);
        let use_idx = self.next_block_index;
        self.next_block_index += 1;
        out.push(format_sse(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": use_idx,
                "content_block": {
                    "type": "server_tool_use",
                    "id": sid,
                    "name": self.web_search_name,
                    "input": {}
                }
            }),
        ));
        let partial = serde_json::to_string(&input).unwrap_or_else(|_| "{}".into());
        if partial != "{}" {
            out.push(format_sse(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": use_idx,
                    "delta": { "type": "input_json_delta", "partial_json": partial }
                }),
            ));
        }
        out.push(format_sse(
            "content_block_stop",
            &json!({ "type": "content_block_stop", "index": use_idx }),
        ));

        let result_content = if let Some(err) = responses_web_search_error(item) {
            err
        } else {
            let mut results = responses_web_search_results_from_action(item);
            if let Some(extra) = extra_results {
                merge_web_search_results(&mut results, extra.to_vec());
            }
            Value::Array(results)
        };
        let res_idx = self.next_block_index;
        self.next_block_index += 1;
        out.push(format_sse(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": res_idx,
                "content_block": {
                    "type": "web_search_tool_result",
                    "tool_use_id": sid,
                    "content": result_content
                }
            }),
        ));
        out.push(format_sse(
            "content_block_stop",
            &json!({ "type": "content_block_stop", "index": res_idx }),
        ));
    }

    fn emit_final(&mut self, out: &mut Vec<String>) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.sent_message_start {
            self.emit_message_start(None, out);
        }
        self.close_active_block(out);
        let mut usage = self.latest_usage.clone().unwrap_or_else(|| map_responses_usage(None));
        attach_web_search_usage(&mut usage, self.web_search_count);
        let stop_reason = self.stop_reason.unwrap_or(if self.has_tool_use { "tool_use" } else { "end_turn" });
        out.push(format_sse(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": stop_reason,
                    "stop_sequence": null
                },
                "usage": usage
            }),
        ));
        out.push(format_sse("message_stop", &json!({"type": "message_stop"})));
    }
}

/// 将上游 OpenAI Responses SSE 字节流实时转为 Anthropic SSE 字节流（边收边发）。
pub fn openai_responses_sse_to_anthropic_sse<E: std::fmt::Display + Send + 'static>(
    upstream: impl Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send {
    openai_responses_sse_to_anthropic_sse_with_web_search(upstream, None)
}

pub fn openai_responses_sse_to_anthropic_sse_with_web_search<E: std::fmt::Display + Send + 'static>(
    upstream: impl Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
    web_search_name: Option<String>,
) -> impl Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut conv = ResponsesSseConverter::with_web_search_name(web_search_name);
        tokio::pin!(upstream);

        while let Some(item) = upstream.next().await {
            match item {
                Ok(bytes) => {
                    for ev in conv.push_bytes(&bytes) {
                        yield Ok(Bytes::from(ev));
                    }
                    if conv.finished {
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
    }
}

// ---------- Google Gemini Native API 转换 ----------

const GEMINI_SYNTH_ID_PREFIX: &str = "gemini_synth_";
const GEMINI_SKIP_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

fn synth_gemini_tool_id() -> String {
    format!("{GEMINI_SYNTH_ID_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

fn is_synth_gemini_tool_id(id: &str) -> bool {
    id.starts_with(GEMINI_SYNTH_ID_PREFIX)
}

/// 判断模型名是否属于 Gemini 3 及以上（多轮无签名 tool call 需补 skip_thought_signature_validator）。
fn requires_gemini_thought_signature(model: &str) -> bool {
    let name = model.rsplit('/').next().unwrap_or(model).trim().to_ascii_lowercase();
    let Some((ver, _)) = name.strip_prefix("gemini-").and_then(|rest| rest.split_once('-')) else {
        return false;
    };
    let major = ver.split('.').next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
    major >= 3
}

/// 判断 schema 是否包含 Gemini `parameters`（受限 OpenAPI 子集）不支持的关键字，需要走 `parametersJsonSchema`。
fn needs_gemini_json_schema(schema: &Value) -> bool {
    match schema {
        Value::Object(obj) => obj.iter().any(|(k, v)| match k.as_str() {
            "type" => v.is_array(),
            "format" | "title" | "description" | "nullable" | "enum" | "maxItems" | "minItems"
            | "required" | "minProperties" | "maxProperties" | "minLength" | "maxLength"
            | "pattern" | "example" | "propertyOrdering" | "default" | "minimum" | "maximum" => false,
            "properties" => v.as_object().is_none_or(|m| m.values().any(needs_gemini_json_schema)),
            "items" => !v.is_object() || needs_gemini_json_schema(v),
            "anyOf" => v.as_array().is_none_or(|a| a.iter().any(needs_gemini_json_schema)),
            _ => true,
        }),
        Value::Array(arr) => arr.iter().any(needs_gemini_json_schema),
        _ => false,
    }
}

fn build_gemini_function_declaration(name: &str, description: Option<&str>, input_schema: Value) -> Value {
    let mut schema = clean_schema(input_schema, true);
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$id");
        if obj.get("type").and_then(Value::as_str) == Some("object") && !obj.contains_key("properties") {
            obj.insert("properties".into(), json!({}));
        }
    }
    let mut decl = Map::new();
    decl.insert("name".into(), json!(name));
    decl.insert("description".into(), json!(description.unwrap_or("")));
    if needs_gemini_json_schema(&schema) {
        decl.insert("parametersJsonSchema".into(), schema);
    } else {
        decl.insert("parameters".into(), schema);
    }
    Value::Object(decl)
}

fn map_gemini_tool_choice(tc: &Value) -> Option<Value> {
    let cfg = match tc {
        Value::String(s) => match s.as_str() {
            "auto" => json!({"mode": "AUTO"}),
            "any" => json!({"mode": "ANY"}),
            "none" => json!({"mode": "NONE"}),
            _ => return None,
        },
        Value::Object(obj) => match obj.get("type").and_then(Value::as_str)? {
            "auto" => json!({"mode": "AUTO"}),
            "any" => json!({"mode": "ANY"}),
            "none" => json!({"mode": "NONE"}),
            "tool" => {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
                json!({"mode": "ANY", "allowedFunctionNames": [name]})
            }
            _ => return None,
        },
        _ => return None,
    };
    Some(json!({"functionCallingConfig": cfg}))
}

fn map_gemini_usage(usage: Option<&Value>) -> Value {
    let prompt = usage.and_then(|u| u.get("promptTokenCount")).and_then(Value::as_u64).unwrap_or(0);
    let output = usage
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            usage
                .and_then(|u| u.get("totalTokenCount"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .saturating_sub(prompt)
        });
    let cached = usage.and_then(|u| u.get("cachedContentTokenCount")).and_then(Value::as_u64).unwrap_or(0);
    // Gemini 的 promptTokenCount 包含 cachedContentTokenCount，Anthropic 的 input_tokens 仅计未命中缓存部分。
    json!({
        "input_tokens": prompt.saturating_sub(cached),
        "output_tokens": output,
        "cache_read_input_tokens": cached,
    })
}

fn map_gemini_stop_reason(reason: Option<&str>, has_tool_use: bool, blocked: bool) -> &'static str {
    if blocked {
        return "refusal";
    }
    match reason {
        Some("MAX_TOKENS") => "max_tokens",
        Some("SAFETY" | "RECITATION" | "SPII" | "BLOCKLIST" | "PROHIBITED_CONTENT") => "refusal",
        _ if has_tool_use => "tool_use",
        _ => "end_turn",
    }
}

/// 合并连续多条均带 tool_result 的 user 消息，满足 Gemini「functionCall 后紧跟单条含全部 functionResponse 的 user turn」约束。
fn coalesce_gemini_tool_result_messages(messages: &[Value]) -> Vec<Value> {
    let is_user = |m: &Value| matches!(m.get("role").and_then(Value::as_str), None | Some("user"));
    let has_tr = |m: &Value| {
        m.get("content")
            .and_then(Value::as_array)
            .is_some_and(|bs| bs.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")))
    };
    let mut out = Vec::with_capacity(messages.len());
    let mut i = 0;
    while i < messages.len() {
        let mut end = i;
        while end < messages.len() && is_user(&messages[end]) {
            end += 1;
        }
        if end == i {
            out.push(messages[i].clone());
            i += 1;
            continue;
        }
        let run = &messages[i..end];
        if run.iter().filter(|m| has_tr(m)).count() > 1 {
            let mut merged = Vec::new();
            for m in run {
                match m.get("content") {
                    Some(Value::String(s)) => merged.push(json!({"type": "text", "text": s})),
                    Some(Value::Array(bs)) => merged.extend(bs.iter().cloned()),
                    _ => {}
                }
            }
            out.push(json!({"role": "user", "content": merged}));
        } else {
            out.extend_from_slice(run);
        }
        i = end;
    }
    out
}

/// 为当前轮次无签名的第一个 functionCall 补上 Gemini 3+ 的 skip_thought_signature_validator 占位符。
fn fill_gemini_skip_thought_signatures(contents: &mut [Value]) {
    let turn_start = contents
        .iter()
        .rposition(|c| {
            c.get("role").and_then(Value::as_str) == Some("user")
                && c.get("parts")
                    .and_then(Value::as_array)
                    .is_some_and(|ps| ps.iter().any(|p| p.get("functionResponse").is_none()))
        })
        .map_or(0, |idx| idx + 1);

    for content in &mut contents[turn_start..] {
        if content.get("role").and_then(Value::as_str) != Some("model") {
            continue;
        }
        if let Some(first_call) = content
            .get_mut("parts")
            .and_then(Value::as_array_mut)
            .and_then(|ps| ps.iter_mut().find(|p| p.get("functionCall").is_some()))
        {
            let has_sig = first_call
                .get("thoughtSignature")
                .or_else(|| first_call.get("thought_signature"))
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            if !has_sig {
                first_call["thoughtSignature"] = json!(GEMINI_SKIP_THOUGHT_SIGNATURE);
            }
        }
    }
}

/// 转换 Anthropic image 内容块 → Gemini inlineData part。
fn convert_image_block_gemini(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    if source.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let mime = source.get("media_type").and_then(Value::as_str).unwrap_or("image/png");
    let data = source.get("data").and_then(Value::as_str).filter(|d| !d.is_empty())?;
    Some(json!({ "inlineData": { "mimeType": mime, "data": data } }))
}

/// Anthropic Messages 请求体 → Google Gemini Native `generateContent` 请求体。
pub fn anthropic_to_gemini(body: &Value) -> Result<Value> {
    let mut result = Map::new();
    let supports_multimodal_fr = body
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(requires_gemini_thought_signature);

    // system → systemInstruction
    if let Some(system) = body.get("system") {
        let text = if let Some(s) = system.as_str() {
            s.to_string()
        } else if let Some(arr) = system.as_array() {
            arr.iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            String::new()
        };
        if !text.is_empty() {
            result.insert("systemInstruction".into(), json!({ "parts": [{ "text": text }] }));
        }
    }

    // messages → contents
    let mut contents = Vec::new();
    if let Some(raw_msgs) = body.get("messages").and_then(Value::as_array) {
        let msgs = coalesce_gemini_tool_result_messages(raw_msgs);

        // 预扫描所有 assistant tool_use，建立 tool_use_id -> name 映射
        let mut tool_name_by_id: HashMap<String, String> = HashMap::new();
        for m in &msgs {
            if m.get("role").and_then(Value::as_str) == Some("assistant")
                && let Some(blocks) = m.get("content").and_then(Value::as_array)
            {
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                        if !id.is_empty() && !name.is_empty() {
                            tool_name_by_id.insert(id.to_string(), name.to_string());
                        }
                    }
                }
            }
        }

        for msg in &msgs {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            let gemini_role = if role == "assistant" { "model" } else { "user" };
            let mut parts = Vec::new();

            match msg.get("content") {
                Some(Value::String(text)) => {
                    if !text.is_empty() {
                        parts.push(json!({ "text": text }));
                    }
                }
                Some(Value::Array(blocks)) => {
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str).unwrap_or("") {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(Value::as_str) {
                                    parts.push(json!({ "text": text }));
                                }
                            }
                            "image" => {
                                if let Some(img) = convert_image_block_gemini(block) {
                                    parts.push(img);
                                }
                            }
                            "tool_use" => {
                                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                                let args = block.get("input").cloned().unwrap_or_else(|| json!({}));
                                let mut fc = json!({ "name": name, "args": args });
                                if !id.is_empty() && !is_synth_gemini_tool_id(id) {
                                    fc["id"] = json!(id);
                                }
                                parts.push(json!({ "functionCall": fc }));
                            }
                            "tool_result" => {
                                let tool_use_id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                                let name = tool_name_by_id
                                    .get(tool_use_id)
                                    .cloned()
                                    .unwrap_or_else(|| "unknown_tool".to_string());
                                let (content_str, media_parts) =
                                    split_tool_result_images(block.get("content"), convert_image_block_gemini);
                                let mut fr = json!({
                                    "name": name,
                                    "response": { "content": content_str }
                                });
                                if !tool_use_id.is_empty() && !is_synth_gemini_tool_id(tool_use_id) {
                                    fr["id"] = json!(tool_use_id);
                                }
                                if supports_multimodal_fr && !media_parts.is_empty() {
                                    fr["parts"] = Value::Array(media_parts);
                                    parts.push(json!({ "functionResponse": fr }));
                                } else {
                                    parts.push(json!({ "functionResponse": fr }));
                                    if !media_parts.is_empty() {
                                        parts.push(json!({
                                            "text": format!("[media output of tool call {tool_use_id}]")
                                        }));
                                        parts.extend(media_parts);
                                    }
                                }
                            }
                            "thinking" | "redacted_thinking" | "server_tool_use" => {}
                            "web_search_tool_result" => {
                                let summary = summarize_web_search_result_block(block);
                                if !summary.is_empty() {
                                    parts.push(json!({ "text": summary }));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }

            if parts.is_empty() {
                continue;
            }

            // Gemini 不允许同一 user turn 混排 functionResponse 和文本，拆分发出
            if gemini_role == "user"
                && parts.iter().any(|p| p.get("functionResponse").is_some())
                && parts.iter().any(|p| p.get("functionResponse").is_none())
            {
                let (frs, rest): (Vec<Value>, Vec<Value>) =
                    parts.into_iter().partition(|p| p.get("functionResponse").is_some());
                contents.push(json!({ "role": "user", "parts": frs }));
                contents.push(json!({ "role": "user", "parts": rest }));
            } else {
                contents.push(json!({ "role": gemini_role, "parts": parts }));
            }
        }
    }

    if body.get("model").and_then(Value::as_str).is_some_and(requires_gemini_thought_signature) {
        fill_gemini_skip_thought_signatures(&mut contents);
    }
    result.insert("contents".into(), Value::Array(contents));

    // generationConfig
    let mut gen_cfg = Map::new();
    if let Some(v) = body.get("max_tokens").filter(|v| !v.is_null()) {
        gen_cfg.insert("maxOutputTokens".into(), v.clone());
    }
    if let Some(v) = body.get("temperature").filter(|v| !v.is_null()) {
        gen_cfg.insert("temperature".into(), v.clone());
    }
    if let Some(v) = body.get("top_p").filter(|v| !v.is_null()) {
        gen_cfg.insert("topP".into(), v.clone());
    }
    if let Some(v) = body.get("stop_sequences").filter(|v| !v.is_null()) {
        gen_cfg.insert("stopSequences".into(), v.clone());
    }
    if !gen_cfg.is_empty() {
        result.insert("generationConfig".into(), Value::Object(gen_cfg));
    }

    // tools：区分普通函数工具与 Anthropic 托管 web_search 工具。
    // Google Gemini / Cloud Code Assist v1internal 不允许 googleSearch 与 functionDeclarations 混用，
    // 仅在请求里只有 web_search 工具（如 Claude Code 内置 WebSearch 专用请求）时启用 googleSearch。
    let mut web_search_names: Vec<&str> = Vec::new();
    let mut has_function_decls = false;
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mut decls: Vec<Value> = Vec::new();
        for t in tools {
            if is_anthropic_web_search_tool(t) {
                if let Some(name) = t.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    web_search_names.push(name);
                }
                continue;
            }
            let Some(name) = t.get("name").and_then(Value::as_str) else {
                continue;
            };
            let desc = t.get("description").and_then(Value::as_str);
            let schema = t.get("input_schema").cloned().unwrap_or_else(|| json!({}));
            decls.push(build_gemini_function_declaration(name, desc, schema));
        }
        if !decls.is_empty() {
            has_function_decls = true;
            result.insert("tools".into(), json!([{ "functionDeclarations": decls }]));
        } else if !web_search_names.is_empty() {
            result.insert("tools".into(), json!([{ "googleSearch": {} }]));
        }
    }

    // toolConfig（仅在存在 functionDeclarations 且 tool_choice 不指向 web_search 时下发）
    let tc_points_to_ws = body
        .get("tool_choice")
        .and_then(Value::as_object)
        .filter(|o| o.get("type").and_then(Value::as_str) == Some("tool"))
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|n| web_search_names.contains(&n));
    if has_function_decls
        && !tc_points_to_ws
        && let Some(tc) = body.get("tool_choice").and_then(map_gemini_tool_choice)
    {
        result.insert("toolConfig".into(), tc);
    }

    Ok(Value::Object(result))
}

/// 从 Gemini candidate 的 `groundingMetadata` 中提取搜索词与结果链接列表。
fn extract_gemini_grounding(candidate: &Value, queries: &mut Vec<String>, results: &mut Vec<Value>) {
    let Some(gm) = candidate.get("groundingMetadata").and_then(Value::as_object) else {
        return;
    };
    if let Some(qs) = gm.get("webSearchQueries").and_then(Value::as_array) {
        for q in qs {
            if let Some(s) = q.as_str().map(str::trim).filter(|s| !s.is_empty())
                && !queries.iter().any(|e| e == s)
            {
                queries.push(s.to_string());
            }
        }
    }
    if let Some(chunks) = gm.get("groundingChunks").and_then(Value::as_array) {
        for ch in chunks {
            if let Some(web) = ch.get("web").and_then(Value::as_object)
                && let Some(uri) = web.get("uri").and_then(Value::as_str).filter(|s| !s.is_empty())
            {
                let title = web.get("title").and_then(Value::as_str);
                merge_web_search_results(results, vec![make_web_search_result(uri, title, None)]);
            }
        }
    }
}

/// 按 Gemini `groundingMetadata` 构造 Anthropic 的 `server_tool_use` + `web_search_tool_result` 块对。
fn build_gemini_grounding_blocks(
    web_search_name: &str,
    queries: &[String],
    results: &[Value],
) -> Option<(Value, Value)> {
    if queries.is_empty() && results.is_empty() {
        return None;
    }
    let sid = format!("srvtoolu_{}", uuid::Uuid::new_v4().simple());
    let query = queries.join("; ");
    let input = if query.is_empty() { json!({}) } else { json!({ "query": query }) };
    let use_blk = json!({
        "type": "server_tool_use",
        "id": sid,
        "name": web_search_name,
        "input": input,
    });
    let res_blk = json!({
        "type": "web_search_tool_result",
        "tool_use_id": sid,
        "content": Value::Array(results.to_vec()),
    });
    Some((use_blk, res_blk))
}

/// Google Gemini Native 非流式响应 → Anthropic Messages 响应。
pub fn gemini_to_anthropic(body: &Value) -> Result<Value> {
    gemini_to_anthropic_with_web_search(body, None)
}

pub fn gemini_to_anthropic_with_web_search(body: &Value, web_search_name: Option<&str>) -> Result<Value> {
    // Cloud Code Assist (v1internal) 把真实 Gemini 响应包在 {"response": {...}} 里
    let body = body.get("response").filter(|r| r.is_object()).unwrap_or(body);
    let id = ensure_msg_id(body.get("responseId").and_then(Value::as_str).unwrap_or(""));
    let model = body.get("modelVersion").and_then(Value::as_str).unwrap_or("");
    let mut usage = map_gemini_usage(body.get("usageMetadata"));

    if let Some(reason) = body.pointer("/promptFeedback/blockReason").and_then(Value::as_str) {
        return Ok(json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": format!("请求被 Gemini 安全策略拦截：{reason}")}],
            "stop_reason": "refusal",
            "stop_sequence": null,
            "usage": usage,
        }));
    }

    let Some(candidate) = body.get("candidates").and_then(Value::as_array).and_then(|c| c.first()) else {
        bail!("Gemini 响应里没有 candidates");
    };

    let mut content = Vec::new();
    let mut has_tool_use = false;

    let mut grounding_queries = Vec::new();
    let mut grounding_results = Vec::new();
    extract_gemini_grounding(candidate, &mut grounding_queries, &mut grounding_results);
    let ws_name = web_search_name.filter(|s| !s.is_empty()).unwrap_or("web_search");
    let mut ws_count = 0u64;
    if let Some((use_blk, res_blk)) = build_gemini_grounding_blocks(ws_name, &grounding_queries, &grounding_results) {
        content.push(use_blk);
        content.push(res_blk);
        ws_count = 1;
    }

    if let Some(parts) = candidate.pointer("/content/parts").and_then(Value::as_array) {
        for part in parts {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                if let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    let mut blk = json!({"type": "thinking", "thinking": text});
                    if let Some(sig) = part
                        .get("thoughtSignature")
                        .or_else(|| part.get("thought_signature"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        blk["signature"] = json!(sig);
                    }
                    content.push(blk);
                }
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                continue;
            }
            if let Some(fc) = part.get("functionCall") {
                has_tool_use = true;
                let call_id = fc
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(synth_gemini_tool_id);
                let name = fc.get("name").and_then(Value::as_str).unwrap_or("");
                let args = match fc.get("args") {
                    Some(v @ Value::Object(_)) => v.clone(),
                    _ => json!({}),
                };
                content.push(json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": args,
                }));
            }
        }
    }

    let stop_reason = map_gemini_stop_reason(candidate.get("finishReason").and_then(Value::as_str), has_tool_use, false);
    attach_web_search_usage(&mut usage, ws_count);

    Ok(json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage,
    }))
}

/// 将 Gemini / Cloud Code Assist SSE 字节流收集为单个 Anthropic Messages 非流式 JSON 响应。
/// 用于 Antigravity 上游统一走 `v1internal:streamGenerateContent?alt=sse`（与原版 agy CLI 一致），
/// 同时兼容客户端请求 `stream: false` 的场景。
pub fn collect_gemini_sse_to_anthropic(bytes: &[u8]) -> Result<Value> {
    collect_gemini_sse_to_anthropic_with_web_search(bytes, None)
}

pub fn collect_gemini_sse_to_anthropic_with_web_search(bytes: &[u8], web_search_name: Option<&str>) -> Result<Value> {
    let text = String::from_utf8_lossy(bytes);
    let mut id = String::new();
    let mut model = String::new();
    let mut acc_thinking = String::new();
    let mut thinking_sig = String::new();
    let mut acc_text = String::new();
    let mut tools: Vec<Value> = Vec::new();
    let mut grounding_queries: Vec<String> = Vec::new();
    let mut grounding_results: Vec<Value> = Vec::new();
    let mut finish_reason: Option<String> = None;
    let mut latest_usage: Option<Value> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let data = rest.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("Gemini 上游返回错误");
            bail!("{msg}");
        }
        let chunk = v.get("response").filter(|r| r.is_object()).unwrap_or(&v);
        if id.is_empty()
            && let Some(s) = chunk.get("responseId").and_then(Value::as_str).filter(|s| !s.is_empty())
        {
            id = s.to_string();
        }
        if model.is_empty()
            && let Some(s) = chunk.get("modelVersion").and_then(Value::as_str).filter(|s| !s.is_empty())
        {
            model = s.to_string();
        }
        if let Some(u) = chunk.get("usageMetadata").filter(|u| u.is_object()) {
            latest_usage = Some(u.clone());
        }
        if let Some(cand) = chunk.get("candidates").and_then(Value::as_array).and_then(|c| c.first()) {
            extract_gemini_grounding(cand, &mut grounding_queries, &mut grounding_results);
            if let Some(parts) = cand.pointer("/content/parts").and_then(Value::as_array) {
                for p in parts {
                    if p.get("thought").and_then(Value::as_bool) == Some(true) {
                        if let Some(t) = p.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            if !acc_thinking.is_empty() && t.starts_with(&acc_thinking) {
                                acc_thinking = t.to_string();
                            } else {
                                acc_thinking.push_str(t);
                            }
                        }
                        if let Some(sig) = p
                            .get("thoughtSignature")
                            .or_else(|| p.get("thought_signature"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            thinking_sig = sig.to_string();
                        }
                        continue;
                    }
                    if let Some(t) = p.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            if !acc_text.is_empty() && t.starts_with(&acc_text) {
                                acc_text = t.to_string();
                            } else {
                                acc_text.push_str(t);
                            }
                        }
                    } else if let Some(fc) = p.get("functionCall").and_then(Value::as_object) {
                        let name = fc.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                        let call_id = fc
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .unwrap_or_else(|| format!("call_{}", tools.len()));
                        let args = fc.get("args").cloned().filter(|v| v.is_object()).unwrap_or_else(|| json!({}));
                        tools.push(json!({ "type": "tool_use", "id": call_id, "name": name, "input": args }));
                    }
                }
            }
            if let Some(fr) = cand.get("finishReason").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                finish_reason = Some(fr.to_string());
            }
        }
    }

    let mut content = Vec::new();
    if !acc_thinking.is_empty() {
        let mut blk = json!({ "type": "thinking", "thinking": acc_thinking });
        if !thinking_sig.is_empty() {
            blk["signature"] = json!(thinking_sig);
        }
        content.push(blk);
    }
    let ws_name = web_search_name.filter(|s| !s.is_empty()).unwrap_or("web_search");
    let mut ws_count = 0u64;
    if let Some((use_blk, res_blk)) = build_gemini_grounding_blocks(ws_name, &grounding_queries, &grounding_results) {
        content.push(use_blk);
        content.push(res_blk);
        ws_count = 1;
    }
    if !acc_text.is_empty() {
        content.push(json!({ "type": "text", "text": acc_text }));
    }
    let has_tool_use = !tools.is_empty();
    content.extend(tools);
    let stop_reason = map_gemini_stop_reason(finish_reason.as_deref(), has_tool_use, false);
    let mut usage = map_gemini_usage(latest_usage.as_ref());
    attach_web_search_usage(&mut usage, ws_count);
    Ok(json!({
        "id": ensure_msg_id(&id),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage,
    }))
}

#[derive(Clone)]
struct GeminiToolSnap {
    id: String,
    name: String,
    args: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiActiveBlock {
    Thinking(u32),
    Text(u32),
}

/// 增量 Google Gemini SSE → Anthropic SSE 状态机，支持任意字节分块、思考块、grounding 联网搜索与增量/累积文本。
pub struct GeminiSseConverter {
    buf: Vec<u8>,
    message_id: String,
    model: String,
    /// 请求携带了托管 web_search 时非空；开启时会缓冲可见文本，待终包 groundingMetadata 到达后按
    /// `server_tool_use` → `web_search_tool_result` → `text` 的 Anthropic 标准顺序输出。
    hosted_web_search_name: Option<String>,
    sent_message_start: bool,
    next_block_index: u32,
    active_block: Option<GeminiActiveBlock>,
    accumulated_thinking: String,
    accumulated_text: String,
    tool_calls: Vec<GeminiToolSnap>,
    grounding_queries: Vec<String>,
    grounding_results: Vec<Value>,
    latest_usage: Option<Value>,
    finish_reason: Option<String>,
    blocked_text: Option<String>,
    finished: bool,
    ended_with_error: bool,
}

impl Default for GeminiSseConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiSseConverter {
    pub fn new() -> Self {
        Self::with_web_search_name(None)
    }

    pub fn with_web_search_name(web_search_name: Option<String>) -> Self {
        Self {
            buf: Vec::new(),
            message_id: String::new(),
            model: String::new(),
            hosted_web_search_name: web_search_name.filter(|s| !s.is_empty()),
            sent_message_start: false,
            next_block_index: 0,
            active_block: None,
            accumulated_thinking: String::new(),
            accumulated_text: String::new(),
            tool_calls: Vec::new(),
            grounding_queries: Vec::new(),
            grounding_results: Vec::new(),
            latest_usage: None,
            finish_reason: None,
            blocked_text: None,
            finished: false,
            ended_with_error: false,
        }
    }

    /// 抢先发出 `message_start` 和一个空的 `thinking` 内容块，让客户端尽早切入 thinking 态，
    /// 避免上游首包思考静默期间被 Claude Code 判定为卡死；后续上游到达的 thought 会直接续写该块。
    pub fn start_early_thinking(&mut self, model: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.model.is_empty() && !model.is_empty() {
            self.model = model.to_string();
        }
        if !self.sent_message_start {
            self.emit_message_start(None, &mut out);
        }
        self.ensure_thinking_block(&mut out);
        out
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// 供已抢先开启 SSE 流后上报上游错误使用（保留 Anthropic 错误结构）。
    pub fn error_event_from_payload(&mut self, payload: &Value) -> String {
        self.finished = true;
        self.ended_with_error = true;
        format_sse("error", payload)
    }

    /// 标准 Anthropic SSE 心跳事件：用于上游长时间静默思考时保持连接活跃，防止客户端看门狗误报断流。
    pub fn ping_event() -> &'static str {
        "event: ping\ndata: {\"type\":\"ping\"}\n\n"
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();

        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line_bytes = self.buf.drain(..=pos).collect::<Vec<u8>>();
            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            self.process_line(&line, &mut out);
            if self.finished {
                self.buf.clear();
                break;
            }
        }
        out
    }

    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if !self.buf.is_empty() {
            let mut line_bytes = std::mem::take(&mut self.buf);
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            self.process_line(&line, &mut out);
        }
        if !self.finished && !self.ended_with_error {
            self.emit_final(&mut out);
        }
        out
    }

    pub fn error_event(&mut self, msg: String) -> String {
        self.finished = true;
        self.ended_with_error = true;
        format_sse(
            "error",
            &json!({
                "type": "error",
                "error": { "type": "api_error", "message": msg }
            }),
        )
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<String>) {
        let Some(rest) = line.strip_prefix("data:") else {
            return;
        };
        let data = rest.trim();
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.emit_final(out);
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return;
        };

        if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
            self.ended_with_error = true;
            self.finished = true;
            let err_type = err.get("status").or_else(|| err.get("type")).and_then(Value::as_str).unwrap_or("api_error");
            let err_msg = err.get("message").and_then(Value::as_str).unwrap_or("Gemini 上游返回错误");
            out.push(format_sse(
                "error",
                &json!({ "type": "error", "error": { "type": err_type, "message": err_msg } }),
            ));
            return;
        }

        // Cloud Code Assist (v1internal) 的 SSE chunk 包在 {"response": {...}} 里
        let chunk = chunk.get("response").filter(|r| r.is_object()).unwrap_or(&chunk);

        if self.message_id.is_empty()
            && let Some(id) = chunk.get("responseId").and_then(Value::as_str).filter(|s| !s.is_empty())
        {
            self.message_id = ensure_msg_id(id);
        }
        if self.model.is_empty()
            && let Some(m) = chunk.get("modelVersion").and_then(Value::as_str).filter(|s| !s.is_empty())
        {
            self.model = m.to_string();
        }
        if let Some(u) = chunk.get("usageMetadata").filter(|u| u.is_object()) {
            self.latest_usage = Some(map_gemini_usage(Some(u)));
        }

        if !self.sent_message_start {
            self.emit_message_start(chunk.get("usageMetadata").filter(|u| u.is_object()), out);
        }

        if let Some(reason) = chunk.pointer("/promptFeedback/blockReason").and_then(Value::as_str) {
            self.blocked_text = Some(format!("请求被 Gemini 安全策略拦截：{reason}"));
        }

        let Some(candidate) = chunk.get("candidates").and_then(Value::as_array).and_then(|c| c.first()) else {
            return;
        };
        extract_gemini_grounding(candidate, &mut self.grounding_queries, &mut self.grounding_results);
        if let Some(fr) = candidate.get("finishReason").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            self.finish_reason = Some(fr.to_string());
        }
        let Some(parts) = candidate.pointer("/content/parts").and_then(Value::as_array) else {
            return;
        };

        // 1. 提取思考内容（thought: true）→ Anthropic thinking / thinking_delta
        let thought_text: String = parts
            .iter()
            .filter(|p| p.get("thought").and_then(Value::as_bool) == Some(true))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        let thought_sig: Option<&str> = parts
            .iter()
            .filter(|p| p.get("thought").and_then(Value::as_bool) == Some(true))
            .find_map(|p| {
                p.get("thoughtSignature")
                    .or_else(|| p.get("thought_signature"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            });
        if !thought_text.is_empty() {
            let is_cumulative =
                !self.accumulated_thinking.is_empty() && thought_text.starts_with(&self.accumulated_thinking);
            let delta = if is_cumulative {
                &thought_text[self.accumulated_thinking.len()..]
            } else {
                thought_text.as_str()
            };
            if !delta.is_empty() {
                let idx = self.ensure_thinking_block(out);
                out.push(format_sse(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "thinking_delta", "thinking": delta }
                    }),
                ));
                if is_cumulative {
                    self.accumulated_thinking = thought_text;
                } else {
                    self.accumulated_thinking.push_str(delta);
                }
            }
        }
        if let Some(sig) = thought_sig
            && let Some(GeminiActiveBlock::Thinking(idx)) = self.active_block
        {
            out.push(format_sse(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": { "type": "signature_delta", "signature": sig }
                }),
            ));
        }

        // 2. 提取可见文本（跳过 thought: true）→ Anthropic text / text_delta
        // 托管 web_search 模式下先缓冲正文，等终包 groundingMetadata 到达后再按标准顺序吐出
        let visible_text: String = parts
            .iter()
            .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        if !visible_text.is_empty() {
            let is_cumulative = !self.accumulated_text.is_empty() && visible_text.starts_with(&self.accumulated_text);
            let delta = if is_cumulative { &visible_text[self.accumulated_text.len()..] } else { visible_text.as_str() };
            if !delta.is_empty() {
                if self.hosted_web_search_name.is_none() {
                    let idx = self.ensure_text_block(out);
                    out.push(format_sse(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": { "type": "text_delta", "text": delta }
                        }),
                    ));
                }
                if is_cumulative {
                    self.accumulated_text = visible_text;
                } else {
                    self.accumulated_text.push_str(delta);
                }
            }
        }

        // 收集 functionCall（按 id 或位置去重合并）
        let incoming: Vec<(Option<String>, String, Value)> = parts
            .iter()
            .filter_map(|p| {
                let fc = p.get("functionCall")?;
                let id = fc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
                let name = fc.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let args = match fc.get("args") {
                    Some(v @ Value::Object(_)) => v.clone(),
                    _ => json!({}),
                };
                Some((id, name, args))
            })
            .collect();
        for (pos, (id, name, args)) in incoming.into_iter().enumerate() {
            let existing = match id.as_deref() {
                Some(inc_id) => self.tool_calls.iter().position(|t| t.id == inc_id),
                None => self.tool_calls.get(pos).filter(|t| is_synth_gemini_tool_id(&t.id)).map(|_| pos),
            };
            if let Some(i) = existing {
                self.tool_calls[i].name = name;
                self.tool_calls[i].args = args;
                if let Some(real_id) = id {
                    self.tool_calls[i].id = real_id;
                }
            } else {
                self.tool_calls.push(GeminiToolSnap {
                    id: id.unwrap_or_else(synth_gemini_tool_id),
                    name,
                    args,
                });
            }
        }
    }

    fn emit_message_start(&mut self, first_usage: Option<&Value>, out: &mut Vec<String>) {
        if self.sent_message_start {
            return;
        }
        self.sent_message_start = true;
        if self.message_id.is_empty() {
            self.message_id = ensure_msg_id("");
        }
        let u = map_gemini_usage(first_usage);
        out.push(format_sse(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": u["input_tokens"],
                        "output_tokens": 0,
                        "cache_read_input_tokens": u["cache_read_input_tokens"],
                    }
                }
            }),
        ));
    }

    fn close_active_block(&mut self, out: &mut Vec<String>) {
        if let Some(active) = self.active_block.take() {
            let idx = match active {
                GeminiActiveBlock::Thinking(i) | GeminiActiveBlock::Text(i) => i,
            };
            out.push(format_sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": idx }),
            ));
        }
    }

    fn ensure_thinking_block(&mut self, out: &mut Vec<String>) -> u32 {
        if let Some(GeminiActiveBlock::Thinking(i)) = self.active_block {
            return i;
        }
        self.close_active_block(out);
        let i = self.next_block_index;
        self.next_block_index += 1;
        self.active_block = Some(GeminiActiveBlock::Thinking(i));
        out.push(format_sse(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": i,
                "content_block": { "type": "thinking", "thinking": "" }
            }),
        ));
        i
    }

    fn ensure_text_block(&mut self, out: &mut Vec<String>) -> u32 {
        if let Some(GeminiActiveBlock::Text(i)) = self.active_block {
            return i;
        }
        self.close_active_block(out);
        let i = self.next_block_index;
        self.next_block_index += 1;
        self.active_block = Some(GeminiActiveBlock::Text(i));
        out.push(format_sse(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": i,
                "content_block": { "type": "text", "text": "" }
            }),
        ));
        i
    }

    fn emit_final(&mut self, out: &mut Vec<String>) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.sent_message_start {
            self.emit_message_start(None, out);
        }

        if self.accumulated_text.is_empty()
            && let Some(blocked) = self.blocked_text.clone()
        {
            let idx = self.ensure_text_block(out);
            out.push(format_sse(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": { "type": "text_delta", "text": blocked }
                }),
            ));
        }

        self.close_active_block(out);

        let ws_name = self.hosted_web_search_name.as_deref().unwrap_or("web_search");
        let mut ws_count = 0u64;
        if let Some((use_blk, res_blk)) =
            build_gemini_grounding_blocks(ws_name, &self.grounding_queries, &self.grounding_results)
        {
            ws_count = 1;
            let use_idx = self.next_block_index;
            self.next_block_index += 1;
            let sid = use_blk["id"].as_str().unwrap_or("");
            let input_json = serde_json::to_string(&use_blk["input"]).unwrap_or_else(|_| "{}".into());
            out.push(format_sse(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": use_idx,
                    "content_block": {
                        "type": "server_tool_use",
                        "id": sid,
                        "name": ws_name,
                        "input": {}
                    }
                }),
            ));
            if input_json != "{}" {
                out.push(format_sse(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": use_idx,
                        "delta": { "type": "input_json_delta", "partial_json": input_json }
                    }),
                ));
            }
            out.push(format_sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": use_idx }),
            ));

            let res_idx = self.next_block_index;
            self.next_block_index += 1;
            out.push(format_sse(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": res_idx,
                    "content_block": res_blk
                }),
            ));
            out.push(format_sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": res_idx }),
            ));
        }

        // 托管 web_search 模式下缓冲的正文在搜索结果块之后吐出
        if self.hosted_web_search_name.is_some() && !self.accumulated_text.is_empty() {
            let text = self.accumulated_text.clone();
            let idx = self.ensure_text_block(out);
            out.push(format_sse(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": { "type": "text_delta", "text": text }
                }),
            ));
            self.close_active_block(out);
        }

        for tc in &self.tool_calls {
            let idx = self.next_block_index;
            self.next_block_index += 1;
            out.push(format_sse(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": {}
                    }
                }),
            ));
            out.push(format_sse(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": serde_json::to_string(&tc.args).unwrap_or_else(|_| "{}".into())
                    }
                }),
            ));
            out.push(format_sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": idx }),
            ));
        }

        let stop_reason = map_gemini_stop_reason(
            self.finish_reason.as_deref(),
            !self.tool_calls.is_empty(),
            self.blocked_text.is_some(),
        );
        let mut usage = self.latest_usage.clone().unwrap_or_else(|| map_gemini_usage(None));
        attach_web_search_usage(&mut usage, ws_count);
        out.push(format_sse(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": stop_reason,
                    "stop_sequence": null
                },
                "usage": usage
            }),
        ));
        out.push(format_sse("message_stop", &json!({"type": "message_stop"})));
    }
}

/// 将上游 Google Gemini SSE 字节流实时转为 Anthropic SSE 字节流（边收边发）。
/// 若指定 `keepalive`，上游长时间无新数据时定期发 Anthropic `ping` 保活。
pub fn gemini_sse_to_anthropic_sse<E: std::fmt::Display + Send + 'static>(
    upstream: impl Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send {
    gemini_sse_to_anthropic_sse_with_options(upstream, None, None)
}

pub fn gemini_sse_to_anthropic_sse_with_keepalive<E: std::fmt::Display + Send + 'static>(
    upstream: impl Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
    keepalive: Option<std::time::Duration>,
) -> impl Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send {
    gemini_sse_to_anthropic_sse_with_options(upstream, keepalive, None)
}

pub fn gemini_sse_to_anthropic_sse_with_options<E: std::fmt::Display + Send + 'static>(
    upstream: impl Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
    keepalive: Option<std::time::Duration>,
    web_search_name: Option<String>,
) -> impl Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut conv = GeminiSseConverter::with_web_search_name(web_search_name);
        tokio::pin!(upstream);

        loop {
            let next_item = match keepalive {
                Some(interval) => tokio::select! {
                    item = upstream.next() => Some(item),
                    _ = tokio::time::sleep(interval) => None,
                },
                None => Some(upstream.next().await),
            };
            let Some(item) = next_item else {
                yield Ok(Bytes::from_static(GeminiSseConverter::ping_event().as_bytes()));
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
                    if conv.finished {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_sse(raw: &str) -> Vec<(String, Value)> {
        raw.split("\n\n")
            .filter(|block| !block.trim().is_empty())
            .map(|block| {
                let mut event = String::new();
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim().to_string();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data = v.trim().to_string();
                    }
                }
                (event, serde_json::from_str(&data).expect("SSE data 应为合法 JSON"))
            })
            .collect()
    }

    #[test]
    fn request_full_mapping() {
        let req = json!({
            "model": "gpt-4o",
            "system": [
                {"type": "text", "text": "你是助手", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": ""},
                {"type": "text", "text": "请简洁回答"}
            ],
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "看这张图"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "QUJD"}},
                        {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}}
                    ]
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "先想一下"},
                        {"type": "redacted_thinking", "data": "secret"},
                        {"type": "text", "text": "我来查一下"},
                        {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.txt"}},
                        {"type": "tool_use", "id": "toolu_2", "name": "list_dir", "input": {"dir": "."}}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "hello"},
                        {"type": "tool_result", "tool_use_id": "toolu_2", "content": [{"type": "text", "text": "line1"}, {"type": "text", "text": "line2"}]}
                    ]
                }
            ],
            "tools": [
                {
                    "name": "read_file",
                    "description": "读文件",
                    "input_schema": {
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "cache_control": {"type": "ephemeral"},
                        "properties": {
                            "path": {"type": "string", "cache_control": {"type": "ephemeral"}}
                        },
                        "required": ["path"]
                    }
                },
                {
                    "name": "ping",
                    "description": "",
                    "input_schema": {}
                }
            ],
            "tool_choice": {"type": "tool", "name": "read_file"},
            "max_tokens": 1024,
            "temperature": 0.5,
            "top_p": 0.9,
            "stop_sequences": ["END"],
            "stream": true,
            "metadata": {"user_id": "u1"},
            "thinking": {"type": "enabled", "budget_tokens": 2000}
        });

        let out = anthropic_to_openai_chat(&req).unwrap();
        assert_eq!(out["model"], "gpt-4o");
        assert_eq!(out["max_tokens"], 1024);
        assert!(out.get("max_completion_tokens").is_none());
        assert_eq!(out["temperature"], 0.5);
        assert_eq!(out["top_p"], 0.9);
        assert_eq!(out["stop"], json!(["END"]));
        assert_eq!(out["stream"], true);
        assert_eq!(out["stream_options"], json!({"include_usage": true}));
        // 不透传 Anthropic 专有字段
        assert!(out.get("metadata").is_none());
        assert!(out.get("thinking").is_none());

        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0], json!({"role": "system", "content": "你是助手\n请简洁回答"}));
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"][0], json!({"type": "text", "text": "看这张图"}));
        assert_eq!(msgs[1]["content"][1], json!({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,QUJD"}}));
        assert_eq!(msgs[1]["content"][2], json!({"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}));

        // assistant 消息丢弃 thinking/redacted_thinking，保留文本和 tool_calls
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "我来查一下");
        let tcs = msgs[2]["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0]["id"], "toolu_1");
        assert_eq!(tcs[0]["function"]["name"], "read_file");
        assert_eq!(serde_json::from_str::<Value>(tcs[0]["function"]["arguments"].as_str().unwrap()).unwrap(), json!({"path": "a.txt"}));

        // 两个 tool_result 各发一条 role: tool
        assert_eq!(msgs[3], json!({"role": "tool", "tool_call_id": "toolu_1", "content": "hello"}));
        assert_eq!(msgs[4], json!({"role": "tool", "tool_call_id": "toolu_2", "content": "line1\nline2"}));

        // tools schema 清理
        let tools = out["tools"].as_array().unwrap();
        assert_eq!(tools[0]["function"]["description"], "读文件");
        let params0 = &tools[0]["function"]["parameters"];
        assert_eq!(params0["type"], "object");
        assert!(params0.get("$schema").is_none());
        assert!(params0.get("cache_control").is_none());
        assert!(params0["properties"]["path"].get("cache_control").is_none());
        // 空 description 省略
        assert!(tools[1]["function"].get("description").is_none());
        assert_eq!(tools[1]["function"]["parameters"]["type"], "object");

        // tool_choice
        assert_eq!(out["tool_choice"], json!({"type": "function", "function": {"name": "read_file"}}));
    }

    #[test]
    fn request_o_series_and_tool_choice_and_empty_system() {
        for model in ["o1", "o1-mini", "o3-mini", "o4-mini", "openai/o3"] {
            let out = anthropic_to_openai_chat(&json!({
                "model": model,
                "system": "",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 500,
                "tool_choice": {"type": "any"}
            }))
            .unwrap();
            assert_eq!(out["max_completion_tokens"], 500, "{model}");
            assert!(out.get("max_tokens").is_none(), "{model}");
            assert_eq!(out["tool_choice"], "required");
            assert_eq!(out["messages"].as_array().unwrap().len(), 1, "空 system 不发");
            assert!(out.get("stream_options").is_none());
        }

        let out_auto = anthropic_to_openai_chat(&json!({"model": "gpt-4o", "messages": [], "tool_choice": {"type": "auto"}})).unwrap();
        assert_eq!(out_auto["tool_choice"], "auto");
        let out_none = anthropic_to_openai_chat(&json!({"model": "gpt-4o", "messages": [], "tool_choice": {"type": "none"}})).unwrap();
        assert_eq!(out_none["tool_choice"], "none");
    }

    #[test]
    fn non_streaming_response_mapping() {
        let up = json!({
            "id": "chatcmpl-123",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "reasoning_content": "应该被丢弃的思考",
                    "content": "好的，马上执行",
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "run", "arguments": "{\"cmd\":\"ls\"}"}
                        },
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": {"name": "broken", "arguments": "{not json"}
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 120,
                "completion_tokens": 35,
                "prompt_tokens_details": {"cached_tokens": 40}
            }
        });
        let out = openai_chat_to_anthropic(&up).unwrap();
        assert_eq!(out["id"], "msg_chatcmpl-123");
        assert_eq!(out["type"], "message");
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["model"], "gpt-4o");
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["stop_sequence"], Value::Null);
        assert_eq!(
            out["usage"],
            json!({"input_tokens": 80, "output_tokens": 35, "cache_read_input_tokens": 40})
        );
        let content = out["content"].as_array().unwrap();
        assert_eq!(content.len(), 3, "reasoning_content 应被丢弃");
        assert_eq!(content[0], json!({"type": "text", "text": "好的，马上执行"}));
        assert_eq!(content[1], json!({"type": "tool_use", "id": "call_1", "name": "run", "input": {"cmd": "ls"}}));
        assert_eq!(content[2], json!({"type": "tool_use", "id": "call_2", "name": "broken", "input": {}}));
    }

    #[test]
    fn streaming_chunked_text_and_tools() {
        let sse_input = concat!(
            "data: {\"id\":\"cmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好\"}}]}\n\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"世界\"}}]}\n\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"p\\\"\"}}]}}]}\n\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":\\\"x\\\"}\"}}]}}]}\n\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"write\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"id\":\"cmpl-1\",\"model\":\"gpt-4o\",\"choices\":[],\"usage\":{\"prompt_tokens\":80,\"completion_tokens\":22,\"prompt_tokens_details\":{\"cached_tokens\":10}}}\n\n",
            "data: [DONE]\n\n"
        );

        // 故意按 7 字节切块（会劈开多字节 UTF-8 中文和行边界）
        let mut conv = SseConverter::new();
        let mut raw = String::new();
        for piece in sse_input.as_bytes().chunks(7) {
            for ev in conv.push_bytes(piece) {
                raw.push_str(&ev);
            }
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }

        let events = parse_sse(&raw);
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start", // index 0: text
                "content_block_delta", // "你好"
                "content_block_delta", // "世界"
                "content_block_stop",  // stop text 0
                "content_block_start", // index 1: tool read
                "content_block_delta", // "{\"p\""
                "content_block_delta", // ":\"x\"}"
                "content_block_stop",  // stop tool 1
                "content_block_start", // index 2: tool write
                "content_block_delta", // "{}"
                "content_block_stop",  // stop tool 2
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[0].1["message"]["id"], "msg_cmpl-1");
        assert_eq!(events[0].1["message"]["model"], "gpt-4o");
        assert_eq!(events[2].1["delta"]["text"], "你好");
        assert_eq!(events[3].1["delta"]["text"], "世界");
        assert_eq!(events[5].1["content_block"], json!({"type": "tool_use", "id": "call_a", "name": "read", "input": {}}));
        assert_eq!(events[12].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(
            events[12].1["usage"],
            json!({"input_tokens": 70, "output_tokens": 22, "cache_read_input_tokens": 10})
        );
    }

    #[test]
    fn streaming_edge_cases_error_eof_and_empty() {
        // 1. 上游发 error 对象
        let mut conv = SseConverter::new();
        let mut raw = String::new();
        for ev in conv.push_bytes(b"data: {\"error\":{\"type\":\"rate_limit_error\",\"message\":\"too fast\"}}\n\n") {
            raw.push_str(&ev);
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "error");
        assert_eq!(events[0].1["error"]["message"], "too fast");

        // 2. 没有 [DONE] 就断流，补齐 content_block_stop / message_delta / message_stop
        let mut conv = SseConverter::new();
        let mut raw = String::new();
        for ev in conv.push_bytes(b"data: {\"id\":\"c2\",\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n") {
            raw.push_str(&ev);
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names,
            vec!["message_start", "content_block_start", "content_block_delta", "content_block_stop", "message_delta", "message_stop"]
        );

        // 3. 一直没内容的流也要发 message_start / message_delta / message_stop
        let mut conv = SseConverter::new();
        let mut raw = String::new();
        for ev in conv.push_bytes(b"data: [DONE]\n\n") {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(names, vec!["message_start", "message_delta", "message_stop"]);
    }

    #[test]
    fn responses_request_mapping() {
        let req = json!({
            "model": "gpt-5.4",
            "system": [
                {"type": "text", "text": "规则一"},
                {"type": "text", "text": "规则二"}
            ],
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "帮我看看"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}}
                    ]
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "思考"},
                        {"type": "text", "text": "调用工具"},
                        {"type": "tool_use", "id": "call_1", "name": "read", "input": {"path": "a.rs"}}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "call_1", "content": "fn main() {}"}
                    ]
                }
            ],
            "tools": [
                {
                    "name": "read",
                    "description": "读文件",
                    "input_schema": {
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "properties": {"path": {"type": "string", "cache_control": {"type": "ephemeral"}}}
                    }
                }
            ],
            "tool_choice": {"type": "tool", "name": "read"},
            "max_tokens": 1,
            "stop_sequences": ["STOP"],
            "stream": true,
            "metadata": {"user_id": "u1"}
        });
        let out = anthropic_to_openai_responses(&req).unwrap();
        assert_eq!(out["model"], "gpt-5.4");
        assert_eq!(out["instructions"], "规则一\n\n规则二");
        assert_eq!(out["max_output_tokens"], 16, "低于 16 的预算抬到 16");
        assert!(out.get("stop_sequences").is_none() && out.get("stop").is_none());
        assert!(out.get("metadata").is_none());
        assert_eq!(out["tool_choice"], json!({"type": "function", "name": "read"}));
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tools"][0]["strict"], false);
        assert_eq!(out["tools"][0]["name"], "read");
        assert_eq!(out["tools"][0]["parameters"]["type"], "object");
        assert!(out["tools"][0]["parameters"].get("$schema").is_none());
        assert!(out["tools"][0]["parameters"]["properties"]["path"].get("cache_control").is_none());

        let input = out["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0], json!({"type": "input_text", "text": "帮我看看"}));
        assert_eq!(input[0]["content"][1], json!({"type": "input_image", "image_url": "data:image/png;base64,QUJD"}));
        assert_eq!(input[1], json!({"role": "assistant", "content": [{"type": "output_text", "text": "调用工具"}]}));
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["name"], "read");
        assert_eq!(input[3], json!({"type": "function_call_output", "call_id": "call_1", "output": "fn main() {}"}));
    }

    #[test]
    fn responses_non_streaming_response() {
        let up = json!({
            "id": "resp_123",
            "model": "gpt-5.4",
            "status": "completed",
            "output": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "内部思考"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "完成"}]},
                {"type": "function_call", "call_id": "fc_1", "name": "ls", "arguments": "{\"dir\":\".\"}"}
            ],
            "usage": {
                "input_tokens": 50,
                "output_tokens": 12,
                "input_tokens_details": {"cached_tokens": 10}
            }
        });
        let out = openai_responses_to_anthropic(&up).unwrap();
        assert_eq!(out["id"], "msg_resp_123");
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["usage"], json!({"input_tokens": 40, "output_tokens": 12, "cache_read_input_tokens": 10}));
        let content = out["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "reasoning 应被丢弃");
        assert_eq!(content[0], json!({"type": "text", "text": "完成"}));
        assert_eq!(content[1], json!({"type": "tool_use", "id": "fc_1", "name": "ls", "input": {"dir": "."}}));

        let failed = json!({"status": "failed", "error": {"message": "bad req"}});
        assert!(openai_responses_to_anthropic(&failed).is_err());
    }

    #[test]
    fn responses_streaming_chunked_and_edges() {
        let sse_input = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_s1\",\"model\":\"gpt-5.4\"}}\n\n",
            "event: response.content_part.added\ndata: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你好\"}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"世界\"}\n\n",
            "event: response.content_part.done\ndata: {\"type\":\"response.content_part.done\",\"part\":{\"type\":\"output_text\",\"text\":\"你好世界\"}}\n\n",
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_x\",\"name\":\"read\"}}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\\\"a\\\"\"}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\":1}\"}\n\n",
            "event: response.function_call_arguments.done\ndata: {\"type\":\"response.function_call_arguments.done\",\"arguments\":\"{\\\"a\\\":1}\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":30,\"output_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":5}}}}\n\n"
        );

        let mut conv = ResponsesSseConverter::new();
        let mut raw = String::new();
        for piece in sse_input.as_bytes().chunks(11) {
            for ev in conv.push_bytes(piece) {
                raw.push_str(&ev);
            }
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }

        let events = parse_sse(&raw);
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start", // text 0
                "content_block_delta", // "你好"
                "content_block_delta", // "世界"
                "content_block_stop",  // stop text 0
                "content_block_start", // tool 1
                "content_block_delta", // "{\"a\""
                "content_block_delta", // ":1}"
                "content_block_stop",  // stop tool 1
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[0].1["message"]["id"], "msg_resp_s1");
        assert_eq!(events[5].1["content_block"], json!({"type": "tool_use", "id": "call_x", "name": "read", "input": {}}));
        assert_eq!(events[9].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(events[9].1["usage"], json!({"input_tokens": 25, "output_tokens": 10, "cache_read_input_tokens": 5}));

        // response.failed → error 事件
        let mut conv = ResponsesSseConverter::new();
        let mut raw = String::new();
        for ev in conv.push_bytes(b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"type\":\"server_error\",\"message\":\"boom\"}}}\n\n") {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "error");
        assert_eq!(events[0].1["error"]["message"], "boom");

        // 断流补齐
        let mut conv = ResponsesSseConverter::new();
        let mut raw = String::new();
        for ev in conv.push_bytes(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n") {
            raw.push_str(&ev);
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names,
            vec!["message_start", "content_block_start", "content_block_delta", "content_block_stop", "message_delta", "message_stop"]
        );
    }

    #[test]
    fn gemini_request_mapping() {
        let req = json!({
            "model": "gemini-3-pro-preview",
            "system": [{"type": "text", "text": "你是助手"}],
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "看图"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "QUJD"}}
                    ]
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "内部"},
                        {"type": "tool_use", "id": "gemini_synth_abc", "name": "read", "input": {"path": "a.rs"}}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "gemini_synth_abc", "content": "ok"},
                        {"type": "text", "text": "继续"}
                    ]
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "call_2", "name": "simple", "input": {"q": "x"}}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "call_2", "content": "done"}
                    ]
                }
            ],
            "tools": [
                {
                    "name": "simple",
                    "description": "简单工具",
                    "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}}
                },
                {
                    "name": "strict_tool",
                    "description": "带 additionalProperties",
                    "input_schema": {
                        "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "type": "object",
                        "properties": {"p": {"type": "string"}},
                        "additionalProperties": false
                    }
                }
            ],
            "tool_choice": {"type": "tool", "name": "simple"},
            "max_tokens": 512,
            "temperature": 0.3,
            "stop_sequences": ["END"]
        });

        let out = anthropic_to_gemini(&req).unwrap();
        assert_eq!(out["systemInstruction"], json!({"parts": [{"text": "你是助手"}]}));
        assert_eq!(out["generationConfig"]["maxOutputTokens"], 512);
        assert_eq!(out["generationConfig"]["stopSequences"], json!(["END"]));
        assert_eq!(out["toolConfig"], json!({"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["simple"]}}));

        let decls = out["tools"][0]["functionDeclarations"].as_array().unwrap();
        assert!(decls[0].get("parameters").is_some() && decls[0].get("parametersJsonSchema").is_none());
        assert!(decls[1].get("parametersJsonSchema").is_some() && decls[1].get("parameters").is_none());

        let contents = out["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][1], json!({"inlineData": {"mimeType": "image/jpeg", "data": "QUJD"}}));
        // 合成 id 不发给上游
        assert_eq!(contents[1]["role"], "model");
        assert!(contents[1]["parts"][0]["functionCall"].get("id").is_none());
        // functionResponse 与 text 拆成两条 user content
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "read");
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["response"]["content"], "ok");
        assert_eq!(contents[3], json!({"role": "user", "parts": [{"text": "继续"}]}));
        // 当前轮次（以纯 functionResponse 结尾）的 Gemini 3 functionCall 自动补 skip_thought_signature_validator
        assert_eq!(contents[4]["role"], "model");
        assert_eq!(contents[4]["parts"][0]["functionCall"]["id"], "call_2");
        assert_eq!(contents[4]["parts"][0]["thoughtSignature"], "skip_thought_signature_validator");
        assert_eq!(contents[5]["parts"][0]["functionResponse"]["name"], "simple");
    }

    #[test]
    fn gemini_non_streaming_response() {
        let up = json!({
            "responseId": "resp-g1",
            "modelVersion": "gemini-2.5-pro",
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {"thought": true, "text": "内部思考"},
                        {"text": "准备读取"},
                        {"functionCall": {"name": "read", "args": {"path": "README.md"}}}
                    ]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 40,
                "candidatesTokenCount": 15,
                "cachedContentTokenCount": 8
            }
        });
        let out = gemini_to_anthropic(&up).unwrap();
        assert_eq!(out["id"], "msg_resp-g1");
        assert_eq!(out["model"], "gemini-2.5-pro");
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["usage"], json!({"input_tokens": 32, "output_tokens": 15, "cache_read_input_tokens": 8}));
        let content = out["content"].as_array().unwrap();
        assert_eq!(content.len(), 3, "thought 应映射为 thinking 块");
        assert_eq!(content[0], json!({"type": "thinking", "thinking": "内部思考"}));
        assert_eq!(content[1], json!({"type": "text", "text": "准备读取"}));
        assert_eq!(content[2]["type"], "tool_use");
        assert_eq!(content[2]["name"], "read");
        assert!(content[2]["id"].as_str().unwrap().starts_with("gemini_synth_"));
    }

    #[test]
    fn gemini_streaming_chunked_and_edges() {
        let sse_input = concat!(
            "data: {\"responseId\":\"g-s1\",\"modelVersion\":\"gemini-2.5-flash\",\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"想一下\"},{\"text\":\"你好\"}]}}]}\n\n",
            "data: {\"responseId\":\"g-s1\",\"modelVersion\":\"gemini-2.5-flash\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"世界\"},{\"functionCall\":{\"id\":\"fc_1\",\"name\":\"ls\",\"args\":{\"dir\":\".\"}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":25,\"candidatesTokenCount\":9,\"cachedContentTokenCount\":5}}\n\n"
        );
        let mut conv = GeminiSseConverter::new();
        let mut raw = String::new();
        for piece in sse_input.as_bytes().chunks(13) {
            for ev in conv.push_bytes(piece) {
                raw.push_str(&ev);
            }
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start", // thinking 0
                "content_block_delta", // "想一下"
                "content_block_stop",  // stop thinking 0
                "content_block_start", // text 1
                "content_block_delta", // "你好"
                "content_block_delta", // "世界"
                "content_block_stop",  // stop text 1
                "content_block_start", // tool 2
                "content_block_delta", // "{\"dir\":\".\"}"
                "content_block_stop",  // stop tool 2
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[0].1["message"]["id"], "msg_g-s1");
        assert_eq!(events[1].1["content_block"], json!({"type": "thinking", "thinking": ""}));
        assert_eq!(events[2].1["delta"], json!({"type": "thinking_delta", "thinking": "想一下"}));
        assert_eq!(events[4].1["content_block"], json!({"type": "text", "text": ""}));
        assert_eq!(events[11].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(events[11].1["usage"], json!({"input_tokens": 20, "output_tokens": 9, "cache_read_input_tokens": 5}));
    }

    #[test]
    fn gemini_early_thinking_reuses_or_closes_block_cleanly() {
        // 1. 抢先进入 thinking 块后，上游陆续返回 thought + text，复用 index=0 不重复发 message_start
        let mut conv = GeminiSseConverter::new();
        let mut raw = conv.start_early_thinking("gemini-4-neon-high").concat();
        let sse_with_thought = concat!(
            "data: {\"response\":{\"responseId\":\"g-e1\",\"modelVersion\":\"gemini-4-neon\",\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"第一步\"}]}}]}}\n\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"结论\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5}}}\n\n"
        );
        for ev in conv.push_bytes(sse_with_thought.as_bytes()) {
            raw.push_str(&ev);
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start", // early thinking 0
                "content_block_delta", // thinking_delta "第一步"
                "content_block_stop",  // stop thinking 0
                "content_block_start", // text 1
                "content_block_delta", // text_delta "结论"
                "content_block_stop",  // stop text 1
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[0].1["message"]["model"], "gemini-4-neon-high");
        assert_eq!(events[2].1["index"], 0);
        assert_eq!(events[2].1["delta"], json!({"type": "thinking_delta", "thinking": "第一步"}));
        assert_eq!(events[4].1["index"], 1);
        assert_eq!(events[5].1["delta"], json!({"type": "text_delta", "text": "结论"}));

        // 2. 抢先进入 thinking 块后，上游直接返回 text（无 thought），先关掉空 thinking 0 再开 text 1
        let mut conv2 = GeminiSseConverter::new();
        let mut raw2 = conv2.start_early_thinking("gemini-4-carbon-high").concat();
        let sse_text_only = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"直接回答\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":8,\"candidatesTokenCount\":4}}}\n\n";
        for ev in conv2.push_bytes(sse_text_only.as_bytes()) {
            raw2.push_str(&ev);
        }
        for ev in conv2.finish() {
            raw2.push_str(&ev);
        }
        let events2 = parse_sse(&raw2);
        let names2: Vec<&str> = events2.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names2,
            vec![
                "message_start",
                "content_block_start", // early thinking 0
                "content_block_stop",  // stop empty thinking 0
                "content_block_start", // text 1
                "content_block_delta", // text_delta "直接回答"
                "content_block_stop",  // stop text 1
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events2[2].1["index"], 0);
        assert_eq!(events2[3].1["index"], 1);
    }

    #[test]
    fn gemini_sse_keepalive_emits_ping_during_silence() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let raw = rt.block_on(async {
            let upstream = async_stream::stream! {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}}\n\n",
                ));
            };
            let mut sse = std::pin::pin!(gemini_sse_to_anthropic_sse_with_keepalive(
                upstream,
                Some(std::time::Duration::from_millis(25)),
            ));
            let mut out = String::new();
            while let Some(chunk) = sse.next().await {
                out.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            }
            out
        });
        assert!(raw.contains("event: ping\ndata: {\"type\":\"ping\"}\n\n"), "{raw}");
        assert!(raw.contains("\"text\":\"ok\""), "{raw}");
        assert!(raw.contains("event: message_stop"), "{raw}");
    }

    #[test]
    fn tool_result_images_extracted_across_protocols() {
        let base_messages = json!([
            {
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_img_1", "name": "Read", "input": {"file_path": "a.png"}},
                    {"type": "tool_use", "id": "call_img_2", "name": "Read", "input": {"file_path": "b.png"}}
                ]
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "call_img_1",
                        "content": [
                            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "IMG_DATA_1"}}
                        ]
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "call_img_2",
                        "content": [
                            {"type": "text", "text": "说明文字"},
                            {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "IMG_DATA_2"}}
                        ]
                    }
                ]
            }
        ]);

        // 1. Gemini 3+/4（含 Antigravity）：图片进入 functionResponse.parts，不序列化进 response.content
        let g4 = anthropic_to_gemini(&json!({
            "model": "gemini-4-neon-high",
            "messages": base_messages
        }))
        .unwrap();
        let g4_contents = g4["contents"].as_array().unwrap();
        assert_eq!(g4_contents.len(), 2);
        let frs = g4_contents[1]["parts"].as_array().unwrap();
        assert_eq!(frs.len(), 2);
        assert_eq!(frs[0]["functionResponse"]["response"]["content"], "[tool result image attached]");
        assert_eq!(
            frs[0]["functionResponse"]["parts"],
            json!([{"inlineData": {"mimeType": "image/png", "data": "IMG_DATA_1"}}])
        );
        assert_eq!(frs[1]["functionResponse"]["response"]["content"], "说明文字");
        assert_eq!(
            frs[1]["functionResponse"]["parts"],
            json!([{"inlineData": {"mimeType": "image/jpeg", "data": "IMG_DATA_2"}}])
        );

        // 2. Gemini 2.x：functionResponse 不放 parts，图片拆到紧随其后的 user content
        let g2 = anthropic_to_gemini(&json!({
            "model": "gemini-2.5-pro",
            "messages": base_messages
        }))
        .unwrap();
        let g2_contents = g2["contents"].as_array().unwrap();
        assert_eq!(g2_contents.len(), 3);
        assert!(g2_contents[1]["parts"][0]["functionResponse"].get("parts").is_none());
        assert!(!g2_contents[1]["parts"][0]["functionResponse"]["response"]["content"]
            .as_str()
            .unwrap()
            .contains("IMG_DATA_1"));
        assert_eq!(
            g2_contents[2]["parts"][1],
            json!({"inlineData": {"mimeType": "image/png", "data": "IMG_DATA_1"}})
        );
        assert_eq!(
            g2_contents[2]["parts"][3],
            json!({"inlineData": {"mimeType": "image/jpeg", "data": "IMG_DATA_2"}})
        );

        // 3. OpenAI Responses / Codex OAuth：output 为 input_text / input_image 数组
        let resp = anthropic_to_openai_responses(&json!({
            "model": "gpt-5.4",
            "messages": base_messages
        }))
        .unwrap();
        let input = resp["input"].as_array().unwrap();
        assert_eq!(
            input[2]["output"],
            json!([{"type": "input_image", "image_url": "data:image/png;base64,IMG_DATA_1"}])
        );
        assert_eq!(
            input[3]["output"],
            json!([
                {"type": "input_text", "text": "说明文字"},
                {"type": "input_image", "image_url": "data:image/jpeg;base64,IMG_DATA_2"}
            ])
        );

        // 4. OpenAI Chat：tool 消息仅留文本，图片集中放到紧随其后的 user 消息
        let chat = anthropic_to_openai_chat(&json!({
            "model": "gpt-4o",
            "messages": base_messages
        }))
        .unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs[1], json!({"role": "tool", "tool_call_id": "call_img_1", "content": "[tool result image attached]"}));
        assert_eq!(msgs[2], json!({"role": "tool", "tool_call_id": "call_img_2", "content": "说明文字"}));
        assert_eq!(msgs[3]["role"], "user");
        assert_eq!(
            msgs[3]["content"][1],
            json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,IMG_DATA_1"}})
        );
        assert_eq!(
            msgs[3]["content"][3],
            json!({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,IMG_DATA_2"}})
        );
    }

    #[test]
    fn gemini_web_search_request_and_grounding_response() {
        // 1. 仅含 web_search_20250305（Claude Code 内置 WebSearch 专用请求）：映射为 googleSearch 且不写 functionCallingConfig
        let req = json!({
            "model": "gemini-4-neon-high",
            "messages": [{"role": "user", "content": "latest rust version"}],
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
            "tool_choice": {"type": "tool", "name": "web_search"}
        });
        assert_eq!(anthropic_web_search_tool_name(&req), Some("web_search"));
        let g = anthropic_to_gemini(&req).unwrap();
        assert_eq!(g["tools"], json!([{ "googleSearch": {} }]));
        assert!(g.get("toolConfig").is_none());

        // 2. 同时带普通函数与 web_search 时：避免 Gemini/v1internal 400，只发 functionDeclarations
        let mixed = json!({
            "model": "gemini-4-neon-high",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [
                {"type": "web_search_20250305", "name": "web_search"},
                {"name": "read_file", "description": "读文件", "input_schema": {"type": "object", "properties": {}}}
            ]
        });
        let gm = anthropic_to_gemini(&mixed).unwrap();
        assert_eq!(gm["tools"][0]["functionDeclarations"][0]["name"], "read_file");
        assert!(gm["tools"][0].get("googleSearch").is_none());

        // 3. 非流式响应带 groundingMetadata → server_tool_use + web_search_tool_result + text + usage
        let resp = json!({
            "response": {
                "responseId": "g_ws_1",
                "modelVersion": "gemini-4-neon",
                "candidates": [{
                    "content": {"role": "model", "parts": [{"text": "Rust 1.100.0 is the latest."}]},
                    "finishReason": "STOP",
                    "groundingMetadata": {
                        "webSearchQueries": ["latest stable rust version"],
                        "groundingChunks": [
                            {"web": {"title": "rust-lang.org", "uri": "https://example.com/rust"}},
                            {"web": {"title": "releases.rs", "uri": "https://example.com/releases"}}
                        ]
                    }
                }],
                "usageMetadata": {"promptTokenCount": 50, "candidatesTokenCount": 12}
            }
        });
        let anth = gemini_to_anthropic_with_web_search(&resp, Some("web_search")).unwrap();
        assert_eq!(anth["content"][0]["type"], "server_tool_use");
        assert_eq!(anth["content"][0]["name"], "web_search");
        assert_eq!(anth["content"][0]["input"]["query"], "latest stable rust version");
        let sid = anth["content"][0]["id"].as_str().unwrap();
        assert_eq!(anth["content"][1]["type"], "web_search_tool_result");
        assert_eq!(anth["content"][1]["tool_use_id"], sid);
        assert_eq!(anth["content"][1]["content"].as_array().unwrap().len(), 2);
        assert_eq!(anth["content"][1]["content"][0]["url"], "https://example.com/rust");
        assert_eq!(anth["content"][2], json!({"type": "text", "text": "Rust 1.100.0 is the latest."}));
        assert_eq!(anth["usage"]["server_tool_use"]["web_search_requests"], 1);

        // 4. 流式 SSE 带 groundingMetadata → 产出 server_tool_use + web_search_tool_result 事件与 usage
        let sse = concat!(
            "data: {\"response\":{\"responseId\":\"g_ws_2\",\"modelVersion\":\"gemini-4-neon\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"最新是 1.100.0\"}]}}]}}\n\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}]},\"finishReason\":\"STOP\",\"groundingMetadata\":{\"webSearchQueries\":[\"rust latest\"],\"groundingChunks\":[{\"web\":{\"title\":\"rust-lang.org\",\"uri\":\"https://example.com/rust\"}}]}}],\"usageMetadata\":{\"promptTokenCount\":20,\"candidatesTokenCount\":5}}}\n\n"
        );
        let mut conv = GeminiSseConverter::with_web_search_name(Some("web_search".into()));
        let mut raw = String::new();
        for ev in conv.push_bytes(sse.as_bytes()) {
            raw.push_str(&ev);
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        let start_types: Vec<&str> = events
            .iter()
            .filter(|(e, _)| e == "content_block_start")
            .filter_map(|(_, v)| v.pointer("/content_block/type").and_then(Value::as_str))
            .collect();
        assert_eq!(start_types, vec!["server_tool_use", "web_search_tool_result", "text"]);
        let delta_ev = events.iter().find(|(e, _)| e == "message_delta").unwrap();
        assert_eq!(delta_ev.1["usage"]["server_tool_use"]["web_search_requests"], 1);

        // 5. collect_gemini_sse_to_anthropic 同样保留 grounding 结果
        let collected = collect_gemini_sse_to_anthropic_with_web_search(sse.as_bytes(), Some("web_search")).unwrap();
        assert_eq!(collected["content"][0]["type"], "server_tool_use");
        assert_eq!(collected["content"][1]["type"], "web_search_tool_result");
        assert_eq!(collected["content"][2]["text"], "最新是 1.100.0");
    }

    #[test]
    fn responses_and_chat_web_search_mapping() {
        let req = json!({
            "model": "gpt-6-luna",
            "messages": [{"role": "user", "content": "search rust"}],
            "tools": [{
                "type": "web_search_20250305",
                "name": "web_search",
                "allowed_domains": ["rust-lang.org"],
                "blocked_domains": ["spam.invalid"]
            }],
            "tool_choice": {"type": "tool", "name": "web_search"}
        });

        // 1. OpenAI Responses API
        let resp_req = anthropic_to_openai_responses(&req).unwrap();
        assert_eq!(
            resp_req["tools"][0],
            json!({
                "type": "web_search",
                "filters": {
                    "allowed_domains": ["rust-lang.org"],
                    "blocked_domains": ["spam.invalid"]
                }
            })
        );
        assert_eq!(resp_req["tool_choice"], json!({"type": "web_search"}));
        assert_eq!(resp_req["include"], json!(["web_search_call.action.sources"]));

        // 2. Codex OAuth Responses
        let codex_req = anthropic_to_codex_oauth_responses(&req).unwrap();
        assert_eq!(codex_req["tools"][0]["type"], "web_search");
        assert_eq!(codex_req["tools"][0]["external_web_access"], true);
        assert!(codex_req["tools"][0]["filters"].get("blocked_domains").is_none());
        assert_eq!(codex_req["tool_choice"], "required");
        let inc = codex_req["include"].as_array().unwrap();
        assert!(inc.contains(&json!("reasoning.encrypted_content")));
        assert!(inc.contains(&json!("web_search_call.action.sources")));

        // 3. OpenAI Chat Completions：过滤托管 web_search，不发无效 tool_choice
        let chat_req = anthropic_to_openai_chat(&req).unwrap();
        assert!(chat_req.get("tools").is_none());
        assert!(chat_req.get("tool_choice").is_none());

        // 4. Responses 非流式响应：web_search_call + url_citation → server_tool_use + web_search_tool_result
        let up_resp = json!({
            "id": "resp_ws_1",
            "status": "completed",
            "model": "gpt-6-luna",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": "rust docs",
                        "sources": [{"type": "url", "url": "https://doc.rust-lang.org/"}]
                    }
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "See Rust docs.",
                        "annotations": [{
                            "type": "url_citation",
                            "url": "https://doc.rust-lang.org/",
                            "title": "Rust Documentation"
                        }]
                    }]
                }
            ],
            "usage": {"input_tokens": 10, "output_tokens": 20}
        });
        let anth = openai_responses_to_anthropic_with_web_search(&up_resp, Some("web_search")).unwrap();
        assert_eq!(anth["content"][0]["type"], "server_tool_use");
        assert_eq!(anth["content"][0]["id"], "ws_1");
        assert_eq!(anth["content"][0]["input"]["query"], "rust docs");
        assert_eq!(anth["content"][1]["type"], "web_search_tool_result");
        assert_eq!(anth["content"][1]["content"][0]["title"], "Rust Documentation");
        assert_eq!(anth["content"][2], json!({"type": "text", "text": "See Rust docs."}));
        assert_eq!(anth["usage"]["server_tool_use"]["web_search_requests"], 1);

        // 5. Responses 流式 SSE
        let sse = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_ws_2\",\"model\":\"gpt-6-luna\"}}\n\n",
            "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_2\",\"status\":\"completed\",\"action\":{\"type\":\"search\",\"query\":\"rust\",\"sources\":[{\"url\":\"https://rust-lang.org/\",\"title\":\"Rust\"}]}}}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Found it.\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\n"
        );
        let mut conv = ResponsesSseConverter::with_web_search_name(Some("web_search".into()));
        let mut raw = String::new();
        for ev in conv.push_bytes(sse.as_bytes()) {
            raw.push_str(&ev);
        }
        for ev in conv.finish() {
            raw.push_str(&ev);
        }
        let events = parse_sse(&raw);
        let start_types: Vec<&str> = events
            .iter()
            .filter(|(e, _)| e == "content_block_start")
            .filter_map(|(_, v)| v.pointer("/content_block/type").and_then(Value::as_str))
            .collect();
        assert_eq!(start_types, vec!["server_tool_use", "web_search_tool_result", "text"]);
        let delta_ev = events.iter().find(|(e, _)| e == "message_delta").unwrap();
        assert_eq!(delta_ev.1["usage"]["server_tool_use"]["web_search_requests"], 1);
    }

    #[test]
    fn usage_mapping_avoids_double_counting_cached_tokens() {
        // OpenAI Chat: prompt_tokens 含 cached_tokens，且 cached > prompt 时 saturating_sub 不下溢
        assert_eq!(
            map_usage(Some(&json!({"prompt_tokens": 100, "completion_tokens": 10, "prompt_tokens_details": {"cached_tokens": 100}}))),
            json!({"input_tokens": 0, "output_tokens": 10, "cache_read_input_tokens": 100})
        );
        assert_eq!(
            map_usage(Some(&json!({"prompt_tokens": 50, "completion_tokens": 5, "prompt_tokens_details": {"cached_tokens": 80}}))),
            json!({"input_tokens": 0, "output_tokens": 5, "cache_read_input_tokens": 80})
        );

        // OpenAI Responses: cached_tokens 从 input_tokens 扣除；原生 cache_read_input_tokens 不扣
        assert_eq!(
            map_responses_usage(Some(&json!({"input_tokens": 100, "output_tokens": 10, "input_tokens_details": {"cached_tokens": 90}}))),
            json!({"input_tokens": 10, "output_tokens": 10, "cache_read_input_tokens": 90})
        );
        assert_eq!(
            map_responses_usage(Some(&json!({"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 90}))),
            json!({"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 90})
        );

        // Gemini: promptTokenCount 含 cachedContentTokenCount
        assert_eq!(
            map_gemini_usage(Some(&json!({"promptTokenCount": 651558, "candidatesTokenCount": 221, "cachedContentTokenCount": 648354}))),
            json!({"input_tokens": 3204, "output_tokens": 221, "cache_read_input_tokens": 648354})
        );
        assert_eq!(
            map_gemini_usage(Some(&json!({"promptTokenCount": 20, "candidatesTokenCount": 5, "cachedContentTokenCount": 50}))),
            json!({"input_tokens": 0, "output_tokens": 5, "cache_read_input_tokens": 50})
        );
    }
}
