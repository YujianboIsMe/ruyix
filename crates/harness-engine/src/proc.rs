//! 托管进程 —— `execute` 的第三种生命周期：**永不退出的服务**。
//!
//! ## 为什么要有这个模块
//!
//! `execute` 原来的语义是"跑完为止"：起进程、等它结束、拿退出码。对编译 / 测试 / git 是对的，
//! 对 `mvn spring-boot:run`、`java -jar app.jar`、`npm run dev` 是错的 —— 而提示词里列的用途
//! 恰好全是"会结束的东西"，**模型根本没有表达"常驻"的词汇**，只能把无界对象塞进有界动词：
//! `start /min`、`start "" /b`、PowerShell `Start-Process`、往 `%TEMP%` 写 bat / ps1。
//! 每绕一层，出错面就扩大一层（引号、cwd、重定向路径全都会漂）。
//!
//! 实测（cloud-shop-admin，2026-09-20）：模型用这些花招反复重启 17 轮，最后结论是"起不来"，
//! 而 `admin-run.log` 里明明白白写着 `Started AdminApplication in 1.168 seconds`、
//! `Tomcat started on port 8083`；那个进程**28 分钟后还活着**（`netstat` → PID 38420）——
//! `cmd /C start …` 立刻退出、子进程被重新挂靠，`exec::kill_tree` 连杀都杀不到。
//!
//! ## 设计（三条，缺一不可）
//!
//! 1. **生命周期是 `execute` 的一个维度，不是第 5 个原语**。"常驻"没有引入新的*效果*
//!    （与"编译"一样都是起进程），差别只在**谁决定它何时结束**。原语族划的是效果，不是时长。
//! 2. **就绪判据是一条命令**（[`StartSpec::ready_cmd`]）。引擎不知道什么是端口、什么是 Spring，
//!    它只是"跑一条命令看退出码"—— 这本来就是 execute 的能力。于是"先定成功判据"
//!    从**提示词的纪律**变成**原语的参数**：模型不是没想过定判据，是**没地方放判据**。
//! 3. **三件事必须由引擎负责，不能让模型发明**：日志路径（消除路径漂移）、输出无阻塞可达
//!    （不留管道 → 没有"管道满、子进程阻塞在写"这回事）、杀进程树（消除孤儿）。
//!
//! ## 结果一定带证据
//!
//! 三个出口各给下一步需要的依据，而不是一个状态位：[`StartKind::Ready`] 附判据命中的那行、
//! [`StartKind::Exited`] 附退出码 + 日志尾 + 判据最后一次结果、[`StartKind::NotReady`]
//! 附判据未命中 + 日志尾。这是治"每次失败就换个新花样"的正解 —— 一个只回状态位的原语，
//! 让"我没证据"和"它坏了"无法区分，模型只能归因"方法不对"，于是换方法。

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::exec;

/// 就绪轮询间隔。服务启动是秒级的事，不需要毫秒级灵敏。
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// 没给就绪判据时的宽限期：起完就返回，但仍等一下"秒退"，
/// 好把 `program not found` 这类**秒死**报成 `Exited` 而不是"已启动"。
const NO_READY_GRACE: Duration = Duration::from_secs(1);
/// 判据命令自己的超时（它必须远小于就绪窗口，否则轮询被它拖住）。
const READY_CMD_TIMEOUT: Duration = Duration::from_secs(10);
/// 就绪窗口的边界
const MIN_READY_TIMEOUT_SECS: u64 = 1;
const MAX_READY_TIMEOUT_SECS: u64 = 600;
/// 同时托管进程数的硬顶（配置再大也压不过它）
const MAX_BACKGROUND_HARD: usize = 16;
/// 日志给模型看的行数 / 字符数
pub const LOG_TAIL_LINES: usize = 40;
const LOG_TAIL_CHARS: usize = 4_000;
/// 读日志时最多回看多少字节（服务跑久了日志会很大）
const LOG_TAIL_BYTES: usize = 64 * 1024;

/// 起一个托管进程要什么。
#[derive(Clone, Debug)]
pub struct StartSpec {
    pub cmd: String,
    /// 就绪判据：**一条命令**，退出码 0 即就绪。`None` = 起完即返，不等它。
    pub ready_cmd: Option<String>,
    /// 就绪窗口（秒）。`None` 用配置里的默认值。
    pub ready_timeout_secs: Option<u64>,
    /// run 结束是否留着。默认 false —— 引擎持有就引擎收，别攒孤儿。
    pub keep_alive: bool,
}

/// 一个托管进程的可展示状态。宿主面板与模型侧看的是**同一份**，不各说各话。
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProcInfo {
    pub handle: String,
    pub pid: u32,
    pub cmd: String,
    /// 日志文件绝对路径 —— 由引擎给定。
    pub log: String,
    pub ready_cmd: Option<String>,
    /// `running` / `ready` / `exited(码)` / `stopped`
    pub state: String,
    pub keep_alive: bool,
    pub elapsed_ms: u128,
    /// 启动的**墙钟时刻**（UNIX 毫秒）。`elapsed_ms` 是单调时长的另一种说法，
    /// 面板要显示"几点起的"，只有时刻能算出来 —— 拿 elapsed 去减是不行的
    /// （进程是这一刻起的，不是"页面打开前 N 毫秒"）。
    pub started_at_ms: u64,
}

