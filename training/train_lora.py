#!/usr/bin/env python3
"""ruyix 命令翻译 LoRA 微调：Qwen2.5-1.5B-Instruct + unsloth。

任务：复刻 src-tauri/src/ai.rs::translate() 的行为 ——
system = command.md，user = [当前项目路径前缀]? + 自然语言，
assistant = ---COMMAND--- / ---LUA--- 双段输出。

环境要求：NVIDIA GPU（CUDA）。unsloth 不支持 macOS 训练，
本机只做脚本/数据校验，训练请在 GPU 机器或 Colab 上执行（见 training/README.md）。

用法：python training/train_lora.py
"""

import json
from pathlib import Path

import torch
from datasets import Dataset
from transformers import TrainingArguments
from trl import SFTTrainer
from unsloth import FastLanguageModel
from unsloth.chat_templates import get_chat_template, train_on_responses_only

# ============================================
# 配置
# ============================================

BASE_MODEL = "unsloth/Qwen2.5-1.5B-Instruct"
REPO_ROOT = Path(__file__).resolve().parents[1]
SYSTEM_PROMPT = (REPO_ROOT / "src-tauri" / "src" / "command.md").read_text(encoding="utf-8")

MAX_SEQ_LENGTH = 4096
LORA_RANK = 16
LORA_ALPHA = 32
LEARNING_RATE = 2e-4
NUM_EPOCHS = 3
EVAL_RATIO = 0.05  # 留 5% 用例做验证集
SEED = 3407

# ============================================
# 数据集：dataset.jsonl → ChatML messages
# ============================================


def load_split() -> tuple[Dataset, Dataset]:
    rows = [
        json.loads(line)
        for line in (REPO_ROOT / "training" / "dataset" / "dataset.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
        if line.strip()
    ]

    def to_messages(row: dict) -> dict:
        # 与 ai.rs::translate() 保持一致：打开项目时注入路径前缀（zh-CN）
        user = row["input"]
        if row.get("project_path"):
            user = f"当前项目路径: {row['project_path']}\n\n{user}"
        return {
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": user},
                {"role": "assistant", "content": row["output"]},
            ]
        }

    dataset = Dataset.from_list([to_messages(r) for r in rows])
    split = dataset.train_test_split(test_size=EVAL_RATIO, seed=SEED)
    return split["train"], split["test"]


def main() -> None:
    train_ds, eval_ds = load_split()
    print(f"训练集 {len(train_ds)} 条 / 验证集 {len(eval_ds)} 条")

    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=BASE_MODEL,
        max_seq_length=MAX_SEQ_LENGTH,
        dtype=None,  # 自动：A100/H100 用 bf16，T4 用 fp16
        load_in_4bit=True,  # 1.5B 在 8GB 显卡上也可训练；显充裕可改 False 提速
    )

    model = FastLanguageModel.get_peft_model(
        model,
        r=LORA_RANK,
        lora_alpha=LORA_ALPHA,
        lora_dropout=0,  # 无过拟合风险（数据量小、轮数少），0 更快
        bias="none",
        use_gradient_checkpointing="unsloth",
        random_state=SEED,
        target_modules=[
            "q_proj", "k_proj", "v_proj", "o_proj",
            "gate_proj", "up_proj", "down_proj",
        ],
    )

    tokenizer = get_chat_template(tokenizer, chat_template="qwen-2.5")

    def to_text(row: dict) -> dict:
        return {
            "text": tokenizer.apply_chat_template(
                row["messages"], tokenize=False, add_generation_prompt=False
            )
        }

    train_ds = train_ds.map(to_text, remove_columns=train_ds.column_names)
    eval_ds = eval_ds.map(to_text, remove_columns=eval_ds.column_names)

    trainer = SFTTrainer(
        model=model,
        tokenizer=tokenizer,
        train_dataset=train_ds,
        eval_dataset=eval_ds,
        dataset_text_field="text",
        max_seq_length=MAX_SEQ_LENGTH,
        packing=False,
        args=TrainingArguments(
            per_device_train_batch_size=8,
            gradient_accumulation_steps=2,
            num_train_epochs=NUM_EPOCHS,
            learning_rate=LEARNING_RATE,
            lr_scheduler_type="cosine",
            warmup_ratio=0.03,
            weight_decay=0.01,
            optim="adamw_8bit",
            fp16=not torch.cuda.is_bf16_supported(),
            bf16=torch.cuda.is_bf16_supported(),
            logging_steps=5,
            eval_strategy="steps",
            eval_steps=20,
            save_strategy="no",
            seed=SEED,
            output_dir="outputs",
            report_to="none",
        ),
    )

    # 只对 assistant 段计算损失（ChatML 标记），system/user 不参与学习
    trainer = train_on_responses_only(
        trainer,
        instruction_part="<|im_start|>user\n",
        response_part="<|im_start|>assistant\n",
    )

    trainer.train()

    # 产物一：LoRA 适配器（给 vLLM --enable-lora / peft 加载）
    model.save_pretrained("outputs/ruyix-command-lora")
    tokenizer.save_pretrained("outputs/ruyix-command-lora")

    # 产物二：合并后的完整模型（给 vLLM / llama.cpp 直接部署）
    model.save_pretrained_merged(
        "outputs/ruyix-command-qwen2.5-1.5b", tokenizer, save_method="merged_16bit"
    )

    # 产物三（可选）：GGUF 量化给 Ollama，取消注释即可
    # model.save_pretrained_gguf(
    #     "outputs/ruyix-command-gguf", tokenizer, quantization_method="q4_k_m"
    # )

    # ============================================
    # 冒烟测试：验证输出格式符合 ai.rs 的解析预期
    # ============================================
    FastLanguageModel.for_inference(model)
    smoke_inputs = [
        "帮我创建python文件叫hello",
        "查看git状态",
        "今晚有空吗",
    ]
    for text in smoke_inputs:
        messages = [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": text},
        ]
        inputs = tokenizer.apply_chat_template(
            messages, tokenize=True, add_generation_prompt=True, return_tensors="pt"
        ).to("cuda")
        output = model.generate(
            input_ids=inputs, max_new_tokens=300, temperature=0.1, do_sample=True
        )
        reply = tokenizer.decode(output[0][inputs.shape[1]:], skip_special_tokens=True)
        ok = ("---COMMAND---" in reply) or ("不支持" in reply) or len(reply) <= 40
        print(f"[{'PASS' if ok else 'FAIL'}] {text!r} → {reply!r}")


if __name__ == "__main__":
    main()
