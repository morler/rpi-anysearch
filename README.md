# rpi-anysearch

把 [AnySearch](https://anysearch.com) 的搜索能力接进 rpi：通用联网搜索、17 个垂直领域
的结构化搜索、最多 5 条查询的并发批量搜索、以及 URL 正文抽取（转 Markdown）。

这是 rpi 的 Rust `cdylib` 扩展，只依赖 `rpi-plugin-sdk`、`serde_json` 和 `ureq`（rustls），
**不需要**修改 rpi 核心，**不需要** Python / Node / jq / curl。

## 安装

```bash
rpi install rpi-anysearch                # 预编译包
rpi install rpi-anysearch --path . --force  # 本地构建
rpi uninstall rpi-anysearch              # 卸载
```

支持 Linux、macOS、Windows。

## 模型看到的工具

| 工具 | 作用 |
| --- | --- |
| `anysearch_search` | 单条搜索：`query`，可加 `tag`/`domain`+`sub_domain`/`params`/`zone`/`language`/`max_results` |
| `anysearch_get_sub_domains` | 垂直领域目录：列出某个 domain 下的 sub_domain 及其参数（必填项标注 `(required)`） |
| `anysearch_batch_search` | 最多 5 条查询并发；共享 `tag`/`params` 自动注入，结果按输入顺序返回，单条失败不影响其它 |
| `anysearch_extract` | 抓取网页正文并转 Markdown |

行为与参考实现 `/var/home/msc802/code/tools/anysearch-cli`（`anysearch search|batch_search|extract|get_sub_domains`）一致：

- 端点 `POST /v1/search`、`GET /v1/sub-domains`、`POST /v1/extract`，默认 `https://api.anysearch.com`。
- 校验规则同样对齐：`domain` 必须配 `sub_domain`；`domain` 必须是 `tag`/`sub_domain` 的前缀；
  `tag` 与 `sub_domain` 同时出现时取值必须相同；`zone` ∈ {`cn`,`intl`}；`max_results` 夹到 1–10；
  批量上限 5 条；`get_sub_domains` 上限 5 个 domain。
- `params` 既接受 `type=stock,symbol=AAPL,cn_code=` 形式，也接受 JSON 对象。

## 认证

```
工具参数 api_key  >  .env (ANYSEARCH_API_KEY)  >  环境变量 ANYSEARCH_API_KEY  >  匿名
```

匿名可用，但速率限制更低。`.env` 的查找顺序（先命中优先）：

1. 工具调用的 `cwd`（宿主注入的 `__rpi.cwd`）下的 `.env`
2. rpi 进程当前目录下的 `.env`
3. `~/.rpi/anysearch.env`

条目形如 `ANYSEARCH_API_KEY=<key>`，允许 `export ` 前缀与引号。端点可用
`ANYSEARCH_API_BASE_URL` 或工具参数 `base_url` 覆盖。

> 查询内容、URL 与 API key 都会发往 `https://api.anysearch.com`。不要把密码、个人信息
> 或商业秘密放进查询里。

## 技能分发

插件通过 `resources_discover` 把内置的 `SKILL.md`（frontmatter + 工具用法、决策流程、
不可信内容规则）交给宿主，并落盘到：

```
~/.rpi/agent/anysearch/SKILL.md      # RPI_CODING_AGENT_DIR 可覆盖前缀
```

路径刻意**不放在** `<agent_dir>/skills/` 下：那个目录会被宿主静态扫描，插件再返回同一份
文件就会以同一个 skill 名字注册两次，每次启动都打一条 collision 警告。写在这里则只有
`resources_discover` 这一个入口。目录名必须是 skill 名（`anysearch`），否则加载器会以
`name "anysearch" does not match parent directory` 拒绝它。

内容变化时才重写。落盘失败不影响工具本身，只是不会自动注入技能说明。

`/anysearch` 命令可离线打印同一份接口参考（不联网、不需要 key）。

## 安全

- `anysearch_extract` 返回的页面正文被明确标注为不可信外部数据（结果首行带
  `> **External page content (untrusted):**`），插件与技能说明都要求模型把它当数据、
  不当指令。
- 工具输出是 Markdown 文本；错误以纯文本返回（`Timeout: ...`、`Connection Error: ...`、
  `Request failed (HTTP 400): ... (request_id: ...)`），不会让模型去解析 JSON 包络。

## 不拖慢 agent

宿主的工具生命周期是 `execute` → `poll`（1ms 间隔）→ `destroy`，因此：

- `execute` 只起一个 `anysearch-request` 工作线程就返回，不做网络 I/O。
- `poll` 只查一次共享结果槽位，读不到就返回 `Pending`，没有阻塞调用。
- `cancel` 只置原子标记，工作线程在发请求前检查它。
- 批量搜索在 `std::thread::scope` 内并发，结果按输入顺序归位。
- HTTP 全局超时 30 秒，连接失败/超时都转成可读文本而不是 panic。

## 开发

```bash
cargo build                    # cdylib 给宿主，rlib 给测试
cargo test                     # 17 个纯函数单测，不需要宿主也不需要 key
cargo fmt --check && cargo clippy --all-targets
rpi dev --no-watch             # 编译加载一次
rpi install --path . --force   # 干净安装路径
```

模块划分：

| 文件 | 职责 |
| --- | --- |
| `src/lib.rs` | `export_plugin!` 入口、四个工具注册、`resources_discover`、`/anysearch` 命令 |
| `src/tool.rs` | 工具生命周期（`spawn_job`/`poll`/`cancel`/`destroy`）、参数校验、工具 schema |
| `src/api.rs` | HTTP 客户端、错误渲染、请求体构造与结果规范化 |
| `src/fmt.rs` | 搜索结果 / 领域目录 / 正文抽取的 Markdown 渲染 |
| `src/env.rs` | `.env` 加载、`params` 与 domain 列表解析 |
| `src/doc.rs` | 接口参考（约会用）与 `SKILL.md` 内容 |
| `src/consts.rs` | 领域列表、批量与 domain 上限、客户端标识头 |

## 不做的事

- 不内置 `register`（自动注册邮箱换 key）：需要用户确认写盘密钥的场景交给用户在
  anysearch 控制台完成。
- 不缓存搜索结果：每次调用都发真实请求。
- 不提供输出格式参数：输出固定为 Markdown。
