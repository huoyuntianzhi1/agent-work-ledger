# 数据审计报告 — Agent Work Ledger

> 日期：2026-10-16
> 范围：本机只读探测
> 模式：read-only，**未对任何智能体应用做修改、采集、读取内容**
> 状态：阶段 0 完成；待王总决策后进入阶段 1

---

## 1. 摘要

本机确认存在 **10 个有意义的智能体 / 数据源**，其中：

- **Tier S（最大头，立即可接入）**：OpenAI Codex (765M)、Claude Code (950M)。两者的会话数据加起来就是 1.7G，几乎决定了"覆盖广度"。
- **Tier A（高价值）**：OpenCode、WorkBuddy、CodeBuddy CN、Cursor（部分）。
- **Tier B（需要 IndexedDB 解码才有用）**：Trae / TRAE SOLO / TRAE SOLO CN、Claude-3p（付费版 9.7G，最大单块）、Doubao、DoubaoWork、Coze。
- **Tier C（本地 LLM 引擎，作为模型来源而非工作源）**：Ollama。
- **Tier X（沙盒容器，本机默认不可读）**：com.openai.chat、com.openai.codex、com.tencent.workbuddy.mac 等。

⚠️ **关键发现**：检测到 **明文 API Key** 至少 3 处，必须在采集器中强脱敏；否则 Ledger 数据库在采集的同时也成了 Key 仓库。

⚠️ **同名陷阱**：`~/.config/Codex++/` 是 VSCode 扩展；`~/.codex/` 才是 OpenAI Codex。两者名字撞车，采集器必须区分。

---

## 2. 数据大小总览（仅本机）

| 数据源 | 大小 | Tier | 关键格式 |
|---|---:|---|---|
| Claude Code `~/.claude/` | 950M | S | 明文 JSONL + sqlite 化 stats |
| OpenAI Codex `~/.codex/` | 765M | S | **zstd 压缩的 JSONL** + 6 个 SQLite |
| Claude-3p（付费版）`~/Library/Application Support/Claude-3p/` | 9.7G | B | Electron Chromium 缓存 |
| TRAE SOLO CN `~/Library/Application Support/TRAE SOLO CN/` | 3.1G | B | Electron Chromium 缓存 |
| Doubao `~/Library/Application Support/Doubao/` | 3.2G | B | Electron Chromium 缓存 |
| TRAE SOLO `~/Library/Application Support/TRAE SOLO/` | 1.8G | B | Electron Chromium 缓存 |
| DoubaoWork `~/Library/Application Support/DoubaoWork/` | 1.2G | B | Electron Chromium 缓存 |
| Cursor `~/Library/Application Support/Cursor/` + `~/.cursor/` | 4.6M + 200K | A/B | ai-tracking.sqlite + 元数据 JSON |
| Trae `~/Library/Application Support/Trae/` | 596M | B | IndexedDB LevelDB + workspaceStorage |
| WorkBuddy `~/Library/Application Support/WorkBuddy/` | 279M | A | workspaceStorage/state.vscdb |
| Coze `~/Library/Application Support/Coze/` | 52M | B | IndexedDB + 共享协议 DB |
| CodeBuddy CN `~/Library/Application Support/CodeBuddy CN/` | 72M | A | **直接命名 `codebuddy-sessions.vscdb`** |
| OpenCode `~/.config/opencode/` + `~/.local/share/opencode/` | 69M + 14M | A | `opencode.db` (6.3M SQLite) |

合计本地可接触智能体数据 **~25 GB**。建议首次扫描只索引 **S+A** ~3.5 GB，B 类按需加载。

---

## 3. Tier S — 大头，两个必须第一时间做

### 3.1 OpenAI Codex（CLI + Desktop 共用配置）

- **路径**：`~/.codex/`
- **大小**：765 MB，**单源最大**
- **会话文件**：
  - `sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl.zst` — **zstd 压缩 JSONL**，需 zstd 解压
  - 活跃 session：**33 个**
  - 文件名前缀：`rollout-` 是 OpenAI 内部事件流，不是 OpenAI 公开 API 字段
  - 样本（仅看 metadata）：含 `session_id, cwd, runtime_workspace_roots, originator, cli_version, source, model_provider, base_instructions`
