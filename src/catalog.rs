use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::state::{SourceFingerprints, fingerprint};

pub mod arb;
mod json;

const MAX_CATALOG_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFormat {
    Json,
    Arb,
}

#[derive(Debug)]
pub struct Catalog {
    format: CatalogFormat,
    root: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationKind {
    Json,
    Arb,
}

#[derive(Debug, PartialEq, Eq)]
pub struct TranslationUnit {
    pub path: String,
    pub source: String,
    pub description: Option<String>,
    pub placeholders: Vec<String>,
    pub kind: TranslationKind,
}

#[derive(Debug)]
pub struct CatalogDiff {
    pub report: DiffReport,
    pub units: Vec<TranslationUnit>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct DiffReport {
    pub locale: String,
    pub missing: Vec<String>,
    pub empty: Vec<String>,
    pub outdated: Vec<String>,
    pub changed: Vec<String>,
    pub extra: Vec<String>,
    pub conflicts: Vec<TypeConflict>,
    pub baseline_missing: bool,
}

impl DiffReport {
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty()
            && self.empty.is_empty()
            && self.outdated.is_empty()
            && self.changed.is_empty()
            && self.extra.is_empty()
            && self.conflicts.is_empty()
            && !self.baseline_missing
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TypeConflict {
    pub path: String,
    pub source_type: &'static str,
    pub target_type: &'static str,
}

impl Catalog {
    pub fn load(path: &Path) -> Result<Self, CatalogError> {
        let format = CatalogFormat::from_path(path)?;
        let metadata = fs::metadata(path).map_err(|source| CatalogError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.len() > MAX_CATALOG_SIZE {
            return Err(CatalogError::TooLarge {
                path: path.to_path_buf(),
                limit: MAX_CATALOG_SIZE,
            });
        }
        let contents = fs::read_to_string(path).map_err(|source| CatalogError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&contents, format).map_err(|error| match error {
            CatalogError::InvalidJson { source, .. } => CatalogError::InvalidJson {
                path: path.to_path_buf(),
                source,
            },
            other => other,
        })
    }

    pub fn load_optional(
        path: &Path,
        expected_format: CatalogFormat,
    ) -> Result<Option<Self>, CatalogError> {
        match fs::metadata(path) {
            Ok(_) => {
                let catalog = Self::load(path)?;
                if catalog.format != expected_format {
                    return Err(CatalogError::FormatMismatch);
                }
                Ok(Some(catalog))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CatalogError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn parse(contents: &str, format: CatalogFormat) -> Result<Self, CatalogError> {
        let root: Value =
            serde_json::from_str(contents).map_err(|source| CatalogError::InvalidJson {
                path: PathBuf::from("<memory>"),
                source,
            })?;
        if format == CatalogFormat::Arb {
            let object = root.as_object().ok_or(CatalogError::ArbRootMustBeObject)?;
            arb::validate_document(object)?;
        }
        Ok(Self { format, root })
    }

    pub fn format(&self) -> CatalogFormat {
        self.format
    }

    pub fn diff(
        &self,
        target: Option<&Catalog>,
        locale: impl Into<String>,
        force: bool,
    ) -> Result<CatalogDiff, CatalogError> {
        self.diff_with_outdated(target, locale, force, &HashSet::new())
    }

    pub(crate) fn diff_with_outdated(
        &self,
        target: Option<&Catalog>,
        locale: impl Into<String>,
        force: bool,
        outdated: &HashSet<&str>,
    ) -> Result<CatalogDiff, CatalogError> {
        if target.is_some_and(|catalog| catalog.format != self.format) {
            return Err(CatalogError::FormatMismatch);
        }
        let mut diff = CatalogDiff {
            report: DiffReport {
                locale: locale.into(),
                missing: Vec::new(),
                empty: Vec::new(),
                outdated: Vec::new(),
                changed: Vec::new(),
                extra: Vec::new(),
                conflicts: Vec::new(),
                baseline_missing: false,
            },
            units: Vec::new(),
        };

        match self.format {
            CatalogFormat::Json => json::diff(
                &self.root,
                target.map(|catalog| &catalog.root),
                "",
                force,
                outdated,
                &mut diff,
            ),
            CatalogFormat::Arb => diff_arb(
                self.root
                    .as_object()
                    .expect("ARB root was validated as an object"),
                target.map(|catalog| {
                    catalog
                        .root
                        .as_object()
                        .expect("ARB root was validated as an object")
                }),
                force,
                outdated,
                &mut diff,
            ),
        }
        Ok(diff)
    }

    pub fn source_fingerprints(&self) -> SourceFingerprints {
        let mut fingerprints = BTreeMap::new();
        match self.format {
            CatalogFormat::Json => json::fingerprints(&self.root, "", &mut fingerprints),
            CatalogFormat::Arb => {
                let source = self
                    .root
                    .as_object()
                    .expect("ARB root was validated as an object");
                for (key, value) in source {
                    if key.starts_with('@') {
                        continue;
                    }
                    let context = arb::context(source, key);
                    fingerprints.insert(
                        pointer_child("", key),
                        fingerprint(
                            CatalogFormat::Arb,
                            value
                                .as_str()
                                .expect("ARB messages were validated as strings"),
                            context.description.as_deref(),
                            context.placeholders.iter().map(String::as_str),
                        ),
                    );
                }
            }
        }
        fingerprints
    }

    pub fn validate_existing_translations(&self, target: &Catalog) -> Result<(), CatalogError> {
        self.validate_existing_translations_except(target, &HashSet::new())
    }

    pub(crate) fn validate_existing_translations_except(
        &self,
        target: &Catalog,
        ignored_paths: &HashSet<&str>,
    ) -> Result<(), CatalogError> {
        if target.format != self.format {
            return Err(CatalogError::FormatMismatch);
        }
        if self.format != CatalogFormat::Arb {
            return Ok(());
        }
        let source = self
            .root
            .as_object()
            .expect("ARB root was validated as an object");
        let target = target
            .root
            .as_object()
            .expect("ARB root was validated as an object");
        for (key, source_value) in source {
            if key.starts_with('@') {
                continue;
            }
            let path = pointer_child("", key);
            if ignored_paths.contains(path.as_str()) {
                continue;
            }
            let Some(target_text) = target.get(key).and_then(Value::as_str) else {
                continue;
            };
            if target_text.is_empty() {
                continue;
            }
            arb::validate_translation(
                key,
                source_value
                    .as_str()
                    .expect("ARB messages were validated as strings"),
                target_text,
            )?;
        }
        Ok(())
    }

    pub fn validate_translation(
        &self,
        unit: &TranslationUnit,
        translation: &str,
    ) -> Result<(), CatalogError> {
        if translation.is_empty() {
            return Err(CatalogError::EmptyTranslation(unit.path.clone()));
        }
        if unit.kind == TranslationKind::Arb {
            arb::validate_translation(
                unit.path.trim_start_matches('/'),
                &unit.source,
                translation,
            )?;
        }
        Ok(())
    }

    pub fn merge(
        &self,
        target: Option<Catalog>,
        mut translations: BTreeMap<String, String>,
        target_locale: &str,
    ) -> Result<Self, CatalogError> {
        if target
            .as_ref()
            .is_some_and(|catalog| catalog.format != self.format)
        {
            return Err(CatalogError::FormatMismatch);
        }
        let target_root = target.map(|catalog| catalog.root);
        let root = match self.format {
            CatalogFormat::Json => json::merge(&self.root, target_root, "", &mut translations)?,
            CatalogFormat::Arb => {
                let target = match target_root {
                    Some(Value::Object(map)) => Some(map),
                    None => None,
                    Some(_) => unreachable!("ARB root was validated as an object"),
                };
                Value::Object(merge_arb(
                    self.root
                        .as_object()
                        .expect("ARB root was validated as an object"),
                    target,
                    &mut translations,
                    target_locale,
                )?)
            }
        };
        if let Some(path) = translations.keys().next() {
            return Err(CatalogError::UnknownTranslation(path.clone()));
        }
        Ok(Self {
            format: self.format,
            root,
        })
    }

    pub fn to_pretty_bytes(&self) -> Result<Vec<u8>, CatalogError> {
        let mut contents =
            serde_json::to_vec_pretty(&self.root).map_err(CatalogError::Serialize)?;
        contents.push(b'\n');
        Ok(contents)
    }
}

impl CatalogFormat {
    pub fn from_path(path: &Path) -> Result<Self, CatalogError> {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("json") => Ok(Self::Json),
            Some("arb") => Ok(Self::Arb),
            _ => Err(CatalogError::UnsupportedFormat(path.to_path_buf())),
        }
    }
}

fn diff_arb(
    source: &Map<String, Value>,
    target: Option<&Map<String, Value>>,
    force: bool,
    outdated: &HashSet<&str>,
    diff: &mut CatalogDiff,
) {
    if target
        .and_then(|map| map.get("@@locale"))
        .and_then(Value::as_str)
        != Some(diff.report.locale.as_str())
    {
        diff.report.changed.push("/@@locale".into());
    }
    for (key, value) in source {
        if key.starts_with("@@") {
            if key != "@@locale" && target.is_some_and(|target| !target.contains_key(key)) {
                diff.report.changed.push(pointer_child("", key));
            }
            continue;
        }
        if key.starts_with('@') {
            if target.and_then(|map| map.get(key)) != Some(value) {
                diff.report.changed.push(pointer_child("", key));
            }
            continue;
        }
        let source_text = value
            .as_str()
            .expect("ARB messages were validated as strings");
        let path = pointer_child("", key);
        match target.and_then(|map| map.get(key)) {
            None => {
                diff.report.missing.push(path.clone());
                push_arb_unit(source, key, source_text, path, diff);
            }
            Some(Value::String(target_text))
                if target_text.is_empty() && !source_text.is_empty() =>
            {
                diff.report.empty.push(path.clone());
                push_arb_unit(source, key, source_text, path, diff);
            }
            Some(Value::String(_)) if force && !source_text.is_empty() => {
                push_arb_unit(source, key, source_text, path, diff);
            }
            Some(Value::String(_)) if outdated.contains(path.as_str()) => {
                diff.report.outdated.push(path.clone());
                push_arb_unit(source, key, source_text, path, diff);
            }
            Some(Value::String(target_text))
                if source_text.is_empty() && !target_text.is_empty() =>
            {
                diff.report.changed.push(path);
            }
            Some(Value::String(_)) => {}
            Some(other) => diff.report.conflicts.push(TypeConflict {
                path,
                source_type: "string",
                target_type: value_kind(other),
            }),
        }
    }
    if let Some(target) = target {
        for key in target.keys() {
            if !key.starts_with("@@") && !source.contains_key(key) {
                diff.report.extra.push(pointer_child("", key));
            }
        }
    }
}

fn push_arb_unit(
    source: &Map<String, Value>,
    key: &str,
    source_text: &str,
    path: String,
    diff: &mut CatalogDiff,
) {
    if source_text.is_empty() {
        return;
    }
    let context = arb::context(source, key);
    diff.units.push(TranslationUnit {
        path,
        source: source_text.to_owned(),
        description: context.description,
        placeholders: context.placeholders,
        kind: TranslationKind::Arb,
    });
}

fn merge_arb(
    source: &Map<String, Value>,
    target: Option<Map<String, Value>>,
    translations: &mut BTreeMap<String, String>,
    target_locale: &str,
) -> Result<Map<String, Value>, CatalogError> {
    let mut target = target.unwrap_or_default();
    target.shift_remove("@@locale");
    let mut output = Map::new();
    output.insert("@@locale".into(), Value::String(target_locale.to_owned()));

    for (key, source_value) in source {
        if key == "@@locale" {
            continue;
        }
        let target_value = target.shift_remove(key);
        if key.starts_with("@@") {
            output.insert(
                key.clone(),
                target_value.unwrap_or_else(|| source_value.clone()),
            );
            continue;
        }
        if key.starts_with('@') {
            output.insert(key.clone(), source_value.clone());
            continue;
        }

        let source_text = source_value
            .as_str()
            .expect("ARB messages were validated as strings");
        let path = pointer_child("", key);
        let value = if source_text.is_empty() {
            Value::String(String::new())
        } else if let Some(translation) = translations.remove(&path) {
            Value::String(translation)
        } else if let Some(Value::String(target_text)) = target_value {
            Value::String(target_text)
        } else {
            return Err(CatalogError::MissingTranslation(path));
        };
        output.insert(key.clone(), value);
    }
    for (key, value) in target {
        if key.starts_with("@@") {
            output.insert(key, value);
        }
    }
    arb::validate_document(&output)?;
    Ok(output)
}

fn pointer_child(parent: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{parent}/{escaped}")
}

fn path_label(path: &str) -> String {
    if path.is_empty() {
        "/".into()
    } else {
        path.to_owned()
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("无法读取本地化文件 {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("本地化文件 {path} 不是有效 JSON: {source}")]
    InvalidJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("本地化文件 {path} 超过 {limit} 字节限制")]
    TooLarge { path: PathBuf, limit: u64 },
    #[error("不支持的本地化文件格式: {0}")]
    UnsupportedFormat(PathBuf),
    #[error("源文件与目标文件格式不一致")]
    FormatMismatch,
    #[error("ARB 文件根节点必须是对象")]
    ArbRootMustBeObject,
    #[error(transparent)]
    Arb(#[from] arb::ArbError),
    #[error("翻译结果不能为空: {0}")]
    EmptyTranslation(String),
    #[error("缺少字段的翻译结果: {0}")]
    MissingTranslation(String),
    #[error("翻译结果包含未知字段: {0}")]
    UnknownTranslation(String),
    #[error("合并时发现类型冲突: {0}")]
    MergeTypeConflict(String),
    #[error("无法序列化本地化文件: {0}")]
    Serialize(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_and_merges_nested_json_incrementally() {
        let source = Catalog::parse(
            r#"{"home":"首页","chat":{"list":"列表"},"enabled":true,"items":["一","二"]}"#,
            CatalogFormat::Json,
        )
        .unwrap();
        let target = Catalog::parse(
            r#"{"home":"Home","chat":{"list":""},"legacy":"keep","enabled":true,"items":["One"]}"#,
            CatalogFormat::Json,
        )
        .unwrap();

        let diff = source.diff(Some(&target), "en-US", false).unwrap();

        assert_eq!(diff.report.empty, ["/chat/list"]);
        assert_eq!(diff.report.missing, ["/items/1"]);
        assert_eq!(diff.report.extra, ["/legacy"]);
        assert_eq!(
            diff.units
                .iter()
                .map(|unit| unit.path.as_str())
                .collect::<Vec<_>>(),
            ["/chat/list", "/items/1"]
        );

        let translations = BTreeMap::from([
            ("/chat/list".into(), "List".into()),
            ("/items/1".into(), "Two".into()),
        ]);
        let merged = source.merge(Some(target), translations, "en-US").unwrap();
        let value: Value = serde_json::from_slice(&merged.to_pretty_bytes().unwrap()).unwrap();

        assert_eq!(value["home"], "Home");
        assert_eq!(value["chat"]["list"], "List");
        assert_eq!(value["items"], serde_json::json!(["One", "Two"]));
        assert!(value.get("legacy").is_none());
    }

    #[test]
    fn reports_type_conflict_without_descending() {
        let source = Catalog::parse(r#"{"chat":{"list":"列表"}}"#, CatalogFormat::Json).unwrap();
        let target = Catalog::parse(r#"{"chat":"Chat"}"#, CatalogFormat::Json).unwrap();

        let diff = source.diff(Some(&target), "en-US", false).unwrap();

        assert_eq!(
            diff.report.conflicts,
            [TypeConflict {
                path: "/chat".into(),
                source_type: "object",
                target_type: "string",
            }]
        );
        assert!(diff.units.is_empty());
    }

    #[test]
    fn force_retranslates_non_empty_strings_only() {
        let source = Catalog::parse(r#"{"title":"标题","empty":""}"#, CatalogFormat::Json).unwrap();
        let target =
            Catalog::parse(r#"{"title":"Title","empty":""}"#, CatalogFormat::Json).unwrap();

        let diff = source.diff(Some(&target), "en-US", true).unwrap();

        assert_eq!(diff.units.len(), 1);
        assert_eq!(diff.units[0].path, "/title");
    }

    #[test]
    fn synchronizes_changed_non_string_values_without_model_output() {
        let source = Catalog::parse(r#"{"enabled":true,"limit":10}"#, CatalogFormat::Json).unwrap();
        let target = Catalog::parse(r#"{"enabled":false,"limit":5}"#, CatalogFormat::Json).unwrap();

        let diff = source.diff(Some(&target), "en-US", false).unwrap();

        assert_eq!(diff.report.changed, ["/enabled", "/limit"]);
        assert!(diff.units.is_empty());
        let merged = source
            .merge(Some(target), BTreeMap::new(), "en-US")
            .unwrap();
        let value: Value = serde_json::from_slice(&merged.to_pretty_bytes().unwrap()).unwrap();
        assert_eq!(value["enabled"], true);
        assert_eq!(value["limit"], 10);
    }

    #[test]
    fn merges_arb_metadata_and_target_locale() {
        let source = Catalog::parse(
            r#"{"@@locale":"zh","hello":"你好 {name}","@hello":{"description":"问候","placeholders":{"name":{"type":"String"}}}}"#,
            CatalogFormat::Arb,
        )
        .unwrap();
        let diff = source.diff(None, "en", false).unwrap();
        assert_eq!(diff.units[0].description.as_deref(), Some("问候"));

        source
            .validate_translation(&diff.units[0], "Hello {name}")
            .unwrap();
        let merged = source
            .merge(
                None,
                BTreeMap::from([("/hello".into(), "Hello {name}".into())]),
                "en",
            )
            .unwrap();
        let value: Value = serde_json::from_slice(&merged.to_pretty_bytes().unwrap()).unwrap();

        assert_eq!(value["@@locale"], "en");
        assert_eq!(value["hello"], "Hello {name}");
        assert_eq!(value["@hello"]["description"], "问候");
    }

    #[test]
    fn refuses_to_fill_missing_translation_with_source_text() {
        let source = Catalog::parse(r#"{"home":"首页"}"#, CatalogFormat::Json).unwrap();

        let error = source.merge(None, BTreeMap::new(), "en-US").unwrap_err();

        assert!(matches!(error, CatalogError::MissingTranslation(_)));
    }

    #[test]
    fn retranslates_only_outdated_source_strings() {
        let source =
            Catalog::parse(r#"{"home":"新首页","chat":"聊天"}"#, CatalogFormat::Json).unwrap();
        let target =
            Catalog::parse(r#"{"home":"Old home","chat":"Chat"}"#, CatalogFormat::Json).unwrap();
        let outdated = HashSet::from(["/home"]);

        let diff = source
            .diff_with_outdated(Some(&target), "en-US", false, &outdated)
            .unwrap();

        assert_eq!(diff.report.outdated, ["/home"]);
        assert_eq!(diff.units.len(), 1);
        assert_eq!(diff.units[0].path, "/home");
    }

    #[test]
    fn mirrors_json_structure_and_clears_empty_source_strings() {
        let source = Catalog::parse(r#"{"title":"","items":["一"]}"#, CatalogFormat::Json).unwrap();
        let target = Catalog::parse(
            r#"{"title":"Title","items":["One","Two"],"legacy":"keep"}"#,
            CatalogFormat::Json,
        )
        .unwrap();

        let diff = source.diff(Some(&target), "en-US", false).unwrap();
        assert_eq!(diff.report.changed, ["/title"]);
        assert_eq!(diff.report.extra, ["/items/1", "/legacy"]);

        let merged = source
            .merge(Some(target), BTreeMap::new(), "en-US")
            .unwrap();
        let value: Value = serde_json::from_slice(&merged.to_pretty_bytes().unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({"title": "", "items": ["One"]}));
    }

    #[test]
    fn arb_merge_removes_deleted_messages_and_preserves_global_metadata() {
        let source = Catalog::parse(
            r#"{"@@locale":"zh","hello":"你好","@hello":{"description":"问候"}}"#,
            CatalogFormat::Arb,
        )
        .unwrap();
        let target = Catalog::parse(
            r#"{"@@locale":"en","@@context":"keep","hello":"Hello","@hello":{"description":"old"},"legacy":"Legacy","@legacy":{"description":"remove"}}"#,
            CatalogFormat::Arb,
        )
        .unwrap();

        let diff = source.diff(Some(&target), "en", false).unwrap();
        assert_eq!(diff.report.extra, ["/legacy", "/@legacy"]);
        let merged = source.merge(Some(target), BTreeMap::new(), "en").unwrap();
        let value: Value = serde_json::from_slice(&merged.to_pretty_bytes().unwrap()).unwrap();

        assert_eq!(value["@@context"], "keep");
        assert_eq!(value["hello"], "Hello");
        assert_eq!(value["@hello"]["description"], "问候");
        assert!(value.get("legacy").is_none());
        assert!(value.get("@legacy").is_none());
    }

    #[test]
    fn arb_fingerprint_changes_with_description_or_placeholders() {
        let first = Catalog::parse(
            r#"{"hello":"你好 {name}","@hello":{"description":"问候","placeholders":{"name":{}}}}"#,
            CatalogFormat::Arb,
        )
        .unwrap();
        let changed_description = Catalog::parse(
            r#"{"hello":"你好 {name}","@hello":{"description":"正式问候","placeholders":{"name":{}}}}"#,
            CatalogFormat::Arb,
        )
        .unwrap();

        assert_ne!(
            first.source_fingerprints()["/hello"],
            changed_description.source_fingerprints()["/hello"]
        );
    }

    #[test]
    fn arb_merge_removes_metadata_deleted_from_source_message() {
        let source = Catalog::parse(r#"{"hello":"你好"}"#, CatalogFormat::Arb).unwrap();
        let target = Catalog::parse(
            r#"{"@@locale":"en","hello":"Hello","@hello":{"description":"old"}}"#,
            CatalogFormat::Arb,
        )
        .unwrap();

        let diff = source.diff(Some(&target), "en", false).unwrap();

        assert_eq!(diff.report.extra, ["/@hello"]);
        let merged = source.merge(Some(target), BTreeMap::new(), "en").unwrap();
        let value: Value = serde_json::from_slice(&merged.to_pretty_bytes().unwrap()).unwrap();
        assert!(value.get("@hello").is_none());
    }

    #[test]
    fn arb_merge_copies_new_source_global_metadata_without_overwriting_target_values() {
        let source = Catalog::parse(
            r#"{"@@locale":"zh","@@context":"source","@@new":"copy","hello":"你好"}"#,
            CatalogFormat::Arb,
        )
        .unwrap();
        let target = Catalog::parse(
            r#"{"@@locale":"en","@@context":"target","hello":"Hello"}"#,
            CatalogFormat::Arb,
        )
        .unwrap();

        let diff = source.diff(Some(&target), "en", false).unwrap();
        assert_eq!(diff.report.changed, ["/@@new"]);
        let merged = source.merge(Some(target), BTreeMap::new(), "en").unwrap();
        let value: Value = serde_json::from_slice(&merged.to_pretty_bytes().unwrap()).unwrap();
        assert_eq!(value["@@context"], "target");
        assert_eq!(value["@@new"], "copy");
    }
}
