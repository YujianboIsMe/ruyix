/**
 * 后端消息的英文翻译（v1.0，bug 1/2 的收尾）。
 *
 * ## 为什么翻译放在前端，而不是改后端
 *
 * 后端（Rust）的错误文案本来就是中文，而且**带插值**（路径、键名、底层错误原因）。
 * 把它改成"错误码 + 参数"是一大改：168 处调用点、一批既有测试的断言文案、宿主与引擎两套口径 ——
 * 收尾阶段做这个，收益是"更好看"，风险是"动了整条错误链路"。而**显示层只有一个收口**
 * （`setStatus`），所以翻译落在这里。
 *
 * ## 怎么翻（三层，先具体后一般）
 *
 * 1. **精确对照**（EXACT）：一次性句子，整句对整句。
 * 2. **句型规则**（RULES）：`<动词短语>失败: <原因>`、`<东西>不存在: <路径>`、`无法<动作>`、
 *    `非法<东西>`、`…不能为空` 这类；动词、名词各有一张**词典**（VERBS / NOUNS），
 *    短语里的词**逐词替换**（`读取产物目录` → `read the artifact directory`），
 *    所以加一条新错误常常不用加规则。
 * 3. **认不出来就原样返回**：英文界面下会露出中文，但**信息不丢**（比翻错强）。
 *
 * ## 覆盖完整性不靠自觉：ui-smoke **U52**
 *
 * U52 扫后端源码里所有 `Err(...)` / `map_err` / `ok_or` / `bail!` 里的中文字面量，
 * 逐条要求 `translate(原文) !== 原文` —— 有新错误没被覆盖就**红**。
 * 没有这条门禁，这张表会在第一次"随手加个错误"时开始腐烂，而且**没有任何症状**
 * （只在英文界面下偶尔冒出一句中文）。
 *
 * ⚠ 改这个文件时注意转义：规则是**正则字面量**，`\s` 就是 `\s`（写成 `\\s` 会变成
 * "字面反斜杠 + s"，规则静默失配 —— 这个坑本轮真踩过，106 条"翻不出来"就是这么来的）。
 */
