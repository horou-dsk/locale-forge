use std::{
    collections::HashSet,
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Component, MAIN_SEPARATOR_STR, Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use unic_langid::LanguageIdentifier;

use crate::{atomic_file, state::state_path};

const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub source: SourceConfig,
    pub output: String,
    pub model: String,
    pub targets: Vec<TargetConfig>,
    #[serde(default)]
    pub translation: TranslationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceConfig {
    pub locale: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetConfig {
    pub locale: String,
    pub language: String,
    /// 覆盖全局输出模板的目标文件路径，相对于项目配置文件解析。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranslationConfig {
    pub batch_size: usize,
    pub timeout_seconds: u64,
    pub max_retries: usize,
}

impl Default for TranslationConfig {
    fn default() -> Self {
        Self {
            batch_size: 40,
            timeout_seconds: 120,
            max_retries: 2,
        }
    }
}

#[derive(Debug)]
pub struct LoadedProjectConfig {
    pub config: ProjectConfig,
    pub config_path: PathBuf,
    pub base_dir: PathBuf,
    pub source_path: PathBuf,
}

impl LoadedProjectConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let (config_path, contents) = read_project_config(path)?;
        let config: ProjectConfig =
            serde_json::from_str(&contents).map_err(|source| ConfigError::Parse {
                path: config_path.clone(),
                source,
            })?;
        let base_dir = config_path
            .parent()
            .expect("absolute configuration path always has a parent")
            .to_path_buf();
        reject_reserved_config_path(&config_path, &base_dir)?;
        validate_project_config(&config, &base_dir)?;
        let source_path = normalize_lexically(&base_dir.join(&config.source.path));

        Ok(Self {
            config,
            config_path,
            base_dir,
            source_path,
        })
    }

    pub fn target_path(&self, target: &TargetConfig) -> PathBuf {
        resolve_target_path(&self.base_dir, &self.config.output, target)
    }
}

fn resolve_target_path(base_dir: &Path, output_template: &str, target: &TargetConfig) -> PathBuf {
    match target.output.as_deref() {
        Some(output) => normalize_lexically(&base_dir.join(output)),
        None => {
            normalize_lexically(&base_dir.join(output_template.replace("{locale}", &target.locale)))
        }
    }
}

pub fn validate_project_config(config: &ProjectConfig, base_dir: &Path) -> Result<(), ConfigError> {
    let source_locale = validate_locale("source.locale", &config.source.locale)?;
    if config.model.trim().is_empty() {
        return Err(ConfigError::Invalid("model 不能为空".into()));
    }
    if !config.output.contains("{locale}") {
        return Err(ConfigError::Invalid(
            "output 必须包含 {locale} 占位符".into(),
        ));
    }
    if config.targets.is_empty() {
        return Err(ConfigError::Invalid("targets 至少需要一个目标语言".into()));
    }
    if !(1..=200).contains(&config.translation.batch_size) {
        return Err(ConfigError::Invalid(
            "translation.batch_size 必须在 1 到 200 之间".into(),
        ));
    }
    if !(1..=600).contains(&config.translation.timeout_seconds) {
        return Err(ConfigError::Invalid(
            "translation.timeout_seconds 必须在 1 到 600 之间".into(),
        ));
    }
    if config.translation.max_retries > 10 {
        return Err(ConfigError::Invalid(
            "translation.max_retries 不能大于 10".into(),
        ));
    }

    let source_format = extension(&config.source.path)?;
    let source_path = normalize_lexically(&base_dir.join(&config.source.path));
    let lock_path = normalize_lexically(&state_path(base_dir));
    if paths_equal(&source_path, &lock_path) {
        return Err(ConfigError::Invalid(format!(
            "源文件不能使用保留的状态文件路径 {}",
            lock_path.display()
        )));
    }
    let mut locales = HashSet::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    for target in &config.targets {
        let target_locale = validate_locale("targets[].locale", &target.locale)?;
        if target_locale == source_locale {
            return Err(ConfigError::Invalid(format!(
                "目标语言 {} 与源语言重复",
                target.locale
            )));
        }
        if target.language.trim().is_empty() {
            return Err(ConfigError::Invalid(format!(
                "目标语言 {} 的 language 不能为空",
                target.locale
            )));
        }
        if !locales.insert(target_locale) {
            return Err(ConfigError::Invalid(format!(
                "目标语言 {} 重复",
                target.locale
            )));
        }

        let target_path = resolve_target_path(base_dir, &config.output, target);
        if extension(&target_path)? != source_format {
            return Err(ConfigError::Invalid(format!(
                "目标文件 {} 与源文件格式不一致",
                target_path.display()
            )));
        }
        if paths_equal(&target_path, &source_path) {
            return Err(ConfigError::Invalid(format!(
                "目标文件 {} 不能覆盖源文件",
                target_path.display()
            )));
        }
        if paths_equal(&target_path, &lock_path) {
            return Err(ConfigError::Invalid(format!(
                "目标文件不能使用保留的状态文件路径 {}",
                lock_path.display()
            )));
        }
        if paths.iter().any(|path| paths_equal(path, &target_path)) {
            return Err(ConfigError::Invalid(format!(
                "多个目标语言生成了同一路径 {}",
                target_path.display()
            )));
        }
        paths.push(target_path);
    }

    Ok(())
}

