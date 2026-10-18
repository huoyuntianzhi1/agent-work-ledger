# Agent Work Ledger — 产品规划 v0.1

> 暂定名：**Agent Work Ledger**（智能体工作台账）
> 日期：2026-10-16
> 状态：规划阶段；阶段 0（数据审计）未启动

---

## 1. 一句话定位

**本地优先、只读聚合、艾森豪威尔驱动的跨智能体工作台账**。

解决的不是"看聊天记录"，而是回答三个问题：

1. 我在指定时间段**实际**做了什么（按项目和产出，不是按智能体）；
2. 哪些是噪声、哪些是核心推进，**为什么**这么判断（带证据）；
3. 接下来一周/一天我**应该**先做什么（按 Eisenhower 矩阵排好）。

差异化锚点：跨智能体聚合 + 项目级合并 + 证据可追溯 + Eisenhower 排程。市面上已有 Rewind/Granola/Screenpipe 倾向单一智能体或屏幕录像，本产品**不读屏幕**。

---

## 2. 对原方案的评价与修正

### 2.1 保留的强项
- 三层模型 `raw_event → work_span → task` 是对的。
- 插件式适配器策略是唯一可行的方式。
- 默认只读 + 密钥脱敏 + Mac Keychain 存 API Key，方向对。
- 阶段 0（数据审计）放在开发之前，非常专业。
- 验收标准具体可测，没有空话。
- "不能用屏幕读取当默认" 立场正确。

### 2.2 必须修正的弱点

| 原方案的薄弱点 | 修正方案 |
|---|---|
| "覆盖所有已安装智能体" 是个伪命题，会变成永远做不完的项目 | 第一版**最多 2–3 个最常用的智能体 + 1 个通用扫描器**，价值先验证再扩 |
| 跨智能体"按任务合并"比想象难 10 倍 | 第一版只做"按项目目录 + 时间窗聚类"，不做语义合并；合并结果给用户确认 |
| Eisenhower 让 AI 自动分类 → 会全是"待确认" | AI 给**建议**，用户**最终决定**；长期目标 / 截止时间由用户显式定义 |
| "工作产出"靠文件修改判定不靠谱 | 引入 **artifact 等级**：draft → committed → tested → deployed → user-confirmed |
| 数据量问题：重度用户一周 5k–10k 条消息 | 增量采集 + 缓存摘要 + 事件类型聚合（不存每条原文） |
| ToS 与合规边界没提 | 写免责说明；只读用户机器上的会话文件，不解析云端 |

### 2.3 没回答的关键问题（这一轮必须回答）
- 模型部署策略：默认本地 / 默认云 / 必须显式选？
- 是否需要云端账号登录？
- 跨设备同步是否需要？（建议：第一版**不做**，纯本地）
- 多用户 vs 单用户？（建议：**单用户单设备**，多用户后期再说）

---

## 3. 核心产品决策（默认方案，待确认）

| 维度 | 决策 | 备选 |
|---|---|---|
| 平台 | macOS 第一版 | 后续 Win/Linux |
| 形态 | Tauri 2 + React + TS + SQLite | Electron / 原生 Swift |
| 存储 | 本机 SQLite，可加密 | Postgres / DuckDB |
| 数据采集 | 适配器 + 通用扫描器 | + OS Accessibility（默认关闭） |
| 模型 | **默认本地 Ollama**，云端显式开启 | 全云 / 全本地 |
| 同步 | **不做** | iCloud Drive 文件级 |
| 计划输出 | Markdown 导出 + 应用内看板，不创建日历 | CalDAV 写入 |
| 自动执行 | **不自动执行** | 触发 hook |
| 项目命名 | "智能体工作台账 / Agent Work Ledger" | — |

---

## 4. 数据模型（修正版）

SQLite 表，按职责分离：

```
sources           # 智能体 / 工具数据源
connector_runs    # 每个适配器每次扫描的元数据
raw_events        # 原始事件（消息、文件、命令、git commit）
work_spans        # 同项目同时段的事件聚合
sessions          # 智能体级别会话（保留对话上下文，但不存全文）
tasks             # 跨智能体合并后的任务
artifacts         # 产出物（文件路径、commit、PR、文档）
evidence          # 结论到证据的多对多表
classifications   # Eisenhower 分类历史
reports           # 日报 / 周报 / 计划
tags              # 用户标签
privacy_rules     # 排除目录与脱敏规则
keychain_refs     # API Key 在 Keychain 的引用（不存明文）
```

### 关键字段示例（tasks）
```
id, title, summary, project_key,
status (draft|open|in_progress|done|blocked|cancelled),
artifact_level (draft|committed|tested|deployed|user_confirmed),
eisenhower_quadrant (Q1|Q2|Q3|Q4|unsure),
importance_score (1-5,user_set),
urgency_score (1-5,user_set or derived),
started_at, ended_at, duration_minutes,
source_ids[], confidence, evidence_ids[],
created_by (auto|user_corrected),
user_notes
```

