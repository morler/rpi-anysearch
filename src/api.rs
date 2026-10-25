//! REST 客户端、错误模型与搜索请求体构造。

use crate::consts::CLIENT_HEADER;
use crate::env;
use serde_json::{json, Map, Value};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// 响应体读取上限:防止恶意/异常响应耗尽宿主内存。
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

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

impl std::error::Error for ApiError {}

#[derive(Clone)]
pub struct Client {
    transport: Arc<dyn Transport>,
    api_key: String,
}

/// HTTP 方法。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Post,
    Get,
}

/// 一次出站请求的描述,还没落到具体网络实现上。
///
/// `query` 刻意保持结构化而不是预先拼进 `path`:百分号编码交给传输层做,
/// 免得在这儿引入编码回归。
#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// 传输层返回的原始响应(尚未解析信封)。
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// 出站请求的唯一出口。
///
/// 抽出来是为了让 `run_*` 的逻辑、并发与鉴权规则能在不发真 HTTP 的情况下被测到;
/// 真实 I/O、超时与响应体积上限全部集中在 [`UreqTransport`]。
pub trait Transport: Send + Sync {
    fn send(&self, request: Request) -> Result<Response, ApiError>;
}

/// 真实的 ureq 传输:负责 URL 拼接、query 附加、超时与响应体积上限。
struct UreqTransport {
    agent: ureq::Agent,
    base_url: String,
}

impl UreqTransport {
    fn new(base_url: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            // 4xx/5xx 也要读完响应体,否则拿不到信封里的 message/request_id。
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

impl Transport for UreqTransport {
    fn send(&self, request: Request) -> Result<Response, ApiError> {
        let url = format!("{}{}", self.base_url, request.path);
        let result = match request.method {
            Method::Post => {
                let mut call = self.agent.post(&url);
                for (name, value) in &request.headers {
                    call = call.header(name.as_str(), value.as_str());
                }
                finish(call.send(request.body.as_deref().unwrap_or("").as_bytes()))
            }
            Method::Get => {
                let mut call = self.agent.get(&url);
                for (name, value) in &request.headers {
                    call = call.header(name.as_str(), value.as_str());
                }
                for (name, value) in &request.query {
                    call = call.query(name.as_str(), value.clone());
                }
                finish(call.call())
            }
        };
        result
    }
}

impl Client {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            transport: Arc::new(UreqTransport::new(base_url)),
            api_key: api_key.to_string(),
        }
    }

    /// 用注入的传输层构造(只给测试用:真实路径不经过这里)。
    #[cfg(test)]
    pub fn with_transport(transport: Arc<dyn Transport>, api_key: &str) -> Self {
        Self {
            transport,
            api_key: api_key.to_string(),
        }
    }

    /// 从 `.env`/环境变量构造客户端;`base_url`/`api_key` 为工具参数覆盖值。
    ///
    /// 继承自环境的 key 不会发往非官方 endpoint(见 [`env::Settings::allows_auth`])。
    pub fn from_env(
        cwd_hint: Option<&str>,
        api_key_flag: Option<&str>,
        base_url_flag: Option<&str>,
    ) -> Result<Self, ApiError> {
        let settings =
            env::resolve(cwd_hint, api_key_flag, base_url_flag).map_err(ApiError::new)?;
        let api_key = if settings.allows_auth() {
            settings.api_key.as_str()
        } else {
            ""
        };
        Ok(Self::new(&settings.base_url, api_key))
    }

