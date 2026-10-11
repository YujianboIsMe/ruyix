//! Capsule 侧存（v1.2 P3）：一次 run 里那些"全量结果"落在哪儿。
//!
//! **为什么要有它**（需求 §2 G4 / 拍板 1）：上下文里被折掉、或被去重命中要"还回去"的内容，
//! 必须**无损可得** —— 靠重跑工具把同样的话再问一遍模型是错的（贵、还可能问出不同的话）。
//! 侧存就是那条"事实永远还在"的路：结果原文落盘，账本只留引用。
//!
//! **放在哪**：`<便携根>/projects/<项目键>/ctx/<run-id>/` —— **绝不写用户仓库**
//! （需求 §3 第 3 条）。`state_root` 由 `config::project_state_root` 给出，与 findings 落盘同一处。
//!
//! **判据**（排期 P3）：召回内容与原文 **sha256 相等**（"无损"的唯一证明）；
//! "为召回而重跑工具"的次数 → 0（召回走磁盘读，不重新执行）。
//!
//! 形状：`<step>-<tool>-<hash8>.txt` 全量结果 + `index.jsonl`（key / digest / path / step / version）。

use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 索引文件名（JSONL，一行一条）
pub const INDEX_FILE: &str = "index.jsonl";

/// 侧存引用：账本里只存这个，**不存正文**（P3 落成之后 §5 那条内存上限自然退场）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ref {
    /// 相对 capsule 根的文件名（`<step>-<tool>-<hash8>.txt`）
    pub file: String,
    /// 正文 sha256（召回时校验；不符 ⇒ 拒绝交付，不静默给脏数据）
    pub sha256: String,
    /// 正文长度（字节）
    pub len: usize,
}

impl Ref {
    /// 侧存的**相对路径**（相对 state_root）—— 写进日志/索引，便于人去翻
    pub fn relative_hint(&self, run_dir: &str) -> String {
        format!("{run_dir}/{}", self.file)
    }
}

/// 一次内容检索的命中（带**来源**：第几轮、哪个工具、什么目标、第几行）。
#[derive(Clone, Debug)]
pub struct Hit {
    pub file: String,
    pub step: u32,
    pub tool: String,
    pub target: String,
    pub line_no: usize,
    pub text: String,
}

/// 索引一行的元数据（**索引要能回答"这条是什么、什么时候的"**）。
///
/// 打包成一个结构而不是散成六个参数：参数一多，调用点就会靠位置对齐 ——
/// 那种地方最容易把 `key` 与 `target` 写反，而且没人看得出来。
pub struct Entry<'a> {
    pub tool: &'a str,
    /// 账本的规范化调用串（"同一个调用"的身份证）
    pub key: &'a str,
    /// 键的哈希（与账本里那条对得上）
    pub digest: u64,
    /// 便于人读的目标（读的路径 / 跑的命令）
    pub target: &'a str,
    /// 执行前的版本快照（JSON）
    pub version: &'a str,
}

/// 一次 run 的侧存目录。
#[derive(Debug)]
pub struct Capsule {
    dir: PathBuf,
    index: PathBuf,
    puts: usize,
    /// 落盘正文的**总字节**（run 小结用）。侧存形态下内存里有界计数恒为 0，
    /// 只报内存会让用户看到「侧存 28 条/0.0KB」⇒ 以为存了个空 —— 真报障（2026-09-27）。
    bytes: usize,
    recalls: usize,
    /// 召回时 sha256 对不上的次数（**必须为 0**；非 0 说明侧存被改过 —— 要如实报出来）
    corrupted: usize,
}

