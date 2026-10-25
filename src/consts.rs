//! 与 AnySearch API 共享的常量。

/// 上报给服务端的客户端标识(与参考 CLI 保持一致)。
pub const CLIENT_HEADER: &str = "skill/3.1.1";

/// 支持的垂直领域。
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

/// 单次 batch 搜索最多条目数。
pub const MAX_BATCH_QUERIES: usize = 5;

/// 单次 sub-domain 查询最多领域数。
pub const MAX_DOMAINS: usize = 5;
