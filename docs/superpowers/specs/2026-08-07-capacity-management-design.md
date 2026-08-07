# 设计:v1.3 记忆容量管理 + neuropil 归档

- **日期**: 2026-08-07
- **状态**: 已批准(brainstorming 逐节确认)
- **关联**: v1.1 neuropils(文件树记忆)、v1.2 consolidate/dream(LLM 巩固 + 主动遗忘)、Karpathy LLM Wiki 策略(gist 442a6bf)
- **术语**: **neuropils** = 文件树内容层(长期知识, 权威源); **mushroom_body** = 主库(SQLite 记忆库: agent 经验 + wiki 临时索引 + 摘要入口 + 边 + 向量)

## 背景与目标

当前库 97MB / 5643 条, 其中 external-wiki 5477 条(97%, importance 中档)占 33.7MB 向量空间。需求: 三层容量治理 —— 物理大小(≤100MB 硬阈值)、记忆条数(LLM 遗忘自然平衡)、全量处理成本(dream 批次)。效果与框架效率优先, 不追求极致瘦身。

## 架构:两层记忆 + 双向流转

```
neuropils 文件树(长期知识, 权威源)             mushroom_body(主库)
┌───────────────────────────────┐            ┌──────────────────────────────┐
│ wiki 全文 / neuropil 化内容     │            │ ① agent 经验(常驻): 全文+向量+图 │
│ (grep/cat/tree 直接读, Git 版  │            │ ② wiki 临时索引(动态): 局部导入   │
│  本化)                         │            │ ③ 摘要入口(neuropil 化后): title+ │
└───────────────────────────────┘            │    摘要+path+边                  │
        ▲        动态加载(局部 import+embed)  └──────────────────────────────┘
        └────── neuropil 化(定期 export+摘要入口) ────────┘
```

- **内容层**: 全文权威源在文件树, 零库空间, 可随时读取/版本化。
- **索引层(mushroom_body)**: 内容索引 + 关联图。agent 经验常驻; wiki 按需动态加载; neuropil 化记忆留摘要入口。

## 机制一: wiki 动态索引

- 需要 wiki 知识时(检索/consolidate): 局部 `import-tree` 导入相关子集 → 自动 embed + 建边(复用现有机制)。
- 索引生命周期 = 使用周期: 容量压力或不再需要时清理(软删), 需要时重建。
- 70k 边中 wiki↔agent 关联随索引生命周期; 入口记录(摘要入口)为边锚点, 边不悬空。
- **零 schema 改动**, 复用 import-tree / edge / forget 全套。

## 机制二: neuropil 化(主库 → 文件树归档)

**触发**: 并入 `dream` 每日流程。

**对象判定**: 规则初筛 + LLM 复核。
- 规则初筛: category ∈ {note, skill} 且内容具定义/参考特征(概念、论文摘要、参考信息); agent 经验(lesson/decision/insight/correction)常驻不筛。
- LLM 复核: dream 中 LLM 对初筛候选输出 `neuropilize` 动作(记忆 id + 目标路径), 确认归档。

**动作**:
1. `export-tree` 导出到 neuropils 文件树(按 project/主题组织)。
2. 主库记忆降级为**摘要入口**: 保留 id/title/摘要(规则截取 content 前 2 句)/路径/边; 全文清空, FTS 只索引 title+摘要; 向量用 title+摘要重新 embed(每条 ~KB 级, 可忽略)。

**入口形态**: 摘要入口(语义可命中)。命中入口 → 返回摘要 + 路径, 需要全文时从文件树读取; 需要深度语义时局部全量 embed 重建。

## 机制三: neuropil 压缩(冷知识归档)

**触发**: 并入 `dream` 每日流程。

**冷判定(双条件, 最保守)**: 主库入口 `last_accessed_at` > 30 天(无检索命中) **且** 文件树 mtime 未修改 > 30 天。

**动作(合并 + 打包)**:
1. **合并归档**: 冷 neuropil 页面合并为归档页(多页 → 1 页带目录), 减少文件数。
2. **打包**: 合并后的冷目录打包为 `archive/<name>.tar.gz`, 移出活动区, 物理瘦身。
3. **入口更新**: 主库摘要入口 path 指向包内路径, 需要时解压读取。

## 机制四: 容量硬阈值(100MB)

**触发**: `add` 时检查(SQLite `page_count` 查询, 高效)。

**驱逐策略**(超限, 顺序执行):
1. 清 wiki 临时索引(可再生, 空间大头 ~33.7MB 向量)。
2. 仍超限 → 低分 agent 记忆软删。评分: `score = (importance × confidence × 1/(1+age_days)) / cost`, `cost = 向量字节 + 全文字节 + 边数×常数`; 低分先驱逐。
3. 驱逐后调 dream/consolidate(LLM 遗忘)语义复核。

**条数治理**: 不设条数上限, 由 LLM 遗忘(dream/consolidate 的 decay/forget)+ 遗忘痕迹自然平衡。

## 机制五: dream 每日流程(三合一)

```
dream(每日 cron):
  1. 全量扫描候选(批次 5)
  2. LLM: 遗忘动作(decay/forget) + neuropilize 复核(规则初筛候选)
  3. 执行: 遗忘(留 trace) → neuropil 化(export + 摘要入口) → 压缩(冷判定 + 合并 + 打包)
  4. 容量报告: 物理水位 / wiki 索引数 / 可回收空间
```

## 监控

- `mnemush status` 扩展容量段: 物理大小/上限、wiki 临时索引数、摘要入口数、可回收空间。
- consolidate/dream 结束时报告水位。

## 错误处理

- 驱逐/归档失败不阻塞主流程(逐条记录, 报告继续)。
- 摘要截取失败(空内容)→ 跳过该候选(不归档)。
- 打包/解压失败 → 保留原文件树, 入口 path 不变。

## 测试面

- 规则初筛判定(哪些 category/特征入选)。
- 摘要截取(前 2 句边界)。
- 入口降级与恢复(export 往返, path 正确)。
- 冷判定(双条件: 入口命中 vs 文件 mtime)。
- 合并归档(多页 → 1 页带目录)。
- 打包/解压(归档包可还原)。
- 驱逐顺序(先 wiki 索引, 后低分 agent)。
- 100MB 阈值触发(page_count 模拟)。
- dream 三合一集成(临时库端到端)。

## 范围外(后续)

- neuropil 化记忆的深度语义重建(局部全量 embed)的自动调度。
- 归档包的内容检索(需解压)。
- 多级冷热分层(热/温/冷)。
