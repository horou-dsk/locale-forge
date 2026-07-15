use std::path::Path;

use anyhow::{Result, anyhow};

use super::select_targets;
use crate::{
    catalog::Catalog,
    cli::StateUpdateArgs,
    config::LoadedProjectConfig,
    state::{TranslationState, state_path},
};

pub(super) fn update(config_path: &Path, arguments: StateUpdateArgs) -> Result<u8> {
    let project = LoadedProjectConfig::load(config_path)?;
    let source = Catalog::load(&project.source_path)?;
    let targets = select_targets(&project.config.targets, &arguments.locales)?;
    let path = state_path(&project.base_dir);
    let loaded_state = TranslationState::load(&path)?;
    let had_state = loaded_state.is_some();
    let mut state = loaded_state
        .unwrap_or_else(|| TranslationState::new(&project.config.source.locale, source.format()));
    let mut changed = false;
    if !state.matches_source(&project.config.source.locale, source.format()) {
        state.reset_source(&project.config.source.locale, source.format());
        changed = true;
    }
    let fingerprints = source.source_fingerprints();
    let mut failures = Vec::new();
    let mut accepted = 0usize;

    for target in targets {
        let target_path = project.target_path(target);
        let target_catalog = match Catalog::load_optional(&target_path, source.format()) {
            Ok(Some(catalog)) => catalog,
            Ok(None) => {
                println!("{}: 目标文件不存在，已跳过", target.locale);
                continue;
            }
            Err(error) => {
                failures.push((target.locale.as_str(), anyhow!(error)));
                continue;
            }
        };

        let validation = source
            .diff(Some(&target_catalog), &target.locale, false)
            .and_then(|diff| {
                if let Some(conflict) = diff.report.conflicts.first() {
                    return Err(crate::catalog::CatalogError::MergeTypeConflict(
                        conflict.path.clone(),
                    ));
                }
                source.validate_existing_translations(&target_catalog)
            });
        if let Err(error) = validation {
            failures.push((target.locale.as_str(), anyhow!(error)));
            continue;
        }

        changed |= state.set_target(&target.locale, fingerprints.clone());
        accepted += 1;
        println!("{}: 已接受现有译文状态", target.locale);
    }

    if had_state || accepted > 0 {
        changed |= state.retain_targets(
            project
                .config
                .targets
                .iter()
                .map(|target| target.locale.as_str()),
        );
        if changed {
            state.save(&path)?;
            println!("已更新 {}", path.display());
        } else {
            println!("翻译基线无需更新");
        }
    }

    if failures.is_empty() {
        return Ok(0);
    }
    eprintln!("以下目标语言状态更新失败：");
    for (locale, error) in failures {
        eprintln!("  {locale}: {error:#}");
    }
    Ok(1)
}