### 重要原则
- **结论 → 证据**链路必须保留，用户可一键查看原始 chat/命令/file。
- 任何字段都有 `created_by` 区分自动推测 / 用户修正。
- 全部 del 走软删除 + tombstone，保留历史。

---

## 5. AI 处理管线（两阶段，修正版）

### 第一阶段：本地结构化（不依赖模型）
```text
raw_events
  ↓ rule + heuristic
work_spans
  ↓ 规则匹配（项目路径、时间窗、文件名聚类）
tasks (candidate)
```

做的事：
- 去重；
- 项目归一化（绝对路径 → 项目名）；
- 文件变更去重；
- 命令分类（test / build / deploy / debug）；
- Git commit 关联；
- 会话失败/中断识别；
- 标签与备注挂载。

### 第二阶段：模型语义层（可选本地/云）
输入：candidate tasks + 关联证据 + 用户长期目标
输出：
- 任务合并建议（带 confidence）；
- 跨智能体故事线（"login 改动"涉及 3 个智能体的版本演变）；
- Eisenhower 建议分类 + reasoning；
- 日报 / 周报 / 复盘；
- 下周计划模板。

约束：
- **每次云调用前显示将发送的字符数与字段**；
- **绝不把 secrets / 排除目录 / 大段原始文件 传给模型**；
- 模型输出必须带 `evidence_ids`，可一键回溯。

---

## 6. Eisenhower 矩阵实现要点

### 输入信号（不是只靠任务文字）
- 用户显式定义：长期目标 / 当前季度 OKR / 截止时间；
- 历史分类（用户经常把哪些任务放哪个象限）；
- 信号：
  - 含截止日期；
  - 命中生产/P0 关键词；
  - 触发其他人等待；
  - 与长期目标有链接；
  - 重复出现 ≥ 3 次；
  - 用户历史分类。

### 输出
- 推荐分类 + 重要性 / 紧急性 1-5 分；
- 推荐时段（晨间深度 / 下午交付 / 晚间复盘）；
- 冲突检测（同一时段多个 Q1 任务）。

### 守则
- 证据不足 → "unsure"，**不强行塞象限**；
- 用户改了分类 → 记录偏好，下次加权。

---

## 7. 技术栈

| 层 | 选型 | 理由 |
|---|---|---|
| 外壳 | Tauri 2 | 体积小、隐私好、Mac 原生菜单栏支持好 |
| UI | React 18 + TypeScript + Vite | 生态成熟 |
| 状态 | Zustand | 简单够用 |
| 样式 | Tailwind + Radix UI | 快速出原型 |
| DB | SQLite + sqlx / Drizzle ORM | 类型安全 |
| 后台采集 | Rust traits adapter | 与 Tauri 同语言 |
| 本地模型 | Ollama（llama3.1 8b / qwen2.5 14b） | 用户可换 |
| 云模型 | Anthropic / OpenAI / 自定义 endpoint | 用户选 |
| 时间线 | 虚拟列表 + 索引 | 长时段性能 |
| 文件索引 | ripgrep + 自维护 hash | 不要每次全文扫 |

---

## 8. 界面（修正版）

第一版六个页面：

1. **Dashboard**：时间窗选择 + 当周/当日摘要卡 + 智能体覆盖状态。
2. **Timeline**：统一时间线，按项目 + 智能体分组，可筛选。
3. **Tasks**：合并后任务列表，状态、artifact 等级、Eisenhower 标签、证据。
4. **Plan**：Eisenhower 四象限看板 + 日/周计划模板导出。
5. **Connectors**：每个数据源的"已发现 / 可读 / 需授权 / 不支持"四态，**绝不假装已采集**。
6. **Settings & Privacy**：时间范围、模型、排除目录、Keychain、备份。

主页面强调"我做了什么"，原始聊天记录藏在二级抽屉。

---

## 9. 隐私与安全（强制默认）

- 默认只读；
- 排除目录清单（用户可改），含 `.ssh/`, `.aws/`, `.gnupg/`, `.npmrc`, `**/secrets/**`, `**/*.pem`, `**/*.key`, `**/credentials*`, 用户配置的更多；
- 脱敏规则：
  - 任何匹配 `(sk-)[A-Za-z0-9]+`、`AKIA[0-9A-Z]{16}`、Bearer/JWT 模式的内容 → 入库前替为 `[REDACTED]`；
  - 命令参数中含 `--token`、`--password`、`-k` 后的值 → 脱敏；
  - 环境变量值不入库；
- 云调用必须显示「将上传 X 条事件 / Y 字符 / 含 N 个文件路径」并确认；
- API Key 存 macOS Keychain，应用层只持有 ref；
- SQLite 可选加密（SQLCipher）；
- 应用启动锁（Touch ID / 系统密码）；
- 一键导出 / 一键销毁；
- 不根据日志自动执行任何命令；
- 不读取屏幕、不发全局 Accessibility hook。

---

## 10. 实施路线（修正版）

