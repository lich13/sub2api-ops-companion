//! Read-only quality calculations. Successful calls and error evidence have
//! independent request-ID namespaces and are never joined heuristically.
use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    error_evidence::local_throttle,
    quota::number,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const ERROR_CURVE: &[(f64, f64)] = &[
    (0.0, 100.0),
    (0.005, 95.0),
    (0.01, 90.0),
    (0.03, 75.0),
    (0.05, 60.0),
    (0.1, 35.0),
    (0.2, 0.0),
];
const PERF_CURVE: &[(f64, f64)] = &[
    (0.25, 0.0),
    (0.5, 40.0),
    (0.75, 70.0),
    (1.0, 85.0),
    (1.5, 95.0),
    (2.0, 100.0),
];
const NON_TEXT: &[&str] = &[
    "/audio", "/images", "/videos", "-image", "-video", "-tts", "-stt", "realtime", "whisper",
    "dall-e",
];

pub fn quantile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut data = values.to_vec();
    data.sort_by(f64::total_cmp);
    let index = (data.len() - 1) as f64 * p;
    let lower = index as usize;
    Some(
        data[lower]
            + (data[(lower + 1).min(data.len() - 1)] - data[lower]) * (index - lower as f64),
    )
}
fn interpolate(value: f64, curve: &[(f64, f64)]) -> f64 {
    if value <= curve[0].0 {
        return curve[0].1;
    }
    for pair in curve.windows(2) {
        let [(x, y), (nx, ny)] = pair else {
            unreachable!()
        };
        if value <= *nx {
            return y + (value - x) * (ny - y) / (nx - x);
        }
    }
    curve.last().unwrap().1
}

pub fn failure_cause(row: &Value) -> Option<&'static str> {
    if local_throttle(row) {
        return None;
    }
    let code = number(if truthy(&row["upstream_status_code"]) {
        &row["upstream_status_code"]
    } else {
        &row["status_code"]
    });
    let text = [
        "message",
        "error_message",
        "upstream_error_message",
        "provider_error_code",
        "provider_error_type",
        "error_type",
        "network_error_type",
        "kind",
    ]
    .into_iter()
    .map(|k| string(&row[k]))
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase();
    let has = |values: &[&str]| values.iter().any(|v| text.contains(v));
    if code == Some(499.0)
        || has(&[
            "client canceled",
            "client cancelled",
            "client disconnected",
            "context canceled",
            "context cancelled",
            "客户端取消",
        ])
    {
        return None;
    }
    if has(&[
        "content_policy_violation",
        "content_filter",
        "safety_violation",
        "content policy",
        "内容策略",
        "内容审核拒绝",
    ]) {
        return None;
    }
    if has(&[
        "usage_limit_reached",
        "usage limit has been reached",
        "insufficient_quota",
        "quota_exceeded",
        "quota exhausted",
        "weekly limit reached",
        "monthly limit reached",
        "可用账号额度等待恢复",
        "账号额度已用尽",
        "余额不足",
        "credits exhausted",
    ]) {
        return None;
    }
    if has(&[
        "client api key",
        "客户端 api key",
        "user rate limit",
        "用户请求参数",
    ]) {
        return None;
    }
    if matches!(code, Some(400.0 | 422.0))
        && has(&[
            "invalid_request",
            "invalid parameter",
            "invalid argument",
            "invalid value",
            "context_length_exceeded",
            "maximum context length",
            "missing required",
        ])
    {
        return None;
    }
    if matches!(code, Some(401.0 | 402.0 | 403.0))
        || has(&[
            "invalid_api_key",
            "authentication",
            "unauthorized",
            "token expired",
            "needs_reauth",
        ])
    {
        return Some("auth");
    }
    if matches!(code, Some(408.0 | 504.0))
        || has(&["timeout", "timed out", "deadline exceeded", "超时"])
    {
        return Some("timeout");
    }
    static STREAM: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\bstream(?:ing)?(?:\b|_)").unwrap());
    if STREAM.is_match(&text) && has(&["failed", "interrupt", "eof", "closed", "broken"]) {
        return Some("stream");
    }
    if has(&["connection", "network", "dial tcp", "dns", "eof", "socket"]) {
        return Some("network");
    }
    Some(if code == Some(429.0) {
        "rate"
    } else {
        "upstream"
    })
}