    /// 组装带鉴权与客户端标识的请求头。key 为空时**不发** `Authorization`,
    /// 而不是发一个空 Bearer。
    fn headers(&self, content_type: Option<&str>) -> Vec<(String, String)> {
        let mut headers = vec![("X-Anysearch-Client".to_string(), CLIENT_HEADER.to_string())];
        if let Some(content_type) = content_type {
            headers.push(("Content-Type".to_string(), content_type.to_string()));
        }
        if !self.api_key.is_empty() {
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", self.api_key),
            ));
        }
        headers
    }

    pub fn post(&self, path: &str, payload: &Value) -> Result<Value, ApiError> {
        let body = serde_json::to_string(payload)
            .map_err(|error| ApiError::new(format!("Failed to serialize request body: {error}")))?;
        let response = self.transport.send(Request {
            method: Method::Post,
            path: path.to_string(),
            query: Vec::new(),
            headers: self.headers(Some("application/json")),
            body: Some(body),
        })?;
        parse_envelope(response.status, &response.body)
    }

    pub fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value, ApiError> {
        let response = self.transport.send(Request {
            method: Method::Get,
            path: path.to_string(),
            query: params
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.clone()))
                .collect(),
            headers: self.headers(None),
            body: None,
        })?;
        parse_envelope(response.status, &response.body)
    }
}

/// 把 ureq 的结果与响应体转成 [`Response`]:错误映射、状态码与体积上限都在这里。
fn finish(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<Response, ApiError> {
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
    let body = response
        .into_body()
        .into_with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|error| {
            ApiError::new(format!(
                "Connection Error: unable to read the response body ({error})."
            ))
        })?;
    Ok(Response { status, body })
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
    // `code` 缺失按 0 处理;存在但不是整数则必须报错——否则 HTTP 200 信封里的
    // 应用层错误("4001")会被当成成功吞掉。
    let code = match object.get("code") {
        None | Some(Value::Null) => 0,
        Some(code) => code.as_i64().ok_or_else(|| {
            ApiError::new(format!(
                "Invalid API response (HTTP {status}): `code` is not an integer ({code})."
            ))
        })?,
    };
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
        Some(match raw {
            Value::String(text) => env::parse_params(text)?,
            Value::Object(map) if !map.is_empty() => raw.clone(),
            Value::Null => Value::Null,
            other => {
                return Err(format!(
                    "`params` must be an object or a key=value string, got {other}"
                ))
            }
        })
        .filter(|params| !params.is_null())
    } else {
        None
    };
    let max_results = match object.get("max_results") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .ok_or_else(|| format!("`max_results` must be an integer, got {value}"))?,
        ),
    };
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

/// 字段是否"由条目自己给出"。只看 key 是否存在(忽略 `null`),这样
/// 显式的 `"max_results": 0` 或 `"tag": ""` 不会被共享值覆盖,由后续校验报错。
fn present(object: &Map<String, Value>, key: &str) -> bool {
    object
        .get(key)
        .map(|value| !value.is_null())
        .unwrap_or(false)
}

fn non_empty_str<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
}

