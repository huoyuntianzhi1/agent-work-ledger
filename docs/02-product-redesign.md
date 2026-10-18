# Agent Work Ledger — v0.2 产品重设计方案

> v0.1 (current) 解决的是"**有没有**"：能 ingest 7 个 connector 的数据，能看到 raw events，能跑 Eisenhower planner，能本地 Ollama 跑 prompt。但 v0.1 不解决用户要的**核心价值**："我今天/这周到底做了什么、什么重要、什么该优先做"。
>
> 本文档重新定义 v0.2 产品形态，并给出可直接执行的实施计划。

## 1. 问题陈述

### 1.1 用户痛点（明确）

用户每天同时跑 Codex + Claude Code + Doubao + 偶尔 MiniMax Agent 等多个 agentic CLI，**散落在 7 个目录、4 种数据格式**（jsonl / sqlite / zstd / chromium-idb）。他想知道的不是"原始 event 是什么"，而是：

1. **今天实际完成了什么**（不是"跑了 50 条命令"——而是"修了 3 个 bug + 写了一篇 design doc + 处理了 12 条 IM 闲聊")
2. **用了哪些 agent / 项目 / 工具**（"今天 70% 时间在 Codex 的 stripe-webhook 项目上，drove tool_use 28 次"）
3. **什么重要 / 什么紧急 / 判断依据**（不是启发式 — 是基于完整上下文的 reasoning）
4. **哪些只是响应噪声 / 低价值**（"那 12 条 Doubao 闲聊其实没产出"）
5. **下一阶段该优先做什么**（基于近期项目 + 未完成 + 阻塞）
6. **按 Eisenhower 生成日 / 周计划**（不是手动输入 — 是 AI 推断 + 接受/拒绝）

### 1.2 v0.1 缺什么

| 用户需求 | v0.1 实际 |
|---|---|
| 时间窗口查询（24h / 7d / 自定义） | 只有 `list_recent_events(limit=N)` 全量排序 |
| 工作单元聚合（agent × project × tool） | `raw_events` 只有扁平 (id, source, kind, occurred_at, payload) |
| Session 摘要 | 完全没 — payload 是原文 |
| 重要性评分 + 依据 | 启发式匹配 "asap/today" → Q1，**无依据** |
| 噪声 vs 产出分类 | 没有评分维度 |
| AI 优先级建议 | 完全没有 |
| 美观 UI | Tailwind 朴素卡片 |

## 2. 核心数据流（v0.2）

```
                 ┌─────────────┐
                 │  raw_events │  ← v0.1 已建（保留，只读源）
                 └────────┬─────┘
                          │ ETL: 归一化为原子活动
                          ▼
              ┌──────────────────────┐
              │   work_activities     │  ← 新表
              │   (single atomic act) │
              └─────────┬────────────┘
                        │ 聚类: project / agent / day
                        ▼
              ┌──────────────────────┐
              │      sessions         │  ← 新表（从 raw_events 按 source 聚类得到）
              └─────────┬────────────┘
                        │ LLM 摘要
                        ▼
              ┌──────────────────────┐
              │  session_summaries    │  ← 新表（LLM 输出 "完成什么 / 产出文件 / 重要性理由"）
              └─────────┬────────────┘
                        │ 时间窗口聚合（24h / 7d / 自定义）
                        ▼
              ┌──────────────────────┐
              │   window_reports      │  ← 新表（dashboard 渲染 + 下阶段建议）
              └─────────┬────────────┘
                        │ 用户接受 / 拒绝
                        ▼
              ┌──────────────────────┐
              │   plan_items v2       │  ← 升级 planner：从 window_reports 自动填充 + 手动调整
              └──────────────────────┘
```

## 3. 三张新表（schema v4）

### 3.1 `work_activities`

原子工作单元。从 raw_events ETL 得到：