#[derive(Clone)]
pub struct Failure {
    pub account_id: i64,
    pub at: DateTime<Utc>,
    pub cause: &'static str,
    pub request_known: bool,
}
pub fn failures(
    rows: &[Value],
    accounts: &BTreeMap<i64, Value>,
    at: DateTime<Utc>,
) -> Vec<Failure> {
    let mut output: Vec<Failure> = vec![];
    let mut found: HashMap<(i64, String), usize> = HashMap::new();
    let start = at - Duration::days(7);
    for row in rows {
        let detailed = row["upstream_errors"].as_array().filter(|v| !v.is_empty());
        if detailed.is_none()
            && (row["error_owner"] != "provider"
                || !matches!(string(&row["error_phase"]), "upstream" | "account_auth"))
        {
            continue;
        }
        for event in detailed
            .map(|v| v.iter().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![row])
        {
            if !event.is_object() {
                continue;
            }
            let Some(id) = number(&event["account_id"])
                .filter(|v| v.fract() == 0.0)
                .map(|v| v as i64)
            else {
                continue;
            };
            if !accounts.contains_key(&id) {
                continue;
            }
            let Some(cause) = failure_cause(event) else {
                continue;
            };
            let time = number(&event["at_unix_ms"])
                .filter(|v| *v > 0.0)
                .and_then(|v| DateTime::from_timestamp_millis(v as i64))
                .or_else(|| parse_datetime(&row["created_at"]));
            let Some(time) = time.filter(|v| start <= *v && *v <= at) else {
                continue;
            };
            let request = if truthy(&row["request_id"]) {
                &row["request_id"]
            } else {
                &row["client_request_id"]
            };
            let known = truthy(request);
            let key = (
                id,
                if known {
                    crate::compat::text(request)
                } else {
                    format!("log:{}", crate::compat::text(&row["id"]))
                },
            );
            let item = Failure {
                account_id: id,
                at: time,
                cause,
                request_known: known,
            };
            if let Some(index) = found.get(&key).copied() {
                if time < output[index].at {
                    output[index] = item;
                }
            } else {
                found.insert(key, output.len());
                output.push(item);
            }
        }
    }
    output
}