/// 测试用的传输层：记录每个请求，按注入的闭包回放响应。
///
/// 用 `cfg(test)` 门控，不进生产二进制，也不扩大公开 API。
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;

    /// 回放闭包：拿到请求，给出响应或错误。
    type Reply = Box<dyn Fn(&Request) -> Result<Response, ApiError> + Send + Sync>;

    pub struct FakeTransport {
        seen: Mutex<Vec<Request>>,
        reply: Reply,
    }

    impl FakeTransport {
        pub fn new(
            reply: impl Fn(&Request) -> Result<Response, ApiError> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                reply: Box::new(reply),
            })
        }

        /// 已收到的请求（按到达顺序）。锁只持有到克隆为止，避免并发测试里死锁。
        pub fn requests(&self) -> Vec<Request> {
            self.seen.lock().unwrap().clone()
        }

        pub fn client(self: &Arc<Self>, api_key: &str) -> Client {
            Client::with_transport(Arc::clone(self) as Arc<dyn Transport>, api_key)
        }
    }

    impl Transport for FakeTransport {
        fn send(&self, request: Request) -> Result<Response, ApiError> {
            self.seen.lock().unwrap().push(request.clone());
            (self.reply)(&request)
        }
    }

    /// 取某个请求头的值（大小写不敏感）。
    pub fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 200 + 信封的成功响应。
    pub fn ok(body: Value) -> Result<Response, ApiError> {
        Ok(Response {
            status: 200,
            body: body.to_string(),
        })
    }

    pub fn response(status: u16, body: Value) -> Result<Response, ApiError> {
        Ok(Response {
            status,
            body: body.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{header, ok, response, FakeTransport};
    use super::*;

    #[test]
    fn post_sends_json_and_client_header() {
        let fake = FakeTransport::new(|_| ok(json!({"code": 0, "data": {}})));
        let client = fake.client("as_sk_secret");
        client
            .post("/v1/search", &json!({ "query": "hi" }))
            .unwrap();
        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.path, "/v1/search");
        assert_eq!(header(request, "content-type"), Some("application/json"));
        assert_eq!(
            header(request, "x-anysearch-client"),
            Some(crate::consts::CLIENT_HEADER)
        );
        assert_eq!(
            header(request, "authorization"),
            Some("Bearer as_sk_secret")
        );
        assert_eq!(request.body.as_deref(), Some(r#"{"query":"hi"}"#));
    }

    #[test]
    fn empty_key_omits_authorization() {
        // 匿名调用不能发出空的 Bearer。
        let fake = FakeTransport::new(|_| ok(json!({"code": 0, "data": {}})));
        let client = fake.client("");
        client.post("/v1/search", &json!({})).unwrap();
        let requests = fake.requests();
        assert_eq!(header(&requests[0], "authorization"), None);
    }

    #[test]
    fn get_keeps_query_structured() {
        let fake = FakeTransport::new(|_| ok(json!({"code": 0, "data": {}})));
        let client = fake.client("");
        client
            .get(
                "/v1/sub-domains",
                &[
                    ("domain", "finance".to_string()),
                    ("domain", "health".to_string()),
                ],
            )
            .unwrap();
        let requests = fake.requests();
        let request = &requests[0];
        assert_eq!(request.method, Method::Get);
        // 同名参数必须重复而非合并,且值不能预先拼进 path。
        assert_eq!(
            request.query,
            vec![
                ("domain".to_string(), "finance".to_string()),
                ("domain".to_string(), "health".to_string()),
            ]
        );
        assert_eq!(request.path, "/v1/sub-domains");
        assert_eq!(header(request, "content-type"), None);
        assert!(request.body.is_none());
    }

    #[test]
    fn transport_error_is_mapped_to_the_shared_error_text() {
        let fake =
            FakeTransport::new(|_| Err(ApiError::new("Timeout: The API request timed out.")));
        let client = fake.client("");
        let error = client.post("/v1/search", &json!({})).unwrap_err();
        assert_eq!(error.render(), "Timeout: The API request timed out.");
    }

    #[test]
    fn http_error_status_is_parsed_from_the_body() {
        // 4xx 也要把信封里的 message / request_id 带出来。
        let fake = FakeTransport::new(|_| {
            response(
                400,
                json!({"code": 4001, "message": "bad tag", "request_id": "req_9"}),
            )
        });
        let client = fake.client("");
        let error = client.post("/v1/search", &json!({})).unwrap_err();
        assert_eq!(error.message, "bad tag");
        assert_eq!(error.render(), "bad tag (request_id: req_9)");
    }

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

    /// 真实 API 冒烟测试：`cargo test -- --ignored` 才跑，需要 ANYSEARCH_API_KEY。
    ///
    /// 覆盖修复后才可能出问题的路径：`resolve` 的 key 来源、`allows_auth`、
    /// 响应体积上限、`parse_envelope` 的宽松 `code` 处理。
    #[test]
    #[ignore = "hits the live AnySearch API"]
    fn live_search_and_extract() {
        let client = Client::from_env(None, None, None).expect("resolve settings");
        let body = search_body("rust programming language", None, None, None, None, Some(2));
        let envelope = client.post("/v1/search", &body).expect("live search");
        let text = crate::fmt::search_response(&envelope);
        assert!(text.starts_with("## Search Results"), "{text}");
        assert!(
            !text.contains("Invalid response"),
            "envelope shape changed: {text}"
        );

        let caps = client
            .get("/v1/sub-domains", &[("domain", "finance".to_string())])
            .expect("live sub-domains");
        let text = crate::fmt::capabilities_response(&caps, &["finance".to_string()]);
        assert!(text.contains("## finance Domain Capabilities"), "{text}");
    }
}
