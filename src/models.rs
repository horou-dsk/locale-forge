use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use directories::BaseDirs;
use secrecy::{SecretString, zeroize::Zeroize};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{Host, Url};

use crate::atomic_file;

/// OpenAI 兼容的远程模型发现客户端。
pub(crate) mod remote;

const MAX_MODEL_STORE_SIZE: u64 = 1024 * 1024;

#[derive(Default, Serialize, Deserialize)]
pub struct ModelStore {
    #[serde(default)]
    profiles: BTreeMap<String, StoredModelProfile>,
}

#[derive(Serialize, Deserialize)]
struct StoredModelProfile {
    url: String,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
}

impl Drop for StoredModelProfile {
    fn drop(&mut self) {
        if let Some(api_key) = &mut self.api_key {
            api_key.zeroize();
        }
    }
}

pub struct ModelProfile {
    pub url: Url,
    pub model: String,
    pub api_key: Option<SecretString>,
}

pub struct ModelSummary<'a> {
    pub name: &'a str,
    pub url: &'a str,
    pub model: &'a str,
    pub has_key: bool,
}

pub(crate) struct ModelConnection<'a> {
    pub url: Url,
    pub api_key: Option<&'a str>,
}

impl ModelStore {
    pub fn load(path: &Path) -> Result<Self, ModelStoreError> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ModelStoreError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if metadata.len() > MAX_MODEL_STORE_SIZE {
            return Err(ModelStoreError::Invalid(format!(
                "模型配置文件超过 {} 字节限制",
                MAX_MODEL_STORE_SIZE
            )));
        }

        let mut contents = fs::read_to_string(path).map_err(|source| ModelStoreError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let parsed = serde_json::from_str(&contents);
        contents.zeroize();
        let store: Self = parsed.map_err(|source| ModelStoreError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        for (name, profile) in &store.profiles {
            validate_name(name)?;
            validate_profile(&profile.url, &profile.model, profile.api_key.as_deref())?;
        }
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<(), ModelStoreError> {
        let mut contents = serde_json::to_vec_pretty(self).map_err(ModelStoreError::Serialize)?;
        contents.push(b'\n');
        let result =
            atomic_file::write(path, &contents, true).map_err(|source| ModelStoreError::Write {
                path: path.to_path_buf(),
                source,
            });
        contents.zeroize();
        result
    }

    pub fn set(
        &mut self,
        name: String,
        url: String,
        model: String,
        api_key: Option<String>,
    ) -> Result<(), ModelStoreError> {
        validate_name(&name)?;
        validate_profile(&url, &model, api_key.as_deref())?;
        self.profiles.insert(
            name,
            StoredModelProfile {
                url,
                model,
                api_key,
            },
        );
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> Result<(), ModelStoreError> {
        self.profiles
            .remove(name)
            .map(drop)
            .ok_or_else(|| ModelStoreError::Missing(name.to_owned()))
    }

    /// 仅更新命名配置使用的远程模型 ID，保留接口地址和密钥。
    pub(crate) fn select_model(
        &mut self,
        name: &str,
        model: String,
    ) -> Result<(), ModelStoreError> {
        validate_model(&model)?;
        let profile = self
            .profiles
            .get_mut(name)
            .ok_or_else(|| ModelStoreError::Missing(name.to_owned()))?;
        profile.model = model;
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.profiles.contains_key(name)
    }

    pub fn summaries(&self) -> impl Iterator<Item = ModelSummary<'_>> {
        self.profiles.iter().map(|(name, profile)| ModelSummary {
            name,
            url: &profile.url,
            model: &profile.model,
            has_key: profile.api_key.is_some(),
        })
    }

    pub fn summary(&self, name: &str) -> Result<ModelSummary<'_>, ModelStoreError> {
        let (name, profile) = self
            .profiles
            .get_key_value(name)
            .ok_or_else(|| ModelStoreError::Missing(name.to_owned()))?;
        Ok(ModelSummary {
            name,
            url: &profile.url,
            model: &profile.model,
            has_key: profile.api_key.is_some(),
        })
    }

    pub(crate) fn connection(&self, name: &str) -> Result<ModelConnection<'_>, ModelStoreError> {
        let profile = self
            .profiles
            .get(name)
            .ok_or_else(|| ModelStoreError::Missing(name.to_owned()))?;
        Ok(ModelConnection {
            url: validate_url(&profile.url)?,
            api_key: profile.api_key.as_deref(),
        })
    }

    pub fn into_profile(mut self, name: &str) -> Result<ModelProfile, ModelStoreError> {
        let mut profile = self
            .profiles
            .remove(name)
            .ok_or_else(|| ModelStoreError::Missing(name.to_owned()))?;
        let url = std::mem::take(&mut profile.url);
        let model = std::mem::take(&mut profile.model);
        let api_key = profile.api_key.take();
        Ok(ModelProfile {
            url: validate_url(&url)?,
            model,
            api_key: api_key.map(SecretString::from),
        })
    }
}

