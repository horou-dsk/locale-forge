use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "locale-forge",
    version,
    about = "使用大语言模型增量翻译 JSON 和 ARB 本地化文件"
)]
pub struct Cli {
    #[arg(long, global = true, default_value = "config.json")]
    pub config: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 创建新的项目配置文件
    Init(InitArgs),
    /// 验证项目配置和本地化文件
    Validate,
    /// 查看源文件与目标文件的字段差异
    Diff(DiffArgs),
    /// 翻译缺失字段或强制重新翻译
    Translate(TranslateArgs),
    /// 管理用户级模型配置
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long = "source-locale")]
    pub source_locale: String,
    #[arg(long)]
    pub output: String,
    #[arg(long)]
    pub model: String,
    #[arg(long = "target", required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    #[arg(long = "locale")]
    pub locales: Vec<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TranslateArgs {
    #[arg(long = "locale")]
    pub locales: Vec<String>,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum ModelCommand {
    /// 创建或覆盖一个模型配置
    Set(ModelSetArgs),
    /// 列出全部模型配置
    List,
    /// 显示一个已脱敏的模型配置
    Show { name: String },
    /// 删除一个模型配置
    Delete { name: String },
}

#[derive(Debug, Args)]
pub struct ModelSetArgs {
    pub name: String,
    #[arg(long)]
    pub url: String,
    #[arg(long)]
    pub model: String,
    #[arg(long, conflicts_with_all = ["key_stdin", "no_key"])]
    pub key: Option<String>,
    #[arg(long, conflicts_with_all = ["key", "no_key"])]
    pub key_stdin: bool,
    #[arg(long, conflicts_with_all = ["key", "key_stdin"])]
    pub no_key: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn parses_repeated_locale_filters() {
        let cli = Cli::try_parse_from([
            "locale-forge",
            "diff",
            "--locale",
            "en-US",
            "--locale",
            "ja-JP",
        ])
        .unwrap();

        let Command::Diff(arguments) = cli.command else {
            panic!("expected diff command");
        };
        assert_eq!(arguments.locales, ["en-US", "ja-JP"]);
    }

    #[test]
    fn rejects_conflicting_key_inputs() {
        let result = Cli::try_parse_from([
            "locale-forge",
            "model",
            "set",
            "default",
            "--url",
            "https://example.com/v1/chat/completions",
            "--model",
            "example",
            "--key",
            "secret",
            "--no-key",
        ]);

        assert!(result.is_err());
    }
}
