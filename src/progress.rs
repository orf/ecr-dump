use indicatif::ProgressStyle;
use tracing::Span;
use tracing_indicatif::span_ext::IndicatifSpanExt;

const PBAR_TEMPLATE: &str = "{span_child_prefix}{span_name} {msg} {percent}% {wide_bar} {per_sec} [{human_pos}/{human_len}]";
const SPINNER_TEMPLATE: &str =
    "{span_child_prefix}{span_name} {spinner} {msg} {human_pos} - {per_sec}";

pub fn set_span_progress(length: usize) -> Span {
    let span = Span::current();
    span.pb_set_style(&ProgressStyle::with_template(PBAR_TEMPLATE).unwrap());
    span.pb_set_length(length as u64);
    span
}

pub fn span_set_spinner() -> Span {
    let span = Span::current();
    span.pb_set_style(&ProgressStyle::with_template(SPINNER_TEMPLATE).unwrap());
    span
}
