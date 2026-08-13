# RAG（检索增强生成）

## 现实痛点
当你接手一个新项目，还要改bug时，找代码往往花费30~60分钟。
以Java Web为例，你要启动一个前端，在浏览器里复现bug，然后看Java日志，甚至需要四处打断点定位问题代码在哪里。

## 解决方案
整个项目所有代码存入本地向量数据库。
技术选型：
- 向量数据库：qdrant-edge（嵌入式，进程内运行，不依赖外部服务）
- 语义嵌入编码：在线嵌入 API（OpenAI 兼容格式，默认 DeepSeek 嵌入 API，1024 维）

### 方案演进记录（2026-08-13）
第一版方案在本地用 ModelScope 下载 Qwen3-Embedding-0.6B 模型（约 600MB）做推理。
实测下载时间过长，故放弃本地模型，改为调用在线嵌入 API：
- 无需下载任何模型文件，即开即用
- 向量维度 1024（默认，可配置）

## 期望效果
用户搜索 特征评估
搜索结果：从高到低
1. src/main/java/com/ysh/quanti/model/eval/Shap.java
2. src/main/test/com/ysh/quanti/model/eval/ShapTest.java
……

---

## 架构总览

```
┌─────────────────────────────────────────────────────────┐
│                    darkhorse-code                       │
│                                                         │
│  ┌──────────┐    ┌──────────────┐    ┌──────────────┐  │
│  │ 菜单栏    │    │  命令栏      │    │  编辑区       │  │
│  │ 【智搜】  │    │ search <关键词>│    │  搜索结果列表 │  │
│  └────┬─────┘    └──────┬───────┘    └──────┬───────┘  │
│       │                 │                   │          │
│       ▼                 ▼                   ▲          │
│  ┌──────────────────────────────────────────────────┐  │
│  │              RAG 模块 (src-tauri/src/rag.rs)     │  │
│  │                                                  │  │
│  │  ┌───────────┐  ┌───────────┐  ┌─────────────┐  │  │
│  │  │ 索引管理器 │  │ 查询引擎  │  │ 嵌入客户端   │  │  │
│  │  │ RagManager│  │ RagManager│  │ EmbeddingC.. │  │  │
│  │  └─────┬─────┘  └─────┬─────┘  └──────┬──────┘  │  │
│  └────────┼──────────────┼───────────────┼─────────┘  │
│           │              │               │            │
│           ▼              ▼               ▼            │
│  ┌────────────┐  ┌────────────┐  ┌───────────────┐   │
│  │ qdrant-edge│  │ 向量相似度  │  │ 在线嵌入 API   │   │
│  │ (嵌入式)   │  │ HNSW检索   │  │ (HTTPS, 默认   │   │
│  │            │  │            │  │  DeepSeek)     │   │
│  └────────────┘  └────────────┘  └───────────────┘   │
│                                                         │
│  存储路径:                                               │
│  ~/.darkhorse/code/                                     │
│  ├── rag.toml   (全局配置：API地址/密钥/模型/维度)        │
│  └── qdrant/    (向量数据库)                             │
│                                                         │
│  项目索引:                                               │
│  ./.darkhorse/code/rag.toml    (索引状态：已索引文件列表)  │
└─────────────────────────────────────────────────────────┘
```

## 组件选型

### 嵌入编码：在线嵌入 API

| 维度 | 说明 |
|------|------|
| 协议 | OpenAI 兼容 embeddings 接口（`POST {api_url}`，请求体 `{"model", "input", "encoding_format": "float"}`） |
| 默认地址 | `https://api.deepseek.com/v1/embeddings` |
| 默认模型 | `deepseek-embedding` |
| 向量维度 | 默认 1024，可在启用弹窗中修改 |
| 认证 | `Authorization: Bearer <api_key>`；未单独配置时复用 `darkhorse.code.ai.api_key` |
| 批量调用 | 索引时每 10 个文件一次请求，减少网络往返 |

选型理由：
- 无需下载 600MB 模型文件，避免 ModelScope/HuggingFace 下载慢的问题
- 用户可自行更换任何 OpenAI 兼容的嵌入服务（替换 API 地址即可）
- 索引和查询延迟取决于网络，本地零模型内存开销

### 向量数据库：qdrant-edge（嵌入式）

| 维度 | 说明 |
|------|------|
| 运行方式 | 进程内嵌入式 shard，非独立子进程 |
| 存储方式 | mmap（`on_disk: true`），内存占用低 |
| 索引算法 | HNSW（层次可导航小世界图），毫秒级检索 |
| 相似度 | Cosine |
| 磁盘占用 | 约 (文件数 × 维度 × 4) 字节，1000 个文件 × 1024 维约 4MB |
| 启动方式 | 按需懒加载，`search off` 时关闭 |

