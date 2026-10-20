//! Slow first-token evidence used by capacity detection.
use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    quota::number,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

const NON_TEXT: &[&str] = &[
    "/audio", "/images", "/videos", "-image", "-video", "-tts", "-stt", "realtime", "whisper",
    "dall-e",
];

fn text_stream(row: &Value) -> Option<String> {
    if row["stream"] != true {
        return None;
    }
    let model = if truthy(&row["upstream_model"]) {
        string(&row["upstream_model"])
    } else {
        string(&row["model"])
    }
    .trim();
    if model.is_empty()
        || [
            "image_count",
            "image_output_tokens",
            "video_count",
            "video_duration_seconds",
        ]
        .into_iter()
        .any(|k| number(&row[k]).is_some_and(|v| v != 0.0))
    {
        return None;
    }
    let source = format!("{} {}", string(&row["inbound_endpoint"]), model).to_lowercase();
    if NON_TEXT.iter().any(|v| source.contains(v)) {
        return None;
    }
    Some(model.into())
}
#[derive(Clone)]
pub struct SlowSample {
    pub(crate) id: i64,
    pub(crate) at: DateTime<Utc>,
    pub(crate) first: f64,
}
pub fn slow_sample(row: &Value, account: &Value) -> Option<SlowSample> {
    if account["platform"] != "openai" || !matches!(string(&account["type"]), "oauth" | "apikey") {
        return None;
    }
    text_stream(row)?;
    let first = number(&row["first_token_ms"]).filter(|v| *v > 0.0)?;
    number(&row["duration_ms"]).filter(|v| *v > first)?;
    number(&row["output_tokens"]).filter(|v| *v >= 0.0)?;
    Some(SlowSample {
        id: integer(&row["id"]).unwrap_or(0),
        at: parse_datetime(&row["created_at"])?,
        first,
    })
}
pub fn slow_warning(samples: &[SlowSample], at: DateTime<Utc>) -> Option<Value> {
    let mut rows: Vec<_> = samples
        .iter()
        .filter(|v| v.at >= at - Duration::days(1) && v.at <= at)
        .collect();
    rows.sort_by_key(|v| std::cmp::Reverse((v.at, v.id)));
    rows.truncate(10);
    if rows.len() < 10 {
        return None;
    }
    let slow = rows.iter().filter(|v| v.first > 10_000.0).count();
    Some(
        json!({"kind":"slow_ttft","sample_count":rows.len(),"slow_count":slow,"threshold_ms":10000,"active":slow >= 8 && rows[0].first > 10_000.0,"latest_sample_id":rows[0].id,"latest_first_token_ms":rows[0].first,"latest_at":datetime(rows[0].at)}),
    )
}
