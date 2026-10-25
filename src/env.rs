//! `.env` 加载与参数解析(沿用参考 CLI 的语义,但插件没有自己的 exe 目录,
//! 因此搜索顺序改为:工具调用 cwd → 宿主进程 cwd → `~/.rpi/anysearch.env`)。

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;

/// 依次加载候选 `.env`,同名 key 先出现的优先。
pub fn load_env(cwd_hint: Option<&str>) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for path in candidates(cwd_hint) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
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
                .trim_start_matches("export ")
                .trim()
                .trim_start_matches('\u{feff}');
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            if key.is_empty() || value.is_empty() {
                continue;
            }
            map.entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    map
}

fn candidates(cwd_hint: Option<&str>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let push = |path: PathBuf, paths: &mut Vec<PathBuf>| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    if let Some(cwd) = cwd_hint.filter(|cwd| !cwd.is_empty()) {
        push(PathBuf::from(cwd).join(".env"), &mut paths);
    }
    if let Ok(cwd) = std::env::current_dir() {
        push(cwd.join(".env"), &mut paths);
    }
    if let Some(home) = home_dir() {
        push(home.join(".rpi").join("anysearch.env"), &mut paths);
    }
    paths
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// `.env` 值优先于进程环境变量(与参考实现一致)。
pub fn lookup(env: &HashMap<String, String>, key: &str) -> String {
    env.get(key)
        .cloned()
        .or_else(|| std::env::var(key).ok())
        .unwrap_or_default()
}

/// 优先级:`api_key` 参数 > `.env` > 环境变量 > 匿名。
pub fn api_key(env: &HashMap<String, String>, flag: Option<&str>) -> String {
    match flag {
        Some(key) if !key.trim().is_empty() => key.trim().to_string(),
        _ => lookup(env, "ANYSEARCH_API_KEY"),
    }
}

pub fn base_url(env: &HashMap<String, String>, flag: Option<&str>) -> String {
    let raw = match flag {
        Some(url) if !url.trim().is_empty() => url.trim().to_string(),
        _ => lookup(env, "ANYSEARCH_API_BASE_URL"),
    };
    if raw.is_empty() {
        "https://api.anysearch.com".to_string()
    } else {
        raw.trim_end_matches('/').to_string()
    }
}

/// 解析 `params`:JSON → `{k:v,...}` → `k=v,k2=v2`;空结果视为无效。
pub fn parse_params(value: &str) -> Option<Value> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(value) {
        return if is_falsy(&parsed) {
            None
        } else {
            Some(parsed)
        };
    }
    if value.starts_with('{') && value.ends_with('}') {
        let inner = value[1..value.len() - 1].trim();
        if !inner.is_empty() {
            let mut map = Map::new();
            for pair in inner.split(',') {
                if let Some(index) = pair.find(':') {
                    let key = unquote(pair[..index].trim());
                    let val = unquote(pair[index + 1..].trim());
                    if !key.is_empty() {
                        map.insert(key, Value::String(val));
                    }
                }
            }
            if !map.is_empty() {
                return Some(Value::Object(map));
            }
        }
    }
    let mut map = Map::new();
    for pair in value.split(',') {
        if let Some(index) = pair.find('=') {
            let key = pair[..index].trim();
            let val = pair[index + 1..].trim();
            if !key.is_empty() {
                map.insert(key.to_string(), Value::String(val.to_string()));
            }
        }
    }
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
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

pub fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(flag) => !flag,
        Value::String(text) => text.is_empty(),
        Value::Number(number) => number.as_f64() == Some(0.0),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
    }
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn unquote(value: &str) -> String {
    value.trim_matches(|c| c == '\'' || c == '"').to_string()
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
        assert_eq!(parse_params("{}"), None);
        assert_eq!(parse_params(""), None);
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
