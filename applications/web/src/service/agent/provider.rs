use super::{AgentEvent, ModelSettings};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::sync::mpsc::Sender;

pub struct ModelReply {
    pub text: String,
    pub items: Vec<Value>,
    pub calls: Vec<ToolCall>,
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

pub fn tool_result(protocol: &str, id: &str, value: &Value) -> Value {
    if protocol == "responses" {
        json!({"type":"function_call_output", "call_id":id, "output":value.to_string()})
    } else {
        json!({"role":"tool", "tool_call_id":id, "content":value.to_string()})
    }
}

pub fn request_body(
    settings: &ModelSettings,
    history: &[Value],
    instructions: &str,
    tools: &[Value],
) -> Value {
    if settings.protocol == "responses" {
        json!({"model":settings.model, "instructions":instructions, "input":history,
            "tools":tools, "parallel_tool_calls":false, "stream":true, "store":false, "include":["reasoning.encrypted_content"]})
    } else {
        let mut messages = vec![json!({"role":"system", "content":instructions})];
        messages.extend_from_slice(history);
        let tools: Vec<_> = tools
            .iter()
            .map(|tool| {
                let mut function = tool.clone();
                if let Some(object) = function.as_object_mut() {
                    object.remove("type");
                }
                json!({"type":"function", "function":function})
            })
            .collect();
        json!({"model":settings.model, "messages":messages, "tools":tools,
            "parallel_tool_calls":false, "stream":true})
    }
}

pub async fn complete(
    settings: &ModelSettings,
    history: &[Value],
    instructions: &str,
    tools: &[Value],
    sender: &Sender<AgentEvent>,
) -> Result<ModelReply, String> {
    let suffix =
        if settings.protocol == "responses" { "/responses" } else { "/chat/completions" };
    let endpoint = format!("{}{suffix}", settings.base_url.trim_end_matches('/'));
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::none());
    let url = reqwest::Url::parse(&endpoint).map_err(|_| "模型服务地址无效")?;
    if url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }) {
        builder = builder.no_proxy();
    }
    let client = builder.build().map_err(|_| "无法初始化模型客户端")?;
    let mut request =
        client.post(endpoint).json(&request_body(settings, history, instructions, tools));
    if !settings.api_key.is_empty() {
        request = request.bearer_auth(&settings.api_key);
    }
    let response = request.send().await.map_err(|e| {
        format!(
            "模型连接失败：{}",
            if e.is_timeout() {
                "请求超时"
            } else {
                "请检查服务地址和网络"
            }
        )
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "模型服务返回 HTTP {}，请检查模型名称、密钥和接口协议",
            response.status().as_u16()
        ));
    }
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut decoder = ReplyDecoder::default();
    let mut size = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "模型响应流中断，请重试")?;
        size += chunk.len();
        if size > 4 * 1024 * 1024 {
            return Err("模型响应过大".into());
        }
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = buffer.drain(..=end).collect();
            let line =
                std::str::from_utf8(&line).map_err(|_| "模型返回无效 UTF-8")?.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                continue;
            }
            if data.is_empty() {
                continue;
            }
            let event: Value =
                serde_json::from_str(data).map_err(|_| "模型返回无效 SSE 数据")?;
            if let Some(delta) = decoder.push(&settings.protocol, &event)? {
                sender
                    .send(AgentEvent::Delta { text: delta })
                    .await
                    .map_err(|_| "已停止生成")?;
            }
        }
    }
    decoder.finish(&settings.protocol)
}

#[derive(Default)]
struct ReplyDecoder {
    text: String,
    output: Option<Vec<Value>>,
    calls: BTreeMap<usize, (String, String, String)>,
    finished: bool,
}

