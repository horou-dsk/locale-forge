use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use unic_langid::LanguageIdentifier;

use crate::{atomic_file, catalog::CatalogFormat};

pub const STATE_FILE_NAME: &str = "locale-forge.lock.json";
const STATE_VERSION: u32 = 1;
const MAX_STATE_SIZE: u64 = 64 * 1024 * 1024;

pub type SourceFingerprints = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TranslationState {
    version: u32,
    source: StateSource,
    #[serde(default)]
    targets: BTreeMap<String, SourceFingerprints>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StateSource {
    locale: String,
    format: StateFormat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum StateFormat {
    Json,
    Arb,
}

impl TranslationState {
    pub fn new(source_locale: &str, format: CatalogFormat) -> Self {
        Self {
            version: STATE_VERSION,
            source: StateSource {
                locale: source_locale.to_owned(),
                format: format.into(),
            },
            targets: BTreeMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>, StateError> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StateError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if metadata.len() > MAX_STATE_SIZE {
            return Err(StateError::TooLarge {
                path: path.to_path_buf(),
                limit: MAX_STATE_SIZE,
            });
        }
        let contents = fs::read_to_string(path).map_err(|source| StateError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let state: Self = serde_json::from_str(&contents).map_err(|source| StateError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        state.validate()?;
        Ok(Some(state))
    }

    pub fn matches_source(&self, source_locale: &str, format: CatalogFormat) -> bool {
        self.source.locale == source_locale && self.source.format == StateFormat::from(format)
    }

    pub fn target(&self, locale: &str) -> Option<&SourceFingerprints> {
        self.targets.get(locale)
    }

    pub fn reset_source(&mut self, source_locale: &str, format: CatalogFormat) {
        self.source = StateSource {
            locale: source_locale.to_owned(),
            format: format.into(),
        };
        self.targets.clear();
    }

    pub fn set_target(&mut self, locale: &str, fingerprints: SourceFingerprints) -> bool {
        if self.targets.get(locale) == Some(&fingerprints) {
            return false;
        }
        self.targets.insert(locale.to_owned(), fingerprints);
        true
    }

    pub fn retain_targets<'a>(&mut self, locales: impl IntoIterator<Item = &'a str>) -> bool {
        let locales: BTreeSet<&str> = locales.into_iter().collect();
        let previous_len = self.targets.len();
        self.targets
            .retain(|locale, _| locales.contains(locale.as_str()));
        self.targets.len() != previous_len
    }

    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        self.validate()?;
        let mut contents = serde_json::to_vec_pretty(self).map_err(StateError::Serialize)?;
        contents.push(b'\n');
        atomic_file::write(path, &contents, false).map_err(|source| StateError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    fn validate(&self) -> Result<(), StateError> {
        if self.version != STATE_VERSION {
            return Err(StateError::UnsupportedVersion(self.version));
        }
        validate_locale(&self.source.locale, "source.locale")?;
        for (locale, fingerprints) in &self.targets {
            validate_locale(locale, "targets 的语言代码")?;
            for (path, fingerprint) in fingerprints {
                if !is_valid_path(path) {
                    return Err(StateError::Invalid(format!(
                        "目标语言 {locale} 包含无效字段路径: {path:?}"
                    )));
                }
                if !is_valid_fingerprint(fingerprint) {
                    return Err(StateError::Invalid(format!(
                        "目标语言 {locale} 的字段 {path} 包含无效 SHA-256 指纹"
                    )));
                }
            }
        }
        Ok(())
    }
}

pub fn state_path(base_dir: &Path) -> PathBuf {
    base_dir.join(STATE_FILE_NAME)
}

pub(crate) fn fingerprint<'a>(
    format: CatalogFormat,
    source: &str,
    description: Option<&str>,
    placeholders: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut hasher = Sha256::new();
    update_part(&mut hasher, b"locale-forge-source-v1");
    update_part(
        &mut hasher,
        match format {
            CatalogFormat::Json => b"json",
            CatalogFormat::Arb => b"arb",
        },
    );
    update_part(&mut hasher, source.as_bytes());
    match description {
        Some(description) => {
            update_part(&mut hasher, b"description:some");
            update_part(&mut hasher, description.as_bytes());
        }
        None => update_part(&mut hasher, b"description:none"),
    }
    for placeholder in placeholders {
        update_part(&mut hasher, placeholder.as_bytes());
    }

    let digest = hasher.finalize();
    let mut output = String::with_capacity(71);
    output.push_str("sha256:");
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn update_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn validate_locale(locale: &str, field: &str) -> Result<(), StateError> {
    if locale.is_empty()
        || !locale
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        || LanguageIdentifier::from_str(&locale.replace('_', "-")).is_err()
    {
        return Err(StateError::Invalid(format!(
            "{field} 不是有效且安全的 BCP-47 语言代码: {locale}"
        )));
    }
    Ok(())
}

fn is_valid_path(path: &str) -> bool {
    path.starts_with('/')
}

fn is_valid_fingerprint(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl From<CatalogFormat> for StateFormat {
    fn from(value: CatalogFormat) -> Self {
        match value {
            CatalogFormat::Json => Self::Json,
            CatalogFormat::Arb => Self::Arb,
        }
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("无法读取状态文件 {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("状态文件 {path} 超过 {limit} 字节限制")]
    TooLarge { path: PathBuf, limit: u64 },
    #[error("状态文件 {path} 不是有效 JSON: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("不支持的状态文件版本: {0}")]
    UnsupportedVersion(u32),
    #[error("状态文件无效: {0}")]
    Invalid(String),
    #[error("无法序列化状态文件: {0}")]
    Serialize(serde_json::Error),
    #[error("无法写入状态文件 {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_state_without_source_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = state_path(directory.path());
        let source_text = "不应写入状态文件";
        let fingerprints = BTreeMap::from([(
            "/home".into(),
            fingerprint(CatalogFormat::Json, source_text, None, std::iter::empty()),
        )]);
        let mut state = TranslationState::new("zh-CN", CatalogFormat::Json);
        state.set_target("en-US", fingerprints);

        state.save(&path).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        let loaded = TranslationState::load(&path).unwrap().unwrap();

        assert!(!contents.contains(source_text));
        assert_eq!(loaded, state);
    }

    #[test]
    fn rejects_unknown_version_without_overwriting_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = state_path(directory.path());
        let contents = r#"{"version":2,"source":{"locale":"zh-CN","format":"json"},"targets":{}}"#;
        fs::write(&path, contents).unwrap();

        let error = TranslationState::load(&path).unwrap_err();

        assert!(matches!(error, StateError::UnsupportedVersion(2)));
        assert_eq!(fs::read_to_string(path).unwrap(), contents);
    }

    #[test]
    fn rejects_invalid_json_without_overwriting_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = state_path(directory.path());
        let contents = "{invalid";
        fs::write(&path, contents).unwrap();

        let error = TranslationState::load(&path).unwrap_err();

        assert!(matches!(error, StateError::Parse { .. }));
        assert_eq!(fs::read_to_string(path).unwrap(), contents);
    }

    #[test]
    fn rejects_invalid_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let path = state_path(directory.path());
        fs::write(
            &path,
            r#"{"version":1,"source":{"locale":"zh-CN","format":"json"},"targets":{"en":{"/home":"sha256:not-a-hash"}}}"#,
        )
        .unwrap();

        let error = TranslationState::load(&path).unwrap_err();

        assert!(error.to_string().contains("无效 SHA-256"));
    }

    #[test]
    fn reset_source_clears_old_target_baselines() {
        let mut state = TranslationState::new("zh-CN", CatalogFormat::Json);
        state.set_target("en", BTreeMap::new());

        state.reset_source("zh-CN", CatalogFormat::Arb);

        assert!(state.matches_source("zh-CN", CatalogFormat::Arb));
        assert!(state.target("en").is_none());
    }

    #[test]
    fn atomically_replaces_existing_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = state_path(directory.path());
        let mut state = TranslationState::new("zh-CN", CatalogFormat::Json);
        state.set_target("en", BTreeMap::new());
        state.save(&path).unwrap();

        state.set_target(
            "ja",
            BTreeMap::from([(
                "/home".into(),
                fingerprint(CatalogFormat::Json, "首页", None, std::iter::empty()),
            )]),
        );
        state.save(&path).unwrap();

        let loaded = TranslationState::load(&path).unwrap().unwrap();
        assert!(loaded.target("en").is_some());
        assert!(loaded.target("ja").is_some());
    }

    #[test]
    fn rejects_oversized_state_before_reading_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = state_path(directory.path());
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_STATE_SIZE + 1).unwrap();

        let error = TranslationState::load(&path).unwrap_err();

        assert!(matches!(error, StateError::TooLarge { .. }));
    }
}
