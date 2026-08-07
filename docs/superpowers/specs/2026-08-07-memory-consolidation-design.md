# 设计:v1.2 记忆巩固与主动遗忘(dream/consolidate)

- **日期**: 2026-08-07
- **状态**: 已批准(brainstorming 逐节确认)
- **关联**: Karpathy LLM Wiki 策略(gist 442a6bf);钟毅团队主动遗忘研究(Shuai et al., Cell 2010;双通路/多巴胺竞争/节律门控);Anthropic Dreams

## 背景与动机

记忆系统需要定期维护整理,参考三个机制:
- **Karpathy wiki 策略**: LLM 定期"编译"记忆库(增量整合、去重、矛盾标注、交叉引用,不覆盖历史)
- **主动遗忘**(钟毅团队等): 遗忘是**主动生化过程**,独立于记忆形成 — Rac1 通路主动抹除记忆痕迹、干扰诱导遗忘、多巴胺双受体(dDA1 获取 vs DAMB 遗忘)构成**获取与遗忘的竞争系统**、Raf/MAPK 保护短期记忆、节律门控消退、双通路分工(不稳定记忆 Rac1/WAVE vs 稳定记忆 Cdc42/Arp2/3)
- **睡眠期巩固/顿悟**: Anthropic Dreams / Letta sleep-time compute — 离线整合、冲突解决、模式涌现(insight)

mnemush 现有:被动 prune(置信度衰减)、edge-decay(Ebbinghaus)、reflect_candidates(输出候选)、auto-merge/supersede 边。**缺:LLM 驱动的主动整合与主动遗忘**。

## 架构:巩固与遗忘竞争平衡

```
┌─ 触发 ─────────────────────────────────────────────┐
│  mnemush consolidate  手动增量(自上次以来新记忆)      │
│  mnemush dream        每日全量(睡眠期巩固+遗忘高峰)  │
│  (cron 调度每日 dream)                              │
└──────────────────────┬─────────────────────────────┘
                       ▼
LLM(MiniMax M3 → DeepSeek V4 Flash fallback)输出结构化 actions:
├─ 巩固类: update / link / merge / insight(顿悟)
└─ 遗忘类: decay(降权) / forget(软删)  ← 独立评估, 带遗忘原因
                       ▼
执行器(自动, --dry-run/--suggest 预览)→ 审计(memory_event + JSON 存档)
```

## 神经科学映射

| 生物机制 | mnemush 映射 |
|---|---|
| Rac1 通路主动抹除(Cell 2010) | LLM 显式评估遗忘目标,定向执行(非被动等待衰减) |
| 干扰诱导遗忘(抑制 Rac1 阻断) | 相似记忆簇冲突 → LLM 判定"谁被干扰遗忘"(弱化旧/保留新) |
| 双通路: 不稳定(Rac1/WAVE) vs 稳定(Cdc42/Arp2/3) | **双阈值**: 低 confidence 记忆易遗忘(低证据即可);高 confidence 需更强信号(矛盾/明确过时)才遗忘 |
| 多巴胺双受体 dDA1 vs DAMB(竞争系统) | 每次 consolidate 都含遗忘评估阶段,与整合平行 |
| Raf/MAPK 保护(短期记忆防意外丢失) | importance ≥ 0.7 / never_prune / identity / 最近 7 天 → 禁止 forget/decay |
| 节律门控(睡眠期记忆消退) | dream 的遗忘强度 > 手动 consolidate(每日睡眠期高峰) |
| 突触削弱渐进性(GluA2 内化) | 遗忘先 `decay`(confidence 降权),多次后跌到阈值才 `forget`(软删),渐进不突删 |

**遗忘痕迹(forgetting trace)**: 遗忘本身是一种信息 —— 每次 `forget` 执行时,
在软删原记忆之外,创建 `forget_trace` 元记忆(被遗忘记忆的标题/内容摘要/
判定时间/原因),category=forget_trace, importance 0.3, 可被检索与 LLM
分析(如发现某类记忆反复被遗忘 → 新洞察),也可被未来 dream 再遗忘
(随问题变化,被遗忘的记忆可能重新值得记住 → trace 不设保护);
trace 被遗忘时不产生 trace-of-trace(防无限递归)。

## 命令接口

- `mnemush consolidate [--dry-run|--suggest] [--project <name>] [--since <ts>]` — 增量巩固+遗忘评估(自上次位置,默认全项目)
- `mnemush dream [--dry-run|--suggest]` — 每日全量(更强遗忘强度);建议 cron 每日一次
- 位置记录: `~/.mnemush/consolidate.json`(last_ts + 上次汇总),增量只取 `created_at > last_ts`
- 输出: 报告(`+N 整合, +M 边, +K insight, -F 遗忘[decay X / forget Y], 审计见 memory_event`)

