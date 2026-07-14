use std::time::Duration;

/// 翻译任务在批处理过程中的可观察事件。
#[derive(Debug)]
pub enum TranslationProgress<'a> {
    /// 已完成批次规划，即将开始翻译目标语言。
    Started {
        locale: &'a str,
        total_units: usize,
        total_batches: usize,
    },
    /// 即将向模型发送一个批次请求。
    BatchAttemptStarted {
        locale: &'a str,
        batch_index: usize,
        total_batches: usize,
        attempt: usize,
        max_attempts: usize,
        unit_count: usize,
    },
    /// 单次请求或响应校验失败，并说明是否继续重试。
    BatchAttemptFailed {
        locale: &'a str,
        batch_index: usize,
        total_batches: usize,
        attempt: usize,
        max_attempts: usize,
        elapsed: Duration,
        reason: &'a str,
        retry_delay: Option<Duration>,
    },
    /// 一个批次已通过校验并合并到内存结果。
    BatchCompleted {
        locale: &'a str,
        batch_index: usize,
        total_batches: usize,
        completed_units: usize,
        total_units: usize,
        elapsed: Duration,
    },
}

/// 同步接收翻译进度；实现不得阻塞翻译任务。
pub trait TranslationProgressReporter: Send + Sync {
    /// 接收不包含完整请求体或译文的进度事件；错误原因应视为不可信文本。
    fn report(&self, progress: TranslationProgress<'_>);
}

/// 丢弃全部事件，用于不需要进度输出的调用方。
pub struct NoopTranslationProgress;

impl TranslationProgressReporter for NoopTranslationProgress {
    fn report(&self, _progress: TranslationProgress<'_>) {}
}
