//! `.env` 加载与参数解析。
//!
//! 搜索顺序:工具调用 cwd → 宿主进程 cwd → `~/.rpi/anysearch.env`。
//! 注意 `.env` 中的 cwd 可能来自不受信任的仓库,所以 endpoint/凭据的安全边界
//! 在 [`base_url`] 与本模块的 [`resolve`] 里做校验,而不是只依赖"文件先出现优先"。

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;

/// 官方 endpoint(未配置时的默认值)。
pub const OFFICIAL_BASE_URL: &str = "https://api.anysearch.com";

/// 依次加载候选 `.env`,同名 key 先出现的优先。
pub fn load_env(cwd_hint: Option<&str>) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for path in candidates(cwd_hint) {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // 文件不存在是正常情况(三个候选中通常只有零到一个是存在的)。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            // 权限、目录同名、非 UTF-8 等属于真实配置故障,不能静默当成"没有"。
            Err(error) => {
                eprintln!("anysearch: cannot read {}: {error}", path.display());
                continue;
            }
        };
        merge_env_text(&mut map, &text);
    }
    map
}

/// 把一个 `.env` 文件的内容并入 `map`;已存在的 key 保持首次出现的值。
///
/// 独立成函数是为了能测"先命中优先"这类语义,而不必去动进程 cwd / HOME。
fn merge_env_text(map: &mut HashMap<String, String>, text: &str) {
    // BOM 只在整文件开头;先去掉,免得它混进第一个 key。
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key
            .trim()
            .strip_prefix("export ")
            .unwrap_or(key.trim())
            .trim();
        let Some(value) = parse_env_value(value) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        // 空值也要保留:`KEY=` 表示"显式置空",这对 `ANYSEARCH_API_KEY=`
        // (强制匿名)是有意义的状态。
        map.entry(key.to_string()).or_insert(value);
    }
}

/// 解析 `.env` 右侧的原始值:剥离一对匹配引号,未加引号时去掉行内注释。
fn parse_env_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(rest) = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
    {
        return Some(rest.to_string());
    }
    // 未加引号:` #` 之后视为注释(`#` 紧贴值则保留,如 `KEY=abc#def`)。
    let value = match raw.find('#') {
        Some(index) if index > 0 && raw[..index].ends_with(char::is_whitespace) => {
            raw[..index].trim_end()
        }
        _ => raw,
    };
    Some(value.to_string())
}

fn candidates(cwd_hint: Option<&str>) -> Vec<PathBuf> {
    candidates_with(cwd_hint, std::env::current_dir().ok(), home_dir())
}

/// 候选路径：工具 cwd → 进程 cwd → `~/.rpi/anysearch.env`，重复路径只留一次。
///
/// 进程 cwd 与 HOME 作为参数传入，否则这两个全局状态会让顺序/去重无法测试。
fn candidates_with(
    cwd_hint: Option<&str>,
    process_cwd: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    if let Some(cwd) = cwd_hint.filter(|cwd| !cwd.is_empty()) {
        push(PathBuf::from(cwd).join(".env"));
    }
    if let Some(cwd) = process_cwd {
        push(cwd.join(".env"));
    }
    if let Some(home) = home {
        push(home.join(".rpi").join("anysearch.env"));
    }
    paths
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// `.env` 值优先于进程环境变量。
pub fn lookup(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key).cloned().or_else(|| std::env::var(key).ok())
}

/// 凭据来源。`Env` 表示继承自宿主环境(可能来自仓库里的 `.env`),
/// `Flag` 表示本次工具调用显式传入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Flag,
    Env,
    Anonymous,
}

/// 一次调用最终生效的连接配置。
#[derive(Debug, Clone)]
pub struct Settings {
    pub base_url: String,
    pub api_key: String,
    pub key_source: KeySource,
}

