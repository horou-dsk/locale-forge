use std::collections::{BTreeMap, BTreeSet};

use formatjs_icu_messageformat_parser::{
    MessageFormatElement, Parser, ParserOptions,
    types::{PluralType, ValidPluralRule},
};
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArbContext {
    pub description: Option<String>,
    pub placeholders: Vec<String>,
}

pub fn validate_document(root: &Map<String, Value>) -> Result<(), ArbError> {
    if root
        .get("@@locale")
        .is_some_and(|locale| !locale.is_string())
    {
        return Err(ArbError::LocaleMustBeString);
    }
    for (key, value) in root {
        if key.starts_with("@@") {
            continue;
        }
        if let Some(message_key) = key.strip_prefix('@') {
            if message_key.is_empty() || !root.contains_key(message_key) {
                return Err(ArbError::InvalidMetadataKey(key.clone()));
            }
            validate_metadata(key, value)?;
            continue;
        }

        let message = value
            .as_str()
            .ok_or_else(|| ArbError::MessageMustBeString(key.clone()))?;
        let ast = parse_message(key, message)?;
        if !message.is_empty()
            && let Some(metadata) = root.get(&format!("@{key}"))
        {
            validate_placeholder_metadata(key, metadata, &ast)?;
        }
    }
    Ok(())
}

