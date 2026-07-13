use std::{
    fs,
    io::{self, IsTerminal, Read},
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::{
    agent::{TranslationAgent, openai::OpenAiClient},
    atomic_file,
    catalog::{Catalog, CatalogFormat, DiffReport},
    cli::{Cli, Command, DiffArgs, InitArgs, ModelCommand, ModelSetArgs, TranslateArgs},
    config::{
        LoadedProjectConfig, ProjectConfig, SourceConfig, TargetConfig, TranslationConfig,
        write_new_project_config,
    },
    models::{ModelStore, ModelSummary, default_model_store_path},
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
            handle_model_command(command)?;
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

async fn translate_target<C: crate::agent::ModelClient>(
    source: &Catalog,
    agent: &TranslationAgent<C>,
    source_locale: &str,
    target: &TargetConfig,
    target_path: &Path,
    force: bool,
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
        .translate(source, source_locale, target, &diff.units)
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

fn handle_model_command(command: ModelCommand) -> Result<()> {
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
        ModelCommand::Show { name } => print_model_summary(store.summary(&name)?)?,
        ModelCommand::Delete { name } => {
            store.delete(&name)?;
            store.save(&path)?;
            println!("已删除模型配置 {name}");
        }
    }
    Ok(())
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