/// 一次后台启动的结果。`kind` 是三个出口 + "没给判据"。
#[derive(Clone, Debug)]
pub struct StartOutcome {
    pub info: ProcInfo,
    pub kind: StartKind,
    pub waited_ms: u128,
}

#[derive(Clone, Debug)]
pub enum StartKind {
    /// 判据命中
    Ready { evidence: String },
    /// 判据没命中，进程自己退了（最常见的"启动失败"长相）
    Exited {
        code: Option<i32>,
        tail: String,
        last_probe: Option<String>,
    },
    /// 窗口内判据未命中，但进程还活着（可能在慢启动）
    NotReady { evidence: String, tail: String },
    /// 没给判据，起完就返回
    Started,
}

struct Managed {
    handle: String,
    /// 属于哪个项目。**收尾要划界**：一次 run 结束只收这个项目的服务，
    /// 不然并行/嵌套的场景（以及测试）会互相踩。
    proj: PathBuf,
    pid: u32,
    cmd: String,
    log: PathBuf,
    ready_cmd: Option<String>,
    keep_alive: bool,
    state: String,
    started: Instant,
    started_at_ms: u64,
    child: Child,
}

impl Managed {
    fn info(&self) -> ProcInfo {
        ProcInfo {
            handle: self.handle.clone(),
            pid: self.pid,
            cmd: self.cmd.clone(),
            log: self.log.to_string_lossy().to_string(),
            ready_cmd: self.ready_cmd.clone(),
            state: self.state.clone(),
            keep_alive: self.keep_alive,
            elapsed_ms: self.started.elapsed().as_millis(),
            started_at_ms: self.started_at_ms,
        }
    }
}

/// 进程表的**主键是 pid**。
///
/// 为什么不是 handle（p1/p2 那种自造编号）：宿主面板、模型、日志里能拿到的一手证据
/// 只有 pid —— `netstat` 给 pid、`tasklist` 给 pid、用户说"杀掉 38420"也是 pid。
/// 拿自造编号当主键，等于在这些证据与表之间插一层翻译，而翻译层正是"看得见却管不着"
/// 的来源（面板按 handle 查，用户按 pid 找，两边对不上）。
/// handle 降级为 `Managed` 里的一个字段，只负责模型侧那句"p1 已就绪"的短引用。
fn table() -> &'static Mutex<HashMap<u32, Managed>> {
    static T: OnceLock<Mutex<HashMap<u32, Managed>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> Result<MutexGuard<'static, HashMap<u32, Managed>>, String> {
    table().lock().map_err(|_| "托管进程表已损坏".to_string())
}

/// 按 handle 找：handle 不再是主键，只在模型侧那一句短引用里出现，线性扫描足够。
fn by_handle_mut<'a>(t: &'a mut HashMap<u32, Managed>, handle: &str) -> Option<&'a mut Managed> {
    t.values_mut().find(|m| m.handle == handle)
}

fn by_handle<'a>(t: &'a HashMap<u32, Managed>, handle: &str) -> Option<&'a Managed> {
    t.values().find(|m| m.handle == handle)
}

/// 当前时刻（UNIX 毫秒）。时钟回拨时退成 0 —— 面板少显示一个时间，
/// 好过让"已启动多久"算出个负数。
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn next_handle() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("p{}", N.fetch_add(1, Ordering::Relaxed) + 1)
}

/// 兜底：进程自己退了，也仍然留在表里 —— 模型要能查它的退出码和日志尾。
pub fn is_dead(state: &str) -> bool {
    state.starts_with("exited") || state == "stopped"
}

fn state_exited(code: Option<i32>) -> String {
    format!(
        "exited({})",
        code.map(|c| c.to_string()).unwrap_or_else(|| "-".into())
    )
}

fn log_dir(proj: &Path) -> PathBuf {
    proj.join(".ruyix").join("proc")
}

/// 轮询一个子进程。`Ok(None)` = 还在跑，`Ok(Some(码))` = 已退出，
/// `Err(())` = 句柄已不在表里（被 `stop` 了），调用方该停止轮询。
fn poll_child(handle: &str) -> Result<Option<Option<i32>>, ()> {
    let Ok(mut t) = lock() else { return Err(()) };
    let Some(m) = by_handle_mut(&mut t, handle) else {
        return Err(());
    };
    if is_dead(&m.state) {
        return Ok(Some(None));
    }
    match m.child.try_wait() {
        Ok(Some(st)) => {
            let code = st.code();
            m.state = state_exited(code);
            Ok(Some(code))
        }
        _ => Ok(None),
    }
}

fn refresh_all(t: &mut HashMap<u32, Managed>) {
    for m in t.values_mut() {
        if is_dead(&m.state) {
            continue;
        }
        if let Ok(Some(st)) = m.child.try_wait() {
            m.state = state_exited(st.code());
        }
    }
}

