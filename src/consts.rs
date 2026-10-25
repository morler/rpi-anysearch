//! 与 AnySearch API 共享的常量。

/// 上报给服务端的客户端标识。
///
/// 版本号来自上游参考 CLI(`skill/3.1.1`),**不是**本 crate 的版本
/// (`Cargo.toml` 里的 `0.1.0`)。上游 skill 升版时只改这一处。
pub const CLIENT_HEADER: &str = "skill/3.1.1";

/// 支持的领域(`general` 也在这个列表里,它是兜底的通用搜索域)。
///
/// 这个数组同时是 JSON Schema 的 `enum`、运行时校验的权威来源,以及
/// `src/doc.rs` 里文档数量的推导来源 —— 只在这里增删领域。
pub const DOMAINS: [&str; 17] = [
    "general",
    "resource",
    "social_media",
    "finance",
    "academic",
    "legal",
    "health",
    "business",
    "security",
    "ip",
    "code",
    "energy",
    "environment",
    "agriculture",
    "travel",
    "film",
    "gaming",
];

/// 单次 batch 搜索最多条目数(API 的每请求上限)。
pub const MAX_BATCH_QUERIES: usize = 5;

/// 单次 `get_sub_domains` 请求最多领域数。
///
/// 这是 API 的每请求上限,与 [`DOMAINS`] 的条目数无关(17 个领域可以分多次查),
/// 不要"顺手"把它改成 `DOMAINS.len()`。
pub const MAX_DOMAINS: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_unique_and_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for domain in DOMAINS {
            assert!(seen.insert(domain), "duplicate domain `{domain}`");
            assert_eq!(
                domain,
                domain.to_lowercase(),
                "domain `{domain}` is not lowercase"
            );
            assert!(
                domain.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "domain `{domain}` must be snake_case ASCII"
            );
        }
    }

    #[test]
    fn general_is_present_and_first() {
        assert_eq!(DOMAINS[0], "general");
    }

    #[test]
    fn client_header_is_pinned() {
        // 上游 skill 版本；随上游升版时一起改。
        assert_eq!(CLIENT_HEADER, "skill/3.1.1");
    }
}
