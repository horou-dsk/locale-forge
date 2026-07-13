use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    CatalogDiff, CatalogError, TranslationKind, TranslationUnit, TypeConflict, path_label,
    pointer_child, value_kind,
};

pub(super) fn diff(
    source: &Value,
    target: Option<&Value>,
    path: &str,
    force: bool,
    catalog_diff: &mut CatalogDiff,
) {
    if let Some(target) = target
        && value_kind(source) != value_kind(target)
    {
        catalog_diff.report.conflicts.push(TypeConflict {
            path: path_label(path),
            source_type: value_kind(source),
            target_type: value_kind(target),
        });
        return;
    }

    match source {
        Value::Object(source_map) => {
            let target_map = target.and_then(Value::as_object);
            for (key, source_value) in source_map {
                let child_path = pointer_child(path, key);
                diff(
                    source_value,
                    target_map.and_then(|map| map.get(key)),
                    &child_path,
                    force,
                    catalog_diff,
                );
            }
            if let Some(target_map) = target_map {
                for key in target_map.keys() {
                    if !source_map.contains_key(key) {
                        catalog_diff.report.extra.push(pointer_child(path, key));
                    }
                }
            }
        }
        Value::Array(source_items) => {
            let target_items = target.and_then(Value::as_array);
            for (index, source_value) in source_items.iter().enumerate() {
                let child_path = pointer_child(path, &index.to_string());
                diff(
                    source_value,
                    target_items.and_then(|items| items.get(index)),
                    &child_path,
                    force,
                    catalog_diff,
                );
            }
            if let Some(target_items) = target_items {
                for index in source_items.len()..target_items.len() {
                    catalog_diff
                        .report
                        .extra
                        .push(pointer_child(path, &index.to_string()));
                }
            }
        }
        Value::String(source_text) => match target.and_then(Value::as_str) {
            None => {
                catalog_diff.report.missing.push(path_label(path));
                push_unit(path, source_text, catalog_diff);
            }
            Some("") if !source_text.is_empty() => {
                catalog_diff.report.empty.push(path_label(path));
                push_unit(path, source_text, catalog_diff);
            }
            Some(_) if force && !source_text.is_empty() => {
                push_unit(path, source_text, catalog_diff);
            }
            Some(_) => {}
        },
        Value::Null | Value::Bool(_) | Value::Number(_) => {
            if target.is_none() {
                catalog_diff.report.missing.push(path_label(path));
            } else if target != Some(source) {
                catalog_diff.report.changed.push(path_label(path));
            }
        }
    }
}

fn push_unit(path: &str, source: &str, diff: &mut CatalogDiff) {
    if !source.is_empty() {
        diff.units.push(TranslationUnit {
            path: path_label(path),
            source: source.to_owned(),
            description: None,
            placeholders: Vec::new(),
            kind: TranslationKind::Json,
        });
    }
}

pub(super) fn merge(
    source: &Value,
    target: Option<Value>,
    path: &str,
    translations: &mut BTreeMap<String, String>,
) -> Result<Value, CatalogError> {
    if let Some(target_value) = target.as_ref()
        && value_kind(source) != value_kind(target_value)
    {
        return Err(CatalogError::MergeTypeConflict(path_label(path)));
    }

    match source {
        Value::Object(source_map) => {
            let mut target_map = match target {
                Some(Value::Object(map)) => map,
                None => Map::new(),
                Some(_) => unreachable!("type conflict was checked above"),
            };
            let mut output = Map::new();
            for (key, source_value) in source_map {
                let target_value = target_map.shift_remove(key);
                let child_path = pointer_child(path, key);
                output.insert(
                    key.clone(),
                    merge(source_value, target_value, &child_path, translations)?,
                );
            }
            output.extend(target_map);
            Ok(Value::Object(output))
        }
        Value::Array(source_items) => {
            let mut target_items = match target {
                Some(Value::Array(items)) => items.into_iter().map(Some).collect::<Vec<_>>(),
                None => Vec::new(),
                Some(_) => unreachable!("type conflict was checked above"),
            };
            let mut output = Vec::with_capacity(source_items.len().max(target_items.len()));
            for (index, source_value) in source_items.iter().enumerate() {
                let target_value = target_items.get_mut(index).and_then(Option::take);
                let child_path = pointer_child(path, &index.to_string());
                output.push(merge(
                    source_value,
                    target_value,
                    &child_path,
                    translations,
                )?);
            }
            output.extend(target_items.into_iter().skip(source_items.len()).flatten());
            Ok(Value::Array(output))
        }
        Value::String(source_text) => {
            let path = path_label(path);
            if let Some(translation) = translations.remove(&path) {
                return Ok(Value::String(translation));
            }
            match target {
                Some(Value::String(target_text)) => Ok(Value::String(target_text)),
                None if source_text.is_empty() => Ok(Value::String(String::new())),
                None => Err(CatalogError::MissingTranslation(path)),
                Some(_) => unreachable!("type conflict was checked above"),
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(source.clone()),
    }
}