维度变更处理：`~/.darkhorse/code/qdrant/vector-dim` 记录当前维度。
修改维度后自动清空向量库重建，索引 hash 记录一并重置，全量重新编码。

---

## 索引策略

### 索引范围

**包含**（可配置）：
`.java`, `.js`, `.ts`, `.jsx`, `.tsx`, `.py`, `.rs`, `.go`, `.c`, `.cpp`, `.h`, `.hpp`, `.cs`, `.rb`, `.php`, `.swift`, `.kt`, `.scala`, `.vue`, `.svelte`, `.html`, `.css`, `.scss`, `.less`, `.md`, `.rst`, `.txt`, `.toml`, `.yaml`, `.yml`, `.json`, `.xml`, `.sql`, `.sh`, `.bat`, `.ps1`, `.proto`, `.graphql`

**排除**：
- 目录：`node_modules/`, `.git/`, `target/`, `.venv/`, `venv/`, `__pycache__/`, `dist/`, `build/`, `.next/`, `out/`, `coverage/`, `.idea/`, `.vscode/`, `.darkhorse/`
- 文件：`*.lock`, `*.min.js`, `*.min.css`, `*.map`, `*.pyc`, `*.class`, `*.o`, `*.so`, `*.dll`, `*.exe`, `*.bin`, `*.png`, `*.jpg`, 等二进制/媒体文件

### 切分策略

单文件不超过 32K 字符时直接整文件编码（当前实现）。
tree-sitter 结构化切分（函数/类级别 chunk）在路线图中，尚未实现。

### 索引时机

| 时机 | 触发方式 | 说明 |
|------|---------|------|
| 首次启用 | 用户点击"接受"后 | 全量索引，状态栏显示进度 "索引中 47/312..." |
| 文件保存 | 即时 | 增量更新，仅重新编码已修改的文件 |
| 文件删除 | 即时 | 从 qdrant 中删除对应向量 |
| 新文件创建 | 即时 | 编码新文件并插入 |
| 重建索引 | 手动触发 | `search reindex` 命令 |
| 维度修改 | 手动触发 | 清空旧索引，全量重新编码 |

索引状态保存在 `./.darkhorse/code/rag.toml`：
```toml
enabled = true
model = ""
last_indexed = "1786526560"
files_count = 312

[files]
"src/main/Calc.java" = "a1b2c3d4"
"src/test/CalcTest.java" = "e5f6g7h8"
```

每文件记录 sha256 前 12 位 hash 用于增量检测：内容未变的文件跳过编码，直接复用旧向量。

---

## 查询流程

```
用户输入 "search 特征评估"
        │
        ▼
┌──────────────────┐
│ 1. 生成查询向量   │  在线嵌入 API 编码 → 1024维向量  (~200-500ms 网络)
└──────┬───────────┘
       │
       ▼
┌──────────────────┐
│ 2. qdrant 检索   │  HNSW 近似搜索 Top-10  (~5ms)
└──────┬───────────┘
       │
       ▼
┌──────────────────┐
│ 3. 结果返回       │  排序列表 → 前端渲染
└──────────────────┘
```

总耗时约 300-600ms（大头是网络往返）。

### 命令语法

```
search <关键词>            # 语义搜索，返回 Top-10
search status              # 查看索引状态
search reindex             # 重建索引
search off                 # 关闭智搜（停止 qdrant）
```

### 搜索结果展示

搜索结果在编辑区以列表形式展示（类似运行输出）。每个结果条目包含：

```
1. 🐍 ShapCalculator.java — 相关度 94%
   src/main/java/com/ysh/quanti/model/eval/ShapCalculator.java:42
   public Matrix computeShapValues(DataFrame df) { ... }

2. 🐍 ShapCalculatorTest.java — 相关度 87%
   src/main/test/com/ysh/quanti/model/eval/ShapCalculatorTest.java:15
   public void testComputeShapValues() { ... }
```

点击条目 → 打开文件并跳转到对应行。

---

## UI 集成

### 菜单栏

在菜单栏添加【智搜】菜单项，默认显示。点击行为：

```
状态：未启用
  → 弹出确认弹窗（见下文）
  → 用户接受 → 保存嵌入 API 配置 + 启动 qdrant + 全量索引

状态：已启用
  → 再次弹出确认弹窗（可修改 API 配置或重建索引）

状态：永久禁用
  → 【智搜】菜单项隐藏（不可恢复，除非手动修改配置文件）
```

### 确认弹窗

用户点击【智搜】时弹窗内容：