#[derive(Clone)]
pub struct Sample {
    pub account_id: i64,
    pub at: DateTime<Utc>,
    pub cohort: Vec<String>,
    pub ttft: Option<f64>,
    pub tps: Option<f64>,
}
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
pub fn performance(row: &Value, account: &Value) -> Option<Sample> {
    let model = text_stream(row)?;
    if row["billing_mode"] == "image" {
        return None;
    }
    let first = number(&row["first_token_ms"]);
    let duration = number(&row["duration_ms"]).filter(|v| *v > 0.0)?;
    let output = number(&row["output_tokens"]).filter(|v| *v > 0.0)?;
    let mut input = number(&row["input_tokens"]).filter(|v| *v >= 0.0)?;
    let mut known = true;
    for key in ["cache_read_tokens", "cache_creation_tokens"] {
        if row[key].is_null() {
            known = false;
            continue;
        }
        input += number(&row[key]).filter(|v| *v >= 0.0)?;
    }
    let size = if !known {
        "未知"
    } else if input <= 8192.0 {
        "≤8K"
    } else if input <= 32768.0 {
        "8–32K"
    } else if input <= 131072.0 {
        "32–128K"
    } else {
        ">128K"
    };
    let fallback = |key| {
        if truthy(&row[key]) {
            crate::compat::text(&row[key])
        } else {
            "default".into()
        }
    };
    Some(Sample {
        account_id: integer(&account["id"])?,
        at: parse_datetime(&row["created_at"])?,
        cohort: vec![
            string(&account["platform"]).into(),
            model,
            fallback("reasoning_effort"),
            fallback("service_tier"),
            if truthy(&row["openai_ws_mode"]) {
                "websocket"
            } else {
                "sse"
            }
            .into(),
            size.into(),
        ],
        ttft: first
            .filter(|v| *v > 0.0 && *v < duration)
            .map(|v| v / 1000.0),
        tps: if output >= 32.0 && duration >= 500.0 {
            Some(output / (duration / 1000.0))
        } else {
            None
        },
    })
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

#[derive(Clone)]
pub struct Baseline {
    p50: f64,
    tail: f64,
    accounts: usize,
    samples: usize,
}
pub type Baselines = BTreeMap<(Vec<String>, String), Baseline>;
fn metric(sample: &Sample, name: &str) -> Option<f64> {
    if name == "ttft" {
        sample.ttft
    } else {
        sample.tps
    }
}
pub fn build_baselines(samples: &[Sample]) -> Baselines {
    let mut grouped: BTreeMap<Vec<String>, BTreeMap<i64, Vec<&Sample>>> = BTreeMap::new();
    for sample in samples {
        for key in [sample.cohort.clone(), sample.cohort[..5].to_vec()] {
            grouped
                .entry(key)
                .or_default()
                .entry(sample.account_id)
                .or_default()
                .push(sample);
        }
    }
    let mut result = BTreeMap::new();
    for (key, accounts) in grouped {
        for (name, tail) in [("ttft", 0.9), ("tps", 0.1)] {
            let eligible: Vec<Vec<f64>> = accounts
                .values()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|v| metric(v, name))
                        .collect::<Vec<_>>()
                })
                .filter(|v| v.len() >= 10)
                .collect();
            let samples = eligible.iter().map(Vec::len).sum();
            if eligible.len() >= 2 && samples >= 50 {
                result.insert(
                    (key.clone(), name.into()),
                    Baseline {
                        p50: quantile(
                            &eligible
                                .iter()
                                .filter_map(|v| quantile(v, 0.5))
                                .collect::<Vec<_>>(),
                            0.5,
                        )
                        .unwrap(),
                        tail: quantile(
                            &eligible
                                .iter()
                                .filter_map(|v| quantile(v, tail))
                                .collect::<Vec<_>>(),
                            0.5,
                        )
                        .unwrap(),
                        accounts: eligible.len(),
                        samples,
                    },
                );
            }
        }
    }
    result
}