fn live_of(t: &HashMap<u32, Managed>) -> Vec<&Managed> {
    t.values().filter(|m| !is_dead(&m.state)).collect()
}

/// 某个项目里还活着的托管进程（容量与"其他托管进程"都按项目划界）
fn live_in(t: &HashMap<u32, Managed>, proj: &Path) -> Vec<ProcInfo> {
    t.values()
        .filter(|m| m.proj.as_path() == proj && !is_dead(&m.state))
        .map(Managed::info)
        .collect()
}

/// 给模型/面板看的一行清单：`p1(ready, pid 38420, 12.4s)  p2(running, pid 100, 0.3s)`
pub fn render_listing(items: &[ProcInfo]) -> String {
    if items.is_empty() {
        return "（无）".into();
    }
    items
        .iter()
        .map(|i| {
            format!(
                "{}({}, pid {}, {:.1}s)",
                i.handle,
                i.state,
                i.pid,
                i.elapsed_ms as f64 / 1000.0
            )
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// 托管的全部进程（含已退出待查的），状态已刷新。宿主面板用。
pub fn listing() -> Vec<ProcInfo> {
    listing_where(|_| true)
}

/// 只列某个项目的托管进程。模型侧的"其他托管进程"该看这个 ——
/// 它关心的是本项目里有没有旧进程还占着资源，不是别的项目。
pub fn listing_for(proj: &Path) -> Vec<ProcInfo> {
    listing_where(|m| m.proj.as_path() == proj)
}

fn listing_where(keep: impl Fn(&Managed) -> bool) -> Vec<ProcInfo> {
    let Ok(mut t) = lock() else {
        return Vec::new();
    };
    refresh_all(&mut t);
    let mut v: Vec<ProcInfo> = t.values().filter(|m| keep(m)).map(Managed::info).collect();
    v.sort_by_key(|i| i.handle.clone());
    v
}

/// 构造托管进程的 [`Command`]：**复用 [`exec::shell_command`]**。
///
/// 这里曾经有一份自己的 `cmd /C` 实现，实测挂了 —— 把整条命令行当普通参数交给 std，
/// Windows 下内部引号会被转义成 `\"`，cmd 认不出可执行文件（详见 [`exec::shell_command`]）。
/// 教训：shell 构造只该有**一份**实现，多一份就是多一个"只有带引号的命令才踩到"的坑。
fn shell_command(cwd: &Path, line: &str) -> Command {
    exec::shell_command(cwd, line)
}

struct ProbeResult {
    hit: bool,
    detail: String,
}

/// 跑一次就绪判据。**只看退出码** —— 判据是一条普通命令，引擎不认识它测的是什么，
/// 这正是"零 app 知识"的落点。
fn probe_ready(cwd: &Path, cmd: &str) -> ProbeResult {
    let out = exec::run_line(cwd, cmd, READY_CMD_TIMEOUT);
    let first = out
        .stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .or_else(|| out.stderr.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    let code = out
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".into());
    let detail = if first.is_empty() {
        format!("exit={code}")
    } else {
        format!("exit={code} {}", exec::clip(first, 200))
    };
    ProbeResult {
        hit: out.passed(),
        detail,
    }
}

fn clip_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let cut = exec::floor_char_boundary(s, s.len() - max);
    format!("…（前文略）\n{}", &s[cut..])
}

/// 读日志尾部。**按字节读、解码放最后一步** —— 按块解会破多字节边界，
/// 所以日志文件存的是原始字节，这里用 [`exec::decode_output`]（严格 UTF-8 → 活动代码页）
/// 一次性解完整段。
pub fn read_log_tail(path: &Path, lines: usize) -> String {
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > LOG_TAIL_BYTES as u64 {
        if f.seek(SeekFrom::Start(len - LOG_TAIL_BYTES as u64))
            .is_err()
        {
            return String::new();
        }
        // 丢掉被截断的半行，免得开头出现一个半个多字节字符
        let mut one = [0u8; 1];
        loop {
            match f.read(&mut one) {
                Ok(0) => break,
                Ok(_) if one[0] == b'\n' => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = exec::decode_output(&buf);
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    clip_tail(&all[start..].join("\n"), LOG_TAIL_CHARS)
}

fn info_of(handle: &str) -> Result<ProcInfo, String> {
    let t = lock()?;
    by_handle(&t, handle)
        .map(Managed::info)
        .ok_or_else(|| format!("handle={handle} 已从进程表移除"))
}

fn set_state(handle: &str, state: &str) {
    if let Ok(mut t) = lock()
        && let Some(m) = by_handle_mut(&mut t, handle)
    {
        m.state = state.to_string();
    }
}

/// 起一个托管进程，并按就绪判据等待。返回 [`StartOutcome`]。
///
/// `max` = 同时托管数上限（含 `keep_alive` 留下的），`default_ready_secs` = 没显式给
/// 就绪窗口时用哪个。两者都由调用方从配置传入 —— 本模块不读全局配置，测试才好写。
///
/// **起进程之前先探一次判据**：如果它现在就已经命中，说明有别的东西满足它（最常见的是
/// 上一轮的进程还占着端口），这次的"就绪"不可能是我们起的这个进程给的 —— 直接拒绝并附证据。
/// 这条是整轮里最值钱的守卫，理由见 [`crate::proc`] 模块文档第 3 条的反面教材。
pub fn start(
    proj: &Path,
    spec: &StartSpec,
    max: usize,
    default_ready_secs: u64,
) -> Result<StartOutcome, String> {
    let dir = log_dir(proj);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建托管目录失败（{}）：{e}", dir.display()))?;

    // ① 判据在启动之前就已命中 → 这次的就绪是假的，别起
    if let Some(rc) = &spec.ready_cmd {
        let p = probe_ready(proj, rc);
        if p.hit {
            let live: Vec<ProcInfo> = match lock() {
                Ok(mut t) => {
                    refresh_all(&mut t);
                    live_in(&t, proj)
                }
                Err(_) => Vec::new(),
            };
            return Err(format!(
                "❌ 这条命令没有执行：就绪判据在启动**之前**就已经命中 —— 说明已经有别的东西满足它，\
                 这次启动出来的\"就绪\"不可能是真的（最常见的原因：上一轮的进程还占着端口 / 文件还在）。\n\
                 判据：{rc}\n命中的那行：{}\n当前托管：{}\n\
                 先 stop 掉旧进程，或者把判据换成能区分本次启动的（例：换个端口，或先清掉旧产物）再起。",
                p.detail,
                render_listing(&live)
            ));
        }
    }

    // ② 容量：只数**本项目**还活着的；退掉的空位可以腾
    {
        let mut t = lock()?;
        refresh_all(&mut t);
        let max = max.clamp(1, MAX_BACKGROUND_HARD);
        while live_in(&t, proj).len() >= max {
            let dead = t
                .iter()
                .filter(|(_, m)| m.proj.as_path() == proj && is_dead(&m.state))
                .min_by_key(|(_, m)| m.started)
                .map(|(k, _)| *k);
            match dead {
                Some(k) => {
                    t.remove(&k);
                }
                None => break,
            }
        }
        let live = live_in(&t, proj);
        if live.len() >= max {
            return Err(format!(
                "❌ 同时托管的进程已达上限 {max}（在跑：{}）。\
                 先用 execute {{\"op\":\"stop\",\"handle\":\"…\"}} 收掉不用的，\
                 或调 ruyix.code.harness.proc.max。",
                render_listing(&live)
            ));
        }
    }

    let handle = next_handle();
    let log_path = dir.join(format!("{handle}.log"));
    let out_file = File::create(&log_path)
        .map_err(|e| format!("创建日志文件失败（{}）：{e}", log_path.display()))?;
    let err_file = out_file
        .try_clone()
        .map_err(|e| format!("克隆日志句柄失败：{e}"))?;

    let mut c = shell_command(proj, &spec.cmd);
    // 输出**直接落文件**：不留管道，就没有"管道满（Windows 64KB）→ 子进程阻塞在写"
    // 这回事（"maven 输出有延迟"有一半是这个）。路径由引擎钉死，模型写不出会漂的 `> 某处.log`。
    c.stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));
    let child = match c.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("启动失败：{e}")),
    };
    let pid = child.id();
    let started = Instant::now();
    {
        let mut t = lock()?;
        t.insert(
            pid,
            Managed {
                handle: handle.clone(),
                proj: proj.to_path_buf(),
                pid,
                cmd: spec.cmd.clone(),
                log: log_path.clone(),
                ready_cmd: spec.ready_cmd.clone(),
                keep_alive: spec.keep_alive,
                state: "running".into(),
                started,
                started_at_ms: now_ms(),
                child,
            },
        );
    }

    let ready_timeout = Duration::from_secs(
        spec.ready_timeout_secs
            .unwrap_or(default_ready_secs)
            .clamp(MIN_READY_TIMEOUT_SECS, MAX_READY_TIMEOUT_SECS),
    );

    let mut last: Option<String> = None;
    loop {
        match poll_child(&handle) {
            // 被别的路径 stop 了（并发场景），别在这儿耗到超时
            Err(()) => return Err(format!("handle={handle} 的进程已被停止，本次等待中止")),
            // 先看死没死：判据没命中就退，正是"启动失败"最常见的长相
            Ok(Some(code)) => {
                let tail = read_log_tail(&log_path, LOG_TAIL_LINES);
                return Ok(StartOutcome {
                    info: info_of(&handle)?,
                    kind: StartKind::Exited {
                        code,
                        tail,
                        last_probe: last,
                    },
                    waited_ms: started.elapsed().as_millis(),
                });
            }
            Ok(None) => {}
        }

        match &spec.ready_cmd {
            Some(rc) => {
                let p = probe_ready(proj, rc);
                let hit = p.hit;
                last = Some(p.detail.clone());
                if hit {
                    set_state(&handle, "ready");
                    return Ok(StartOutcome {
                        info: info_of(&handle)?,
                        kind: StartKind::Ready { evidence: p.detail },
                        waited_ms: started.elapsed().as_millis(),
                    });
                }
                if started.elapsed() >= ready_timeout {
                    let tail = read_log_tail(&log_path, LOG_TAIL_LINES);
                    return Ok(StartOutcome {
                        info: info_of(&handle)?,
                        kind: StartKind::NotReady {
                            evidence: p.detail,
                            tail,
                        },
                        waited_ms: started.elapsed().as_millis(),
                    });
                }
            }
            None => {
                if started.elapsed() >= NO_READY_GRACE {
                    return Ok(StartOutcome {
                        info: info_of(&handle)?,
                        kind: StartKind::Started,
                        waited_ms: started.elapsed().as_millis(),
                    });
                }
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 查一个托管进程。句柄不存在时，报错里带上**当前有哪些** —— 光说"没有"治不了瞎猜。
pub fn status(handle: &str) -> Result<ProcInfo, String> {
    let mut t = lock()?;
    let live: Vec<ProcInfo> = {
        refresh_all(&mut t);
        live_of(&t).iter().map(|m| m.info()).collect()
    };
    let Some(m) = by_handle(&t, handle) else {
        return Err(format!(
            "没有 handle={handle} 这个托管进程。当前：{}",
            render_listing(&live)
        ));
    };
    Ok(m.info())
}

/// 按 pid 查。宿主面板的一手证据是 pid（netstat / tasklist / 用户嘴里都是 pid），
/// 不该让面板先去猜一个 handle。
pub fn status_pid(pid: u32) -> Result<ProcInfo, String> {
    let mut t = lock()?;
    refresh_all(&mut t);
    t.get(&pid).map(Managed::info).ok_or_else(|| {
        format!(
            "没有 pid={pid} 这个托管进程。当前：{}",
            render_listing(&live_of(&t).iter().map(|m| m.info()).collect::<Vec<_>>())
        )
    })
}

/// 读某个托管进程的日志尾部（已按活动代码页解码）。
pub fn log_tail(handle: &str, lines: usize) -> Result<String, String> {
    let path = {
        let t = lock()?;
        by_handle(&t, handle)
            .map(|m| m.log.clone())
            .ok_or_else(|| format!("没有 handle={handle} 这个托管进程"))?
    };
    Ok(read_log_tail(&path, lines))
}

/// 停掉一个托管进程，**连子进程树一起**（`exec::kill_tree`：Windows 走 taskkill /T，
/// Unix 对进程组发 SIGKILL）。
///
/// 这是必需项不是优化项：只杀 `mvn` 不杀 `java`，就是又造一个占着端口的孤儿。
pub fn stop(handle: &str) -> Result<ProcInfo, String> {
    let found = {
        let t = lock()?;
        by_handle(&t, handle).map(|m| m.pid)
    };
    match found {
        Some(pid) => stop_pid(pid),
        None => {
            let live: Vec<ProcInfo> = match lock() {
                Ok(mut t) => {
                    refresh_all(&mut t);
                    live_of(&t).iter().map(|x| x.info()).collect()
                }
                Err(_) => Vec::new(),
            };
            Err(format!(
                "没有 handle={handle} 这个托管进程。当前：{}",
                render_listing(&live)
            ))
        }
    }
}

/// 按 pid 停掉一个托管进程 —— 宿主面板的"停止"按钮走这条：面板看得到 pid，
/// 却在表里按 handle 存，就等于让用户拿着 pid 去找一个他查不到的编号。
pub fn stop_pid(pid: u32) -> Result<ProcInfo, String> {
    let mut m = {
        let mut t = lock()?;
        refresh_all(&mut t);
        let live: Vec<ProcInfo> = live_of(&t).iter().map(|x| x.info()).collect();
        match t.remove(&pid) {
            Some(m) => m,
            None => {
                return Err(format!(
                    "没有 pid={pid} 这个托管进程。当前：{}",
                    render_listing(&live)
                ));
            }
        }
    };
    if !is_dead(&m.state) {
        exec::kill_tree(m.pid);
        let _ = m.child.wait();
        m.state = "stopped".into();
    }
    Ok(m.info())
}

/// 收尾的公共实现：只处理 `pick` 选中的那些。**引擎持有就引擎收** —— 不这么做，
/// `start` 那种"脱离进程树的孤儿"就会原样重演（实测：一个端口被占了 28 分钟，
/// 还让后面每一次重启都拿到假的失败信号）。
///
/// `honor_keep_alive = true` 时，显式声明了 `keep_alive` 的进程留着**且仍在表里**
/// （宿主面板可见、可停），其余照收。
fn shutdown_where(
    pick: impl Fn(&Managed) -> bool,
    honor_keep_alive: bool,
) -> (Vec<ProcInfo>, Vec<ProcInfo>) {
    let mut stopped = Vec::new();
    let mut kept = Vec::new();
    let victims: Vec<Managed> = {
        let Ok(mut t) = lock() else {
            return (stopped, kept);
        };
        refresh_all(&mut t);
        let pids: Vec<u32> = t.keys().copied().collect();
        let mut victims = Vec::new();
        for k in pids {
            let (target, keep) = match t.get(&k) {
                Some(m) => (pick(m), m.keep_alive && !is_dead(&m.state)),
                None => continue,
            };
            if !target {
                continue;
            }
            if keep && honor_keep_alive {
                if let Some(m) = t.remove(&k) {
                    kept.push(m.info());
                    t.insert(k, m);
                }
                continue;
            }
            if let Some(m) = t.remove(&k) {
                victims.push(m);
            }
        }
        victims
    };
    for mut m in victims {
        // 已经自己退掉的只是从表里清掉，不进"收掉了"的名单
        if is_dead(&m.state) {
            continue;
        }
        exec::kill_tree(m.pid);
        let _ = m.child.wait();
        m.state = "stopped".into();
        stopped.push(m.info());
    }
    (stopped, kept)
}

/// 一次 run 结束时的收尾：**只收这个项目的**。别的项目（并行 / 嵌套，以及测试）
/// 的服务不归这次 run 管 —— 越界收会踩到别人。
///
/// 返回 `(收掉的, 留下的)`。
pub fn shutdown_for(proj: &Path, honor_keep_alive: bool) -> (Vec<ProcInfo>, Vec<ProcInfo>) {
    shutdown_where(|m| m.proj.as_path() == proj, honor_keep_alive)
}

/// 宿主退出时的收尾：**全收**（跨项目）。少这一步，关掉 IDE 就又攒下一批占着端口的孤儿。
pub fn shutdown_all(honor_keep_alive: bool) -> (Vec<ProcInfo>, Vec<ProcInfo>) {
    shutdown_where(|_| true, honor_keep_alive)
}

/// 进程表的**测试期互斥**：表是进程级全局的，测试并行跑会互相踩 —— 一个测试的 shutdown
/// 会收掉另一个的进程，`clear_table` 会把别人的条目直接抹掉（连进程都不杀）。
/// 因此凡是动这份表的测试都要先拿这把锁，**包括别的模块里真起后台进程的测试**
/// （如 agent 的交付对账回归：它得等进程活过 run 结束，被别人 clear 掉就成了假失败）。
#[cfg(test)]
pub(crate) fn table_lock() -> std::sync::MutexGuard<'static, ()> {
    static G: OnceLock<Mutex<()>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// 清空进程表（单测隔离用）。**不杀进程** —— 要杀用 [`shutdown_all`]。
#[cfg(test)]
fn clear_table() {
    if let Ok(mut t) = lock() {
        t.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 进程表的测试期互斥统一用上层那个 [`table_lock`]（模块级那份），这里不再自带一把锁 ——
    // 两把锁各锁各的等于没锁：别的模块真起进程的测试会因为「持的是另一把」而互相踩。

    fn tmp_proj(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ruyix-proc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::create_dir_all(&d);
        d
    }

    const MARKER: &str = "proc-ready-marker.txt";
    const NO_MARKER: &str = "proc-no-such-marker-zzz.txt";

    /// 活一会儿的命令。**不能**用 `timeout`（它重定向了 stdin 会直接报错退掉）。
    fn sleeper() -> &'static str {
        #[cfg(target_os = "windows")]
        return "ping -n 60 127.0.0.1";
        #[cfg(not(target_os = "windows"))]
        return "sleep 60";
    }

    /// 起来后写一个标记文件，再挂着 —— 这样"就绪判据"不必依赖端口，测试也能机器无关。
    fn touch_marker_and_wait() -> String {
        #[cfg(target_os = "windows")]
        return format!("echo started > {MARKER} & {}", sleeper());
        #[cfg(not(target_os = "windows"))]
        return format!("touch {MARKER}; {}", sleeper());
    }

    /// 打印点东西然后以退出码 7 结束
    fn print_and_die() -> String {
        #[cfg(target_os = "windows")]
        return "echo boom-proc & exit /b 7".into();
        #[cfg(not(target_os = "windows"))]
        return "echo boom-proc; exit 7".into();
    }

    /// 标记文件在不在 —— 由"本次启动"写出来，所以启动前必定不命中
    fn predicate_marker() -> String {
        #[cfg(target_os = "windows")]
        return format!("type {MARKER}");
        #[cfg(not(target_os = "windows"))]
        return format!("cat {MARKER}");
    }

    fn predicate_never() -> String {
        #[cfg(target_os = "windows")]
        return format!("type {NO_MARKER}");
        #[cfg(not(target_os = "windows"))]
        return format!("cat {NO_MARKER}");
    }

    fn spec(cmd: String, ready: Option<String>, keep_alive: bool) -> StartSpec {
        StartSpec {
            cmd,
            ready_cmd: ready,
            ready_timeout_secs: Some(5),
            keep_alive,
        }
    }

    fn cleanup(proj: &Path) {
        let (_, _) = shutdown_for(proj, false);
        clear_table();
        let _ = std::fs::remove_dir_all(proj);
    }

    #[test]
    fn ready_predicate_hits_and_reports_the_matching_line() {
        let _g = table_lock();
        let proj = tmp_proj("ready");
        let out = start(
            &proj,
            &spec(touch_marker_and_wait(), Some(predicate_marker()), false),
            4,
            5,
        )
        .expect("应当起来");
        match &out.kind {
            StartKind::Ready { evidence } => {
                assert!(evidence.contains("exit=0"), "证据要带判据结果: {evidence}");
                assert!(
                    evidence.contains("started"),
                    "证据要带命中的那行: {evidence}"
                );
            }
            other => panic!("应当是 Ready，实际 {other:?}"),
        }
        assert_eq!(out.info.state, "ready");
        assert!(out.info.pid > 0);
        assert!(out.info.log.ends_with(".log"));
        // 退出码缺席也算"没死"，与 is_dead 的判据一致
        assert!(!is_dead(&out.info.state));
        let st = status(&out.info.handle).expect("查得到");
        assert_eq!(st.state, "ready");
        stop(&out.info.handle).expect("停得掉");
        cleanup(&proj);
    }

    /// 表的主键是 pid：面板（以及用户、netstat、tasklist）手里的一手证据只有 pid，
    /// 必须能直接查、直接停，不能先翻译成自造编号。
    #[test]
    fn the_table_is_keyed_by_pid_and_carries_a_wall_clock_start() {
        let _g = table_lock();
        let proj = tmp_proj("pidkey");
        let out = start(&proj, &spec(sleeper().into(), None, false), 4, 5).expect("应当起来");
        let pid = out.info.pid;

        let st = status_pid(pid).expect("按 pid 查得到");
        assert_eq!(st.pid, pid, "按 pid 查回来的就是这个 pid");
        assert!(
            st.started_at_ms > 1_700_000_000_000,
            "要带墙钟启动时刻（面板要显示几点起的）: {}",
            st.started_at_ms
        );

        let stopped = stop_pid(pid).expect("按 pid 停得掉");
        assert_eq!(stopped.state, "stopped");
        assert!(status_pid(pid).is_err(), "停掉之后按 pid 也查不到");
        cleanup(&proj);
    }

    #[test]
    fn without_a_predicate_it_returns_as_soon_as_started() {
        let _g = table_lock();
        let proj = tmp_proj("noready");
        let out = start(&proj, &spec(sleeper().into(), None, false), 4, 5).expect("应当起来");
        assert!(matches!(out.kind, StartKind::Started));
        assert_eq!(out.info.state, "running");
        stop(&out.info.handle).expect("停得掉");
        cleanup(&proj);
    }

    #[test]
    fn a_process_that_dies_reports_code_and_log_tail() {
        let _g = table_lock();
        let proj = tmp_proj("dies");
        let out = start(
            &proj,
            &spec(print_and_die(), Some(predicate_never()), false),
            4,
            5,
        )
        .expect("启动本身成功");
        match &out.kind {
            StartKind::Exited { code, tail, .. } => {
                assert_eq!(*code, Some(7), "退出码要如实带回来");
                assert!(tail.contains("boom-proc"), "日志尾要带回来: {tail:?}");
            }
            other => panic!("应当是 Exited，实际 {other:?}"),
        }
        assert!(is_dead(&out.info.state), "状态要落成 exited");
        // 已退出的进程仍然可查、可读日志 —— 不然模型拿不到失败原因
        assert!(status(&out.info.handle).is_ok());
        let tail = log_tail(&out.info.handle, LOG_TAIL_LINES).expect("读得到");
        assert!(tail.contains("boom-proc"));
        cleanup(&proj);
    }

    #[test]
    fn not_ready_when_the_predicate_never_hits_but_the_process_lives() {
        let _g = table_lock();
        let proj = tmp_proj("notready");
        let mut s = spec(sleeper().into(), Some(predicate_never()), false);
        s.ready_timeout_secs = Some(1);
        let out = start(&proj, &s, 4, 5).expect("应当起来");
        match &out.kind {
            StartKind::NotReady { evidence, .. } => {
                assert!(
                    evidence.contains("exit="),
                    "要带判据最后一次的结果: {evidence}"
                );
            }
            other => panic!("应当是 NotReady，实际 {other:?}"),
        }
        // 关键：判据没命中不等于进程死了，状态仍应是"活着"，否则模型会以为启动失败
        assert!(!is_dead(&out.info.state), "{}", out.info.state);
        stop(&out.info.handle).expect("停得掉");
        cleanup(&proj);
    }

    #[test]
    fn a_predicate_that_already_hits_is_refused_before_anything_starts() {
        let _g = table_lock();
        let proj = tmp_proj("prehit");
        let first = start(
            &proj,
            &spec(touch_marker_and_wait(), Some(predicate_marker()), false),
            4,
            5,
        )
        .expect("第一个应当起来");
        assert!(matches!(first.kind, StartKind::Ready { .. }));

        // 同一个判据再起一次：标记文件已经在了 → 拒绝，并把"当前托管"摆出来
        let err = start(
            &proj,
            &spec(sleeper().into(), Some(predicate_marker()), false),
            4,
            5,
        )
        .expect_err("判据已命中就不该再起");
        assert!(
            err.contains(&first.info.handle),
            "报错要点名在跑的句柄: {err}"
        );
        assert!(err.contains("当前托管"), "报错要带证据: {err}");

        stop(&first.info.handle).expect("停得掉");
        cleanup(&proj);
    }

    #[test]
    fn max_is_enforced_with_an_evidence_carrying_message() {
        let _g = table_lock();
        let proj = tmp_proj("max");
        let a = start(&proj, &spec(sleeper().into(), None, false), 1, 5).expect("第一个应当起来");
        let err =
            start(&proj, &spec(sleeper().into(), None, false), 1, 5).expect_err("到上限就该拒");
        assert!(err.contains(&a.info.handle), "报错要点名在跑的句柄: {err}");
        stop(&a.info.handle).expect("停得掉");
        cleanup(&proj);
    }

    #[test]
    fn stopped_handles_disappear_and_unknown_ones_name_the_live_set() {
        let _g = table_lock();
        let proj = tmp_proj("stophandle");
        let a = start(&proj, &spec(sleeper().into(), None, false), 4, 5).expect("应当起来");
        let stopped = stop(&a.info.handle).expect("停得掉");
        assert_eq!(stopped.state, "stopped");
        assert!(status(&a.info.handle).is_err(), "停掉的句柄不该再查得到");
        assert!(stop(&a.info.handle).is_err(), "再停一次也该报错");
        let err = status("p999999").expect_err("不存在的句柄要报错");
        assert!(err.contains("p999999") && err.contains("当前"), "{err}");
        cleanup(&proj);
    }

    #[test]
    fn shutdown_takes_everything_down() {
        let _g = table_lock();
        let proj = tmp_proj("shutdown");
        start(&proj, &spec(sleeper().into(), None, false), 4, 5).expect("起 1");
        start(&proj, &spec(sleeper().into(), None, true), 4, 5).expect("起 2");

        // honor_keep_alive = true：keep_alive 那个留着，且**仍在表里**（宿主面板可见可停）
        let (stopped, kept) = shutdown_for(&proj, true);
        assert_eq!(stopped.len(), 1, "应当收掉非 keep_alive 的那个");
        assert_eq!(kept.len(), 1, "keep_alive 的应当留下");
        assert_eq!(listing_for(&proj).len(), 1, "留下的仍在表里");

        // 宿主退出时传 false：全收
        let (stopped2, kept2) = shutdown_for(&proj, false);
        assert_eq!(stopped2.len(), 1);
        assert!(kept2.is_empty());
        assert!(listing_for(&proj).is_empty());

        // **划界**：一次 run 只收自己项目的。越界收会踩到别人 ——
        // 这正是本轮实现时真踩到的坑（agent 的 run 收尾把另一个项目的进程杀了）。
        let other = tmp_proj("shutdown-other");
        start(&other, &spec(sleeper().into(), None, false), 4, 5).expect("别的项目起一个");
        let (cross, _) = shutdown_for(&proj, false);
        assert!(cross.is_empty(), "本项目的 run 结束不该动别的项目");
        assert_eq!(listing_for(&other).len(), 1, "别的项目那个要还活着");
        let _ = shutdown_for(&other, false);
        let _ = std::fs::remove_dir_all(&other);
        cleanup(&proj);
    }

    #[test]
    fn log_tail_keeps_the_end_and_decodes_the_active_codepage() {
        let _g = table_lock();
        let proj = tmp_proj("tail");
        let p = proj.join("x.log");
        let mut bytes = Vec::new();
        for i in 1..=100 {
            bytes.extend_from_slice(format!("line-{i}\n").as_bytes());
        }
        std::fs::write(&p, &bytes).unwrap();
        let tail = read_log_tail(&p, 5);
        assert!(tail.contains("line-100"), "要保留**末尾**: {tail}");
        assert!(!tail.contains("line-1\n"), "不该把整份日志倒出来");

        // GBK 字节（"不是内部或外部命令"）必须解成可读中文。
        // 只在中文机器上断言 —— 英文机器活动代码页是 1252，这是预期内的差异。
        if cfg!(target_os = "windows") && exec::ansi_codepage() == 936 {
            let gbk: Vec<u8> = vec![
                0xB2, 0xBB, 0xCA, 0xC7, 0xC4, 0xDA, 0xB2, 0xBF, 0xBB, 0xF2, 0xCD, 0xE2, 0xB2, 0xBF,
                0xC3, 0xFC, 0xC1, 0xEE, 0x0A,
            ];
            let gp = proj.join("gbk.log");
            std::fs::write(&gp, &gbk).unwrap();
            assert!(
                read_log_tail(&gp, 10).contains("不是内部或外部命令"),
                "GBK 日志要解得开"
            );
        }
        assert_eq!(read_log_tail(&proj.join("nope.log"), 10), "");
        cleanup(&proj);
    }
}