```sql
CREATE TABLE work_activities (
    id              TEXT PRIMARY KEY,        -- act_<uuid>
    source_id       TEXT NOT NULL,           -- sources.id
    session_id      TEXT,                    -- 归属 session（nullable：noise session 无 session）
    project_key     TEXT,                    -- 从 payload 抽出（git remote / cwd basename）
    tool_kind       TEXT,                    -- 'bash' | 'edit' | 'read' | 'search' | 'chat' | 'browser' | 'agent_run' | 'unknown'
    description     TEXT,                    -- 一句话描述（从 payload 抽出）
    started_at      TEXT NOT NULL,           -- RFC3339
    duration_ms     INTEGER,                 -- 从 started_at / ended_at 算
    has_artifact    INTEGER NOT NULL DEFAULT 0,  -- 0/1，是否产生了 file / commit / PR
    artifact_paths  TEXT,                    -- JSON array（git diff 抽出的 changed files）
    raw_event_ids   TEXT NOT NULL,           -- JSON array of source raw_events.id
    created_at      TEXT NOT NULL
);
CREATE INDEX idx_wa_source_started ON work_activities(source_id, started_at);
CREATE INDEX idx_wa_project_started ON work_activities(project_key, started_at);
```

### 3.2 `sessions`

按 source_id + 时间聚类得到的 session 单元：

```sql
CREATE TABLE sessions_summary (        -- 注意不与 v0.1 sessions 表冲突，命名 sessions_summary
    id              TEXT PRIMARY KEY,    -- sess_<uuid>
    source_id       TEXT NOT NULL,       -- sources.id（codex / claude_code / doubao ...）
    external_id     TEXT,                -- 原 agent 的 session id（jsonl first record id 等）
    project_key     TEXT,
    started_at      TEXT NOT NULL,
    ended_at        TEXT,
    activity_count  INTEGER NOT NULL,
    tool_breakdown  TEXT NOT NULL,       -- JSON { bash: 12, edit: 5, ... }
    artifact_count  INTEGER NOT NULL,
    status          TEXT NOT NULL,       -- 'completed' | 'aborted' | 'noise'
    created_at      TEXT NOT NULL
);
```

### 3.3 `session_summaries`

LLM 生成的 session 摘要：

```sql
CREATE TABLE session_summaries (
    id              TEXT PRIMARY KEY,    -- sum_<uuid>
    session_id      TEXT NOT NULL UNIQUE REFERENCES sessions_summary(id) ON DELETE CASCADE,
    one_liner       TEXT NOT NULL,       -- "修了 Codex 项目的 stripe webhook 重试逻辑"
    what_happened   TEXT NOT NULL,       -- 多句话描述
    artifacts       TEXT,                -- JSON array of artifact paths
    importance      INTEGER NOT NULL,    -- 0..10
    importance_reason TEXT,               -- "因为涉及线上支付 webhook，可能影响订单丢失"
    is_noise        INTEGER NOT NULL,    -- 0/1，启发式 + LLM 联合判断
    noise_reason    TEXT,
    model           TEXT NOT NULL,       -- 用哪个模型生成的
    generated_at    TEXT NOT NULL
);
```

### 3.4 `window_reports`

时间窗口报告：

```sql
CREATE TABLE window_reports (
    id              TEXT PRIMARY KEY,    -- rep_<uuid>
    range_start     TEXT NOT NULL,
    range_end       TEXT NOT NULL,
    label           TEXT NOT NULL,       -- 'today' | 'yesterday' | 'last_7d' | 'custom'
    headline        TEXT NOT NULL,       -- 报告一句话总结
    completed       TEXT NOT NULL,       -- JSON array of {one_liner, source, project}
    noise_summary   TEXT NOT NULL,
    next_priorities TEXT NOT NULL,       -- JSON array of {text, rationale, suggested_quadrant}
    stats           TEXT NOT NULL,       -- JSON { total_activities, by_agent: {...}, by_project: {...} }
    generated_at    TEXT NOT NULL,
    expires_at      TEXT                 -- 缓存 24h
);
```

## 4. AI pipeline（v0.2 新增）

四个 LLM 调用，每个独立 IPC 暴露，方便降级：

### 4.1 `summarize_session(session_id)`

输入：session 下的所有 raw_events payload + 已抽取的 activities。
输出：填 session_summaries。

Prompt 模板：

