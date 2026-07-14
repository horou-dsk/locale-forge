use std::{
    fs,
    io::{self, IsTerminal, Read, Write},
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde_json::json;

mod progress;

use self::progress::ConsoleTranslationProgress;
use crate::{
    agent::{TranslationAgent, openai::OpenAiClient, progress::TranslationProgressReporter},
    atomic_file,
    catalog::{Catalog, CatalogFormat, DiffReport},
    cli::{Cli, Command, DiffArgs, InitArgs, ModelCommand, ModelSetArgs, TranslateArgs},
    config::{
        LoadedProjectConfig, ProjectConfig, SourceConfig, TargetConfig, TranslationConfig,
        update_project_model, write_new_project_config,
    },
    models::{
        ModelStore, ModelSummary, default_model_store_path,
        remote::{AvailableModel, fetch_available_models},
    },
    terminal::escape_controls,
};

pub async fn execute(cli: Cli) -> Result<u8> {
    match cli.command {
        Command::Init(arguments) => {
            initialize(cli.config, arguments)?;
            Ok(0)
        }
        Command::Validate => {
            validate(&cli.config)?;
            Ok(0)
        }
        Command::Diff(arguments) => diff(&cli.config, arguments),
        Command::Translate(arguments) => translate(&cli.config, arguments).await,
        Command::Model { command } => {
            handle_model_command(command, &cli.config).await?;
            Ok(0)
        }
    }
}

fn validate(config_path: &Path) -> Result<()> {
    let project = LoadedProjectConfig::load(config_path)?;
    let source = Catalog::load(&project.source_path)?;
    for target in &project.config.targets {
        Catalog::load_optional(&project.target_path(target), source.format())?;
    }
    let model_store = ModelStore::load(&default_model_store_path()?)?;
    if !model_store.contains(&project.config.model) {
        bail!("模型配置不存在: {}", project.config.model);
    }
    println!(
        "配置有效：源语言 {}，目标语言 {} 个，格式 {}",
        project.config.source.locale,
        project.config.targets.len(),
        format_name(source.format())
    );
    Ok(())
}

fn diff(config_path: &Path, arguments: DiffArgs) -> Result<u8> {
    let project = LoadedProjectConfig::load(config_path)?;
    let source = Catalog::load(&project.source_path)?;
    let targets = select_targets(&project.config.targets, &arguments.locales)?;
    let mut reports = Vec::with_capacity(targets.len());
    let mut has_differences = false;
    for target in targets {
        let target_catalog = Catalog::load_optional(&project.target_path(target), source.format())?;
        let result = source.diff(target_catalog.as_ref(), &target.locale, false)?;
        has_differences |= !result.report.is_clean();
        reports.push(result.report);
    }

    if arguments.json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        for report in &reports {
            print_diff_report(report);
        }
    }
    Ok(if has_differences { 2 } else { 0 })
}

async fn translate(config_path: &Path, arguments: TranslateArgs) -> Result<u8> {
    let project = LoadedProjectConfig::load(config_path)?;
    let source = Catalog::load(&project.source_path)?;
    let targets = select_targets(&project.config.targets, &arguments.locales)?;
    let model_store = ModelStore::load(&default_model_store_path()?)?;
    let profile = model_store.into_profile(&project.config.model)?;
    let client = OpenAiClient::new(profile, project.config.translation.timeout_seconds)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let agent = TranslationAgent::new(
        client,
        project.config.translation.batch_size,
        project.config.translation.max_retries,
    );
    let progress = ConsoleTranslationProgress;
    let mut failures = Vec::new();

    for target in targets {
        let target_path = project.target_path(target);
        match translate_target(
            &source,
            &agent,
            &project.config.source.locale,
            target,
            &target_path,
            arguments.force,
            &progress,
        )
        .await
        {
            Ok(count) => println!(
                "{}: 已写入 {}，翻译 {} 个字段",
                target.locale,
                target_path.display(),
                count
            ),
            Err(error) => failures.push((target.locale.as_str(), error)),
        }
    }

    if failures.is_empty() {
        return Ok(0);
    }
    eprintln!("以下目标语言翻译失败：");
    for (locale, error) in failures {
        eprintln!("  {locale}: {error:#}");
    }
    Ok(1)
}

