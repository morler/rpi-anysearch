//! `rpi-anysearch` — AnySearch 工具插件。
//!
//! 注册四个工具：
//! - `anysearch_search`：单条搜索（通用或垂直）
//! - `anysearch_batch_search`：最多 5 条查询并发
//! - `anysearch_extract`：抓取网页正文（Markdown）
//! - `anysearch_get_sub_domains`：垂直领域目录（垂直搜索前必查）
//!
//! 另外通过 `resources_discover` 把一个 `SKILL.md` 分发给宿主，并写入
//! `<agent_dir>/skills/anysearch/SKILL.md`（`agent_dir` 默认 `~/.rpi/agent`，可用
//! `RPI_CODING_AGENT_DIR` 覆盖），让模型知道各工具的使用顺序（先发现
//! sub_domain 再搜索）。这个路径就是宿主静态扫描 skill 的目录：两者指向同一个
//! 文件时宿主按名去重、只注册一次，插件更新会真正生效。

// `export_plugin!` 生成的入口会 deref 宿主传入的指针，lint 看不到宏展开。
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod api;
mod consts;
mod doc;
mod env;
mod fmt;
mod tool;

use std::path::{Path, PathBuf};

use rpi_plugin_sdk::callbacks::free_string as plugin_free_string;
use rpi_plugin_sdk::{PluginApi, StableToolSchema, StbString, StbStringRef};

rpi_plugin_sdk::export_plugin!(|api: &PluginApi| {
    if let Some(declare) = api.declare {
        let rc = declare(StbStringRef::from_str(
            r#"{"priority":55,"platforms":["linux","macos","windows"]}"#,
        ));
        if rc != 0 {
            return rc;
        }
    }

    let Some(register_tool) = api.register_tool else {
        return 1;
    };
    // schema 用 `Box::leak` 保活到进程退出：`register_tool` 拿到的是裸指针，
    // 只有宿主深拷贝结构体和三个 `StbString` 缓冲时才安全。SDK 没有把这个复制
    // 约定写成可验证的契约，泄漏几个 schema 的代价远小于 use-after-free。
    for spec in tool::tool_specs() {
        let schema: &'static StableToolSchema = Box::leak(Box::new(StableToolSchema {
            name: StbString::from_string(spec.name.to_string()),
            description: StbString::from_string(spec.description.to_string()),
            parameters: StbString::from_string(spec.parameters),
        }));
        let rc = register_tool(
            schema as *const StableToolSchema,
            spec.execute,
            tool::poll,
            tool::cancel,
            tool::destroy,
            plugin_free_string,
        );
        if rc != 0 {
            return rc;
        }
    }

    if let Some(register_resources_discover) = api.register_resources_discover {
        let rc = register_resources_discover(
            on_resources_discover,
            plugin_free_string,
            std::ptr::null_mut(),
        );
        if rc != 0 {
            return rc;
        }
    }

    if let Some(register_command) = api.register_command {
        let rc = register_command(
            StbStringRef::from_str("anysearch"),
            StbStringRef::from_str("Print the AnySearch interface reference (offline)"),
            on_doc_command,
        );
        if rc != 0 {
            return rc;
        }
    }

    0
});

// ---------------------------------------------------------------------------
// 资源发现：把内置 skill 写到 agent 目录并交给宿主
// ---------------------------------------------------------------------------