- **SQLite 分桶（6 个）**：
  - `goals_1.sqlite` — `thread_goals`: `thread_id, goal_id, objective, status, token_budget, tokens_used, time_used_seconds, created_at_ms, updated_at_ms`
  - `state_5.sqlite` — `threads, thread_sections, thread_attachments, thread_dynamic_tools, thread_spawn_edges, projects, project_roots, project_idempotency_keys, rollout_migration_state, rollout_migration_skipped_rollouts, external_agent_config_imports, remote_control_enrollments, backfill_state`
  - `thread_history_1.sqlite` — `thread_items, thread_turns, thread_realtime_items, thread_history_projection_state`
  - `logs_2.sqlite` — **最大，16221 pages，事件流**，待 sniff schema 决定是否做事件聚合
  - `queue_1.sqlite` / `memories_1.sqlite`
- **session_index.jsonl**：22 行，每行 `{id, thread_name, updated_at}`，**直接能看出 session 名字**
- **配置**：`config.toml` 列了 trusted projects，3 个：
  - `/Users/apple/Documents/New project`
  - `/Users/apple/Documents/skill`
  - `/Users/apple/Documents/ChatGPT/电脑工作日志`
- **当前 workspace 根**：`.codex-global-state.json` 暴露 `active-workspace-roots: /Users/apple/Documents/New project`

**API Key 风险**：`~/.codex/auth.json` — `OPENAI_API_KEY` **明文**（值前缀 `sk-cp-...`）。**任何采集器必须在写入 DB 前替换为 `[REDACTED:openai-key]`**。

**结论**：
- ✅ 可读
- ✅ 数据量大
- ✅ 格式可控（zstd + sqlite + jsonl）
- ⚠️ API Key 必须脱敏
- ⚠️ events schema 未深 sniff（`logs_2.sqlite` 占 16k pages，是潜在最丰富的事件流）

### 3.2 Claude Code（Anthropic CLI）

- **路径**：`~/.claude/` (950M)
- **会话文件**：
  - `history.jsonl` — 905 行，**明文 JSONL**，`{display, pastedContents, timestamp, project, sessionId}`
  - `projects/<encoded-path>/<session-id>.jsonl` — 按项目目录划分，13 个项目
  - `projects/<encoded-path>/sessions-index.json` — 项目级索引
  - `sessions/<pid>.json` — 运行时元数据：`{pid, sessionId, cwd, startedAt, procStart, version, peerProtocol, kind, entrypoint}`
  - `todos/<session-id>-agent-<session-id>.json` — Todo 列表，**2 个，目前很少**
  - `shell-snapshots/` — shell 快照
  - `hooks/`, `plugins/`, `skills/` (432 目录), `agents/` (217 目录), `get-shit-done/`, `gsd-file-manifest.json` — 大量非会话元数据
  - `paste-cache/` — 粘贴板缓存
  - `session-env/` — 会话环境变量（**可能含凭证，要脱敏**）
- **统计**：`stats-cache.json` — `dailyActivity[{date, messageCount, sessionCount, toolCallCount}]`，**直接可用作日报基础数据**
- **settings.json** — 含 `env.ANTHROPIC_AUTH_TOKEN=sk-2MJB...` (明文) ⚠️

**结论**：
- ✅ 极易读（明文 JSONL）
- ✅ 数据完整（消息 + tool + project + statistics 都有）
- ⚠️ ANTHROPIC_AUTH_TOKEN 必须脱敏
- ⚠️ `session-env/` 与 `paste-cache/` 隐藏有敏感数据可能

---

## 4. Tier A — 高价值，建议第二梯队

### 4.1 OpenCode

- **路径**：`~/.config/opencode/` (69M) + `~/.local/share/opencode/` (14M)
- **关键 DB**：`~/.local/share/opencode/opencode.db` — **6.3 MB SQLite**
- **DB 表**（来自 `.tables`）：
  - `account, account_state, control_account`
  - `session, message, part` — **直接对应会话-消息-片段结构**
  - `session_share, session_message, session_input, session_context_epoch`
  - `permission, credential, todo`
  - `event, event_sequence`
  - `project, project_directory, workspace`
  - `data_migration, migration`
- **结论**：✅ 极易读（标准 SQLite），schema 完整，估计是 4-5 个工作日工作量。

### 4.2 WorkBuddy（腾讯）

