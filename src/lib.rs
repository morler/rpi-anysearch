//! `rpi-anysearch` — AnySearch 工具插件。
//!
//! 注册四个工具：
//! - `anysearch_search`：单条搜索（通用或垂直）
//! - `anysearch_batch_search`：最多 5 条查询并发
//! - `anysearch_extract`：抓取网页正文（Markdown）
//! - `anysearch_get_sub_domains`：垂直领域目录（垂直搜索前必查）
//!
//! 另外通过 `resources_discover` 把一个 `SKILL.md` 分发给宿主，并在
//! `~/.rpi/agent/skills/anysearch/SKILL.md` 写入/刷新该文件，让模型知道各工具的
//! 使用顺序（先发现 sub_domain 再搜索）。

// `export_plugin!` 生成的入口会 deref 宿主传入的指针，lint 看不到宏展开。
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod api;
mod consts;
mod doc;
mod env;
mod fmt;
mod tool;

use std::ffi::c_void;
use std::path::PathBuf;

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
    for spec in tool::tool_specs() {
        let schema = Box::new(StableToolSchema {
            name: StbString::from_string(spec.name.to_string()),
            description: StbString::from_string(spec.description.to_string()),
            parameters: StbString::from_string(spec.parameters),
        });
        let schema_ptr = &*schema as *const StableToolSchema;
        let rc = register_tool(
            schema_ptr,
            spec.execute,
            tool::poll,
            tool::cancel,
            tool::destroy,
            plugin_free_string,
        );
        // 宿主在 register_tool 内部已复制 schema 字符串。
        drop(schema);
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
        let path = skill_path();
        let payload = match install_skill(&path) {
            Ok(()) => serde_json::json!({ "skillPaths": [path.to_string_lossy()] }),
            // 落盘失败时仍返回空数组；工具本身照常可用。
            Err(_) => serde_json::json!({ "skillPaths": [] }),
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

fn skill_path() -> PathBuf {
    // Deliberately NOT under `<agent_dir>/skills`: that directory is scanned
    // statically by the host, so a copy there would register the same skill
    // name twice and emit a collision warning on every startup. The parent
    // directory must be the skill name, or the loader rejects the metadata.
    agent_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("anysearch")
        .join("SKILL.md")
}

fn install_skill(path: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = doc::skill_markdown();
    // 只在内容变化时写盘，避免每次 reload 都动文件。
    if std::fs::read_to_string(path).ok().as_deref() == Some(content.as_str()) {
        return Ok(());
    }
    std::fs::write(path, content)
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

// 静默未使用参数类型（`c_void` 在 handler 宏签名里出现）。
#[allow(dead_code)]
fn _assert_ffi(_: *mut c_void) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_payload_points_at_skill_md() {
        let path = skill_path();
        assert!(path.ends_with("anysearch/SKILL.md"));
        assert!(path.is_absolute() || std::env::var_os("HOME").is_none());
    }

    #[test]
    fn skill_path_is_outside_the_statically_scanned_dir() {
        // `<agent_dir>/skills` is loaded by the host on its own; writing there
        // would duplicate the skill name and log a collision every start.
        let path = skill_path();
        let skills_dir = agent_dir().map(|dir| dir.join("skills"));
        if let Some(skills_dir) = skills_dir {
            assert!(!path.starts_with(skills_dir));
        }
    }
}
