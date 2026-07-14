# Locale Forge

Locale Forge 是一个 Rust 命令行翻译工具。它读取一个 JSON 或 Flutter ARB 源文件，比较现有目标语言文件的字段差异，并通过支持严格 JSON Schema 的 OpenAI 兼容接口补齐译文。

## 功能

- 递归翻译嵌套 JSON 对象和数组，保留数字、布尔值及 `null`。
- 默认只翻译缺失或空字符串字段，保留已有译文和额外字段。
- 支持 ARB 元数据、占位符、ICU 复数及 `select` 表达式校验。
- 每个目标语言独立原子写入；单个语言失败不会破坏原文件。
- 模型 URL、名称和密钥保存在用户级命名配置中。
- 可从 OpenAI 兼容接口查询模型列表并快速切换远程模型或项目模型配置。

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

生成的 `config.json` 可为每个目标语言补充更准确的 `language`、可选 `prompt` 和独立 `output`。所有相对路径均以配置文件所在目录为基准。

配置模型：

```powershell
locale-forge model set default `
  --url https://example.com/v1/chat/completions `
  --model model-name
```

未指定密钥参数时会隐藏输入。自动化环境可使用 `--key-stdin`；本地无鉴权模型可使用 `--no-key`。`--key <值>` 虽受支持，但可能将密钥暴露在 shell 历史中。

Linux、macOS 和 Windows 均将模型配置保存在用户主目录下的 `~/.locale-forge/models.json`。目录会在首次保存模型配置时自动创建。

查询并切换模型：

```powershell
# 查询 default 配置对应接口提供的模型
locale-forge model available default
locale-forge model available default --json

# 精确校验模型 ID 后切换；省略 ID 时进入编号选择
locale-forge model select default gpt-5.5
locale-forge model select default

# 将当前项目的 config.json 切换到 default 命名配置
locale-forge --config config.json model activate default
```

`available` 和 `select` 默认从 Chat Completions 地址推导同源 `/models` 地址；非标准接口可通过 `--url <同源地址>` 临时覆盖。`model list` 仍只列出本地命名配置，`select` 只更新该配置的远程模型 ID，`activate` 只更新项目配置的 `model` 字段。

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
    },
    {
      "locale": "ja-JP",
      "language": "Japanese",
      "output": "locales/ja.json"
    }
  ],
  "translation": {
    "batch_size": 40,
    "timeout_seconds": 120,
    "max_retries": 2
  }
}
```

`targets[].output` 会覆盖该语言的全局 `output` 模板。例如上例仍使用 `ja-JP` 作为翻译语言代码和 ARB 的 `@@locale`，但文件写入 `locales/ja.json`。未配置时继续按全局模板生成文件名。

JSON 源只生成 JSON，ARB 源只生成 ARB。模型必须支持 Chat Completions 的 `response_format.type=json_schema` 严格结构化输出；工具不会静默降级到普通 JSON 模式。

## 安全说明

HTTPS 可连接任意主机；明文 HTTP 仅允许 `localhost` 或回环 IP。客户端禁止重定向，模型输出必须通过本地结构和 ICU 校验后才能写入文件。密钥不会显示在 `model list` 或 `model show` 输出中。
