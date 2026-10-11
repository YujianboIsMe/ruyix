# ruyix 命令翻译 LoRA 微调

用 [unsloth](https://github.com/unslothai/unsloth) 在 **Qwen2.5-1.5B-Instruct** 上 LoRA 微调一个小模型，
替代云端 LLM 完成 ruyix 的「自然语言 → DSL 命令 + Lua 缓存」翻译任务
（即 `src-tauri/src/ai.rs::translate()`，系统提示词 `src-tauri/src/command.md`）。

## 目录结构

```
training/
├── README.md                  # 本文件
├── train_lora.py              # unsloth 训练脚本（含冒烟测试）
└── dataset/
    ├── build_dataset.py       # 数据集生成器（含 Lua 结构校验）
    └── dataset.jsonl          # 生成的数据集，96 条
```

数据集每行：`{"project_path": str|null, "input": 用户输入, "output": 期望回复}`。
训练时脚本自动注入 command.md 作为 system prompt，并按 `ai.rs` 的真实行为
给带 `project_path` 的样本加 `当前项目路径: <path>` 前缀 —— 训练分布与线上完全一致。

覆盖范围：command.md 全部命令族（open/close/new/rename/del/refresh/config/run/help/git）、
多命令输出、项目路径上下文、不支持的操作（规则 3）、闲聊（规则 4）。

## 环境要求

- **NVIDIA GPU（CUDA）**，显存 ≥ 8GB（4bit LoRA 下 1.5B 模型足够；Colab 免费 T4 可跑）
- Python 3.10+，PyTorch 2.x

> ⚠️ unsloth 不支持在 macOS 上训练（依赖 CUDA/Triton）。Mac 上可以运行
> `build_dataset.py` 生成/校验数据集和 `python3 -m py_compile train_lora.py` 检查语法，
> 实际训练请在 GPU 机器或 Google Colab 上进行。

## 训练步骤

```bash
# 1. 安装 unsloth（CUDA 环境）
pip install unsloth

# 2.（可选）重新生成/扩充数据集
python3 training/dataset/build_dataset.py

# 3. 训练（约 96 条 × 3 轮，T4 约 10 分钟）
python training/train_lora.py
```

超参：LoRA r=16 / α=32，7 组投影层全挂，lr 2e-4，cosine，3 epochs，
只对 assistant 段计损（`train_on_responses_only`，ChatML 标记）。

产物（在 `training/outputs/`）：

| 目录 | 内容 | 用途 |
|---|---|---|
| `ruyix-command-lora/` | LoRA 适配器 | vLLM `--enable-lora` / PEFT 加载 |
| `ruyix-command-qwen2.5-1.5b/` | 合并后完整模型 | 直接部署 |
| `ruyix-command-gguf/`（需取消脚本注释） | q4_k_m 量化 GGUF | Ollama / llama.cpp |

训练结束自动跑 3 条冒烟用例，校验输出含 `---COMMAND---`/`---LUA---` 或符合闲聊规则。

## 部署回 ruyix

ruyix 走 OpenAI 兼容的 `/chat/completions`（见 `ai.rs::load_ai_config`），任选其一：

**vLLM（推荐，部署合并模型）**

```bash
vllm serve training/outputs/ruyix-command-qwen2.5-1.5b --port 8000
```

**Ollama（GGUF）**

```bash
ollama create ruyix-command -f Modelfile   # Modelfile 里 FROM 指向 GGUF 产物
ollama serve
# Ollama 的 OpenAI 兼容端点: http://127.0.0.1:11434/v1
```

然后在 ruyix 命令栏配置（密钥随便填非空即可）：

```
config add -g ruyix.code.ai.api_url http://127.0.0.1:8000/v1
config add -g ruyix.code.ai.api_key local
config add -g ruyix.code.ai.model ruyix-command-qwen2.5-1.5b
```

## 后续扩充

- 线上 `learn.lua`（`<项目>/.ruyix/code/learn.lua`）是真实用户输入的积累，
  Lua 命中的输入/命令对可直接挖成新样本 —— 建议写个脚本定期回灌数据集再增量微调。
- `ai.rs::check_executable()`（运行目标推断）是第二个任务，目前未纳入本数据集；
  如需覆盖，按其各自的 system/user 格式追加样本即可（SFT 支持多任务混训）。