async fn translate_target<C: crate::agent::ModelClient, P: TranslationProgressReporter + ?Sized>(
    source: &Catalog,
    agent: &TranslationAgent<C>,
    source_locale: &str,
    target: &TargetConfig,
    target_path: &Path,
    force: bool,
    progress: &P,
) -> Result<usize> {
    let target_catalog = Catalog::load_optional(target_path, source.format())?;
    let target_missing = target_catalog.is_none();
    let diff = source.diff(target_catalog.as_ref(), &target.locale, force)?;
    if !diff.report.conflicts.is_empty() {
        bail!(
            "存在 {} 个类型冲突，请先修复目标文件",
            diff.report.conflicts.len()
        );
    }
    if !target_missing
        && diff.units.is_empty()
        && diff.report.missing.is_empty()
        && diff.report.empty.is_empty()
        && diff.report.changed.is_empty()
    {
        println!("{}: 没有需要翻译的字段", target.locale);
        return Ok(0);
    }

    let translated_count = diff.units.len();
    let translations = agent
        .translate_with_progress(source, source_locale, target, &diff.units, progress)
        .await?;
    let merged = source.merge(target_catalog, translations, &target.locale)?;
    let contents = merged.to_pretty_bytes()?;
    atomic_file::write(target_path, &contents, false)
        .with_context(|| format!("无法原子写入 {}", target_path.display()))?;
    Ok(translated_count)
}

fn select_targets<'a>(
    targets: &'a [TargetConfig],
    locales: &[String],
) -> Result<Vec<&'a TargetConfig>> {
    if locales.is_empty() {
        return Ok(targets.iter().collect());
    }
    for locale in locales {
        if !targets.iter().any(|target| target.locale == *locale) {
            bail!("目标语言未在 config.json 中配置: {locale}");
        }
    }
    Ok(targets
        .iter()
        .filter(|target| locales.contains(&target.locale))
        .collect())
}

fn print_diff_report(report: &DiffReport) {
    println!("{}:", report.locale);
    print_paths("缺失", &report.missing);
    print_paths("空值", &report.empty);
    print_paths("结构值变化", &report.changed);
    print_paths("额外", &report.extra);
    if report.conflicts.is_empty() {
        println!("  类型冲突: 0");
    } else {
        println!("  类型冲突: {}", report.conflicts.len());
        for conflict in &report.conflicts {
            println!(
                "    {}: {} -> {}",
                conflict.path, conflict.source_type, conflict.target_type
            );
        }
    }
}

fn print_paths(label: &str, paths: &[String]) {
    println!("  {label}: {}", paths.len());
    for path in paths {
        println!("    {path}");
    }
}

fn format_name(format: CatalogFormat) -> &'static str {
    match format {
        CatalogFormat::Json => "JSON",
        CatalogFormat::Arb => "ARB",
    }
}

fn initialize(config_path: std::path::PathBuf, arguments: InitArgs) -> Result<()> {
    if let Some(parent) = config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建配置目录 {}", parent.display()))?;
    }
    let targets = arguments
        .targets
        .into_iter()
        .map(|locale| TargetConfig {
            language: locale.clone(),
            locale,
            output: None,
            prompt: None,
        })
        .collect();
    let config = ProjectConfig {
        source: SourceConfig {
            locale: arguments.source_locale,
            path: arguments.source,
        },
        output: arguments.output,
        model: arguments.model,
        targets,
        translation: TranslationConfig::default(),
    };
    write_new_project_config(&config_path, &config)?;
    println!("已创建 {}", config_path.display());
    Ok(())
}

async fn handle_model_command(command: ModelCommand, config_path: &Path) -> Result<()> {
    let path = default_model_store_path()?;
    let mut store = ModelStore::load(&path)?;
    match command {
        ModelCommand::Set(arguments) => {
            let (name, url, model, key) = read_model_arguments(arguments)?;
            store.set(name.clone(), url, model, key)?;
            store.save(&path)?;
            println!("已保存模型配置 {name}");
        }
        ModelCommand::List => {
            for summary in store.summaries() {
                print_model_summary(summary)?;
            }
        }
        ModelCommand::Available(arguments) => {
            let models =
                fetch_available_models(&store, &arguments.name, arguments.url.as_deref()).await?;
            print_available_models(&models, arguments.json)?;
        }
        ModelCommand::Select(arguments) => {
            if arguments.model.is_none() {
                require_interactive_model_selection()?;
            }
            let models =
                fetch_available_models(&store, &arguments.name, arguments.url.as_deref()).await?;
            let selected = select_available_model(models, arguments.model)?;
            store.select_model(&arguments.name, selected.id)?;
            store.save(&path)?;
            println!(
                "已将模型配置 {} 切换为 {}",
                arguments.name,
                store.summary(&arguments.name)?.model
            );
        }
        ModelCommand::Activate { name } => {
            if !store.contains(&name) {
                bail!("模型配置不存在: {name}");
            }
            if update_project_model(config_path, name.clone())? {
                println!("已将项目配置切换为模型配置 {name}");
            } else {
                println!("项目配置已使用模型配置 {name}");
            }
        }
        ModelCommand::Show { name } => print_model_summary(store.summary(&name)?)?,
        ModelCommand::Delete { name } => {
            store.delete(&name)?;
            store.save(&path)?;
            println!("已删除模型配置 {name}");
        }
    }
    Ok(())
}