pub fn metric_window(samples: &[Sample], name: &str, baselines: &Baselines) -> Value {
    let rows: Vec<_> = samples
        .iter()
        .filter_map(|s| metric(s, name).map(|v| (s, v)))
        .collect();
    let mut groups: BTreeMap<Vec<String>, Vec<f64>> = BTreeMap::new();
    for (sample, value) in &rows {
        let key = if baselines.contains_key(&(sample.cohort.clone(), name.into())) {
            sample.cohort.clone()
        } else {
            sample.cohort[..5].to_vec()
        };
        if baselines.contains_key(&(key.clone(), name.into())) {
            groups.entry(key).or_default().push(*value);
        }
    }
    let compared: usize = groups.values().map(Vec::len).sum();
    let mut details = vec![];
    let mut score = 0.0;
    let tail = if name == "ttft" { 0.9 } else { 0.1 };
    for (key, values) in groups {
        let base = &baselines[&(key.clone(), name.into())];
        let p50 = quantile(&values, 0.5).unwrap();
        let ptail = quantile(&values, tail).unwrap();
        let (r50, rtail) = if name == "ttft" {
            (base.p50 / p50, base.tail / ptail)
        } else {
            (p50 / base.p50, ptail / base.tail)
        };
        let part = 0.7 * interpolate(r50, PERF_CURVE) + 0.3 * interpolate(rtail, PERF_CURVE);
        score += part * values.len() as f64 / compared as f64;
        details.push(json!({"platform":key[0],"model":key[1],"reasoning_effort":key[2],"service_tier":key[3],"transport":key[4],"input_bucket":key.get(5).map(String::as_str).unwrap_or("全部输入规模"),"samples":values.len(),"p50":p50,"tail":ptail,"baseline_p50":base.p50,"baseline_tail":base.tail,"baseline_accounts":base.accounts,"baseline_samples":base.samples,"score":part}));
    }
    let values: Vec<_> = rows.iter().map(|(_, v)| *v).collect();
    json!({"samples":rows.len(),"compared":compared,"coverage":if rows.is_empty() {0.0} else {compared as f64/rows.len() as f64},"score":if compared > 0 {Some(score)} else {None},"p50":quantile(&values,0.5),"tail":quantile(&values,tail),"cohorts":details})
}
pub fn metric_result(
    samples: &[Sample],
    name: &str,
    baselines: &Baselines,
    recent: DateTime<Utc>,
) -> Value {
    let all = metric_window(samples, name, baselines);
    let current = metric_window(
        &samples
            .iter()
            .filter(|v| v.at >= recent)
            .cloned()
            .collect::<Vec<_>>(),
        name,
        baselines,
    );
    let history = metric_window(
        &samples
            .iter()
            .filter(|v| v.at < recent)
            .cloned()
            .collect::<Vec<_>>(),
        name,
        baselines,
    );
    let mut chosen = all.clone();
    let mut mode = "7d";
    if integer(&current["samples"]).unwrap_or(0) >= 10 {
        chosen = current.clone();
        mode = "24h";
        if current["score"].is_number() && history["score"].is_number() {
            for key in ["score", "coverage", "p50", "tail"] {
                chosen[key] = json!(
                    0.7 * number(&current[key]).unwrap() + 0.3 * number(&history[key]).unwrap()
                );
            }
            mode = "70/30";
        }
    }
    chosen["mode"] = mode.into();
    chosen["samples"] = all["samples"].clone();
    chosen["recent"] = current;
    chosen["history"] = history;
    chosen["all"] = all;
    chosen
}
fn rel_window(success: usize, failures: usize) -> Value {
    let total = success + failures;
    let rate = if total == 0 {
        None
    } else {
        Some(failures as f64 / total as f64)
    };
    json!({"successes":success,"failures":failures,"total":total,"rate":rate,"score":rate.map(|v|interpolate(v,ERROR_CURVE))})
}
pub fn reliability(
    successes: &[DateTime<Utc>],
    errors: &[Failure],
    recent: DateTime<Utc>,
) -> Value {
    let mut all = rel_window(successes.len(), errors.len());
    let current = rel_window(
        successes.iter().filter(|v| **v >= recent).count(),
        errors.iter().filter(|v| v.at >= recent).count(),
    );
    let history = rel_window(
        successes.iter().filter(|v| **v < recent).count(),
        errors.iter().filter(|v| v.at < recent).count(),
    );
    let mut mode = "7d";
    let mut score = all["score"].clone();
    let mut rate = all["rate"].clone();
    if integer(&current["total"]).unwrap_or(0) >= 20 {
        mode = "24h";
        score = current["score"].clone();
        rate = current["rate"].clone();
        if integer(&history["total"]).unwrap_or(0) > 0 {
            mode = "70/30";
            score = json!(0.7 * number(&score).unwrap() + 0.3 * number(&history["score"]).unwrap());
            rate = json!(0.7 * number(&rate).unwrap() + 0.3 * number(&history["rate"]).unwrap());
        }
    }
    let mut causes: BTreeMap<&str, usize> = BTreeMap::new();
    for e in errors {
        *causes.entry(e.cause).or_default() += 1;
    }
    all["score"] = score;
    all["effective_rate"] = rate;
    all["mode"] = mode.into();
    all["recent"] = current;
    all["history"] = history;
    all["causes"] = json!(causes);
    all
}