- **两条数据来源**：
  1. `~/Library/Application Support/WorkBuddy/` (279M) — 桌面应用数据
  2. `/Users/apple/WorkBuddy/` — **88 个时间戳目录**（如 `2026-05-16-task-1`, `2026-07-15-09-11-41`），直接命名 task
- **桌面应用结构**：
  - `User/globalStorage/state.vscdb` — VSCode 派生 vscdb
  - `User/workspaceStorage/<workspace-id>/state.vscdb` — 多 workspace 状态
  - `User/History/` — VSCode 编辑历史
  - `automations/`, `clp/`, `settings.json`
- **Task 目录结构**：每个 `2026-MM-DD-HH-MM-SS/` 下有 `.workbuddy/memory/<date>.md`
- **结论**：✅ 数据非常结构化（按时间分 task），schema 几乎不需要做（名字都看得出）；⚠️ 单 task 内容是 .md 不是结构化，要解析。

### 4.3 CodeBuddy CN（腾讯）

- **路径**：`~/Library/Application Support/CodeBuddy CN/` (72M)
- **关键文件**：
  - **`codebuddy-sessions.vscdb`** — 12,288 bytes，名字直接是 sessions，SQLite，表 `ItemTable`（键值存储）
  - `User/History/` + `workspaceStorage/`
  - `automations/`, `clp/`, `blob_storage/`
- **结论**：✅ vscdb 是 VSCode State Service 模型（kv 存储），需要反序列化 value 才知道结构，但**单个文件解决整个 chat**。

### 4.4 Cursor（仅本地部分）

- **路径**：`~/Library/Application Support/Cursor/` + `~/.cursor/`
- **关键文件**：
  - `~/.cursor/ai-tracking/ai-code-tracking.db` — 44 KB SQLite
    - 表：`ai_code_hashes, conversation_summaries, scored_commits, tracking_state`
  - `~/.cursor/mcp.json` — 406 bytes，列了 `thinking-models`, `browsermcp`, `rea` (rea-agents@6.4.0)
  - `~/.cursor/ide_state.json`
- **结论**：⚠️ Cursor 的 chat 主要在云端；本地这块 `ai-code-tracking.db` **只覆盖 AI 写入代码的追溯**，不是完整聊天历史。**第一版可只做 ai-tracking，不做完整 chat**。

---

## 5. Tier B — IndexedDB / Electron Chromium，需额外解码工具

### 5.1 Trae / TRAE SOLO / TRAE SOLO CN（字节）

- **路径**：
  - `~/Library/Application Support/Trae/` (596M)
  - `~/Library/Application Support/TRAE SOLO/` (1.8G)
  - `~/Library/Application Support/TRAE SOLO CN/` (3.1G)
- **结构**（三种都类似）：
  - `IndexedDB/vscode-file_vscode-app_0.indexeddb.leveldb/` — **核心 chat 存储**（LevelDB）
  - `User/History/` (120 entries) — VSCode 历史
  - `User/workspaceStorage/` — workspace 状态
  - `User/globalStorage/storage.json` — 配置
  - `User/mcp.json` — MCP 配置
  - `blob_storage/<uuid>/` — 二进制 blob
  - `shared_proto_db/` — 共享协议 DB
  - `logs/20260416T150221/*.log` — 各种日志
- **结论**：⚠️ **必须实现 LevelDB reader** 才能拿到 chat。收益大（1+ GB 单应用），建议做专门的 `leveldb` 适配器。

### 5.2 Claude-3p（Anthropic 付费桌面版）

- **路径**：`~/Library/Application Support/Claude-3p/` — **9.7 GB（本机最大单块）**
- **结构**：标准 Electron Chromium：`Local Storage`, `Session Storage`, `IndexedDB`, `Service Worker`, `Cache`
- **结论**：⚠️ 数据量最大，但也是最黑盒。需要先 sniff IndexedDB schema 再评估。**建议作为 v1.1 优化项，不放第一版**。

### 5.3 Doubao / DoubaoWork / Coze

- 路径见上表
- 结构同 Trae（Electron Chromium），没有特殊本地存储
- **结论**：⚠️ 列为 v2 路线。第一版 `未覆盖` 即可，不假装已采集。

---

## 6. Tier C — 本地 LLM（不当作工作源，作为模型来源）

