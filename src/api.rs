//! REST 客户端、错误模型与搜索请求体构造。

use crate::consts::CLIENT_HEADER;
use crate::env;
use serde_json::{json, Map, Value};
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub message: String,
    pub request_id: String,
    pub data: Option<Value>,
}

impl ApiError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            request_id: String::new(),
            data: None,
        }
    }

    /// 面向模型和用户的单行错误(含 request_id 与可选响应数据)。
    pub fn render(&self) -> String {
        let mut text = if self.request_id.is_empty() {
            self.message.clone()
        } else {
            format!("{} (request_id: {})", self.message, self.request_id)
        };
        if let Some(data) = &self.data {
            if data.as_object().map(|map| !map.is_empty()).unwrap_or(false) {
                text.push_str(&format!("\nResponse data: {data}"));
            }
        }
        text
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// 从 `.env`/环境变量构造客户端;`base_url`/`api_key` 为工具参数覆盖值。
    pub fn from_env(
        cwd_hint: Option<&str>,
        api_key_flag: Option<&str>,
        base_url_flag: Option<&str>,
    ) -> Self {
        let settings = env::load_env(cwd_hint);
        Self::new(
            &env::base_url(&settings, base_url_flag),
            &env::api_key(&settings, api_key_flag),
        )
    }

    pub fn post(&self, path: &str, payload: &Value) -> Result<Value, ApiError> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self
            .agent
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-Anysearch-Client", CLIENT_HEADER);
        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        finish(request.send(body.as_bytes()))
    }

    pub fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value, ApiError> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self
            .agent
            .get(&url)
            .header("X-Anysearch-Client", CLIENT_HEADER);
        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }
        for (name, value) in params {
            request = request.query(name, value.clone());
        }
        finish(request.call())
    }
}

