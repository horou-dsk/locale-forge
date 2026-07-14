use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use assert_cmd::prelude::*;
use locale_forge::{
    config::{ProjectConfig, SourceConfig, TargetConfig, TranslationConfig},
    models::ModelStore,
};
use predicates::prelude::*;
use serde_json::{Value, json};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::method};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_locale-forge"))
}

fn project_config(targets: Vec<TargetConfig>) -> ProjectConfig {
    ProjectConfig {
        source: SourceConfig {
            locale: "zh-CN".into(),
            path: "locales/zh.json".into(),
        },
        output: "locales/{locale}.json".into(),
        model: "test".into(),
        targets,
        translation: TranslationConfig {
            batch_size: 40,
            timeout_seconds: 5,
            max_retries: 0,
        },
    }
}

fn target(locale: &str, language: &str) -> TargetConfig {
    TargetConfig {
        locale: locale.into(),
        language: language.into(),
        output: None,
        prompt: None,
    }
}

#[test]
fn init_creates_config_and_refuses_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    let arguments = [
        "--config",
        config_path.to_str().unwrap(),
        "init",
        "--source",
        "locales/zh.json",
        "--source-locale",
        "zh-CN",
        "--output",
        "locales/{locale}.json",
        "--model",
        "default",
        "--target",
        "en-US",
    ];

    binary().args(arguments).assert().success();
    binary()
        .args(arguments)
        .assert()
        .failure()
        .stderr(predicate::str::contains("无法写入配置文件"));
}

#[test]
fn diff_reports_missing_fields_and_uses_exit_code_two() {
    let directory = tempfile::tempdir().unwrap();
    let locales = directory.path().join("locales");
    fs::create_dir(&locales).unwrap();
    fs::write(
        locales.join("zh.json"),
        r#"{"home":"首页","chat":{"list":"列表"}}"#,
    )
    .unwrap();
    fs::write(locales.join("en-US.json"), r#"{"home":"Home"}"#).unwrap();
    let config_path = directory.path().join("config.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&project_config(vec![target(
            "en-US",
            "English (United States)",
        )]))
        .unwrap(),
    )
    .unwrap();

    binary()
        .args(["--config", config_path.to_str().unwrap(), "diff", "--json"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("/chat/list"));
}

#[test]
fn translate_writes_target_specific_output() {
    let directory = tempfile::tempdir().unwrap();
    let locales = directory.path().join("locales");
    fs::create_dir(&locales).unwrap();
    fs::write(locales.join("zh.json"), "{}").unwrap();
    let config_path = directory.path().join("config.json");
    let mut japanese = target("ja-JP", "Japanese");
    japanese.output = Some("locales/ja.json".into());
    fs::write(
        &config_path,
        serde_json::to_vec(&project_config(vec![japanese])).unwrap(),
    )
    .unwrap();
    let model_store_path = directory.path().join("models.json");
    let mut model_store = ModelStore::default();
    model_store
        .set(
            "test".into(),
            "http://127.0.0.1:9/v1/chat/completions".into(),
            "test-model".into(),
            None,
        )
        .unwrap();
    model_store.save(&model_store_path).unwrap();

    binary()
        .env("LOCALE_FORGE_MODEL_STORE", &model_store_path)
        .args(["--config", config_path.to_str().unwrap(), "translate"])
        .assert()
        .success();

    let target: Value =
        serde_json::from_slice(&fs::read(locales.join("ja.json")).unwrap()).unwrap();
    assert_eq!(target, json!({}));
    assert!(!locales.join("ja-JP.json").exists());
}

struct FirstSuccessThenFailure {
    calls: AtomicUsize,
}

impl Respond for FirstSuccessThenFailure {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "{\"translations\":{\"t0\":\"Home\"}}"}}]
            }))
        } else {
            ResponseTemplate::new(400).set_body_string("structured output unsupported")
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translate_commits_each_locale_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let locales = directory.path().join("locales");
    fs::create_dir(&locales).unwrap();
    fs::write(locales.join("zh.json"), r#"{"home":"首页"}"#).unwrap();
    fs::write(locales.join("ja-JP.json"), r#"{"home":"既存"}"#).unwrap();

    let config_path = directory.path().join("config.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&project_config(vec![
            target("en-US", "English (United States)"),
            target("ja-JP", "Japanese"),
        ]))
        .unwrap(),
    )
    .unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(FirstSuccessThenFailure {
            calls: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&server)
        .await;
    let model_store_path = directory.path().join("models.json");
    let mut model_store = ModelStore::default();
    model_store
        .set("test".into(), server.uri(), "test-model".into(), None)
        .unwrap();
    model_store.save(&model_store_path).unwrap();

    binary()
        .env("LOCALE_FORGE_MODEL_STORE", &model_store_path)
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "translate",
            "--force",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("ja-JP"));

    let english: Value =
        serde_json::from_slice(&fs::read(locales.join("en-US.json")).unwrap()).unwrap();
    let japanese: Value =
        serde_json::from_slice(&fs::read(locales.join("ja-JP.json")).unwrap()).unwrap();
    assert_eq!(english["home"], "Home");
    assert_eq!(japanese["home"], "既存");
}