impl ReplyDecoder {
    fn push(&mut self, protocol: &str, event: &Value) -> Result<Option<String>, String> {
        if event.get("error").is_some()
            || matches!(
                event["type"].as_str(),
                Some("error" | "response.failed" | "response.incomplete")
            )
        {
            return Err("模型未能完成回复，请检查服务配置后重试".into());
        }
        let delta = if protocol == "responses" {
            if event["type"] == "response.completed" {
                self.output = event["response"]["output"].as_array().cloned();
                self.finished = true;
            }
            if event["type"] == "response.output_text.delta" {
                event["delta"].as_str()
            } else {
                None
            }
        } else {
            let choice = &event["choices"][0];
            if let Some(reason) = choice["finish_reason"].as_str() {
                if !matches!(reason, "stop" | "tool_calls") {
                    return Err(format!(
                        "模型回复未完成（{reason}），请重试或缩小需求范围"
                    ));
                }
                self.finished = true;
            }
            if let Some(calls) = choice["delta"]["tool_calls"].as_array() {
                for call in calls {
                    let index =
                        call["index"].as_u64().ok_or("工具调用缺少 index")? as usize;
                    let entry = self.calls.entry(index).or_default();
                    if let Some(id) = call["id"].as_str() {
                        entry.0.push_str(id);
                    }
                    if let Some(name) = call["function"]["name"].as_str() {
                        entry.1.push_str(name);
                    }
                    if let Some(args) = call["function"]["arguments"].as_str() {
                        entry.2.push_str(args);
                    }
                }
            }
            choice["delta"]["content"].as_str()
        };
        if let Some(delta) = delta {
            self.text.push_str(delta);
            Ok(Some(delta.to_owned()))
        } else {
            Ok(None)
        }
    }

    fn finish(self, protocol: &str) -> Result<ModelReply, String> {
        if !self.finished {
            return Err("模型连接提前结束，请重试".into());
        }
        if protocol == "responses" {
            // Preserve all output items, including reasoning, for the next tool round.
            let items = self.output.ok_or("Responses 回复缺少 output")?;
            let mut calls = Vec::new();
            for item in &items {
                if item["type"] == "function_call" {
                    calls.push(ToolCall {
                        id: item["call_id"].as_str().ok_or("缺少 call_id")?.into(),
                        name: item["name"].as_str().ok_or("缺少工具名称")?.into(),
                        arguments: item["arguments"]
                            .as_str()
                            .ok_or("缺少工具参数")?
                            .into(),
                    });
                }
            }
            Ok(ModelReply { text: self.text, items, calls })
        } else {
            let calls: Vec<_> = self
                .calls
                .into_values()
                .map(|(id, name, arguments)| ToolCall { id, name, arguments })
                .collect();
            if calls.iter().any(|c| c.id.is_empty() || c.name.is_empty()) {
                return Err("工具调用不完整".into());
            }
            let mut item = json!({"role":"assistant", "content":self.text});
            if !calls.is_empty() {
                item["tool_calls"] = json!(calls.iter().map(|c| json!({"id":c.id, "type":"function", "function":{"name":c.name,"arguments":c.arguments}})).collect::<Vec<_>>());
            }
            Ok(ModelReply { text: self.text, items: vec![item], calls })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_assembles_fragmented_tool_arguments() {
        let mut decoder = ReplyDecoder::default();
        decoder.push("chat-completions", &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"ask_user","arguments":"{\"q\":"}}]}}]})).unwrap();
        decoder.push("chat-completions", &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"问题\"}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
        assert_eq!(
            decoder.finish("chat-completions").unwrap().calls[0].arguments,
            "{\"q\":\"问题\"}"
        );
    }

    #[test]
    fn responses_preserves_reasoning_and_call_ids() {
        let mut decoder = ReplyDecoder::default();
        let output = json!([{"type":"reasoning","id":"r","encrypted_content":"opaque"},{"type":"function_call","call_id":"call-2","name":"catalog","arguments":"{}"}]);
        decoder
            .push(
                "responses",
                &json!({"type":"response.completed","response":{"output":output}}),
            )
            .unwrap();
        let reply = decoder.finish("responses").unwrap();
        assert_eq!(json!(reply.items), output);
        assert_eq!(reply.calls[0].id, "call-2");
        assert_eq!(tool_result("responses", "call-2", &json!({}))["call_id"], "call-2");
    }

    #[test]
    fn truncated_response_cannot_produce_a_plan() {
        assert!(ReplyDecoder::default().finish("responses").is_err());
    }
}