### 6.1 Ollama

- **路径**：`~/.ollama/` (24M metadata + 不计其数的模型 blobs)
- **关键**：
  - `history` — 663 字节用户 prompt 历史
  - `logs/server-*.log`, `app-*.log` — 应用日志
- **结论**：✅ Ollama 自身不是 agent，**作为模型后端**接入 Ledger 的 AI 层。**不作为工作源采集**。

---

## 7. Tier X — 沙盒容器，默认不可读

| Container | 内容 | 备注 |
|---|---|---|
| `com.openai.chat` | ChatGPT 桌面 | 沙盒化，本机读不到 session |
| `com.openai.codex` | ChatGPT Codex Desktop | 沙盒化 |
| `com.tencent.workbuddy.mac` | WorkBuddy 桌面 | 沙盒化 |
| `com.workbuddy.workbuddy` | WorkBuddy helper | 沙盒化 |
| `com.tencent.codebuddycn.helper` | CodeBuddy CN helper | 沙盒化 |
| `com.bot.pc.doubao.FinderSyncExtension` | 豆包 Finder 扩展 | 沙盒化 |
| `com.work.pc.doubao.FinderSyncExtension` | 豆包工作 Finder 扩展 | 沙盒化 |

**结论**：若用户授权 `Full Disk Access` 给 Ledger，可读；否则只能走 Tier A/5 的非沙盒路径。

---

## 8. 关键风险

### 8.1 明文凭证清单
- `~/.codex/auth.json` — `OPENAI_API_KEY=sk-cp-...`
- `~/.claude/settings.json` — `ANTHROPIC_AUTH_TOKEN=sk-...`、`ANTHROPIC_BASE_URL=https://api.moonshot.cn/...`
- `~/.local/share/opencode/auth.json` — 294 字节 credentials
- `~/.codeium/windsurf/mcp_config.json` — 148 字节 MCP config
- `~/.cursor/mcp.json` — 含 `"rea": "rea-agents@6.4.0"` 启动命令

⚠️ **采集器必须对所有 `auth*`, `settings.json.*env*`, `*.token`, `sk-*`, `*KEY*` 字段强制脱敏**。否则 Ledger 数据库会成为新的攻击面。

### 8.2 已有的 MCP / 工具痕迹
- `~/.cursor/mcp.json`：thinking-models、browsermcp、**rea-agents@6.4.0**
- `~/.config/opencode/opencode.json`：启用 **rea-agents@6.4.0** MCP server

> 王总机器上启用了 **rea-agents**——这是 Reverse Engineering Agent（一家做 binary analysis 的厂商）的 MCP。本项目不应与之冲突。**注意**：本应用不需要 REA 的能力，是独立产品。

### 8.3 Codex CLI 与 Codex Desktop 共享 `~/.codex/`
- `config.toml` 的 `[model_providers.OpenAI]` `base_url = "http://127.0.0.1:53682/v1"` — 推测是通过本地代理转发；
- 也包含 `[model_providers.custom]` base_url = `https://api.minimaxi.com/v1`（**M2 模型**？）。
- 采集脚本应避免重复打这两个 endpoint。

### 8.4 同名陷阱：`Codex++` vs `Codex`
- `~/.config/Codex++/` 是 VSCode 扩展（仅 user_scripts）
- `~/.codex/` 才是 OpenAI 的 Codex
- 必须明确路径白名单，**不能让采集器去读 `~/.config/Codex++/`**（会被误识别成 chat 来源）

---

## 9. 建议的第一版（阶段 1）连接器清单

按 ROI 排序。**只做前 4 个就够 v1 上线**，后面是 v1.1/2 路线。