fn print_available_models(models: &[AvailableModel], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(models)?);
        return Ok(());
    }
    if models.is_empty() {
        println!("未返回可用模型");
        return Ok(());
    }
    for model in models {
        if let Some(owned_by) = &model.owned_by {
            println!(
                "{}\t{}",
                escape_controls(&model.id),
                escape_controls(owned_by)
            );
        } else {
            println!("{}", escape_controls(&model.id));
        }
    }
    Ok(())
}

fn select_available_model(
    models: Vec<AvailableModel>,
    requested: Option<String>,
) -> Result<AvailableModel> {
    if models.is_empty() {
        bail!("远程接口未返回可用模型");
    }
    if let Some(requested) = requested {
        return models
            .into_iter()
            .find(|model| model.id == requested)
            .ok_or_else(|| anyhow::anyhow!("远程模型列表中不存在: {requested}"));
    }
    println!("可用模型：");
    for (index, model) in models.iter().enumerate() {
        println!("  {}. {}", index + 1, escape_controls(&model.id));
    }
    let index = loop {
        print!("请选择模型编号: ");
        io::stdout().flush().context("无法刷新终端输出")?;
        let mut input = String::new();
        if io::stdin()
            .read_line(&mut input)
            .context("无法读取模型编号")?
            == 0
        {
            bail!("未读取到模型编号");
        }
        match parse_model_selection(&input, models.len()) {
            Ok(index) => break index,
            Err(error) => eprintln!("{error}"),
        }
    };
    Ok(models
        .into_iter()
        .nth(index)
        .expect("validated model selection index"))
}

fn require_interactive_model_selection() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("非交互环境必须指定模型 ID");
    }
    Ok(())
}

fn parse_model_selection(input: &str, model_count: usize) -> Result<usize> {
    let selection: usize = input.trim().parse().context("请输入有效的模型编号")?;
    if !(1..=model_count).contains(&selection) {
        bail!("模型编号必须在 1 到 {model_count} 之间");
    }
    Ok(selection - 1)
}

fn read_model_arguments(
    arguments: ModelSetArgs,
) -> Result<(String, String, String, Option<String>)> {
    let key = if let Some(key) = arguments.key {
        eprintln!(
            "警告: --key 的值可能保存在 shell 历史或进程参数中；建议使用隐藏输入或 --key-stdin。"
        );
        Some(key)
    } else if arguments.key_stdin {
        let mut key = String::new();
        io::stdin()
            .read_to_string(&mut key)
            .context("无法从标准输入读取 API key")?;
        while matches!(key.as_bytes().last(), Some(b'\n' | b'\r')) {
            key.pop();
        }
        Some(key)
    } else if arguments.no_key {
        None
    } else {
        if !io::stdin().is_terminal() {
            bail!("非交互环境必须指定 --key、--key-stdin 或 --no-key");
        }
        Some(rpassword::prompt_password("API key: ").context("无法读取 API key")?)
    };
    Ok((arguments.name, arguments.url, arguments.model, key))
}

fn print_model_summary(summary: ModelSummary<'_>) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "name": summary.name,
            "url": summary.url,
            "model": summary.model,
            "has_key": summary.has_key,
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_based_model_selection() {
        assert_eq!(parse_model_selection("2\n", 3).unwrap(), 1);
        assert!(parse_model_selection("0", 3).is_err());
        assert!(parse_model_selection("4", 3).is_err());
        assert!(parse_model_selection("invalid", 3).is_err());
    }
}