impl Capsule {
    /// 建目录 + 备好索引。`state_root` = 便携根下的**项目桶**（不是用户仓库）。
    ///
    /// 目录名用 `workspace::new_run_id`（全仓库同一套命名），前缀 `ctx` 一眼能认出是谁的。
    pub fn create(state_root: &Path, slug: &str) -> std::io::Result<Self> {
        let run = crate::workspace::new_run_id(slug);
        let dir = state_root.join("ctx").join(run);
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            index: dir.join(INDEX_FILE),
            dir,
            puts: 0,
            bytes: 0,
            recalls: 0,
            corrupted: 0,
        })
    }

    /// 接一个**调用方定好的**目录（要建）。
    ///
    /// 与 [`Capsule::create`] 的区别只有一处：`create` 自己生成 run-id，这条不生成 ——
    /// 当同一 run 目录里还要放别的文件时（v1.5 的 `transcript.jsonl` 就是），
    /// 目录必须由调用方定一次、两边共用，否则一个 run 会散成两个目录。
    pub fn create_at(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            index: dir.join(INDEX_FILE),
            dir: dir.to_path_buf(),
            puts: 0,
            bytes: 0,
            recalls: 0,
            corrupted: 0,
        })
    }

    /// 只接一个现成目录（测试用；不建目录）
    #[cfg(test)]
    pub fn at(dir: PathBuf) -> Self {
        Self {
            index: dir.join(INDEX_FILE),
            dir,
            puts: 0,
            bytes: 0,
            recalls: 0,
            corrupted: 0,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `(落盘条数, 召回次数, 校验失败次数)`
    /// 落盘正文的总字节（run 小结用；这是**磁盘**占用，不是内存里的）
    pub fn disk_bytes(&self) -> usize {
        self.bytes
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (self.puts, self.recalls, self.corrupted)
    }

    /// 正文的 sha256（十六进制小写）
    pub fn sha256(body: &str) -> String {
        let mut h = Sha256::new();
        h.update(body.as_bytes());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 全量结果落盘（`.part` + rename 原子写，与 modelstore 同一套）+ 追加一条索引行。
    pub fn put(&mut self, step: u32, e: Entry<'_>, body: &str) -> std::io::Result<Ref> {
        let Entry {
            tool,
            key,
            digest,
            target,
            version,
        } = e;
        let sum = Self::sha256(body);
        let file = format!("{step:04}-{tool}-{}.txt", &sum[..8.min(sum.len())]);
        let path = self.dir.join(&file);
        let tmp = self.dir.join(format!("{file}.part"));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        let line = serde_json::json!({
            "file": file,
            "step": step,
            "tool": tool,
            "key": key,
            "digest": digest,
            "target": target,
            "version": version,
            "sha256": sum,
            "len": body.len(),
        });
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.index)?;
        writeln!(f, "{line}")?;
        self.puts += 1;
        self.bytes += body.len();
        Ok(Ref {
            file,
            sha256: sum,
            len: body.len(),
        })
    }

    /// **按内容检索**（v1.5 第 1 服）：在**已折掉/复用过的**正文里找。
    ///
    /// 为什么第 1 服要的是"内容"而不是"按 key"：折掉的东西**模型手里已经没有 key 了** ——
    /// 它是"我第 12 轮读过的那个文件里写过什么来着"，不是"我上次调用的规范串是什么"。
    ///
    /// 逐条命中都带**来源**（第几轮 / 哪个工具 / 什么目标）：记忆类结果没有来源标注就没法
    /// 与"磁盘上现在写的"摆在一起比较（需求 §4 的权威序）。
    pub fn search(&mut self, q: &str, limit: usize) -> Result<Vec<Hit>, String> {
        let needle = q.to_lowercase();
        if needle.trim().is_empty() {
            return Err("检索词是空的".into());
        }
        let raw = match std::fs::read_to_string(&self.index) {
            Ok(s) => s,
            // 侧存还没建过（这一轮还没折过东西）—— 那是**合法的空**，不是错误
            Err(_) => return Ok(Vec::new()),
        };
        let mut out: Vec<Hit> = Vec::new();
        for line in raw.lines() {
            if out.len() >= limit {
                break;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let file = v.get("file").and_then(|x| x.as_str()).unwrap_or("");
            let sha = v.get("sha256").and_then(|x| x.as_str()).unwrap_or("");
            if file.is_empty() || sha.is_empty() {
                continue;
            }
            // **sha256 校验照旧**：检索也不能绕过"绝不静默给脏数据"这条（P3 的硬标准）
            let body = self.get(&Ref {
                file: file.to_string(),
                sha256: sha.to_string(),
                len: v.get("len").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
            })?;
            let step = v.get("step").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let tool = v
                .get("tool")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let target = v
                .get("target")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            for (i, l) in body.lines().enumerate() {
                if out.len() >= limit {
                    break;
                }
                if l.to_lowercase().contains(&needle) {
                    out.push(Hit {
                        file: file.to_string(),
                        step,
                        tool: tool.clone(),
                        target: target.clone(),
                        line_no: i + 1,
                        text: l.trim().to_string(),
                    });
                }
            }
        }
        Ok(out)
    }

    /// 召回：读回来**逐字节**（sha256 校验；不符就报错，绝不静默交付）。
    pub fn get(&mut self, r: &Ref) -> Result<String, String> {
        let p = self.dir.join(&r.file);
        let body = std::fs::read_to_string(&p)
            .map_err(|e| format!("侧存读不出 {}（{}）：{e}", r.file, self.dir.display()))?;
        let sum = Self::sha256(&body);
        if sum != r.sha256 {
            self.corrupted += 1;
            return Err(format!(
                "侧存内容与记录不一致：sha256 {sum} ≠ 记录的 {}（{}）—— 拒绝交付这份内容",
                r.sha256, r.file
            ));
        }
        self.recalls += 1;
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ruyix_capsule_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 判据 1（P3 的硬标准）：召回内容与原文**逐字节一致**，sha256 也相等。
    #[test]
    fn recall_is_byte_identical_to_the_original() {
        let d = tmp("rt");
        let mut c = Capsule::at(d.clone());
        let body = "第一行\n第二行 with utf8 中文\n";
        let r = c
            .put(
                3,
                Entry {
                    tool: "read",
                    key: "read a.rs",
                    digest: 0xabc,
                    target: "a.rs",
                    version: "{}",
                },
                body,
            )
            .unwrap();
        assert_eq!(r.len, body.len());
        assert_eq!(r.sha256, Capsule::sha256(body));
        assert_eq!(Capsule::sha256(&c.get(&r).unwrap()), Capsule::sha256(body));
        assert_eq!(c.get(&r).unwrap(), body, "必须逐字节相同");
        assert_eq!(c.stats(), (1, 2, 0));
    }

    /// 索引是 JSONL，一行一条，字段够人去翻（key / step / tool / sha256 / len）
    #[test]
    fn index_gets_one_line_per_result() {
        let d = tmp("idx");
        let mut c = Capsule::at(d.clone());
        c.put(
            1,
            Entry {
                tool: "read",
                key: "read a.rs",
                digest: 1,
                target: "a.rs",
                version: "{}",
            },
            "AAA",
        )
        .unwrap();
        c.put(
            2,
            Entry {
                tool: "execute",
                key: "git grep x",
                digest: 2,
                target: ".",
                version: "{}",
            },
            "BBB",
        )
        .unwrap();
        let idx = std::fs::read_to_string(d.join(INDEX_FILE)).unwrap();
        let lines: Vec<&str> = idx.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["step"], 2);
        assert_eq!(v["tool"], "execute");
        assert_eq!(v["sha256"], Capsule::sha256("BBB"));
        assert_eq!(v["len"], 3);
    }

    /// 侧存被改过 ⇒ **拒绝交付**（不是静默给脏数据）：这条比"能读回来"更重要
    #[test]
    fn a_tampered_capsule_file_is_refused() {
        let d = tmp("tamper");
        let mut c = Capsule::at(d.clone());
        let r = c
            .put(
                1,
                Entry {
                    tool: "read",
                    key: "k",
                    digest: 1,
                    target: "a.rs",
                    version: "{}",
                },
                "原始内容",
            )
            .unwrap();
        std::fs::write(d.join(&r.file), "被改过的内容").unwrap();
        let e = c.get(&r).unwrap_err();
        assert!(e.contains("sha256"), "{e}");
        assert!(e.contains("拒绝交付"), "{e}");
        assert_eq!(c.stats().2, 1, "校验失败次数要如实计");
    }

    /// 原子写：不该留下 `.part` 残骸（写一半崩掉才可能有，正常路径必须干净）
    #[test]
    fn put_leaves_no_part_file_behind() {
        let d = tmp("part");
        let mut c = Capsule::at(d.clone());
        c.put(
            1,
            Entry {
                tool: "read",
                key: "k",
                digest: 1,
                target: "a.rs",
                version: "{}",
            },
            "x",
        )
        .unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "不该有 .part 残骸");
    }
}
