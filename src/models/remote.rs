use std::{collections::BTreeMap, time::Duration};

use reqwest::{Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use url::Url;

use super::{ModelConnection, ModelStore, ModelStoreError, validate_url};
use crate::terminal::escape_controls;

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 远程模型列表中的可选择模型。
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct AvailableModel {
    /// 请求模型接口时使用的精确模型 ID。
    pub id: String,
    /// 上游返回的可选模型所有者。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
}

/// 使用命名配置的地址和密钥获取远程模型列表。
pub(crate) async fn fetch_available_models(
    store: &ModelStore,
    name: &str,
    override_url: Option<&str>,
) -> Result<Vec<AvailableModel>, RemoteModelsError> {
    let connection = store.connection(name)?;
    let url = resolve_models_url(&connection, override_url)?;
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("locale-forge/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(RemoteModelsError::BuildClient)?;
    let mut request = client.get(url);
    if let Some(api_key) = connection.api_key {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().await.map_err(RemoteModelsError::Request)?;
    let status = response.status();
    let bytes = read_limited(response).await?;
    if !status.is_success() {
        let mut details: String = String::from_utf8_lossy(&bytes).chars().take(2048).collect();
        if let Some(api_key) = connection.api_key {
            details = details.replace(api_key, "[REDACTED]");
        }
        details = escape_controls(&details).into_owned();
        return Err(RemoteModelsError::Http { status, details });
    }
    let models = parse_models(&bytes)?;
    if connection.api_key.is_some_and(|api_key| {
        models.iter().any(|model| {
            model.id.contains(api_key)
                || model
                    .owned_by
                    .as_deref()
                    .is_some_and(|owned_by| owned_by.contains(api_key))
        })
    }) {
        return Err(RemoteModelsError::SensitiveDataEcho);
    }
    Ok(models)
}

fn resolve_models_url(
    connection: &ModelConnection<'_>,
    override_url: Option<&str>,
) -> Result<Url, RemoteModelsError> {
    if let Some(raw) = override_url {
        let url = validate_url(raw).map_err(RemoteModelsError::InvalidUrl)?;
        if !same_origin(&connection.url, &url) {
            return Err(RemoteModelsError::CrossOrigin);
        }
        return Ok(url);
    }

    let mut segments: Vec<_> = connection
        .url
        .path_segments()
        .ok_or(RemoteModelsError::CannotDeriveUrl)?
        .collect();
    if segments.last() == Some(&"") {
        segments.pop();
    }
    if !segments.ends_with(&["chat", "completions"]) {
        return Err(RemoteModelsError::CannotDeriveUrl);
    }

    let mut url = connection.url.clone();
    url.set_query(None);
    let mut path = url
        .path_segments_mut()
        .map_err(|_| RemoteModelsError::CannotDeriveUrl)?;
    path.pop_if_empty().pop().pop().push("models");
    drop(path);
    Ok(url)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.port_or_known_default() == right.port_or_known_default()
}

async fn read_limited(mut response: reqwest::Response) -> Result<Vec<u8>, RemoteModelsError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(RemoteModelsError::ResponseTooLarge);
    }
    let mut output = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(RemoteModelsError::Request)? {
        if output.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(RemoteModelsError::ResponseTooLarge);
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

fn parse_models(bytes: &[u8]) -> Result<Vec<AvailableModel>, RemoteModelsError> {
    let response: ModelsResponse =
        serde_json::from_slice(bytes).map_err(RemoteModelsError::Parse)?;
    let mut models = BTreeMap::new();
    for model in response.data {
        if model.id.trim().is_empty() || model.id.chars().any(char::is_control) {
            return Err(RemoteModelsError::InvalidModelId);
        }
        models.entry(model.id).or_insert(model.owned_by);
    }
    Ok(models
        .into_iter()
        .map(|(id, owned_by)| AvailableModel { id, owned_by })
        .collect())
}

/// 获取和解析远程模型列表时发生的错误。
#[derive(Debug, thiserror::Error)]
pub(crate) enum RemoteModelsError {
    #[error("无法从模型接口 URL 推导模型列表地址；请使用 --url 指定同源 /v1/models 地址")]
    CannotDeriveUrl,
    #[error("模型列表 URL 无效: {0}")]
    InvalidUrl(ModelStoreError),
    #[error("模型列表 URL 必须与模型接口 URL 同源")]
    CrossOrigin,
    #[error("模型配置错误: {0}")]
    Store(#[from] ModelStoreError),
    #[error("无法创建模型列表 HTTP 客户端: {0}")]
    BuildClient(reqwest::Error),
    #[error("模型列表请求失败: {0}")]
    Request(reqwest::Error),
    #[error("模型列表响应超过 8 MiB 限制")]
    ResponseTooLarge,
    #[error("模型列表接口返回 HTTP {status}: {details}")]
    Http {
        status: reqwest::StatusCode,
        details: String,
    },
    #[error("模型列表响应不是有效的 OpenAI 模型列表: {0}")]
    Parse(serde_json::Error),
    #[error("模型列表响应包含空模型 ID 或控制字符")]
    InvalidModelId,
    #[error("模型列表响应包含敏感凭据，已拒绝输出")]
    SensitiveDataEcho,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<RemoteModel>,
}

#[derive(Deserialize)]
struct RemoteModel {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::*;

    fn connection(url: &str) -> ModelConnection<'_> {
        ModelConnection {
            url: url.parse().unwrap(),
            api_key: None,
        }
    }

    #[test]
    fn derives_models_url_from_chat_completions_path() {
        let chat_connection =
            connection("https://example.com/openai/v1/chat/completions?unsupported=query");

        let url = resolve_models_url(&chat_connection, None).unwrap();

        assert_eq!(url.as_str(), "https://example.com/openai/v1/models");

        let chat_connection = connection("https://example.com/v1/chat/completions/");
        let url = resolve_models_url(&chat_connection, None).unwrap();
        assert_eq!(url.as_str(), "https://example.com/v1/models");
    }

    #[test]
    fn accepts_same_origin_override_and_rejects_cross_origin() {
        let connection = connection("https://example.com/v1/chat/completions");

        let url = resolve_models_url(
            &connection,
            Some("https://example.com/custom/models?group=one"),
        )
        .unwrap();

        assert_eq!(url.as_str(), "https://example.com/custom/models?group=one");
        assert!(resolve_models_url(&connection, Some("https://other.example/v1/models")).is_err());
    }

    #[tokio::test]
    async fn fetches_authenticated_models_sorted_and_deduplicated() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id": "z-model", "owned_by": "provider-z"},
                    {"id": "a-model"},
                    {"id": "z-model", "owned_by": "duplicate"}
                ]
            })))
            .mount(&server)
            .await;
        let mut store = ModelStore::default();
        store
            .set(
                "test".into(),
                format!("{}/v1/chat/completions", server.uri()),
                "old-model".into(),
                Some("secret".into()),
            )
            .unwrap();

        let models = fetch_available_models(&store, "test", None).await.unwrap();

        assert_eq!(
            models,
            [
                AvailableModel {
                    id: "a-model".into(),
                    owned_by: None,
                },
                AvailableModel {
                    id: "z-model".into(),
                    owned_by: Some("provider-z".into()),
                }
            ]
        );
    }

    #[test]
    fn rejects_invalid_model_ids() {
        for response in [
            br#"{"data":[{"id":"  "}]}"#.as_slice(),
            br#"{"data":[{"id":"model\nname"}]}"#.as_slice(),
        ] {
            let error = parse_models(response).unwrap_err();

            assert!(matches!(error, RemoteModelsError::InvalidModelId));
        }
    }

    #[tokio::test]
    async fn omits_auth_without_key_and_rejects_redirects() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", "/redirected-models"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut store = ModelStore::default();
        store
            .set(
                "test".into(),
                format!("{}/v1/chat/completions", server.uri()),
                "old-model".into(),
                None,
            )
            .unwrap();

        let error = fetch_available_models(&store, "test", None)
            .await
            .unwrap_err();

        assert!(matches!(error, RemoteModelsError::Http { status, .. } if status == 302));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].headers.contains_key("authorization"));
    }

    #[tokio::test]
    async fn rejects_malformed_and_oversized_responses() {
        for (response, expected) in [
            (
                ResponseTemplate::new(200).set_body_json(serde_json::json!({})),
                "不是有效的 OpenAI 模型列表",
            ),
            (
                ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
                "超过 8 MiB",
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(response)
                .mount(&server)
                .await;
            let mut store = ModelStore::default();
            store
                .set(
                    "test".into(),
                    format!("{}/v1/chat/completions", server.uri()),
                    "old-model".into(),
                    None,
                )
                .unwrap();

            let error = fetch_available_models(&store, "test", None)
                .await
                .unwrap_err();

            assert!(error.to_string().contains(expected));
        }
    }

    #[tokio::test]
    async fn redacts_key_from_error_and_rejects_echoed_key() {
        for response in [
            ResponseTemplate::new(401).set_body_string("rejected secret"),
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "model-secret"}]
            })),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(response)
                .mount(&server)
                .await;
            let mut store = ModelStore::default();
            store
                .set(
                    "test".into(),
                    format!("{}/v1/chat/completions", server.uri()),
                    "old-model".into(),
                    Some("secret".into()),
                )
                .unwrap();

            let error = fetch_available_models(&store, "test", None)
                .await
                .unwrap_err();

            assert!(!error.to_string().contains("secret"));
        }
    }
}