pub fn context(root: &Map<String, Value>, key: &str) -> ArbContext {
    let metadata = root.get(&format!("@{key}")).and_then(Value::as_object);
    let description = metadata
        .and_then(|value| value.get("description"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let placeholders = metadata
        .and_then(|value| value.get("placeholders"))
        .and_then(Value::as_object)
        .map(|value| value.keys().cloned().collect())
        .unwrap_or_default();
    ArbContext {
        description,
        placeholders,
    }
}

pub fn validate_translation(key: &str, source: &str, target: &str) -> Result<(), ArbError> {
    let source_ast = parse_message(key, source)?;
    let target_ast = parse_message(key, target)?;

    let source_variables = collect_variables(&source_ast)?;
    let target_variables = collect_variables(&target_ast)?;
    if source_variables != target_variables {
        return Err(ArbError::VariableMismatch {
            key: key.to_owned(),
            source_variables,
            target_variables,
        });
    }

    let mut source_selects = Vec::new();
    let mut target_selects = Vec::new();
    collect_selects(&source_ast, &mut source_selects);
    collect_selects(&target_ast, &mut target_selects);
    source_selects.sort();
    target_selects.sort();
    if source_selects != target_selects {
        return Err(ArbError::SelectMismatch(key.to_owned()));
    }

    let mut source_plurals = Vec::new();
    let mut target_plurals = Vec::new();
    collect_plurals(&source_ast, &mut source_plurals);
    collect_plurals(&target_ast, &mut target_plurals);
    source_plurals.sort();
    target_plurals.sort();
    if source_plurals != target_plurals {
        return Err(ArbError::PluralMismatch(key.to_owned()));
    }

    Ok(())
}

fn parse_message(key: &str, message: &str) -> Result<Vec<MessageFormatElement>, ArbError> {
    Parser::new(
        message,
        ParserOptions {
            ignore_tag: true,
            requires_other_clause: true,
            ..ParserOptions::default()
        },
    )
    .parse()
    .map_err(|source| ArbError::InvalidMessage {
        key: key.to_owned(),
        details: source.to_string(),
    })
}

fn validate_metadata(key: &str, value: &Value) -> Result<(), ArbError> {
    let metadata = value
        .as_object()
        .ok_or_else(|| ArbError::MetadataMustBeObject(key.to_owned()))?;
    if metadata
        .get("description")
        .is_some_and(|description| !description.is_string())
    {
        return Err(ArbError::InvalidDescription(key.to_owned()));
    }
    if metadata
        .get("placeholders")
        .is_some_and(|placeholders| !placeholders.is_object())
    {
        return Err(ArbError::InvalidPlaceholders(key.to_owned()));
    }
    Ok(())
}

fn validate_placeholder_metadata(
    key: &str,
    metadata: &Value,
    ast: &[MessageFormatElement],
) -> Result<(), ArbError> {
    let Some(placeholders) = metadata
        .as_object()
        .and_then(|value| value.get("placeholders"))
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    let metadata_names: BTreeSet<&str> = placeholders.keys().map(String::as_str).collect();
    let variables = collect_variables(ast)?;
    let variable_names: BTreeSet<&str> = variables.keys().map(String::as_str).collect();
    if metadata_names != variable_names {
        return Err(ArbError::PlaceholderMetadataMismatch(key.to_owned()));
    }
    Ok(())
}

fn collect_variables(ast: &[MessageFormatElement]) -> Result<BTreeMap<String, String>, ArbError> {
    let mut variables = BTreeMap::new();
    collect_variables_into(ast, &mut variables)?;
    Ok(variables)
}

fn collect_variables_into(
    ast: &[MessageFormatElement],
    variables: &mut BTreeMap<String, String>,
) -> Result<(), ArbError> {
    for element in ast {
        let variable = match element {
            MessageFormatElement::Argument(value) => Some((&value.value, "argument")),
            MessageFormatElement::Number(value) => Some((&value.value, "number")),
            MessageFormatElement::Date(value) => Some((&value.value, "date")),
            MessageFormatElement::Time(value) => Some((&value.value, "time")),
            MessageFormatElement::Select(value) => Some((&value.value, "select")),
            MessageFormatElement::Plural(value) => Some((&value.value, "plural")),
            MessageFormatElement::Tag(value) => Some((&value.value, "tag")),
            MessageFormatElement::Literal(_) | MessageFormatElement::Pound(_) => None,
        };
        if let Some((name, kind)) = variable
            && let Some(previous) = variables.insert(name.clone(), kind.to_owned())
            && previous != kind
        {
            return Err(ArbError::ConflictingVariableType {
                name: name.clone(),
                first: previous,
                second: kind.to_owned(),
            });
        }
        match element {
            MessageFormatElement::Select(value) => {
                for option in value.options.values() {
                    collect_variables_into(&option.value, variables)?;
                }
            }
            MessageFormatElement::Plural(value) => {
                for option in value.options.values() {
                    collect_variables_into(&option.value, variables)?;
                }
            }
            MessageFormatElement::Tag(value) => {
                collect_variables_into(&value.children, variables)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SelectSignature {
    variable: String,
    keys: BTreeSet<String>,
}

fn collect_selects(ast: &[MessageFormatElement], output: &mut Vec<SelectSignature>) {
    for element in ast {
        match element {
            MessageFormatElement::Select(value) => {
                output.push(SelectSignature {
                    variable: value.value.clone(),
                    keys: value.options.keys().cloned().collect(),
                });
                for option in value.options.values() {
                    collect_selects(&option.value, output);
                }
            }
            MessageFormatElement::Plural(value) => {
                for option in value.options.values() {
                    collect_selects(&option.value, output);
                }
            }
            MessageFormatElement::Tag(value) => collect_selects(&value.children, output),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PluralSignature {
    variable: String,
    plural_type: &'static str,
    offset: i32,
    exact_keys: BTreeSet<String>,
}

fn collect_plurals(ast: &[MessageFormatElement], output: &mut Vec<PluralSignature>) {
    for element in ast {
        match element {
            MessageFormatElement::Plural(value) => {
                output.push(PluralSignature {
                    variable: value.value.clone(),
                    plural_type: match value.plural_type {
                        PluralType::Cardinal => "cardinal",
                        PluralType::Ordinal => "ordinal",
                    },
                    offset: value.offset,
                    exact_keys: value
                        .options
                        .keys()
                        .filter_map(|key| match key {
                            ValidPluralRule::Exact(value) => Some(value.clone()),
                            _ => None,
                        })
                        .collect(),
                });
                for option in value.options.values() {
                    collect_plurals(&option.value, output);
                }
            }
            MessageFormatElement::Select(value) => {
                for option in value.options.values() {
                    collect_plurals(&option.value, output);
                }
            }
            MessageFormatElement::Tag(value) => collect_plurals(&value.children, output),
            _ => {}
        }
    }
}

#[derive(Debug, Error)]
pub enum ArbError {
    #[error("ARB 的 @@locale 必须是字符串")]
    LocaleMustBeString,
    #[error("ARB 消息 {0} 必须是字符串")]
    MessageMustBeString(String),
    #[error("ARB 元数据键无对应消息: {0}")]
    InvalidMetadataKey(String),
    #[error("ARB 元数据 {0} 必须是对象")]
    MetadataMustBeObject(String),
    #[error("ARB 元数据 {0} 的 description 必须是字符串")]
    InvalidDescription(String),
    #[error("ARB 元数据 {0} 的 placeholders 必须是对象")]
    InvalidPlaceholders(String),
    #[error("ARB 消息 {key} 的 ICU 语法无效: {details}")]
    InvalidMessage { key: String, details: String },
    #[error("ARB 消息 {0} 的 placeholders 元数据与实际变量不一致")]
    PlaceholderMetadataMismatch(String),
    #[error("ICU 变量 {name} 同时使用了 {first} 和 {second} 类型")]
    ConflictingVariableType {
        name: String,
        first: String,
        second: String,
    },
    #[error(
        "ARB 翻译 {key} 的变量不一致: source={source_variables:?}, target={target_variables:?}"
    )]
    VariableMismatch {
        key: String,
        source_variables: BTreeMap<String, String>,
        target_variables: BTreeMap<String, String>,
    },
    #[error("ARB 翻译 {0} 的 select 分支不一致")]
    SelectMismatch(String),
    #[error("ARB 翻译 {0} 的复数变量、类型、offset 或精确数字分支不一致")]
    PluralMismatch(String),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn validates_document_metadata_and_icu_message() {
        let document = json!({
            "hello": "Hello {name}",
            "@hello": {
                "description": "Greeting",
                "placeholders": {"name": {"type": "String"}}
            },
            "count": "{count, plural, =0{None} one{One} other{# items}}"
        });

        validate_document(document.as_object().unwrap()).unwrap();
    }

    #[test]
    fn allows_locale_specific_plural_categories() {
        validate_translation(
            "count",
            "{count, plural, =0{None} one{One} other{# items}}",
            "{count, plural, =0{Нет} one{# элемент} few{# элемента} many{# элементов} other{# элемента}}",
        )
        .unwrap();
    }

    #[test]
    fn rejects_changed_exact_plural_branch() {
        let error = validate_translation(
            "count",
            "{count, plural, =0{None} other{# items}}",
            "{count, plural, =1{Один} other{# элементов}}",
        )
        .unwrap_err();

        assert!(matches!(error, ArbError::PluralMismatch(_)));
    }

    #[test]
    fn rejects_changed_select_keys() {
        let error = validate_translation(
            "pronoun",
            "{gender, select, male{he} female{she} other{they}}",
            "{gender, select, man{他} female{她} other{他们}}",
        )
        .unwrap_err();

        assert!(matches!(error, ArbError::SelectMismatch(_)));
    }

    #[test]
    fn rejects_placeholder_metadata_mismatch() {
        let document = json!({
            "hello": "Hello {name}",
            "@hello": {"placeholders": {"user": {"type": "String"}}}
        });

        let error = validate_document(document.as_object().unwrap()).unwrap_err();

        assert!(matches!(error, ArbError::PlaceholderMetadataMismatch(_)));
    }

    #[test]
    fn allows_placeholder_metadata_for_empty_pending_translation() {
        let document = json!({
            "hello": "",
            "@hello": {"placeholders": {"name": {"type": "String"}}}
        });

        validate_document(document.as_object().unwrap()).unwrap();
    }

    #[test]
    fn rejects_non_string_locale_metadata() {
        let document = json!({"@@locale": 42, "hello": "Hello"});

        let error = validate_document(document.as_object().unwrap()).unwrap_err();

        assert!(matches!(error, ArbError::LocaleMustBeString));
    }
}