## LLM 管道

1. **收集候选**: 新记忆(增量: 自 last_ts; dream: 分批全量)+ 每条的邻居/同 topic 记忆(上下文)
2. **组装 prompt**: 系统提示(角色=记忆巩固者,含遗忘评估指令 + 保护规则 + 输出 schema)+ 候选记忆(标题/分类/内容截断/tags/confidence)+ 当前时间
3. **LLM 调用**: MiniMax M3(`api.minimax.chat`),HTTP 超时 60s,失败自动 fallback DeepSeek V4 Flash(`DEEPSEEK_API_KEY`);两者都失败 → 报错退出(不部分执行)
4. **输出**: 严格 JSON `{ "actions": [...] }`(一次调用一批,<=20 条候选/token 可控);非法 JSON 重试 1 次后跳过该批并报错

## 动作 schema

```json
{"type":"update","id":"...","content":"修订后内容","reason":"..."}
{"type":"link","source":"...","target":"...","etype":"related|supports|contradicts","strength":0.6}
{"type":"merge","keep":"...","absorb":"..."}            // absorb 并入 keep, absorb 软删
{"type":"insight","title":"...","content":"...","links":["id1","id2"]}  // 顿悟: 新模式记忆+边
{"type":"decay","id":"...","factor":0.5,"reason":"干扰|过时|冗余"}      // 主动遗忘: confidence 降权
{"type":"forget","id":"...","reason":"过时|冗余|被取代|干扰"}           // 软删
```

## 执行器与审计

**执行器**(`--dry-run` 只打印;`--suggest` 输出 JSON 不执行;默认自动):
- `update`: `MemoryApi::update`(保留 id/created_at,重算 content_hash)
- `link`: `EdgeApi::link`(幂等,provenance=`consolidate:{reason}`)
- `merge`: 复用 auto-merge 逻辑(absorb 内容并入 keep,absorb 软删 + 边重定向)
- `insight`: `add(category=insight, source=Consolidate)` + 与 links 建边
- `decay`: confidence *= factor(最小 0.05 下限),写 memory_event(`consolidate_decay`, reason)
- `forget`: 软删(可恢复 30 天)+ memory_event(`consolidate_forget`, reason)
- **执行顺序**: link → update → merge → decay → forget(遗忘最后,基于新状态)
- **失败原子性**: 单条动作失败记录错误继续(不整体回滚);报告汇总错误数

**审计**: 所有动作写 `memory_event`(类型 `consolidate_*` / `dream_*`,携带 reason)+ 报告。LLM 原始响应存 `~/.mnemush/eval/consolidate-<ts>.json`(可追溯)。

## 保护规则(写入系统提示)

importance ≥ 0.7 / never_prune / identity / 最近 7 天创建 → 禁止 forget/decay;冲突时只能 update 或标 contradicts 边。

## 测试(TDD, 新增 `consolidate.rs`)

1. LLM 响应解析: 合法/非法 JSON(重试 1 次后跳过该批并报错)
2. decay: confidence 降到阈值以下仍保留;多次 decay 才到 0.05 下限
3. forget: 软删 + 事件记录 + 保护规则(importance≥0.7 拒绝)
4. merge: 吸收内容合并 + 旧记忆软删 + 边重定向
5. insight: 创建 insight 记忆 + 与 links 建边
6. 增量位置: consolidate 后 last_ts 更新,重跑不重复处理
7. fallback: MINIMAX 失败 → DeepSeek 调用(用 mock HTTP server 测)
8. 双阈值: 低 confidence 记忆被 decay 时高 confidence 豁免(需更强 reason)

**LLM 客户端 mock**: 测试用本地 TCP mock server 返回预设 JSON(不真调 API);真实调用留端到端验证。

## 非目标(本次不做)

- 自动 cron/session_end 触发(用户 cron 自行调度;扩展钩子留后续)
- 容量管理(腾空间阈值)—— 生物"腾空间"暂不做,已有 prune
- 多用户/多 agent 协调

## 参考

- Karpathy LLM Wiki: https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f
- Shuai Y et al., Forgetting Is Regulated through Rac Activity in Drosophila. Cell 140(4), 2010
- 钟毅课题组: 双通路(生命学院新闻)、DAMB/Gq 遗忘(Neuron 2012)、节律门控
- Anthropic Dreams / OpenAI ChatGPT memory dreaming
- mnemush 现有: neuropils(文件树记忆, v1.1)、edge-decay、reflect_candidates、auto-merge
