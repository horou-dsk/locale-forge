use std::time::Duration;

use crate::{
    agent::progress::{TranslationProgress, TranslationProgressReporter},
    terminal::escape_controls,
};

pub(super) struct ConsoleTranslationProgress;

impl TranslationProgressReporter for ConsoleTranslationProgress {
    fn report(&self, progress: TranslationProgress<'_>) {
        match progress {
            TranslationProgress::Started {
                locale,
                total_units,
                total_batches,
            } => eprintln!("{locale}: 待翻译 {total_units} 个字段，共 {total_batches} 批"),
            TranslationProgress::BatchAttemptStarted {
                locale,
                batch_index,
                total_batches,
                attempt,
                max_attempts,
                unit_count,
            } => eprintln!(
                "{locale}: [{batch_index}/{total_batches}] 第 {attempt}/{max_attempts} 次请求，包含 {unit_count} 个字段"
            ),
            TranslationProgress::BatchAttemptFailed {
                locale,
                batch_index,
                total_batches,
                attempt,
                max_attempts,
                elapsed,
                reason,
                retry_delay,
            } => {
                let elapsed = format_duration(elapsed);
                let reason = escape_controls(reason);
                if let Some(delay) = retry_delay {
                    eprintln!(
                        "{locale}: [{batch_index}/{total_batches}] 第 {attempt}/{max_attempts} 次请求失败（{elapsed}），{} 后重试：{reason}",
                        format_duration(delay)
                    );
                } else {
                    eprintln!(
                        "{locale}: [{batch_index}/{total_batches}] 第 {attempt}/{max_attempts} 次请求失败（{elapsed}），不再重试：{reason}"
                    );
                }
            }
            TranslationProgress::BatchCompleted {
                locale,
                batch_index,
                total_batches,
                completed_units,
                total_units,
                elapsed,
            } => eprintln!(
                "{locale}: [{batch_index}/{total_batches}] 已完成 {completed_units}/{total_units} 个字段，本批耗时 {}",
                format_duration(elapsed)
            ),
        }
    }
}

fn format_duration(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{} 毫秒", duration.as_millis())
    } else {
        format!("{:.1} 秒", duration.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_short_and_long_durations() {
        assert_eq!(format_duration(Duration::from_millis(250)), "250 毫秒");
        assert_eq!(format_duration(Duration::from_millis(1_250)), "1.2 秒");
    }
}