#[cfg(windows)]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

pub fn write_new_project_config(path: &Path, config: &ProjectConfig) -> Result<(), ConfigError> {
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    reject_reserved_config_path(path, base_dir)?;
    validate_project_config(config, base_dir)?;
    let contents =
        serde_json::to_string_pretty(config).map_err(|source| ConfigError::Serialize { source })?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(path).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    use std::io::Write;
    file.write_all(contents.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
}

fn reject_reserved_config_path(config_path: &Path, base_dir: &Path) -> Result<(), ConfigError> {
    let lock_path = normalize_lexically(&state_path(base_dir));
    if paths_equal(&normalize_lexically(config_path), &lock_path) {
        return Err(ConfigError::Invalid(format!(
            "配置文件不能使用保留的状态文件路径 {}",
            lock_path.display()
        )));
    }
    Ok(())
}

/// 原子更新项目配置引用的命名模型配置；值未变化时不写文件。
pub(crate) fn update_project_model(path: &Path, model: String) -> Result<bool, ConfigError> {
    let (config_path, contents) = read_project_config(path)?;
    let mut config: ProjectConfig =
        serde_json::from_str(&contents).map_err(|source| ConfigError::Parse {
            path: config_path.clone(),
            source,
        })?;
    let base_dir = config_path
        .parent()
        .expect("absolute configuration path always has a parent");
    validate_project_config(&config, base_dir)?;
    if config.model == model {
        return Ok(false);
    }
    config.model = model;
    validate_project_config(&config, base_dir)?;

    let mut document: serde_json::Value =
        serde_json::from_str(&contents).map_err(|source| ConfigError::Parse {
            path: config_path.clone(),
            source,
        })?;
    let object = document
        .as_object_mut()
        .ok_or_else(|| ConfigError::Invalid("配置根节点必须是对象".into()))?;
    object.insert(
        "model".into(),
        serde_json::Value::String(std::mem::take(&mut config.model)),
    );
    let mut output =
        serde_json::to_vec_pretty(&document).map_err(|source| ConfigError::Serialize { source })?;
    output.push(b'\n');
    atomic_file::write(&config_path, &output, false).map_err(|source| ConfigError::Write {
        path: config_path,
        source,
    })?;
    Ok(true)
}

fn read_project_config(path: &Path) -> Result<(PathBuf, String), ConfigError> {
    let config_path = absolute_path(path)?;
    let metadata = fs::metadata(&config_path).map_err(|source| ConfigError::Read {
        path: config_path.clone(),
        source,
    })?;
    if metadata.len() > MAX_CONFIG_SIZE {
        return Err(ConfigError::Invalid(format!(
            "配置文件超过 {} 字节限制",
            MAX_CONFIG_SIZE
        )));
    }
    let contents = fs::read_to_string(&config_path).map_err(|source| ConfigError::Read {
        path: config_path.clone(),
        source,
    })?;
    Ok((config_path, contents))
}

fn absolute_path(path: &Path) -> Result<PathBuf, ConfigError> {
    if path.is_absolute() {
        return Ok(normalize_lexically(path));
    }
    env::current_dir()
        .map(|directory| normalize_lexically(&directory.join(path)))
        .map_err(ConfigError::CurrentDirectory)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut prefix: Option<OsString> = None;
    let mut rooted = false;
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(value) => prefix = Some(value.as_os_str().to_owned()),
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if segments
                    .last()
                    .is_some_and(|segment| segment != OsStr::new(".."))
                {
                    segments.pop();
                } else if !rooted {
                    segments.push(OsString::from(".."));
                }
            }
            Component::Normal(value) => segments.push(value.to_owned()),
        }
    }

    let mut normalized = PathBuf::new();
    if let Some(prefix) = prefix {
        normalized.push(prefix);
    }
    if rooted {
        normalized.push(Path::new(MAIN_SEPARATOR_STR));
    }
    normalized.extend(segments);
    normalized
}

