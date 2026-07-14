use std::{collections::BTreeMap, time::Duration};

use async_trait::async_trait;
use thiserror::Error;

use crate::{
    catalog::{Catalog, CatalogError, TranslationUnit},
    config::TargetConfig,
};

pub mod openai;

const MAX_UNIT_BYTES: usize = 256 * 1024;
const MAX_BATCH_BYTES: usize = 512 * 1024;

pub struct ModelRequest<'a> {
    pub source_locale: &'a str,
    pub target: &'a TargetConfig,
    pub units: &'a [TranslationUnit],
    pub correction: Option<&'a str>,
}

#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn translate_batch(
        &self,
        request: ModelRequest<'_>,
    ) -> Result<BTreeMap<String, String>, ModelClientError>;
}

pub struct TranslationAgent<C> {
    client: C,
    batch_size: usize,
    max_retries: usize,
}

impl<C: ModelClient> TranslationAgent<C> {
    pub fn new(client: C, batch_size: usize, max_retries: usize) -> Self {
        Self {
            client,
            batch_size,
            max_retries,
        }
    }

    pub async fn translate(
        &self,
        catalog: &Catalog,
        source_locale: &str,
        target: &TargetConfig,
        units: &[TranslationUnit],
    ) -> Result<BTreeMap<String, String>, AgentError> {
        for unit in units {
            if unit.source.len() > MAX_UNIT_BYTES {
                return Err(AgentError::UnitTooLarge(unit.path.clone()));
            }
        }

        let mut translations = BTreeMap::new();
        let mut start = 0;
        while start < units.len() {
            let end = batch_end(units, start, self.batch_size);
            let batch = &units[start..end];
            let batch_translations = self
                .translate_batch(catalog, source_locale, target, batch)
                .await?;
            translations.extend(batch_translations);
            start = end;
        }
        Ok(translations)
    }

    async fn translate_batch(
        &self,
        catalog: &Catalog,
        source_locale: &str,
        target: &TargetConfig,
        units: &[TranslationUnit],
    ) -> Result<BTreeMap<String, String>, AgentError> {
        let mut correction = None;
        for attempt in 0..=self.max_retries {
            let request = ModelRequest {
                source_locale,
                target,
                units,
                correction: correction.as_deref(),
            };
            match self.client.translate_batch(request).await {
                Ok(response) => match validate_batch(catalog, units, response) {
                    Ok(translations) => return Ok(translations),
                    Err(error) => correction = Some(error.to_string()),
                },
                Err(error) if error.retryable => correction = Some(error.message),
                Err(error) => return Err(AgentError::Client(error.message)),
            }

            if attempt < self.max_retries {
                let multiplier = 1_u64 << attempt.min(5);
                tokio::time::sleep(Duration::from_millis(250 * multiplier)).await;
            }
        }

        Err(AgentError::RetriesExhausted(
            correction.unwrap_or_else(|| "模型未返回有效结果".into()),
        ))
    }
}

fn batch_end(units: &[TranslationUnit], start: usize, batch_size: usize) -> usize {
    let limit = start.saturating_add(batch_size).min(units.len());
    let mut size = 0;
    let mut end = start;
    while end < limit {
        let next_size = size + units[end].source.len();
        if end > start && next_size > MAX_BATCH_BYTES {
            break;
        }
        size = next_size;
        end += 1;
    }
    end.max(start + 1)
}

