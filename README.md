# Locale Forge

Locale Forge 是一个 Rust 命令行翻译工具。它读取一个 JSON 或 Flutter ARB 源文件，比较现有目标语言文件的字段差异，并通过支持严格 JSON Schema 的 OpenAI 兼容接口补齐译文。

## 功能

- 递归翻译嵌套 JSON 对象和数组，保留数字、布尔值及 `null`。
- 默认只翻译缺失或空字符串字段，保留已有译文和额外字段。
- 支持 ARB 元数据、占位符、ICU 复数及 `select` 表达式校验。
- 每个目标语言独立原子写入；单个语言失败不会破坏原文件。
- 模型 URL、名称和密钥保存在用户级命名配置中。

## 构建

```powershell
cargo build --release
cargo test
```

生成的程序位于 `target/release/locale-forge.exe`，也可以在开发时使用 `cargo run -- <参数>`。

## 快速开始

创建项目配置：

```powershell
locale-forge init --source locales/zh.json --source-locale zh-CN `
  --output "locales/{locale}.json" --model default `
  --target en-US --target ja-JP
```

生成的 `config.json` 可为每个目标语言补充更准确的 `language` 和可选 `prompt`。所有相对路径均以配置文件所在目录为基准。

配置模型：

```powershell
locale-forge model set default `
  --url https://example.com/v1/chat/completions `
  --model model-name
```

未指定密钥参数时会隐藏输入。自动化环境可使用 `--key-stdin`；本地无鉴权模型可使用 `--no-key`。`--key <值>` 虽受支持，但可能将密钥暴露在 shell 历史中。

Linux、macOS 和 Windows 均将模型配置保存在用户主目录下的 `~/.locale-forge/models.json`。目录会在首次保存模型配置时自动创建。

检查并执行翻译：

```powershell
locale-forge validate
locale-forge diff
locale-forge diff --locale en-US --json
locale-forge translate
locale-forge translate --locale ja-JP --force
```

`diff` 会报告缺失、空值、非字符串结构值变化、额外字段和类型冲突。无差异时返回 `0`，存在差异时返回 `2`；配置或运行错误返回 `1`。`translate` 默认处理所有目标语言，`--force` 会重新翻译全部非空源字符串。

## 配置示例

```json
{
  "source": { "locale": "zh-CN", "path": "locales/zh.json" },
  "output": "locales/{locale}.json",
  "model": "default",
  "targets": [
    {
      "locale": "en-US",
      "language": "English (United States)",
      "prompt": "Use concise product UI language"
    }
  ],
  "translation": {
    "batch_size": 40,
    "timeout_seconds": 120,
    "max_retries": 2
  }
}
```

JSON 源只生成 JSON，ARB 源只生成 ARB。模型必须支持 Chat Completions 的 `response_format.type=json_schema` 严格结构化输出；工具不会静默降级到普通 JSON 模式。

## 安全说明

HTTPS 可连接任意主机；明文 HTTP 仅允许 `localhost` 或回环 IP。客户端禁止重定向，模型输出必须通过本地结构和 ICU 校验后才能写入文件。密钥不会显示在 `model list` 或 `model show` 输出中。
