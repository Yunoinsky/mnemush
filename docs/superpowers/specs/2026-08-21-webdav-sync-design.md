# 设计:WebDAV 跨设备同步(记忆更新时自动触发)

- **日期**: 2026-08-21
- **状态**: 已批准(brainstorming 确认)
- **关联**: v1.0 sync(Git 传输)、v1.5 DSH 插件、坚果云 WebDAV(Zotero 附件方案同凭证)
- **版本**: v1.6.0

## 背景与目标

跨设备记忆同步当前只有 Git 传输(`mnemush sync init/export/import`, 手动 push/pull, 需建私有仓库)。用户要求:
1. **优先 WebDAV 方案**(坚果云作为 WebDAV 的一个选项 —— 用户已有坚果云账号 + 应用密码, 与 Zotero 附件同步同信任级)
2. **记忆更新时自动触发同步**(不手动 push, 写入即同步)

## 架构:复用 sync 编解码, WebDAV 只做传输

```
任何记忆写入(add/update/soft_delete)
  → dirty 标记 + 30s 去抖
  → export 快照 → tar.gz → HTTP PUT → WebDAV(mnemush-sync.tar.gz)
另一设备: 启动时 webdav-pull → GET → 解包 → import
```

## 组件

### 1. 传输层:`mnemush sync webdav-push` / `webdav-pull`

- **push**: `sync export` 快照到临时目录 → tar.gz 打包(tar 0.4 + flate2, 已有) → HTTP PUT 到 `<url>/mnemush-sync.tar.gz`(ureq, 已有)
- **pull**: HTTP GET → 解包 → `sync import`(冲突保留本地新版, 拒绝旧 schema —— 复用现有 import 语义)
- **凭证**(环境变量, 不落命令行):
  - `MNEMUSH_WEBDAV_URL`(默认 `https://dav.jianguoyun.com/dav/mnemush/` —— 坚果云)
  - `MNEMUSH_WEBDAV_USER`(坚果云邮箱)
  - `MNEMUSH_WEBDAV_PASS`(坚果云应用密码, 非账号密码)
- 任意标准 WebDAV 可配(URL 覆盖默认即换服务商)
- 超时: push/pull 各 120s;失败 → 明确报错 + 保留 dirty 标记

### 2. 自动触发(记忆更新时)

- `MemoryApi::add` / `update` / `soft_delete` 成功后 → `mark_sync_dirty()`:
  - 写 `~/.mnemush/sync-dirty` 标记文件(记录时间戳)
  - 去抖 30s: 30s 内的多次写入只触发一次 push
- 触发方式: spawn 异步(不阻塞写入路径); 30s 去抖窗口内重复写入刷新时间戳
- **配置开关**: `[sync] webdav_enabled`(默认 false, 配好凭证才开)+ `webdav_debounce_secs`(默认 30)
- **失败重试**: push 失败 → dirty 标记保留; 下次写入或手动 `webdav-push` 重试
- 禁用: `--no-sync` 或 `[sync] webdav_enabled = false`

### 3. 启动时 pull(可选)

- `mnemush` 启动(CLI 首次命令)或 pi 插件 session_start 时: 若有 dirty 本地修改且 WebDAV 快照更新 → pull?
  - pull 与 push 同为逐条合并(实时双向); pi 插件可在 session_start 后台 pull(可选, 后续)

## 数据流

```
macOS(写记忆) ──30s 去抖──► tar.gz ──PUT──► 坚果云
Windows(启动) ──GET──► import(冲突留本地新版)
```

## 错误处理

- push/pull 失败: 明确报错 + dirty 保留(可重试), 不阻塞记忆写入
- 凭证缺失: `[sync] webdav_enabled=true` 但无 env → push 报错提示配置
- 快照损坏(解包失败): import 拒绝 + 保留原库(不覆盖)
- 网络超时: 120s, 失败重试

## 测试面

- webdav-push 打包正确(tar.gz 含 memory/edges/embeddings/identity)
- webdav-pull 解包 + import 往返(与 Git sync 同快照格式)
- 去抖: 30s 内多次写入 → 1 次 push; 30s 后新写入 → 新 push
- dirty 标记: 写入→标记, push 成功→清除, 失败→保留
- 凭证: env 缺失报错; URL 覆盖(非坚果云)
- 禁用开关
- **合并**: 同 id 较新者赢(updated_at)、新 id 并集、删除传播(双向)、边去重、向量跟随
- **乐观锁**: ETag 不符 → 重合并重 PUT(模拟双写竞态)

## 实时合并 + 删除传播(v1.6 核心)

WebDAV 是**交换媒介**, 同步靠逐条合并收敛(非整包覆盖):

### push 流程
```
1. GET 远程快照(拿当前状态 + ETag)
2. 与本地逐条合并:
   - 记忆: 同 id 比 updated_at → 较新者赢(含 deleted_at 删除传播)
   - 新 id → 并集插入
   - 软删(deleted_at 较新)→ 远端同步删除; 远端较新且已删 → 本地软删
   - 边: 按 id 并集 + UNIQUE(source,target,type) 去重
   - 向量: 随记忆 id 跟随(记忆较新 → 用其向量)
3. 合并结果 PUT(带 If-Match/ETag 乐观锁)
4. 若 ETag 不符(远程已变)→ 重取重合并再 PUT(竞态收敛)
```

### pull 流程
```
GET 远程快照 → 与本地逐条合并(同上, 方向反向) → 写入本地
```

### 收敛性质
- 单写设备: 立即一致(写入即 push)
- 双写同时: 乐观锁 → 后合并者重试 → 收敛(最终一致)
- 删除: 传播(两设备都会看到删除)

### 局限(诚实记录)
- 时钟偏差: updated_at 依赖设备时钟, 偏差大时短暂互相覆盖(个人双设备可接受)
- WebDAV 无 git 历史, 无法回滚(快照覆盖)

## 范围外(后续)

- 启动时自动 pull(暂手动/显式)
- 加密快照(坚果云私有账号已够)
- C 方案(定时 cron 同步)