> **启用智能搜索**
>
> 使用该功能需要：
> 1. 将项目代码发送到在线嵌入 API 进行向量编码；
> 2. 启动本地向量数据库（qdrant-edge 嵌入式，内存占用低）；
> 3. 对项目中的源代码和文档建立语义索引（首次约 1–5 分钟，取决于网络速度）；
> 4. 代码会发送到您填写的嵌入 API 进行编码，请确认该服务可信。
>
> **嵌入模型 API 地址**：[https://api.deepseek.com/v1/embeddings]（示例已预填，可修改）
> **向量维度**：[1024]
>
> ┌──────────┐  ┌──────────┐  ┌──────────┐
> │   接受   │  │ 暂不接受  │  │ 永久禁用  │
> └──────────┘  └──────────┘  └──────────┘

三个按钮的行为：

| 按钮 | 行为 |
|------|------|
| 接受 | 校验 API 地址 → 保存配置（`rag_set_embedding_config`）→ 全量索引（`rag_reindex`）→ 状态栏提示就绪 |
| 暂不接受 | 关闭弹窗，下次点击【智搜】再次弹窗 |
| 永久禁用 | 设置 `permanently_disabled = true`（全局配置），隐藏【智搜】菜单项 |

API Key 缺省时自动复用 AI 配置 `darkhorse.code.ai.api_key`（runtime → project → global）。
若未配置 Key 且嵌入 API 返回认证错误，状态栏会提示先执行
`config add -g darkhorse.code.ai.api_key <你的密钥>`。

### 配置持久化

全局配置 `~/.darkhorse/code/rag.toml`（扁平键，不走四层配置系统）：
```toml
enabled = true
permanently_disabled = false
api_url = "https://api.deepseek.com/v1/embeddings"
api_key = "sk-xxxx"            # 可选，缺省复用 darkhorse.code.ai.api_key
model = "deepseek-embedding"   # 可选，嵌入模型名
dim = 1024                     # 可选，向量维度，默认 1024
```

项目配置 `./.darkhorse/code/rag.toml`：
```toml
enabled = true
last_indexed = "1786526560"
files_count = 312

[files]
"src/main/Calc.java" = "a1b2c3d4"
```

---

## 性能预算

| 指标 | 目标值 | 说明 |
|------|--------|------|
| 首次全量索引 | 1–5 分钟（300 文件） | 每 10 文件一次 API 请求，约 30 次请求 |
| 增量索引 | < 1 秒（单文件保存） | 单次 API 请求 |
| 单次查询 | 300–600ms | 嵌入 API 网络往返 + qdrant 检索 5ms |
| qdrant 内存 | 低（mmap） | 嵌入式 shard，无独立进程 |
| 嵌入内存 | 0 | 推理在云端完成 |
| 向量存储 | ~4MB / 1000 文件 | 1024 维 × 4 字节 × 1000 |

### 资源释放

- `search off` → 停止 qdrant shard
- 嵌入客户端仅为 reqwest HTTP 客户端，无资源占用
- 下次查询时自动重启 qdrant

---

## 安全与隐私

- **向量数据在本机**：qdrant 数据只存储在本地磁盘 `~/.darkhorse/code/qdrant/`
- **代码会离开本机**：文件内容和查询词会被发送到用户填写的嵌入 API 进行编码。
  请确认该服务可信。这是与本地模型方案（全离线）的本质区别。
- **不收集**：本 IDE 自身不收集任何数据
- **认证**：API Key 存储于本地配置文件 `~/.darkhorse/code/rag.toml`，仅随请求发送到嵌入 API

---

## 实现路线图

### 第一期（MVP，已完成）

- [x] 在线嵌入 API 客户端（OpenAI 兼容格式，批量编码）
- [x] qdrant-edge 嵌入式向量库（维度可配置，维度变更自动重建）
- [x] 全量索引（整文件编码，hash 增量检测，批量调用 API）
- [x] `search <关键词>` 命令
- [x] 智搜菜单 + 确认弹窗（API 地址 + 维度输入）+ 永久禁用
- [x] 索引状态存储（rag.toml）

### 第二期（优化）

- [x] 增量索引（文件保存/删除/重命名时自动更新）
- [ ] tree-sitter 结构化切分（函数/类级别的 chunk）
- [ ] 搜索结果跳转到文件 + 行号
- [x] 索引进度实时显示

### 第三期（增强）

- [ ] 嵌入服务切换 UI（不同 API 地址/模型的快速切换）
- [ ] 搜索结果预览（hover 显示代码片段）
- [ ] AI 命令自动注入相关搜索结果作为上下文

---

## 注意事项

本IDE追求轻量、急速。
嵌入编码依赖在线 API，首次启用无需下载模型文件。
因此在菜单栏上加一个菜单【智搜】，默认不启用该功能。
用户点击【智搜】后，弹窗说明资源消耗与隐私影响，并要求填写嵌入 API 地址（示例预填 DeepSeek 嵌入 API 地址，默认维度 1024），由用户决定是否启用。
点击"永久禁用"后，永久隐藏【智搜】菜单（写全局配置 `permanently_disabled = true`）。