rpi_plugin_sdk::string_handler! {
    /// `resources_discover(cwd, reason, out, user_data) -> i32`。
    ///
    /// 插件无法得知自身 `.so` 路径（SDK 无 dladdr 类 API），因此把内置 SKILL.md
    /// 落盘到 agent 目录下的稳定位置，再把这个路径交给宿主。
    fn on_resources_discover((_cwd, _reason), out, _user_data) {
        let payload = match skill_path() {
            None => {
                eprintln!(
                    "anysearch: cannot determine the agent directory (RPI_CODING_AGENT_DIR, \
                     HOME and USERPROFILE are all unset); skipping SKILL.md installation"
                );
                serde_json::json!({ "skillPaths": [] })
            }
            Some(path) => match install_skill(&path) {
                Ok(()) => serde_json::json!({ "skillPaths": [path.to_string_lossy()] }),
                Err(error) => {
                    eprintln!(
                        "anysearch: failed to install skill at {}: {error}",
                        path.display()
                    );
                    serde_json::json!({ "skillPaths": [] })
                }
            },
        };
        i32::from(!out.write(payload.to_string()))
    }
}

fn agent_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RPI_CODING_AGENT_DIR") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return Some(path);
        }
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".rpi").join("agent"))
}

/// 解析 skill 落盘路径；无法确定绝对 agent 目录时返回 `None`。
///
/// 写在 `<agent_dir>/skills/anysearch/SKILL.md`，也就是宿主静态扫描的目录。
/// 这与“把副本放到扫描目录会重复注册”的直觉相反，但实测（rpi 0.8.5）是：
/// 宿主按 skill 名去重，**不告警**，静态扫描的副本与 `resources_discover`
/// 返回的路径指向同一个文件时只会注册一次。反过来，放到 `skills/` 之外会让
/// 用户手拷贝的旧副本静默胜出，插件更新永远不生效。
fn skill_path() -> Option<PathBuf> {
    agent_dir().map(|dir| dir.join("skills").join("anysearch").join("SKILL.md"))
}

fn install_skill(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = doc::skill_markdown();
    // 只在内容变化时写盘，避免每次 reload 都动文件。
    if std::fs::read_to_string(path).ok().as_deref() == Some(content.as_str()) {
        return Ok(());
    }
    // 先写同级临时文件再 rename：`fs::write` 会先截断，并发读取（或中途崩溃）
    // 会看到半截内容；rename 同时替换已存在的符号链接而不是跟随它。
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// `/anysearch` 命令：离线打印接口说明
// ---------------------------------------------------------------------------

rpi_plugin_sdk::string_handler! {
    fn on_doc_command(_args_json, out, _user_data) {
        let text = doc::render();
        i32::from(!out.write(
            serde_json::json!({ "kind": "message", "text": text, "markdown": true }).to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_payload_points_at_skill_md() {
        if let Some(path) = skill_path() {
            assert!(path.ends_with("anysearch/SKILL.md"), "{}", path.display());
            assert!(path.is_absolute(), "{}", path.display());
        }
    }

    #[test]
    fn skill_path_lives_in_the_scanned_skills_dir() {
        // 必须落在宿主静态扫描的 `skills/<name>/SKILL.md`：与 `resources_discover`
        // 返回的路径是同一个文件时，宿主按名去重只注册一次（实测 rpi 0.8.5），
        // 因此内容是插件写的那份而不是用户手拷贝的旧版本。
        if let (Some(path), Some(skills_dir)) =
            (skill_path(), agent_dir().map(|dir| dir.join("skills")))
        {
            assert!(path.starts_with(&skills_dir), "{}", path.display());
            assert_eq!(
                path.parent().and_then(|parent| parent.file_name()),
                Some(std::ffi::OsStr::new("anysearch")),
                "the parent directory must be the skill name: {}",
                path.display()
            );
        }
    }

    #[test]
    fn install_skill_writes_via_rename() {
        let dir = std::env::temp_dir().join(format!("anysearch-skill-{}", std::process::id()));
        let path = dir.join("skills").join("anysearch").join("SKILL.md");
        install_skill(&path).unwrap();
        assert!(path.exists());
        // 临时文件不应残留。
        assert!(!path.with_extension("md.tmp").exists());
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.starts_with("---\nname: anysearch\n"));
        // 第二次写入是 no-op（内容相同）。
        install_skill(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        std::fs::remove_dir_all(&dir).ok();
    }
}