### 阶段 0 — 数据审计（**今天，必须做**）
目标：回答"这台机能读到什么，不能读什么"。

执行：
1. 只读遍历 `/Users/apple` 顶层；
2. 对每个候选智能体扫描配置 / 会话目录，读取文件头、统计条目数、取样内容（不存全文）；
3. 输出 `connectors-feasibility.md`：
   - 每个智能体的实际路径；
   - 文件格式（jsonl / sqlite / protobuf / 自定义）；
   - 时间范围；
   - 是否需要授权；
   - 是否云端唯一；
   - 推荐采集方式；
4. 同步在仓库里建 `connectors/` 空目录，留下设计契约，但不实现。

验收：
- ✅ 能告诉我 **Codex++、Claude Code、WorkBuddy、DoubaoWork、CodeBuddy** 各自的路径和是否可读；
- ✅ 给出本机独有的发现（例如 `.config/Codex++/` 这个非主流路径）；
- ✅ 给出"哪些永远拿不到"清单；
- ✅ 不写任何适配器代码。

### 阶段 1 — 本地采集 MVP（采集 + 时间线，不上 AI）
目标：证明数据真的能读，能展示时间线。

范围：
- 1–2 个适配器（优先最熟的一个 + 一个通用扫描器）；
- SQLite + raw_events + work_spans；
- Timeline 页能筛选按项目 / 智能体；
- 文件 / Git commit 关联；
- 原始记录查看；
- 简单规则聚合（按项目 + 时间窗）。

不做：跨智能体合并、AI 分类、Eisenhower。

验收：
- 时间线能展示真实数据；
- 适配器失败时显示明确错误，不装死；
- 跑 1 周没人报数据丢失。

### 阶段 2 — AI 汇总
范围：
- 本地模型（Ollama）默认接入；
- 跨会话任务候选合并（带 confidence，要求人工确认）；
- 日报 / 周报生成；
- 项目视图与产出摘要。

不做：Eisenhower 自动分类。

### 阶段 3 — Eisenhower 与计划模板
范围：
- 四象限分类 + 用户手动覆盖；
- 长期目标与 OKR 输入；
- 日计划 + 周计划 + 每日复盘模板；
- Markdown / JSON / CSV 导出；
- 与 Apple Reminders（可选，手动确认）。

### 阶段 4 — 桌面交付与生态
- Tauri 安装包 / 菜单栏常驻；
- 后台增量采集（launchd timer）；
- 自动备份；
- 应用内更新；
- 完整 Connectors 诊断页 + 健康看板；
- 增加更多适配器。

---

## 11. 第一版（阶段 1）验收标准（不可妥协）

- ✅ 用户可选择起止时间；
- ✅ 至少 2 个真实智能体可读；
- ✅ Timeline 上能区分讨论 / 命令 / 文件修改 / Git 提交；
- ✅ 每个智能体显示真实采集状态，不假装；
- ✅ 排除目录生效，敏感文件不入库；
- ✅ 可一键导出当前报告为 Markdown；
- ✅ 应用崩溃 / 适配器失败时明确报错；
- ✅ 云模型默认关闭，开启前明确告知。

---

## 12. 你需要拍板的事

如果同意默认方案，下列决策可以暂时不动：

| 决策点 | 默认 | 待确认 |
|---|---|---|
| 模型默认本地 Ollama | ✅ | 是否需要支持云端双打 |
| 跨设备同步 | 不做 | 后期再说 |
| 是否写 Apple 日历 / 提醒事项 | 不自动 | 第一版不集成 |
| 项目命名 | "智能体工作台账" | 是否改名 |
| 第一批适配器 | Codex++ + Claude Code + 1 个通用扫描器 | 是否先砍掉不熟的 |
| iCloud / 网盘备份 | 不做 | 后面加 |

---

## 13. 下一步（王总拍板后立即可执行）

1. ✅ 是否按上述默认方案立项：**Yes / 部分修改 / 重新设计**；
2. ✅ 启动阶段 0 数据审计（只读、无副作用、半小时完成）；
3. ✅ 输出 `connectors-feasibility.md` 后再决定第一版连接器清单。

---

## 附录 A — 已在本机发现的相关痕迹（仅路径嗅探结果）

> 仅用于可行性证据，**不构成对产品的承诺**。

- Codex++：`~/.config/Codex++/`
- Claude Code：`~/.claude.json`、`~/.claude/`、`/Users/apple/claude/`（目录级）
- WorkBuddy：`/Users/apple/WorkBuddy/`
- DoubaoWork：`/Users/apple/DoubaoWork/`
- CodeBuddy：`/Users/apple/CodeBuddy/`
- Coze：`/Users/apple/Coze/`
- ai_completion：`/Users/apple/ai_completion/`
- bisheng：`/Users/apple/bisheng/`
- Multiple agent harnesses with skills: crush / opencode / goose / mux / qwen / openhands

阶段 0 必须证实每条路径的真实可读性与文件结构。