```
You are summarizing one agentic session. Given the raw events, produce:
1. one_liner: ≤ 30 words describing what happened
2. what_happened: 2-4 sentences
3. artifacts: array of file paths produced/modified (empty if none)
4. importance: 0-10 integer
5. importance_reason: ≤ 30 words citing specific evidence from events
6. is_noise: boolean (true if no real artifact AND user message was short)
7. noise_reason: ≤ 20 words

Be specific, no filler. Return ONLY the JSON object.
```

### 4.2 `score_importance(session_id)`

独立调用，让前端 / 用户重打分而不重新摘要：

输入：existing summary + recent context (last 5 sessions' summaries)。
输出：新的 importance + reason。

### 4.3 `generate_window_report(range_label, range_start, range_end)`

输入：时间窗口下所有 session_summaries。
输出：填 window_reports。

Prompt：

```
You are producing a working-period report. Given session summaries in
[range_start, range_end], produce a JSON object:

{
  "headline": "≤20 words",
  "completed": [ { "one_liner": "...", "source": "codex", "project": "..." } ],
  "noise_summary": "≤40 words",
  "next_priorities": [ { "text": "...", "rationale": "...",
                          "suggested_quadrant": 1|2|3|4 } ]
}

Priorities rationale MUST cite specific recent evidence (session ids
or artifacts), not generic advice.
```

### 4.4 `prioritize_pending_items(template_id)`

输入：plan_items v2 中 status != done + 最近的 window_report。
输出：排序 + 重新 quadrant 建议（用户 accept / reject 单条）。

### 4.5 降级策略

- daemon unreachable → 用启发式 fallback（基于 tool_kind + artifact_count + session duration）
- OOM / context too long → 取最近的 N 个 session，丢弃旧的

## 5. UI 重设计

参考 Linear / Notion / Vercel 风格。引入 shadcn/ui 同级组件库。

### 5.1 导航

```
[🏠 Dashboard] [📊 Sessions] [💡 Insights] [🗓️ Planner] [⚙️ Settings]
```

### 5.2 Dashboard（主）

左 2/3：

- **今日时间轴**：横向时间线，每段按 source 着色，hover 显示该时间段做了什么
- **今日完成了什么**：从 window_reports.today.completed 列出，每条带来源 badge

右 1/3：

- **Agent 活动占比**（环形图，按 source 计数）
- **项目分布**（按 project_key）
- **下一步建议**（window_reports.today.next_priorities top 3，每条点击 accept → 加到 plan）

### 5.3 Sessions

按日期分组，每个 session 一张卡片：

```
┌─────────────────────────────────────────────────┐
│ [Codex] stripe-webhook · 14:23-15:48 (1h25m) │
│ LLM 一句话摘要                                  │
│ 改了 3 个文件（artifact list）                 │
│ Importance: 8/10 — 因为线上 webhook            │
│                                                  │
│ [+ Add to plan]  [Expand events]  [Rerun LLM] │
└─────────────────────────────────────────────────┘
```

filter：agent / project / importance / 时间段。

### 5.4 Insights

3 张大卡片：today / yesterday / last_7d。每张是 LLM 报告：

- Headline（大）
- ✅ Completed（带 source badge）
- ⚠️ Noise summary
- 💡 Next priorities（可一键 push 到 plan）

可手动触发 "Regenerate"。

### 5.5 Planner v2

现有 4-quadrant grid 保留，但加上：

- 顶部 strip 显示"从 Insights 推送" 的 pending items（卡片带 "Accept" / "Reject" 按钮）
- 每条 item 显示 `source_session_id` link → 跳到 Sessions 页

### 5.6 Settings

- Ollama endpoint / model
- redact_prompts 开关（默认开）
- LLM 调用开关（"Disable all AI" → 全部走启发式 fallback，0 LLM 调用）

## 6. ETL pipeline（不依赖 LLM，纯规则）

`work_activities` + `sessions_summary` 可以纯规则算，不调 LLM：

### 6.1 Connector 升级

每个 connector 在 scan 时同时输出 activities。但 v0.2 保留 v0.1 raw_events 双写（向后兼容），新增 ETL step 把 raw_events → activities。

ETL 规则：

- Codex / Claude Code / OpenCode：已有 JSONL，可以解析每条 message 提取 `tool_use.name` / `tool_use.input.file_path` / `tool_use.duration`
- WorkBuddy：meta.json 直接含 task title + status
- Doubao / MiniMax Agent：当前 IDB 数据只能给 raw_event（解码 V8 IndexedDB 是 v0.3 work）；活动粒度 = 每条 record 一条 activity，tool_kind = 'chat'
- project_key：从 cwd / git remote 抽，chat 类 = 'misc'

### 6.2 Session 聚类

按 (source_id, project_key) 分组，gap > 30 分钟切新 session。

## 7. 实施顺序

### Phase A：schema + ETL（纯 Rust，无 LLM）

1. `db.rs` 加 migration v3 → v4（三张新表）
2. `etl.rs` 模块：raw_events → work_activities → sessions_summary 聚类
3. `auto_rescan_all` 在每次 scan 后立即触发 ETL（增量）
4. IPC：`list_work_activities(filter)` / `list_sessions_summary(filter)` / `recompute_etl()`

**Phase A 验收**：dashboard 能看到按 source / project / tool 分布的 stats（不用 LLM 也知道用了什么 agent 多久）

### Phase B：LLM session 摘要（Ollama）

5. `summarize.rs`：`summarize_session(session_id)` IPC
6. `sessions.rs` UI：每个 session 卡片有"摘要 + 重要性"显示
7. 后台 batch：所有 status='completed' 但还没摘要的 session 自动摘要

**Phase B 验收**：Sessions 页面每个 session 有 AI 一句话摘要 + 重要性评分 + 理由

### Phase C：时间窗口报告

8. `window.rs`：`generate_window_report(range_label)` IPC
9. Insights 页面渲染 report
10. 后台 batch：today/yesterday/last_7d 各缓存一份

**Phase C 验收**：Insights 页有 today/yesterday/weekly 三张报告卡

### Phase D：Planner v2 接 insights

11. plan_items.source_session_id FK + schema v4 迁移
12. Insights → Planner 一键 push（每次 push 创建 plan_item 并标记 `from_insight=true`）
13. Planner 顶部 "Pending from insights" strip

**Phase D 验收**：从 Insights 的 next_priorities 点 accept → 出现在 planner 对应 quadrant

### Phase E：UI 全面重设计

14. 引入 shadcn/ui 同级组件（CSS only，避免引入 Tailwind 之外的依赖）—— 或在现有 Tailwind 上重写 design tokens
15. 4 个页面重写

## 8. v0.2 验收标准

- 用户打开 app 看到 Dashboard：**今天做了什么 + 用了什么 agent / 项目**（5 秒可读）
- Insights 页 today 报告：**实际产出 + 噪声占比 + 下一步建议**（每条带 reasoning）
- Sessions 页：每个 session 有摘要 + 重要性 + 改动的文件
- Planner：可以从 Insights 一键 push，4 quadrant 自动填充 + 用户可调
- LLM daemon 离线时：所有 AI 路径 graceful fallback 到启发式，**不崩**
- 性能：1000 activities / 50 sessions / 7 sources 下，dashboard render < 200ms

## 9. 不在 v0.2 范围内

- 多用户 / 多机 sync
- 云端 LLM provider
- V8 IndexedDB 二进制 decoder（doubao / MiniMax Agent 内容解析）→ 推到 v0.3
- 自定义 dashboard widget
- 历史回放（"过去 N 周发生了什么"）

---

## 待你拍板

1. **Phase A 先动**（纯规则，1-2 天），还是直接 Phase B 开始调 LLM？
2. **UI 库选型**：（a）继续 Tailwind + 自写 design tokens（不引入新 npm 依赖）；（b）引入 shadcn/ui（≈5 个新 npm 依赖）。
3. **LLM 调用预算**：默认每次 summarize_session 调一次 Ollama，每次 generate_window_report 调一次。如果当天有 30 个 session + 3 个窗口报告 = 33 次本地 LLM 调用（每次数秒到 30s）。是否 OK？要不要加 cache 策略（同一 session_id + 同一 model 24h 内不重复）？
4. **v0.1 兼容性**：v0.2 migration v3 → v4 是否保留所有 v0.1 raw_events（只新增表）？默认是的，但 sessions 表命名要避开 v0.1 `sessions` 表（已用于 phase 1-G 的 session_timeline）。

确认后我开始 Phase A。