(function () {
  "use strict";

  // ============================================
  // 词典：动词（`X 失败` / `无法 X` / `读取 X` 里的动作）
  // ============================================
  const VERBS = {
    "建目录": "create directory",
    "改名": "rename",
    "写盘": "write to disk",
    "落盘": "flush to disk",
    "体积": "size",
    "不符": "mismatch",
    "客户端": "client",
    "清单": "manifest",
    "建": "create",
    "读向量": "read the vector",
    "查信念": "query beliefs",
    "改信念": "update the belief",
    "清向量": "clear vectors",
    "写信念": "write the belief",
    "开事务": "begin the transaction",
    "清派生层": "clear the derived layer",
    "读账本头": "read the ledger head",
    "写账本": "write to the ledger",
    "读账本": "read the ledger",
    "词法检索": "lexical search",
    "写收据": "write the receipt",
    "读收据": "read the receipt",
    "取向量": "read the vector",
    "加载": "load",
    "编码": "encode",
    "解码": "decode",
    "语言检测": "detect the language",
    "取模型": "fetch the model",
    "分词": "tokenize",
    "分词": "tokenize",
    "构造": "build",
    "前向": "forward pass",
    "池化": "pool",
    "编入": "compile in",
    "打开": "open",
    "创建目录": "create the directory",
    "创建父目录": "create the parent directory",
    "创建文件": "create the file",
    "创建备份目录": "create the backup directory",
    "创建知识库目录": "create the KB directory",
    "创建日志文件": "create the log file",
    "创建托管目录": "create the managed dir",
    "创建运行目录": "create the run directory",
    "创建会话目录": "create the session directory",
    "创建临时目录": "create the temp directory",
    "写入": "write",
    "写入配置": "write the config",
    "写文件": "write the file",
    "写文件行": "write file lines",
    "写入运行记录": "write the run record",
    "写入会话临时文件": "write the session temp file",
    "写入注册表": "write the registry",
    "写模板": "write the template",
    "读": "read",
    "读取": "read",
    "读取文件": "read the file",
    "读取文件信息": "read file info",
    "读取产物": "read the artifact",
    "读取产物目录": "read the artifact directory",
    "读取会话": "read the session",
    "读取知识库注册表": "read the KB registry",
    "读取运行记录": "read the run record",
    "删除目录": "delete the directory",
    "删除文件": "delete the file",
    "删除配置": "delete the config",
    "删除会话": "delete the session",
    "序列化": "serialize",
    "序列化会话": "serialize the session",
    "序列化注册表": "serialize the registry",
    "序列化运行记录": "serialize the run record",
    "解析": "parse",
    "解析响应": "parse the response",
    "解析任务": "parse the task",
    "解析运行记录": "parse the run record",
    "解析知识库注册表": "parse the KB registry",
    "启动": "start",
    "启动进程": "start the process",
    "执行": "execute",
    "保存": "save",
    "重命名": "rename",
    "打开索引库": "open the index DB",
    "打开日志": "open the log",
    "调整终端大小": "resize the terminal",
    "网络请求": "network request",
    "配置锁": "the config lock",
    "停止任务": "stop the task",
    "提交索引事务": "commit the index transaction",
    "开启事务": "begin the transaction",
    "检索语句准备": "prepare the search statement",
    "规范化根目录": "canonicalize the root",
    "规范化目录": "canonicalize the directory",
    "克隆日志句柄": "clone the log handle",
    "定位日志": "locate the log",
    "建运行时": "create the runtime",
    "建知识库目录": "create the KB directory",
    "获取": "get",
    "请求": "request",
    "发现": "discover",
    "备份": "back up",
    "返回": "returned",
    "网络": "network",
    "检索": "search",
    "清": "clear",
    "写": "write",
    "创建": "create",
    "启动 git": "start git",
    "删除": "delete",
    "构建": "build",
    "预置既有文件": "seed existing files",
    "修复": "repair",
  };

  // ============================================
  // 词典：名词（`X 不存在` / `没有 X` / `X 不能为空` 里的东西）
  // ============================================
  const NOUNS = {
    "权重": "weights",
    "张量": "tensor",
    "文本": "text",
    "不对": "wrong",
    "步": "step",
    "音频": "the audio",
    "抑制表": "the suppression table",
    "项目桶": "the project bucket",
    "文字": "text",
    "MCP服务器": "MCP server",
    "A2Aagent": "A2A agent",
    "项目目录": "project directory",
    "项目桶": "project bucket",
    "项目": "project",
    "文件": "file",
    "路径": "path",
    "配置键": "config key",
    "配置": "config",
    "暂存目录": "stage directory",
    "暂存": "stage",
    "运行目录": "run directory",
    "运行记录": "run record",
    "日志": "log",
    "会话": "session",
    "知识库": "knowledge base",
    "注册表": "registry",
    "任务集": "task set",
    "任务": "task",
    "索引状态": "index state",
    "索引": "index",
    "这个来源": "this source",
    "来源": "source",
    "这次运行": "this run",
    "托管进程": "managed process",
    "进程": "process",
    "子进程": "child process",
    "句柄": "handle",
    "目录": "directory",
    "命令": "command",
    "语言": "language",
    "列表": "list",
    "格式": "format",
    "顶层": "top level",
    "表": "table",
    "沙箱": "sandbox",
    "响应": "response",
    "通道": "channel",
    "参数": "args",
    "运行目标": "run target",
    "终端目标": "terminal target",
    "编辑": "edit",
    "桶名": "bucket name",
    "全局": "global scope",
    "项目级": "project scope",
    "表单": "form",
    "专门功能": "a dedicated feature",
    "超时": "timeout",
    "通道": "channel",
    "产物目录": "artifact directory",
    "产物": "artifact",
    "备份目录": "backup directory",
    "模板": "template",
    "临时文件": "temp file",
    "子进程": "child process",
    "面板": "panel",
    "输出": "output",
    "夹具": "fixture",
    "客户端": "client",
    "行": "rows",
    "项": "item",
    "步骤": "step",
    "模型调用": "model call",
    "进程表": "process table",
    "绝对路径": "absolute path",
    "盘符路径": "drive-letter path",
    "向上越界路径": "upward out-of-bounds path",
    "非法字符": "illegal characters",
  };

  // ============================================
  // 精确对照：一次性句子（键是后端原样文案）
  // ============================================
  const EXACT = {
    // GPU：构建没带 cuda 特性 —— 这句会经 `device_note` 显示在状态栏，英文界面下必须能翻
    "构建未带 cuda 特性（要 --features cuda，且需要 CUDA Toolkit）":
      "this build has no cuda feature (rebuild with --features cuda and the CUDA Toolkit)",
    "音频是空的": "the audio is empty",
    "{language}（语言 token 表里没有）": "{language} (not in the language token table)",
    "不认识的语言标记": "unknown language tag",
    "所有 token 都被抑制了（抑制表配错？）": "every token was suppressed (bad suppression table?)",
    "录音太短（不到 0.2 秒）": "the recording is too short (under 0.2s)",
    "没收到音频数据": "no audio data received",
    "未配置模型目录（宿主未设、也没有 RUYIX_MEM_MODEL_DIR）": "no model directory configured (host did not set one, and RUYIX_MEM_MODEL_DIR is unset)",
    "{e}；记忆仍按词法模式工作": "{e}; memory stays in lexical mode",
    "{e}；录音仍会保存到项目桶，只是暂时转不成文字": "{e}; the recording is still saved into the project bucket, it just cannot be turned into text yet",
    "这个环境没有 WebAudio（解不了录音）": "no WebAudio here (cannot decode the recording)",
    "这个环境没有麦克风接口（navigator.mediaDevices 缺失）": "no microphone API here (navigator.mediaDevices missing)",
    "这个环境没有 MediaRecorder（录不了）": "no MediaRecorder here (cannot record)",
    "这段录音是空的（没采到数据）": "that recording is empty (no data captured)",
    "没听出内容（可能太短或太吵）—— 再录一次试试": "nothing recognised (too short or too noisy) — try recording again",
    "空命令": "empty command",
    "空路径": "empty path",
    "路径为空": "empty path",
    "未配置模型目录（宿主未设、也没有 RUYIX_MEM_MODEL_DIR）":
      "the model directory is not configured (the host did not set it, and RUYIX_MEM_MODEL_DIR is unset)",
    "体积不符：拿到 {done}，清单写 {}": "size mismatch: got {done}, the manifest says {}",
    "本构建未编入向量腿（cargo feature `embed` 关闭）—— 记忆会退化为纯词法检索": "this build has no vector leg (cargo feature `embed` is off) — memory falls back to lexical search",
    "向量腿未编入": "the vector leg is not compiled in",
    "as-of 查询失败: {e}": "as-of query failed: {e}",
    "记忆库未安装": "memory store is not installed",
    "记忆库未安装（启动时打开失败）": "memory store is not installed (it failed to open at startup)",
    "重建失败: {e}": "rebuild failed: {e}",
    "读不到主题 {}：{e}": "cannot read the theme {}: {e}",
    "插件的 highlights.scm 编译失败：{e}":
      "the plugin's highlights.scm failed to compile: {e}",
    "插件给了 query 覆盖，但 `{other}` 没有内置解析器":
      "the plugin supplies a query override but `{other}` has no built-in parser",
    "未执行": "not executed",
    "连不上 fs": "cannot reach the fs",
    "路径不是文件": "path is not a file",
    "路径不是目录": "path is not a directory",
    "路径不是目录，请输入项目文件夹路径": "path is not a directory — please pick a project folder",
    "路径不是合法 UTF-8": "path is not valid UTF-8",
    "git status 失败": "git status failed",
    "消息不能为空": "message must not be empty",
    "任务描述不能为空": "task description must not be empty",
    "项目名称不能为空": "project name must not be empty",
    "工具名不能为空": "tool name must not be empty",
    "技能名不能为空": "skill name must not be empty",
    "名称与命令不能为空": "name and command must not be empty",
    "系统时间异常": "system clock is off",
    "托管进程表已损坏": "the managed-process table is corrupted",
    "运行时配置在内存中": "runtime config lives in memory",
    "项目路径为空字符串": "project path is an empty string",
    "未打开项目，无法使用项目配置 (-p)": "no project open — project config (-p) is unavailable",
    "无法确定存储键": "cannot determine the storage key",
    "尚未配置 API Key": "API key is not configured yet",
    "写入线程异常": "the write thread panicked",
    "句柄线程异常": "the handle thread panicked",
    "启动线程异常": "the start thread panicked",
    "验证线程异常": "the verify thread panicked",
    "MCP服务器已停止": "MCP server stopped",
    "MCP服务器已退出": "MCP server exited",
    "MCP响应通道关闭": "MCP response channel closed",
    "无法获取子进程stdin": "cannot get the child process stdin",
    "无法获取子进程stdout": "cannot get the child process stdout",
    "A2A 响应既非 task 也非 message": "A2A response is neither a task nor a message",
    "替换前后完全一致": "before and after are identical",
    "files 是空的": "files is empty",
    "输出里没有files数组": "the output has no files array",
    "不是一个JSON对象": "not a JSON object",
    "read缺少args.path": "read is missing args.path",
    "暂存目录里没有可写回的文件": "the stage directory has no files to write back",
    "容器里要跑什么？命令为空": "what should run in the container? the command is empty",
    "规划结果里没有任何可执行步骤": "the plan has no executable steps",
    "这次运行还没有计划，请先生成计划": "this run has no plan yet — generate one first",
    "复核只允许read": "the reviewer only allows read",
    "规则表解析失败": "failed to parse the rule table",
    "熵管理未启用": "entropy management is disabled",
    "熵扫描无法启动": "the entropy scan cannot start",
    "[{}] 由专门功能管理，不能通过配置表单写入":
      "[{}] is managed by a dedicated feature and cannot be written from the config form",
    "这次运行没有产物目录": "this run has no artifact directory",
    "MCP 服务器未连接": "MCP server is not connected",
    "无法启动 git": "cannot start git",
    "不是合法 JSON": "not valid JSON",
    "没有找到这次运行": "no such run",
    "找不到这次运行": "no such run",
    "非法 bucket 名": "invalid bucket name",
    "判据正则写错了": "the criterion regex is wrong",
    "[{section}] 由专门功能管理，不能通过配置表单写入":
      "[{section}] is managed by a dedicated feature and cannot be written from the config form",
    "无法启动 git（请确认已安装 Git 并加入 PATH）: {}":
      "cannot start git (make sure Git is installed and on PATH): {}",
    "无法启动 git（请确认已安装 Git 并加入 PATH）":
      "cannot start git (make sure Git is installed and on PATH)",
    "这次运行没有产物目录: {}": "this run has no artifact directory: {}",
    "MCP 服务器未连接: {name}（先 mcp_start）":
      "MCP server is not connected: {name} (run mcp_start first)",
    "MCP 服务器未连接: {name}": "MCP server is not connected: {name}",
    "MCP 响应通道关闭": "MCP response channel closed",
    "MCP 请求超时": "MCP request timed out",
    "复核只允许 read，收到 {tool:?}": "the reviewer only allows read, got {tool:?}",
    "read 缺少 args.path": "read is missing args.path",
    "不是一个 JSON 对象": "not a JSON object",
    "输出里没有 files 数组": "the output has no files array",
    "修复建议不是合法 JSON": "the repair suggestion is not valid JSON",
    "之前的文件已写入，见备份目录": "earlier files were already written — see the backup directory",
    "拒绝绝对路径": "refused an absolute path",
    "拒绝盘符路径": "refused a drive-letter path",
    "拒绝向上越界路径": "refused an upward out-of-bounds path",
    "路径含非法字符": "path contains illegal characters",
    "路径片段以点结尾": "a path segment ends with a dot",
    "规范化后为空路径": "path is empty after normalization",
    "写入位置越出运行目录": "the write target escapes the run directory",
    "路径越界": "path out of bounds",
    "判据正则写错了": "the criterion regex is wrong",
    "这个来源还没有索引": "this source has no index yet",
    "### `{}`\\n\\n（读不到：{e}）\\n\\n": "### `{}`\\n\\n(cannot read: {e})\\n\\n",
    "越界访问被拒绝": "out-of-bounds access refused",
    "，无法回滚": ", cannot roll back",
    "handle={handle} 的进程已被停止，本次等待中止":
      "the process with handle={handle} was stopped — this wait was aborted",
  };

  // ============================================
  // 句型规则（顺序即优先级：先具体、后一般）
  //
  // 每条规则的产出都过两道关：拼词 `join()` 里任一段是 null 就整条作废；
  // 产出仍含中文也作废 —— **宁可退回原文，也不产出"半英半中"的怪句子**。
  // ============================================
  const RULES = [
    // ① `<动词短语>失败（<上下文>）: <原因>`
    [
      /^(.+?)(?:失败|出错|错误)（(.+?)）[:：]\s*(.+)$/,
      (m) => join(phraseOrNull(m[1]), ` failed (${m[2]}): `, m[3]),
    ],
    // ②⁻ `<A>第 <N> 步失败: <原因>`（"解码第 3 步失败: …"）—— 必须排在通用 ② 之前
    [
      /^(.+?)第\s*(.+?)\s*步(?:失败|出错|错误)[:：]\s*(.+)$/,
      (m) => join(phraseOrNull(m[1]), " failed at step ", m[2], ": ", m[3]),
    ],
    // ② `<动词短语>失败{}: <原因>` / `<动词短语>失败: <原因>`
    //    占位符可能夹在"失败"和冒号之间（`写入失败 {}: {e}`、`创建目录失败 {}：{e}`）——
    //    不认这个夹心，规则会**静默失配**（看着像规则写对了，实际一条都没匹配）。
    [
      /^(.+?)(?:失败|出错|错误)\s*(?:\{[^}]*\}(?:\s*[^:：\n{]{0,20}\s*\{[^}]*\})*)?\s*[:：]\s*(.+)$/,
      (m) => join(phraseOrNull(m[1]), " failed: ", m[2]),
    ],
    // ②′ `<动词短语>错误 ({}): <原因>` —— 括号夹在中间
    [
      /^(.+?)(?:失败|出错|错误)\s*\((.+?)\)\s*[:：]\s*(.+)$/,
      (m) => join(phraseOrNull(m[1]), " error (", m[2], "): ", m[3]),
    ],
    // ②″ `<东西>超时（<上下文>）: <值>`
    [
      /^(.+?)超时(?:（(.+?)）)?(?:[:：]\s*(.*))?$/,
      (m) => join(phraseOrNull(m[1]), " timed out", m[2] ? ` (${m[2]})` : "", m[3] ? ": " + m[3] : ""),
    ],
    // ②‴ `<东西>不存在或不是<东西>: <值>`
    [
      /^(.+?)不存在或不是(.+?)[:：]\s*(.+)$/,
      (m) => join(phraseOrNull(m[1]), " does not exist or is not a ", lower(phraseOrNull(m[2])), ": ", m[3]),
    ],
    // ②⁗ `<东西>关闭`
    [/^(.+?)关闭$/, (m) => join(phraseOrNull(m[1]), " closed")],
    // ②⁵ `<A>的<B>失败: <原因>`（"步骤 N 的模型调用失败"）
    [
      /^(.+?)的(.+?)(?:失败|出错|错误)[:：]\s*(.+)$/,
      (m) => join(phraseOrNull(m[2]), " of ", phraseOrNull(m[1]), " failed: ", m[3]),
    ],
    // ②⁶ `<A>里没有<B>` / `<A>还没有<B>（<补充>）`
    [/^(.+?)里没有(.+)$/, (m) => join(phraseOrNull(m[1]), " has no ", phraseOrNull(m[2]))],
    [
      /^(.+?)还没有(.+?)(?:（(.*)）)?$/,
      (m) => join(phraseOrNull(m[1]), " has no ", phraseOrNull(m[2]), " yet", m[3] ? ` (${m[3]})` : ""),
    ],
    // ②⁷ `<A>已从<B>移除`
    [/^(.+?)已从(.+?)移除$/, (m) => join(phraseOrNull(m[1]), " was removed from ", phraseOrNull(m[2]))],
    // ②⁸ `没有<X>这个<Y>`
    [/^没有\s*(.+?)\s*这个(.+)$/, (m) => join("no ", lower(phraseOrNull(m[2])), " named ", m[1])],
    // ②⁹ `第 N 条编辑：读不到 X —— Y`
    [
      /^第\s*(.+?)\s*条编辑[:：]\s*读不到\s*(.+?)\s*——\s*(.+)$/,
      (m) => join("edit #", m[1], ": cannot read ", m[2], " — ", m[3]),
    ],
    // ③⁰ `<A>不是 git 仓库[，无法回滚]`
    [/^(.+?)不是\s*git\s*仓库(.*)$/, (m) => join(m[1], " is not a git repository", tr(m[2]) ? tr(m[2]) : "")],
    // ③¹ `<A>不是合法 JSON：<原因>；片段：<片段>`
    [
      /^(.+?)不是合法\s*JSON[:：]\s*(.+)$/,
      (m) => join(phraseOrNull(m[1]), " is not valid JSON: ", m[2]),
    ],
    // ③ `<东西>不存在: <值>`
    [
      /^(.+?)不存在\s*(?:\{[^}]*\})?\s*(?:[:：]\s*(.*))?$/,
      (m) => join(phraseOrNull(m[1]), " not found", m[2] ? ": " + m[2] : ""),
    ],
    // ④ `无法<动作>: <原因>` / `无法<动作>`
    [/^无法(.+?)[:：]\s*(.+)$/, (m) => join("cannot ", lower(phraseOrNull(m[1])), ": ", m[2])],
    [/^无法(.+)$/, (m) => join("cannot ", lower(phraseOrNull(m[1])))],
    // ⑤ `拒绝<东西>: <值>`
    [/^拒绝(.+?)[:：]\s*(.+)$/, (m) => join("refused ", lower(phraseOrNull(m[1])), ": ", m[2])],
    // ⑥ `<东西>不能为空`
    [/^(.+?)不能为空$/, (m) => join("", lower(phraseOrNull(m[1])), " must not be empty")],
    // ⑦ `<东西>是空的` / `<A>没有<B>`
    [/^(.+?)是空的(?:[:：]\s*(.*))?$/, (m) => join(phraseOrNull(m[1]), " is empty", m[2] ? ": " + m[2] : "")],
    [
      /^(.+?)没有(.+?)(?:[:：]\s*(.*))?$/,
      (m) => join(phraseOrNull(m[1]), " has no ", phraseOrNull(m[2]), m[3] ? ": " + m[3] : ""),
    ],
    [/^没有(.+)$/, (m) => join("no ", lower(phraseOrNull(m[1])))],
    // ⑧ `未<动作>`
    [/^未(.+)$/, (m) => join("not ", lower(phraseOrNull(m[1])))],
    // ⑨ `<东西>已<状态>`
    [
      /^(.+?)已(停止|退出|损坏)$/,
      (m) =>
        join(
          phraseOrNull(m[1]),
          " ",
          m[2] === "停止" ? "stopped" : m[2] === "退出" ? "exited" : "is corrupted"
        ),
    ],
    // ⑩ `读不到 <东西>：<原因>`
    [/^读不到\s*(.+?)[:：]\s*(.+)$/, (m) => join("cannot read ", phraseOrNull(m[1]), ": ", m[2])],
    // ⑪ `第 N 条编辑：<原因>（<文件>）`
    [/^第\s*(.+?)\s*条编辑[:：]\s*(.+)$/, (m) => join("edit #", m[1], ": ", m[2])],
    // ⑫ `非法<东西>: <值>` / `未知<东西>: <值>`
    [/^非法(.+?)(?:[:：]\s*(.*))?$/, (m) => join("invalid ", lower(phraseOrNull(m[1])), m[2] ? ": " + m[2] : "")],
    [/^未知(.+?)[:：]\s*(.+)$/, (m) => join("unknown ", lower(phraseOrNull(m[1])), ": ", m[2])],
    // ⑬ `<东西>已存在`
    [
      /^(.+?)已存在(?:[:：]\s*(.*))?$/,
      (m) => join(phraseOrNull(m[1]), " already exists", m[2] ? ": " + m[2] : ""),
    ],
    // ⑭ `<A>不在<B>里/中: <值>`
    [
      /^(.+?)不在(.+?)[里中](?:[:：]\s*(.*))?$/,
      (m) => join(phraseOrNull(m[1]), " is not in the ", lower(phraseOrNull(m[2])), m[3] ? ": " + m[3] : ""),
    ],
    // ⑮ `<A>不能保存为<B>`
    [
      /^(.+?)不能保存为(.+?)(?:[:：]\s*(.*))?$/,
      (m) => join(phraseOrNull(m[1]), " cannot be saved as ", lower(phraseOrNull(m[2])), m[3] ? ": " + m[3] : ""),
    ],
    // ⑯ `<A>顶层不是表`
    [/^(.+?)顶层不是表$/, (m) => join(phraseOrNull(m[1]), " top level is not a table")],
    // ⑰ `<A>失败: <原因>（<补充>）` —— 尾巴的中文括号也要翻
    [
      /^(.+?)(?:失败|出错|错误)[:：]\s*(.+?)（(.+?)）$/,
      (m) => join(phraseOrNull(m[1]), " failed: ", m[2], " (", tr(m[3]), ")"),
    ],
    // ⑱ `<A>（<补充>）`
    [/^(.+?)（(.+?)）$/, (m) => join(phraseOrNull(m[1]), " (", phraseOrNull(m[2]), ")")],
  ];

  function dictValue(k) {
    return VERBS[k] || NOUNS[k] || null;
  }

  /**
   * 词典键 → 正则（**词间允许任意空白**）。
   *
   * 为什么要允许空白：后端有的文案带空格（`MCP 服务器不存在`、`A2A 请求失败`），
   * 有的不带（`MCP服务器已停止`）。逐键写两遍迟早漏一个 —— 允许空白就一份顶两用。
   * 长键优先（`创建目录` 必须先于 `目录`），否则会被拆成"创建"+"目录"两截。
   */
  let _phraseRules = null;
  function phraseRules() {
    if (_phraseRules) return _phraseRules;
    const keys = [...Object.keys(VERBS), ...Object.keys(NOUNS)].sort((a, b) => b.length - a.length);
    _phraseRules = keys.map((k) => {
      const body = k
        .split("")
        .map((c) => c.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
        .join("\\s*");
      // 名词两侧补空格：`读取产物目录` 逐词换完是 `read the artifactdirectory`，
      // 补空格再压一次连续空白就是 `read the artifact directory`。
      const val = NOUNS[k] ? ` ${NOUNS[k]} ` : dictValue(k);
      return [new RegExp(body, "g"), val];
    });
    return _phraseRules;
  }

  /**
   * **短语级**替换：把一个短语里出现的词典词逐词换成英文（长词优先）。
   *
   * 换完还剩中文的短语一律**不用**（见 [`phraseOrNull`]）。
   */
  function phrase(s) {
    let out = String(s == null ? "" : s);
    for (const [re, val] of phraseRules()) {
      out = out.replace(re, val);
    }
    return out.replace(/\s+/g, " ").trim();
  }

  /** 三层"能用就用"：EXACT → 规则 → 短语；都不行返回 null。小片段（括号补充）走它。 */
  function tr(s) {
    if (s == null) return null;
    const k = String(s).trim();
    if (!/[一-鿿]/.test(k)) return k;
    if (EXACT[k]) return EXACT[k];
    const byRule = byRules(k);
    if (byRule) return byRule;
    return phraseOrNull(k);
  }

  /** 短语翻译的"能用就用"包装：不干净就返回 null。 */
  function phraseOrNull(s) {
    const out = phrase(s);
    return /[\u4e00-\u9fff]/.test(out) ? null : out;
  }

  /** 拼词：任一段为 null 就整条作废。 */
  function join(...parts) {
    if (parts.some((p) => p === null || p === undefined)) return null;
    return parts.join("");
  }

  function lower(s) {
    return s ? s.charAt(0).toLowerCase() + s.slice(1) : s;
  }

  /**
   * 把后端文案翻成英文；**认不出来就原样返回**（信息不丢，只是仍显示中文）。
   *
   * 输入可能是 `"保存失败: permission denied"` 这种整句，也可能是前端自己拼的
   * `"工具列表刷新失败: " + 后端消息`。
   */
  function translate(raw) {
    if (typeof raw !== "string" || !/[\u4e00-\u9fff]/.test(raw)) return raw;
    const s = raw.trim();
    if (EXACT[s]) return EXACT[s];
    // EXACT 也可以只当**前缀**用：`MCP 服务器未连接` + `: {name}`（后端常在后面接值）
    for (const [k, v] of Object.entries(EXACT)) {
      if (s.length <= k.length || !s.startsWith(k)) continue;
      const rest = s.slice(k.length);
      if (/^[:：]/.test(rest)) return v + ": " + rest.slice(1).trim();
      // 后缀是值或括号补充（`判据正则写错了 {pattern}: {e}`、`熵管理未启用（…）`）：
      // 原样接上 —— 里面是值不是文案，不需要翻
      if (/^\s|^（/.test(rest)) return v + (rest.startsWith(" ") ? "" : " ") + rest.trim();
    }
    const hit = byRules(s);
    if (hit) return hit;
    // 从左往右试每个分隔点：第一个"后半截能翻干净"的地方就是分界
    // （取最后一个分隔点会错 —— 后半句自己就可能带冒号：`…失败: 创建目录失败: x`）
    for (let i = 0; i < s.length - 1; i += 1) {
      const two = s.slice(i, i + 2);
      if (two !== ": " && two !== "：") continue;
      const tailRaw = s.slice(i + 1).trim();
      if (!/[\u4e00-\u9fff]/.test(tailRaw)) continue;
      const tail = byRules(tailRaw) || (EXACT[tailRaw] ? EXACT[tailRaw] : null);
      if (tail) return s.slice(0, i + 1) + " " + tail;
    }
    return raw;
  }

  /** 走规则；产出含中文或作废则返回 null。 */
  function byRules(s) {
    for (const [re, fn] of RULES) {
      const m = re.exec(s);
      if (!m) continue;
      const out = fn(m);
      if (out && !/[\u4e00-\u9fff]/.test(out)) return out;
    }
    return null;
  }

  window.BackendMsg = { translate, VERBS, NOUNS, EXACT, RULES, phrase };
})();