impl Settings {
    /// 是否可以把凭据发往当前 endpoint。
    ///
    /// 环境继承来的 key 只发往官方 host:否则一个仓库内 `.env` 把
    /// `ANYSEARCH_API_BASE_URL` 指向攻击者域名,就能把宿主的 key 顺带发出去。
    pub fn allows_auth(&self) -> bool {
        self.key_source == KeySource::Flag || is_official(&self.base_url)
    }
}

/// 优先级:`api_key` 参数 > `.env` > 环境变量 > 匿名。
pub fn api_key(env: &HashMap<String, String>, flag: Option<&str>) -> (String, KeySource) {
    match flag {
        Some(key) if !key.trim().is_empty() => (key.trim().to_string(), KeySource::Flag),
        _ => match lookup(env, "ANYSEARCH_API_KEY") {
            Some(key) if !key.trim().is_empty() => (key.trim().to_string(), KeySource::Env),
            _ => (String::new(), KeySource::Anonymous),
        },
    }
}

/// 解析并校验 endpoint:必须是 `https://<host>`,不做任何静默降级。
pub fn base_url(env: &HashMap<String, String>, flag: Option<&str>) -> Result<String, String> {
    let raw = match flag {
        Some(url) if !url.trim().is_empty() => url.trim().to_string(),
        _ => lookup(env, "ANYSEARCH_API_BASE_URL").unwrap_or_default(),
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(OFFICIAL_BASE_URL.to_string());
    }
    let trimmed = raw.trim_end_matches('/');
    let Some(rest) = trimmed.strip_prefix("https://") else {
        return Err(format!(
            "`base_url` must use https:// (got `{raw}`); plain http and other schemes are rejected"
        ));
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err(format!("`base_url` has no host: `{raw}`"));
    }
    // 只允许 host[:port][/path],拒绝空 host、空白、内嵌凭据(userinfo)等。
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty()
        || host.contains('@')
        || host.contains(char::is_whitespace)
        || host.starts_with(':')
    {
        return Err(format!("`base_url` has an invalid host: `{raw}`"));
    }
    Ok(trimmed.to_string())
}

/// 解析一次调用的生效配置。
pub fn resolve(
    cwd_hint: Option<&str>,
    api_key_flag: Option<&str>,
    base_url_flag: Option<&str>,
) -> Result<Settings, String> {
    let env = load_env(cwd_hint);
    let (api_key, key_source) = api_key(&env, api_key_flag);
    Ok(Settings {
        base_url: base_url(&env, base_url_flag)?,
        api_key,
        key_source,
    })
}

fn is_official(base_url: &str) -> bool {
    let rest = base_url
        .trim_end_matches('/')
        .strip_prefix("https://")
        .unwrap_or_default();
    let host = rest.split('/').next().unwrap_or_default();
    let official = OFFICIAL_BASE_URL
        .strip_prefix("https://")
        .unwrap_or_default();
    host.eq_ignore_ascii_case(official)
}

/// 解析 `params`:JSON 对象 → `{k:v,...}` → `k=v,k2=v2`。
///
/// 只接受对象:非对象的 `params` 会被服务端当成无关错误拒绝,本地报错更清楚。
pub fn parse_params(value: &str) -> Result<Value, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("`params` is empty".to_string());
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(value) {
        return match parsed {
            Value::Object(map) if !map.is_empty() => Ok(Value::Object(map)),
            Value::Object(_) => Err("`params` must not be an empty object".to_string()),
            other => Err(format!(
                "`params` must be a JSON object or key=value pairs, got {}",
                kind_of(&other)
            )),
        };
    }
    let inner = value
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .map(str::trim)
        .unwrap_or(value);
    let separator = if inner.contains('=') { '=' } else { ':' };
    let mut map = Map::new();
    for pair in split_pairs(inner) {
        let Some((key, val)) = pair.split_once(separator) else {
            return Err(format!(
                "`params` entry `{pair}` is not `key{separator}value`"
            ));
        };
        let key = unquote(key.trim());
        if key.is_empty() {
            return Err(format!("`params` entry `{pair}` has an empty key"));
        }
        map.insert(key, Value::String(unquote(val.trim())));
    }
    if map.is_empty() {
        return Err("`params` must contain at least one key".to_string());
    }
    Ok(Value::Object(map))
}