pub fn default_model_store_path() -> Result<PathBuf, ModelStoreError> {
    if let Some(path) = env::var_os("LOCALE_FORGE_MODEL_STORE") {
        return Ok(PathBuf::from(path));
    }
    BaseDirs::new()
        .map(|directories| model_store_path(directories.home_dir()))
        .ok_or(ModelStoreError::NoHomeDirectory)
}

fn model_store_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".locale-forge").join("models.json")
}

pub fn validate_url(raw: &str) -> Result<Url, ModelStoreError> {
    let url = Url::parse(raw).map_err(|source| ModelStoreError::InvalidUrl {
        value: raw.to_owned(),
        source,
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ModelStoreError::Invalid(
            "模型 URL 不能包含用户名或密码".into(),
        ));
    }
    if url.fragment().is_some() {
        return Err(ModelStoreError::Invalid(
            "模型 URL 不能包含 fragment".into(),
        ));
    }

    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback(&url) => Ok(url),
        "http" => Err(ModelStoreError::Invalid(
            "HTTP 模型 URL 仅允许 localhost 或回环 IP".into(),
        )),
        _ => Err(ModelStoreError::Invalid(
            "模型 URL 必须使用 HTTPS，或对回环地址使用 HTTP".into(),
        )),
    }
}

fn validate_name(name: &str) -> Result<(), ModelStoreError> {
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(ModelStoreError::Invalid(
            "模型配置名称只能包含字母、数字、点、短横线和下划线".into(),
        ));
    }
    Ok(())
}

fn validate_profile(url: &str, model: &str, api_key: Option<&str>) -> Result<(), ModelStoreError> {
    validate_url(url)?;
    validate_model(model)?;
    if api_key.is_some_and(str::is_empty) {
        return Err(ModelStoreError::Invalid(
            "API key 不能为空；无鉴权服务请使用 --no-key".into(),
        ));
    }
    Ok(())
}

fn validate_model(model: &str) -> Result<(), ModelStoreError> {
    if model.trim().is_empty() {
        return Err(ModelStoreError::Invalid("模型名称不能为空".into()));
    }
    Ok(())
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[derive(Debug, Error)]
pub enum ModelStoreError {
    #[error("无法读取模型配置 {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("模型配置 {path} 不是有效 JSON: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("无法序列化模型配置: {0}")]
    Serialize(serde_json::Error),
    #[error("无法写入模型配置 {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("模型配置无效: {0}")]
    Invalid(String),
    #[error("模型 URL 无效 {value}: {source}")]
    InvalidUrl {
        value: String,
        #[source]
        source: url::ParseError,
    },
    #[error("模型配置不存在: {0}")]
    Missing(String),
    #[error("无法确定当前用户的主目录")]
    NoHomeDirectory,
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn accepts_https_and_loopback_http_only() {
        assert!(validate_url("https://example.com/v1/chat/completions").is_ok());
        assert!(validate_url("http://localhost:11434/v1/chat/completions").is_ok());
        assert!(validate_url("http://127.0.0.1:8080/v1/chat/completions").is_ok());
        assert!(validate_url("http://192.168.1.5/v1/chat/completions").is_err());
    }

    #[test]
    fn stores_models_under_locale_forge_in_home_directory() {
        let path = model_store_path(Path::new("/home/example"));

        assert_eq!(path, Path::new("/home/example/.locale-forge/models.json"));
    }

    #[test]
    fn stores_and_resolves_secret_without_exposing_it_in_summary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.json");
        let mut store = ModelStore::default();
        store
            .set(
                "default".into(),
                "https://example.com/v1/chat/completions".into(),
                "example-model".into(),
                Some("top-secret".into()),
            )
            .unwrap();
        store.save(&path).unwrap();

        let loaded = ModelStore::load(&path).unwrap();
        let summary = loaded.summary("default").unwrap();
        assert!(summary.has_key);
        let profile = loaded.into_profile("default").unwrap();
        assert_eq!(profile.api_key.unwrap().expose_secret(), "top-secret");
    }

    #[test]
    fn supports_explicit_no_key_profile() {
        let mut store = ModelStore::default();
        store
            .set(
                "local".into(),
                "http://[::1]:8080/v1/chat/completions".into(),
                "local-model".into(),
                None,
            )
            .unwrap();

        assert!(!store.summary("local").unwrap().has_key);
    }

    #[test]
    fn selects_model_without_changing_connection_or_key() {
        let mut store = ModelStore::default();
        store
            .set(
                "default".into(),
                "https://example.com/v1/chat/completions".into(),
                "old-model".into(),
                Some("top-secret".into()),
            )
            .unwrap();

        store.select_model("default", "new-model".into()).unwrap();

        let summary = store.summary("default").unwrap();
        assert_eq!(summary.url, "https://example.com/v1/chat/completions");
        assert_eq!(summary.model, "new-model");
        assert!(summary.has_key);
        let profile = store.into_profile("default").unwrap();
        assert_eq!(profile.api_key.unwrap().expose_secret(), "top-secret");
    }
}
