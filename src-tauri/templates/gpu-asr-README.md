# GPU 加速（可选）

语音转写默认跑 **CPU**。这个目录里**什么都没有，就表示不用 GPU** —— 这是合法的默认状态，
不影响任何功能。

想用 GPU：把下面几个 DLL 从 CUDA Toolkit 拷进来，重启 ruyix 即可；
想退回 CPU：把它们删掉（或整个目录删掉），再重启。

来源目录：`C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.x\bin\`

| 需要的文件 | 作用 |
|---|---|
| `cudart64_*.dll` | CUDA 运行时 |
| `cublas64_*.dll` `cublasLt64_*.dll` | 矩阵乘（candle 的 GPU 后端依赖） |
| `nvrtc64_*.dll` | 运行时编译算子内核 |

## 为什么单独放这里，而不是塞在 exe 旁边

发行形态是「一个 exe + `global/` + `projects/` + `plugins/`」。这几个 DLL 加在一起有几百 MB，
放在 exe 旁边就等于把绿色版毁了；放在这个目录里，**装与不装都合法**，删目录即卸载。

## 怎么确认真的用上了

会话里录音转写时，状态栏那一行会写明跑在哪，例如：

```
识别中（3.775 秒音频，本机推理，GPU（CUDA），trim 窗口（按真实长度编码），剪静音）…
识别中（3.775 秒音频，本机推理，CPU（未用 GPU：构建未带 cuda 特性），trim 窗口…）…
```

**「未用 GPU：」后面那句就是没用上的具体原因**（构建没带 cuda 特性 / DLL 缺失 / 驱动太旧 /
算力代号没编进内核）—— 不会出现「你以为在用 GPU，其实在跑 CPU」这种沉默。

## 前提

1. exe 必须是**带 cuda 特性的构建**：`cargo build --release -p ruyix --features harness-engine/cuda`
2. 需要 CUDA Toolkit ≥ 12.8（RTX 50 系这类算力 sm_120 的卡）
3. 不需要改配置也能用（`voice.gpu` 默认 `auto`：探测到、且真的起得来，就用）；想强制关掉 GPU，
   把 `voice.gpu` 设成 `off`。