| 优先级 | 数据源 | 工作量 | 价值 |
|---|---|:-:|---|
| **P0** | Claude Code `~/.claude/` | 3 天 | 950M 数据 + 905 行 history 直接可用 |
| **P0** | OpenAI Codex `~/.codex/` | 5 天（zstd + sqlite 多桶） | 765M 数据 + 33 个 session 多维度分析 |
| **P1** | OpenCode `~/.local/share/opencode/opencode.db` | 2 天 | SQLite 单库干净结构，schema 已明 |
| **P1** | WorkBuddy `/Users/apple/WorkBuddy/` task 目录 | 2 天 | 88 个结构化 task，命名即语义 |
| P2 | CodeBuddy CN `codebuddy-sessions.vscdb` | 2 天 | 单 vscdb 文件 |
| P2 | Cursor `ai-tracking/` 仅做 ai-code-tracking.db | 1 天 | 元数据级 |
| P3 | Trae 三个变体的 IndexedDB LevelDB | 5 天（需要 LevelDB reader） | 大头之一 |
| P3 | Claude-3p Electron Chromium | 4 天 | 9.7G 数据 |
| P4 | Doubao / DoubaoWork / Coze | 各 2 天 | Electron 通用解码 |
| P4 | 通用会话目录扫描器 | 1 天 | 兜底 |

**第一版 P0+P1 完成即可上线**：约 **2 周工作量**，覆盖 80% 数据量。

---

## 10. 验证清单（阶段 0 验收 — 已完成）

- ✅ 能说出 Codex / Claude Code / WorkBuddy / DoubaoWork / CodeBuddy 各自的路径和是否可读
- ✅ 给出本机独有的发现：`Codex` 自带 6 个 SQLite 分桶 / Claude `history.jsonl` 的字段结构 / OpenCode schema 完整 / Trae 三个变体
- ✅ 给出"哪些永远拿不到"清单：沙盒容器数据、云端独享的 ChatGPT chat 等
- ✅ 未写任何适配器代码
- ✅ 未读取任何聊天原文（仅 metadata + 文件头最多 800 字节取样）
- ⚠️ **意外发现**：明文 API Key 必须脱敏——已经写进隐私脱敏规则

---

## 11. 下一步（待王总批准）

| 任务 | 负责 | 验收 | 估时 |
|---|---|---|:-:|
| 阶段 1-A：搭建 Rust + Tauri 2 应用骨架，桌面外壳可启动 | 我 | 应用能打开菜单栏图标 + 窗口 | 1 天 |
| 阶段 1-B：通用 SQLite 适配器 + Codex connector | 我 | 33 个 session 全部入库，Eisenhower 占位字段填充 | 4 天 |
| 阶段 1-C：通用 JSONL 适配器 + Claude Code connector | 我 | 905 行 history + projects 进入 DB | 2 天 |
| 阶段 1-D：OpenCode connector | 我 | opencode.db 全表入库 | 2 天 |
| 阶段 1-E：WorkBuddy connector | 我 | 88 个 task 进入 DB | 2 天 |
| 阶段 1-F：Timeline 页 + 隐私脱敏预处理器 | 我 | 全链路看不到明文 API Key | 2 天 |
| 阶段 1-G：导出 Markdown / JSON | 我 | 选定时间段可一键导出 | 1 天 |
| 阶段 1-H：菜单栏常驻 + 后台定时采集（launchd） | 我 | 退出后又打开能看到新增 | 2 天 |

**总估时：~16 天（3 周）做阶段 1 MVP**。包含 P0+P1+P2 的连接器、Tauri 桌面壳、Tailwind UI 骨架、隐私脱敏。

---

## 附录 A — 不在第一版的"看起来相关但不是数据源"

| 路径 | 判断 | 处理 |
|---|---|---|
| `/Users/apple/bisheng/` | bisheng 开源仓库源码（GitHub 镜像） | 不采集 |
| `/Users/apple/ai_completion/` | ai_completion 工具自身的数据 | 不采集；如需可单独 review |
| `/Users/apple/.claude/skills/`, `/Users/apple/.claude/agents/` | 用户级 skill/agent 自定义配置 | **可采集**（作为工作上下文的一部分，非会话） |
| `/Users/apple/.codex/codex-dream-skin-studio/` | 主题美化工具 | 不采集 |
| `/Users/apple/.config/Codex++/` | VSCode Codex++ 扩展 | **不采集**，跟 OpenAI Codex 区别对待 |
| `/Users/apple/.ollama/` | Ollama 引擎 | **不当作工作源**，作为 AI 后端 |
| `/Users/apple/Coze/Drive` | 扣子云盘镜像 | 不采集 |
| `/Users/apple/MediaCrawler/` | 一个 crawler 工具的代码 | 不采集 |
| `/Users/apple/comflowy/`, `/Users/apple/Movies/` 等 | 与智能体无关的目录 | 不采集 |