fn extension(path: &Path) -> Result<String, ConfigError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| ConfigError::Invalid(format!("文件 {} 缺少扩展名", path.display())))?;
    match extension.as_str() {
        "json" | "arb" => Ok(extension),
        _ => Err(ConfigError::Invalid(format!(
            "文件 {} 仅支持 .json 或 .arb",
            path.display()
        ))),
    }
}

fn validate_locale(field: &str, locale: &str) -> Result<LanguageIdentifier, ConfigError> {
    if locale.is_empty()
        || !locale
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ConfigError::Invalid(format!("{field} 不是安全的语言代码")));
    }
    LanguageIdentifier::from_str(&locale.replace('_', "-"))
        .map_err(|_| ConfigError::Invalid(format!("{field} 不是有效的 BCP-47 语言代码: {locale}")))
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("无法读取配置文件 {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("配置文件 {path} 不是有效 JSON: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("配置无效: {0}")]
    Invalid(String),
    #[error("无法序列化配置: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
    #[error("无法写入配置文件 {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("无法读取当前目录: {0}")]
    CurrentDirectory(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> ProjectConfig {
        ProjectConfig {
            source: SourceConfig {
                locale: "zh-CN".into(),
                path: "locales/zh.json".into(),
            },
            output: "locales/{locale}.json".into(),
            model: "default".into(),
            targets: vec![TargetConfig {
                locale: "en-US".into(),
                language: "English (United States)".into(),
                output: None,
                prompt: None,
            }],
            translation: TranslationConfig::default(),
        }
    }

    #[test]
    fn resolves_paths_relative_to_config_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, serde_json::to_vec(&valid_config()).unwrap()).unwrap();

        let loaded = LoadedProjectConfig::load(&path).unwrap();

        assert_eq!(loaded.source_path, directory.path().join("locales/zh.json"));
        assert_eq!(
            loaded.target_path(&loaded.config.targets[0]),
            directory.path().join("locales/en-US.json")
        );
    }

    #[test]
    fn target_output_overrides_global_template() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = valid_config();
        config.targets[0].output = Some("locales/en.json".into());
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();

        let loaded = LoadedProjectConfig::load(&path).unwrap();

        assert_eq!(
            loaded.target_path(&loaded.config.targets[0]),
            directory.path().join("locales/en.json")
        );
        assert_eq!(loaded.config.targets[0].locale, "en-US");
    }

    #[test]
    fn rejects_output_without_locale_placeholder() {
        let mut config = valid_config();
        config.output = "locales/output.json".into();

        let error = validate_project_config(&config, Path::new(".")).unwrap_err();

        assert!(error.to_string().contains("{locale}"));
    }

    #[test]
    fn rejects_duplicate_target_locale() {
        let mut config = valid_config();
        config.targets.push(config.targets[0].clone());

        let error = validate_project_config(&config, Path::new(".")).unwrap_err();

        assert!(error.to_string().contains("重复"));
    }

    #[test]
    fn rejects_equivalent_locale_spellings() {
        let mut config = valid_config();
        config.targets.push(TargetConfig {
            locale: "en-us".into(),
            language: "English".into(),
            output: None,
            prompt: None,
        });

        let error = validate_project_config(&config, Path::new(".")).unwrap_err();

        assert!(error.to_string().contains("重复"));
    }

    #[test]
    fn rejects_mixed_source_and_target_formats() {
        let mut config = valid_config();
        config.output = "locales/{locale}.arb".into();

        let error = validate_project_config(&config, Path::new(".")).unwrap_err();

        assert!(error.to_string().contains("格式不一致"));
    }

    #[test]
    fn rejects_target_output_with_different_format() {
        let mut config = valid_config();
        config.targets[0].output = Some("locales/en.arb".into());

        let error = validate_project_config(&config, Path::new(".")).unwrap_err();

        assert!(error.to_string().contains("格式不一致"));
    }

    #[test]
    fn rejects_paths_that_collide_after_normalization() {
        let mut config = valid_config();
        config.output = "locales/{locale}/../output.json".into();
        config.targets.push(TargetConfig {
            locale: "ja-JP".into(),
            language: "Japanese".into(),
            output: None,
            prompt: None,
        });

        let error = validate_project_config(&config, Path::new("C:/project")).unwrap_err();

        assert!(error.to_string().contains("同一路径"));
    }

    #[test]
    fn rejects_duplicate_target_specific_outputs() {
        let mut config = valid_config();
        config.targets[0].output = Some("locales/shared.json".into());
        config.targets.push(TargetConfig {
            locale: "ja-JP".into(),
            language: "Japanese".into(),
            output: Some("locales/shared.json".into()),
            prompt: None,
        });

        let error = validate_project_config(&config, Path::new(".")).unwrap_err();

        assert!(error.to_string().contains("同一路径"));
    }

    #[test]
    fn rejects_reserved_state_file_as_source_or_target() {
        let mut source = valid_config();
        source.source.path = "locale-forge.lock.json".into();
        let error = validate_project_config(&source, Path::new(".")).unwrap_err();
        assert!(error.to_string().contains("保留的状态文件"));

        let mut target = valid_config();
        target.targets[0].output = Some("locale-forge.lock.json".into());
        let error = validate_project_config(&target, Path::new(".")).unwrap_err();
        assert!(error.to_string().contains("保留的状态文件"));
    }

    #[test]
    fn rejects_reserved_state_file_as_config_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("locale-forge.lock.json");

        let error = write_new_project_config(&path, &valid_config()).unwrap_err();

        assert!(error.to_string().contains("配置文件不能使用"));
        assert!(!path.exists());
    }

    #[test]
    fn updates_project_model_and_skips_existing_value() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut document = serde_json::to_value(valid_config()).unwrap();
        document["extension"] = serde_json::json!({"preserve": true});
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

        assert!(update_project_model(&path, "new-profile".into()).unwrap());
        let contents_after_update = fs::read(&path).unwrap();
        let updated: ProjectConfig = serde_json::from_slice(&contents_after_update).unwrap();
        assert_eq!(updated.model, "new-profile");
        let updated_document: serde_json::Value =
            serde_json::from_slice(&contents_after_update).unwrap();
        assert_eq!(updated_document["extension"]["preserve"], true);

        assert!(!update_project_model(&path, "new-profile".into()).unwrap());
        assert_eq!(fs::read(&path).unwrap(), contents_after_update);
    }
}