/// 按逗号切分,但保留引号内的逗号。
fn split_pairs(inner: &str) -> Vec<&str> {
    let mut pairs = Vec::new();
    let mut start = 0usize;
    let mut quote: Option<char> = None;
    for (index, ch) in inner.char_indices() {
        match (quote, ch) {
            (Some(active), current) if current == active => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(ch),
            (None, ',') => {
                let pair = inner[start..index].trim();
                if !pair.is_empty() {
                    pairs.push(pair);
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    let tail = inner[start..].trim();
    if !tail.is_empty() {
        pairs.push(tail);
    }
    pairs
}

/// 解析 `domains`:JSON(数组或单值)或逗号分隔列表。
pub fn parse_json_list(value: &str) -> Vec<String> {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::Array(items)) => items.iter().map(scalar_to_string).collect(),
        Ok(other) => vec![scalar_to_string(&other)],
        Err(_) => value
            .split(',')
            .map(|item| item.trim())
            .filter(|item| !item.is_empty())
            .map(String::from)
            .collect(),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn unquote(value: &str) -> String {
    if let Some(rest) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
    {
        return rest.to_string();
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_accept_json_and_pairs() {
        assert_eq!(
            parse_params(r#"{"type":"stock","symbol":"AAPL"}"#).unwrap(),
            serde_json::json!({"type": "stock", "symbol": "AAPL"})
        );
        assert_eq!(
            parse_params("type=stock,symbol=AAPL,cn_code=").unwrap(),
            serde_json::json!({"type": "stock", "symbol": "AAPL", "cn_code": ""})
        );
        assert_eq!(
            parse_params("{type:stock,symbol:AAPL}").unwrap(),
            serde_json::json!({"type": "stock", "symbol": "AAPL"})
        );
        assert!(parse_params("{}").is_err());
        assert!(parse_params("").is_err());
        assert!(parse_params("5").is_err());
        assert!(parse_params("[1,2]").is_err());
        assert!(parse_params("true").is_err());
    }

    #[test]
    fn params_keep_quoted_commas() {
        assert_eq!(
            parse_params(r#"q="a,b",zone=cn"#).unwrap(),
            serde_json::json!({"q": "a,b", "zone": "cn"})
        );
    }

    #[test]
    fn base_url_requires_https_and_host() {
        let env = HashMap::new();
        assert_eq!(
            base_url(&env, None).unwrap(),
            "https://api.anysearch.com".to_string()
        );
        assert_eq!(
            base_url(&env, Some("https://api.anysearch.com/")).unwrap(),
            "https://api.anysearch.com".to_string()
        );
        assert!(base_url(&env, Some("http://api.anysearch.com")).is_err());
        assert!(base_url(&env, Some("https://")).is_err());
        assert!(base_url(&env, Some("/")).is_err());
        assert!(base_url(&env, Some("https://user:pw@evil.example")).is_err());
        assert!(base_url(&env, Some("file:///etc/passwd")).is_err());
    }

    #[test]
    fn env_key_is_not_sent_to_a_repo_supplied_endpoint() {
        let env = HashMap::from([("ANYSEARCH_API_KEY".to_string(), "as_sk_x".to_string())]);
        let (key, source) = api_key(&env, None);
        let settings = Settings {
            base_url: "https://attacker.example".to_string(),
            api_key: key,
            key_source: source,
        };
        assert_eq!(settings.key_source, KeySource::Env);
        assert!(!settings.allows_auth());

        // 显式传入的 key 允许发往自定义 endpoint。
        let flagged = Settings {
            base_url: settings.base_url.clone(),
            api_key: settings.api_key.clone(),
            key_source: KeySource::Flag,
        };
        assert!(flagged.allows_auth());

        // 环境继承的 key + 官方 endpoint 允许。
        let official = Settings {
            base_url: OFFICIAL_BASE_URL.to_string(),
            api_key: settings.api_key.clone(),
            key_source: KeySource::Env,
        };
        assert!(official.allows_auth());
    }

    #[test]
    fn env_values_strip_one_quote_pair_and_comments() {
        assert_eq!(parse_env_value("\"a b\"").unwrap(), "a b");
        assert_eq!(parse_env_value("'a b'").unwrap(), "a b");
        assert_eq!(parse_env_value("value # note").unwrap(), "value");
        assert_eq!(parse_env_value("abc#def").unwrap(), "abc#def");
        assert_eq!(parse_env_value("").unwrap(), "");
    }

    #[test]
    fn env_candidates_keep_priority_order() {
        let paths = candidates_with(
            Some("/work/project"),
            Some(PathBuf::from("/work/project")),
            Some(PathBuf::from("/home/u")),
        );
        // 工具 cwd 与进程 cwd 相同时只保留一份。
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/work/project/.env"),
                PathBuf::from("/home/u/.rpi/anysearch.env"),
            ]
        );
    }

    #[test]
    fn env_candidates_skip_absent_sources() {
        let paths = candidates_with(Some("/a"), Some(PathBuf::from("/b")), None);
        assert_eq!(
            paths,
            vec![PathBuf::from("/a/.env"), PathBuf::from("/b/.env"),]
        );

        // 空 cwd 提示等同于未提供，避免把 `.env` 当成当前目录。
        let paths = candidates_with(Some(""), None, Some(PathBuf::from("/home/u")));
        assert_eq!(paths, vec![PathBuf::from("/home/u/.rpi/anysearch.env")]);

        assert!(candidates_with(None, None, None).is_empty());
    }

    #[test]
    fn merge_env_text_keeps_the_first_occurrence() {
        let mut map = HashMap::new();
        merge_env_text(&mut map, "ANYSEARCH_API_KEY=first\nOTHER=1\n");
        // 后续文件不得覆盖已加载的值。
        merge_env_text(&mut map, "ANYSEARCH_API_KEY=second\nOTHER=2\nTHIRD=3\n");
        assert_eq!(
            map.get("ANYSEARCH_API_KEY").map(String::as_str),
            Some("first")
        );
        assert_eq!(map.get("OTHER").map(String::as_str), Some("1"));
        assert_eq!(map.get("THIRD").map(String::as_str), Some("3"));
    }

    #[test]
    fn merge_env_text_ignores_noise_but_keeps_explicit_empties() {
        let mut map = HashMap::new();
        merge_env_text(
            &mut map,
            "\u{feff}# comment\n\nexport KEY=value\nNOEQUALS\n=orphan\nEMPTY=\n",
        );
        assert_eq!(map.get("KEY").map(String::as_str), Some("value"));
        // `KEY=` 是"显式置空"，必须保留而不是丢弃。
        assert_eq!(map.get("EMPTY").map(String::as_str), Some(""));
        assert!(!map.contains_key("NOEQUALS"));
        assert!(!map.contains_key(""));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn env_files_handle_bom_and_export() {
        let dir = std::env::temp_dir().join(format!("anysearch-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "\u{feff}export ANYSEARCH_API_KEY=as_sk_bom\n").unwrap();
        let env = load_env(Some(dir.to_str().unwrap()));
        assert_eq!(
            env.get("ANYSEARCH_API_KEY").map(String::as_str),
            Some("as_sk_bom")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn domain_lists_accept_json_and_csv() {
        assert_eq!(parse_json_list("finance,health"), vec!["finance", "health"]);
        assert_eq!(
            parse_json_list(r#"["finance","health"]"#),
            vec!["finance", "health"]
        );
        assert_eq!(parse_json_list("finance"), vec!["finance"]);
    }
}