pub fn calculate(
    accounts: &[Value],
    usage: &[Value],
    errors: &[Value],
    at: DateTime<Utc>,
    baselines: Option<&Baselines>,
) -> (BTreeMap<i64, Value>, Baselines) {
    let live: BTreeMap<i64, Value> = accounts
        .iter()
        .filter(|a| {
            matches!(string(&a["platform"]), "openai" | "grok")
                && matches!(string(&a["type"]), "oauth" | "apikey")
                && !truthy(&a["deleted_at"])
        })
        .filter_map(|v| integer(&v["id"]).map(|id| (id, v.clone())))
        .collect();
    let start = at - Duration::days(7);
    let recent = at - Duration::days(1);
    let mut success_by: BTreeMap<i64, Vec<DateTime<Utc>>> = BTreeMap::new();
    let mut samples_by: BTreeMap<i64, Vec<Sample>> = BTreeMap::new();
    let mut slow_by: BTreeMap<i64, Vec<SlowSample>> = BTreeMap::new();
    let mut seen = HashSet::new();
    for row in usage {
        let Some(id) = integer(&row["account_id"]) else {
            continue;
        };
        let Some(account) = live.get(&id) else {
            continue;
        };
        let Some(time) = parse_datetime(&row["created_at"]).filter(|v| *v >= start && *v <= at)
        else {
            continue;
        };
        if !seen.insert(row["id"].to_string()) {
            continue;
        }
        success_by.entry(id).or_default().push(time);
        if let Some(sample) = performance(row, account) {
            samples_by.entry(id).or_default().push(sample);
        }
        if let Some(sample) = slow_sample(row, account) {
            slow_by.entry(id).or_default().push(sample);
        }
    }
    let mut failure_by: BTreeMap<i64, Vec<Failure>> = BTreeMap::new();
    for e in failures(errors, &live, at) {
        failure_by.entry(e.account_id).or_default().push(e);
    }
    let baselines = baselines.cloned().unwrap_or_else(|| {
        build_baselines(&samples_by.values().flatten().cloned().collect::<Vec<_>>())
    });
    let mut output = BTreeMap::new();
    for id in live.keys().copied() {
        let successes = success_by.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        let failed = failure_by.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        let samples = samples_by.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        let mut rel = reliability(successes, failed, recent);
        let mut ttft = metric_result(samples, "ttft", &baselines, recent);
        let mut tps = metric_result(samples, "tps", &baselines, recent);
        let total = integer(&rel["total"]).unwrap_or(0);
        rel["missing_reason"] = if total < 20 {
            json!("insufficient_calls")
        } else {
            Value::Null
        };
        for (metric, absent) in [
            (&mut ttft, "first_token_missing"),
            (&mut tps, "throughput_unavailable"),
        ] {
            let count = integer(&metric["samples"]).unwrap_or(0);
            metric["missing_reason"] = if total < 20 {
                json!("insufficient_calls")
            } else if count == 0 {
                json!(absent)
            } else if count < 10 {
                json!("insufficient_calls")
            } else if number(&metric["coverage"]).unwrap_or(0.0) < 0.6 || metric["score"].is_null()
            {
                json!("no_comparable_baseline")
            } else {
                Value::Null
            };
        }
        let complete = total >= 20
            && [&ttft, &tps].into_iter().all(|m| {
                integer(&m["samples"]).unwrap_or(0) >= 10
                    && number(&m["coverage"]).unwrap_or(0.0) >= 0.6
                    && m["score"].is_number()
            });
        let raw_score = if complete {
            Some(
                0.5 * number(&rel["score"]).unwrap()
                    + 0.25 * number(&ttft["score"]).unwrap()
                    + 0.25 * number(&tps["score"]).unwrap(),
            )
        } else {
            None
        };
        let mut cap = 100;
        let mut reasons: Vec<&str> = vec![];
        let last_success = successes.iter().max().copied().unwrap_or(start);
        let consecutive: Vec<_> = failed
            .iter()
            .filter(|e| {
                e.request_known && e.at >= at - Duration::minutes(15) && e.at > last_success
            })
            .collect();
        if consecutive.len() >= 3 {
            cap = 39;
            reasons.push("连续失败");
            if consecutive.iter().any(|e| e.cause == "auth") {
                reasons.push("认证失败");
            }
        }
        if total >= 20 && number(&rel["effective_rate"]).is_some_and(|v| v >= 0.03) {
            cap = cap.min(if number(&rel["effective_rate"]).unwrap() >= 0.1 {
                59
            } else {
                84
            });
        }
        for (m, label, bad, severe, reverse) in [
            (&ttft, "首字偏慢", 30.0, 60.0, false),
            (&tps, "输出偏慢", 5.0, 2.0, true),
        ] {
            if let Some(p50) = number(&m["p50"]).filter(|p| {
                integer(&m["samples"]).unwrap_or(0) >= 10
                    && if reverse { *p <= bad } else { *p >= bad }
            }) {
                let severity = if reverse {
                    p50 <= severe
                } else {
                    p50 >= severe
                };
                cap = cap.min(if severity { 49 } else { 79 });
                if severity {
                    reasons.push(label);
                }
            }
        }
        let score = raw_score.map(|v| (v + 0.5).floor().min(cap as f64) as i64);
        let grade = if cap < 60 || score.is_some_and(|v| v < 60) {
            "red"
        } else if score.is_some_and(|v| v >= 85) {
            "green"
        } else {
            "yellow"
        };
        let status = if complete {
            "complete"
        } else if total > 0 {
            "insufficient"
        } else {
            "empty"
        };
        let mut losses: Vec<(f64, &str)> = vec![];
        if let Some(score) = number(&rel["score"]) {
            let dominant = failed
                .iter()
                .max_by_key(|e| {
                    (
                        integer(&rel["causes"][e.cause]).unwrap_or(0),
                        std::cmp::Reverse(
                            failed.iter().position(|v| v.cause == e.cause).unwrap_or(0),
                        ),
                    )
                })
                .map(|v| v.cause)
                .unwrap_or("upstream");
            let label = match dominant {
                "auth" => "认证失败",
                "timeout" => "频繁超时",
                "network" => "连接不稳",
                "stream" => "流式中断",
                "rate" => "频繁限流",
                _ => "错误偏多",
            };
            losses.push(((100.0 - score) * 0.5, label));
        }
        for (m, label) in [(&ttft, "首字偏慢"), (&tps, "输出偏慢")] {
            if let Some(score) = number(&m["score"]) {
                let ratio = number(&m["tail"])
                    .zip(number(&m["p50"]))
                    .filter(|(_, p)| *p != 0.0)
                    .map(|(t, p)| t / p)
                    .unwrap_or(1.0);
                let unstable = if label == "首字偏慢" {
                    ratio >= 3.0
                } else {
                    ratio <= 0.33
                };
                losses.push((
                    (100.0 - score) * 0.25,
                    if unstable { "波动较大" } else { label },
                ));
            }
        }
        let mut unique = HashSet::new();
        reasons.retain(|v| unique.insert(*v));
        reasons.truncate(2);
        if reasons.is_empty() {
            if status != "complete" {
                if cap < 60 {
                    reasons.push("错误偏多");
                } else {
                    for metric in [&rel, &ttft, &tps] {
                        let label = match string(&metric["missing_reason"]) {
                            "insufficient_calls" => "调用不足",
                            "first_token_missing" => "首字未记录",
                            "throughput_unavailable" => "无有效输出样本",
                            "no_comparable_baseline" => "暂无可比基准",
                            _ => continue,
                        };
                        if !reasons.contains(&label) {
                            reasons.push(label);
                        }
                    }
                    reasons.truncate(2);
                }
            } else if grade == "green" {
                reasons.push("表现良好");
            } else {
                losses.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(a.1)));
                for (loss, label) in &losses {
                    if *loss >= 5.0 && !reasons.contains(label) {
                        reasons.push(*label);
                    }
                }
                reasons.truncate(2);
                if reasons.is_empty() {
                    if let Some((_, label)) = losses.first() {
                        reasons.push(label);
                    }
                }
            }
        }
        let warning = slow_warning(slow_by.get(&id).map(Vec::as_slice).unwrap_or(&[]), at)
            .filter(|v| v["active"] == true);
        output.insert(id,json!({"account_id":id,"score":score,"grade":grade,"reasons":reasons,"sample_status":status,"computed_at":datetime(at),"data_status":"fresh","cap":cap,"period":{"start":datetime(start),"end":datetime(at),"recent_start":datetime(recent)},"reliability":rel,"ttft":ttft,"throughput":tps,"consecutive_failures":consecutive.len(),"coverage":number(&ttft["coverage"]).unwrap().min(number(&tps["coverage"]).unwrap()),"warnings":warning.into_iter().collect::<Vec<_>>()}));
    }
    (output, baselines)
}