fn validate_batch(
    catalog: &Catalog,
    units: &[TranslationUnit],
    mut response: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, AgentError> {
    if response.len() != units.len() {
        return Err(AgentError::InvalidBatch(format!(
            "模型返回 {} 个结果，预期 {} 个",
            response.len(),
            units.len()
        )));
    }
    let mut translations = BTreeMap::new();
    for (index, unit) in units.iter().enumerate() {
        let id = format!("t{index}");
        let translation = response
            .remove(&id)
            .ok_or_else(|| AgentError::InvalidBatch(format!("模型缺少结果 {id}")))?;
        catalog.validate_translation(unit, &translation)?;
        translations.insert(unit.path.clone(), translation);
    }
    if let Some(id) = response.keys().next() {
        return Err(AgentError::InvalidBatch(format!("模型返回未知结果 {id}")));
    }
    Ok(translations)
}

#[derive(Debug)]
pub struct ModelClientError {
    pub message: String,
    pub retryable: bool,
}

impl ModelClientError {
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("翻译单元超过大小限制: {0}")]
    UnitTooLarge(String),
    #[error("模型请求失败: {0}")]
    Client(String),
    #[error("模型批次无效: {0}")]
    InvalidBatch(String),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("模型重试次数耗尽: {0}")]
    RetriesExhausted(String),
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;
    use crate::catalog::{CatalogFormat, TranslationKind};

    struct FakeClient {
        responses: Mutex<VecDeque<Result<BTreeMap<String, String>, ModelClientError>>>,
    }

    #[async_trait]
    impl ModelClient for FakeClient {
        async fn translate_batch(
            &self,
            _request: ModelRequest<'_>,
        ) -> Result<BTreeMap<String, String>, ModelClientError> {
            self.responses.lock().unwrap().pop_front().unwrap()
        }
    }

    fn target() -> TargetConfig {
        TargetConfig {
            locale: "en-US".into(),
            language: "English (United States)".into(),
            output: None,
            prompt: None,
        }
    }

    #[tokio::test]
    async fn translates_units_in_batches() {
        let catalog = Catalog::parse(r#"{"a":"甲","b":"乙"}"#, CatalogFormat::Json).unwrap();
        let diff = catalog.diff(None, "en-US", false).unwrap();
        let client = FakeClient {
            responses: Mutex::new(VecDeque::from([
                Ok(BTreeMap::from([("t0".into(), "A".into())])),
                Ok(BTreeMap::from([("t0".into(), "B".into())])),
            ])),
        };
        let agent = TranslationAgent::new(client, 1, 0);

        let result = agent
            .translate(&catalog, "zh-CN", &target(), &diff.units)
            .await
            .unwrap();

        assert_eq!(result["/a"], "A");
        assert_eq!(result["/b"], "B");
    }

    #[tokio::test]
    async fn retries_semantically_invalid_arb_translation() {
        let catalog = Catalog::parse(
            r#"{"hello":"你好 {name}","@hello":{"placeholders":{"name":{}}}}"#,
            CatalogFormat::Arb,
        )
        .unwrap();
        let diff = catalog.diff(None, "en-US", false).unwrap();
        let client = FakeClient {
            responses: Mutex::new(VecDeque::from([
                Ok(BTreeMap::from([("t0".into(), "Hello {user}".into())])),
                Ok(BTreeMap::from([("t0".into(), "Hello {name}".into())])),
            ])),
        };
        let agent = TranslationAgent::new(client, 40, 1);

        let result = agent
            .translate(&catalog, "zh-CN", &target(), &diff.units)
            .await
            .unwrap();

        assert_eq!(result["/hello"], "Hello {name}");
    }

    #[tokio::test]
    async fn retries_retryable_client_failures() {
        let catalog = Catalog::parse(r#"{"home":"首页"}"#, CatalogFormat::Json).unwrap();
        let diff = catalog.diff(None, "en-US", false).unwrap();
        let client = FakeClient {
            responses: Mutex::new(VecDeque::from([
                Err(ModelClientError::retryable("HTTP 429")),
                Ok(BTreeMap::from([("t0".into(), "Home".into())])),
            ])),
        };
        let agent = TranslationAgent::new(client, 40, 1);

        let result = agent
            .translate(&catalog, "zh-CN", &target(), &diff.units)
            .await
            .unwrap();

        assert_eq!(result["/home"], "Home");
    }

    #[test]
    fn splits_batch_by_source_size() {
        let units = [
            TranslationUnit {
                path: "/a".into(),
                source: "a".repeat(300_000),
                description: None,
                placeholders: Vec::new(),
                kind: TranslationKind::Json,
            },
            TranslationUnit {
                path: "/b".into(),
                source: "b".repeat(300_000),
                description: None,
                placeholders: Vec::new(),
                kind: TranslationKind::Json,
            },
        ];

        assert_eq!(batch_end(&units, 0, 40), 1);
    }
}