fn finish(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<Value, ApiError> {
    let response = match result {
        Ok(response) => response,
        Err(ureq::Error::Timeout(_)) => {
            return Err(ApiError::new("Timeout: The API request timed out."))
        }
        Err(error) => {
            return Err(ApiError::new(format!(
                "Connection Error: Unable to reach the API endpoint ({error})."
            )))
        }
    };
    let status = response.status().as_u16();
    let mut body = response.into_body();
    let text = body
        .read_to_string()
        .map_err(|_| ApiError::new("Connection Error: unable to read the response body."))?;
    parse_envelope(status, &text)
}

fn parse_envelope(status: u16, text: &str) -> Result<Value, ApiError> {
    let body: Value = match serde_json::from_str(text) {
        Ok(body) => body,
        Err(_) => {
            let preview: String = text.chars().take(500).collect();
            return Err(ApiError::new(format!(
                "Invalid JSON response (HTTP {status}): {preview}"
            )));
        }
    };
    let Some(object) = body.as_object() else {
        return Err(ApiError::new(format!(
            "Invalid API response (HTTP {status})."
        )));
    };
    let code = object
        .get("code")
        .and_then(|code| code.as_i64())
        .unwrap_or(0);
    if status >= 400 || code != 0 {
        let message = object
            .get("message")
            .and_then(|message| message.as_str())
            .filter(|message| !message.is_empty())
            .map(String::from)
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(ApiError {
            message,
            request_id: object
                .get("request_id")
                .and_then(|id| id.as_str())
                .unwrap_or_default()
                .to_string(),
            data: object.get("data").cloned(),
        });
    }
    Ok(body)
}

pub fn clamp_max_results(value: i64) -> i64 {
    value.clamp(1, 10)
}

/// 单条搜索的 REST 请求体(query 必填,其余字段可选)。
pub fn search_body(
    query: &str,
    tag: Option<&str>,
    params: Option<&Value>,
    zone: Option<&str>,
    language: Option<&str>,
    max_results: Option<i64>,
) -> Value {
    let mut body = Map::new();
    body.insert("query".to_string(), json!(query));
    if let Some(tag) = tag {
        body.insert("tag".to_string(), json!(tag));
    }
    if let Some(params) = params {
        body.insert("params".to_string(), params.clone());
    }
    if let Some(zone) = zone {
        body.insert("zone".to_string(), json!(zone));
    }
    if let Some(language) = language {
        body.insert("language".to_string(), json!(language));
    }
    if let Some(max_results) = max_results {
        body.insert(
            "max_results".to_string(),
            json!(clamp_max_results(max_results)),
        );
    }
    Value::Object(body)
}

/// 把 batch 条目规范化成 REST 请求体(兼容 `sub_domain`/`sub_domain_params` 字段)。
pub fn normalize_item(item: &Value) -> Result<Value, String> {
    let Some(object) = item.as_object() else {
        return Err("each query item must be an object".to_string());
    };
    let query = object
        .get("query")
        .and_then(|query| query.as_str())
        .filter(|query| !query.trim().is_empty())
        .ok_or_else(|| "query is required".to_string())?;
    let tag = non_empty_str(object, "tag").or_else(|| non_empty_str(object, "sub_domain"));
    let params = if let Some(raw) = object
        .get("params")
        .or_else(|| object.get("sub_domain_params"))
    {
        match raw.as_str() {
            Some(text) => Some(
                env::parse_params(text)
                    .ok_or_else(|| "params must be valid JSON or key=value pairs".to_string())?,
            ),
            None => Some(raw.clone()),
        }
        .filter(|params| !env::is_falsy(params))
    } else {
        None
    };
    let max_results = object.get("max_results").and_then(|value| value.as_i64());
    Ok(search_body(
        query,
        tag,
        params.as_ref(),
        non_empty_str(object, "zone"),
        non_empty_str(object, "language"),
        max_results,
    ))
}

/// 把共享参数注入到尚未自带该字段的条目上(条目自身字段优先)。
pub fn inject_shared(
    queries: &mut [Value],
    tag: Option<&str>,
    params: Option<&Value>,
    max_results: Option<i64>,
) {
    for item in queries.iter_mut() {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        if let Some(tag) = tag {
            if !present(object, "tag") && !present(object, "sub_domain") {
                object.insert("tag".to_string(), json!(tag));
            }
        }
        if let Some(params) = params {
            if !present(object, "params") && !present(object, "sub_domain_params") {
                object.insert("params".to_string(), params.clone());
            }
        }
        if let Some(max_results) = max_results {
            if !present(object, "max_results") {
                object.insert(
                    "max_results".to_string(),
                    json!(clamp_max_results(max_results)),
                );
            }
        }
    }
}

fn present(object: &Map<String, Value>, key: &str) -> bool {
    object
        .get(key)
        .map(|value| !env::is_falsy(value))
        .unwrap_or(false)
}

fn non_empty_str<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_body_omits_absent_fields() {
        let body = search_body("hello", None, None, None, None, None);
        assert_eq!(body, serde_json::json!({"query": "hello"}));
    }

    #[test]
    fn max_results_is_clamped() {
        assert_eq!(clamp_max_results(0), 1);
        assert_eq!(clamp_max_results(42), 10);
        let body = search_body("q", Some("finance.quote"), None, None, None, Some(50));
        assert_eq!(body["max_results"], serde_json::json!(10));
    }

    #[test]
    fn normalize_item_maps_legacy_fields() {
        let item = serde_json::json!({
            "query": "AAPL",
            "sub_domain": "finance.quote",
            "sub_domain_params": "type=stock,symbol=AAPL,cn_code="
        });
        let body = normalize_item(&item).unwrap();
        assert_eq!(body["query"], serde_json::json!("AAPL"));
        assert_eq!(body["tag"], serde_json::json!("finance.quote"));
        assert_eq!(body["params"]["symbol"], serde_json::json!("AAPL"));
    }

    #[test]
    fn inject_shared_keeps_item_values() {
        let mut queries = vec![
            serde_json::json!({"query": "a"}),
            serde_json::json!({"query": "b", "tag": "code.doc", "max_results": 2}),
        ];
        inject_shared(&mut queries, Some("finance.quote"), None, Some(5));
        assert_eq!(queries[0]["tag"], serde_json::json!("finance.quote"));
        assert_eq!(queries[0]["max_results"], serde_json::json!(5));
        assert_eq!(queries[1]["tag"], serde_json::json!("code.doc"));
        assert_eq!(queries[1]["max_results"], serde_json::json!(2));
    }

    #[test]
    fn envelope_errors_carry_request_id() {
        let err = parse_envelope(
            200,
            r#"{"code":4001,"message":"bad tag","request_id":"req_1"}"#,
        )
        .unwrap_err();
        assert_eq!(err.message, "bad tag");
        assert_eq!(err.request_id, "req_1");
        assert!(err.render().contains("request_id: req_1"));
    }
}