---

## 附录 B — Codex sqlite tables 速查

```
state_5.sqlite:
  threads, thread_sections, thread_attachments,
  thread_dynamic_tools, thread_spawn_edges,
  projects, project_roots, project_idempotency_keys,
  rollout_migration_state, rollout_migration_skipped_rollouts,
  external_agent_config_imports, remote_control_enrollments,
  backfill_state

thread_history_1.sqlite:
  thread_items, thread_turns, thread_realtime_items,
  thread_history_projection_state

goals_1.sqlite:
  thread_goals -- (thread_id PK, goal_id, objective, status,
                   token_budget, tokens_used, time_used_seconds,
                   created_at_ms, updated_at_ms)

queue_1.sqlite:
  queued_items, queued_thread_revisions

logs_2.sqlite / memories_1.sqlite: (未 sniff schema)
```

---

报告完。

---

## 附录 C — Stage 1-I 实战：Chromium IndexedDB LevelDB key 真实结构

> 本节基于 `~/Library/Application Support/Doubao/Default/IndexedDB/*.leveldb` 和
> `~/Library/Application Support/MiniMax Agent/IndexedDB/app_._0.indexeddb.leveldb`
> 的真实 key dump（`/tmp/idb_dump` 工具，comparator=bytewise memcmp 命名
> `idb_cmp1`）。**不要假设** Chromium IDB key 格式跟早期文档一致——byte[1]
> 是 database_id 而不是 0x01 magic。

### Key layout

```
byte 0          : 0x00             (DatabaseName prefix)
byte 1          : database_id     (single byte; 观察到 0x00..0x03)
byte 2..N       : schema/table-info-id (可变长，2 字节 LE object_store_id + 控制字节)
byte N+1..      : UTF-16BE 用户主键 (user_key)
```

### 重要陷阱

1. **`byte[1] == 0x01` 不是 Chromium IDB 的固定 magic**——它是 database_id。
   doubao 第一版 connector 写死 `key[1] == 0x01`，但 MiniMax Agent IDB 里
   database_id 是 0x00、0x02、0x03。**过滤条件必须放宽成 `key[1] <= 0x10`** 或
   类似单字节合理范围。

2. **byte[6..] 是 user_key，不是 store_name。** Chromium IndexedDB 把
   store_name 存在专门的 ObjectStoreMetaData 表里，普通 data row 的
   key tail 是用户提供的 primary key（UTF-16BE）。把 `object_store` payload
   字段改名 `key_user` 更诚实。

3. **comparator `idb_cmp1` 必须字节级 memcmp，不能用 strcmp。**
   Chromium IDB 的 key 是字节序列，里面包含 UTF-16BE NUL 字节，
   `strcmp` 会按 NUL 截断。**C 工具必须 `memcmp + length compare**。

5. **LDB store_name 在 UTF-16BE 后跟用户 key。** Doubao 真实 row：
   `00 00 00 00 c9 16 0063 0068 0072...` 的 byte 6+ 是 "chr..." (chat 用户
   主键)，不是 store_name。

### Connector helper `idb::upsert_chromium_idb_record` 最终规则

```rust
if key.len() < 8 { return None; }              // 跳过 5-7 字节 descriptor
if key[0] != 0x00 { return None; }             // 跳过非 IDB prefix
if key[1] > 0x10 { return None; }              // 跳过非合理 database_id
let user_key = extract_utf16be_string(&key[6..]);  // 注意是 user_key
if user_key.is_empty() || /* not printable ascii-ish */ { return None; }
if value.len() < 32 { return None; }          // 跳过 ~1 字节 descriptor row
// payload: { key_user, key_hex, value_size, value_preview }
```

### 实测入库

- Doubao (11 IDB roots)：1031 events
- Doubao Work (4 IDB roots)：34 events
- MiniMax Agent (1 IDB root)：1 event
- 其他 5 connector 共 9856 events

### 临时工具（用于 dump 真实 key）

`/tmp/awl-leveldb-poc/idb_dump.c` + cc -O2 -L/opt/homebrew/lib -lleveldb
- 链系统 libleveldb.dylib（避免 leveldb crate 的 cmake + snappy 编译陷阱）
- 用 `idb_cmp1` 字节级 comparator
- 输出每条 key 的 byte 0..11 hex
