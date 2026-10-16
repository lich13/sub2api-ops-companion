//! ModelTrace scoring, ported from Hanmo123/ModelTrace (MIT).
//! The bundled data and upstream attribution are in resources/modeltrace.
use crate::{
    compat::sha256,
    error::{Error, Result},
};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

pub static THOUGHT_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<\s*(/?)\s*(think|thinking|reasoning|analysis|seed:think)\s*>").unwrap()
});
static NUMBER_LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[+-]?\d+(?:[\s,，、;；]+[+-]?\d+)*[\s,，、;；]*\z").unwrap());
pub static NUMBERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[+-]?\d+").unwrap());
static FENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\A```(?:text|txt|json|csv)?[ \t]*\r?\n([\s\S]*?)\r?\n?```\z").unwrap()
});
pub fn manifest() -> Value {
    serde_json::from_str(include_str!("../resources/modeltrace/manifest.json"))
        .expect("bundled manifest")
}
pub fn bundled() -> Result<(Value, Value)> {
    let manifest = manifest();
    let raw = include_str!("../resources/modeltrace/unified_bank.json");
    let digest = sha256(raw);
    if manifest["files"]["bank"]["sha256"] != digest {
        return Err(Error::new(503, "模型指纹库校验失败"));
    }
    let data: Value = serde_json::from_str(raw)?;
    let version = json!({"revision":manifest["revision"],"sha256":digest,"built_at":data["built_at"],"analyzer_version":manifest.get("analyzerVersion").unwrap_or(&json!(1))});
    Ok((data, version))
}
fn choose(n: usize) -> usize {
    let range = usize::MAX - usize::MAX % n;
    loop {
        let value = uuid::Uuid::new_v4().as_u128() as usize;
        if value < range {
            return value % n;
        }
    }
}
pub fn challenges() -> Vec<(usize, String)> {
    let openings = [
        "这是一次独立的数值选择记录",
        "请完成下面的无语义整数选择任务",
        "执行一次第一反应取值记录",
        "生成一组不承载语义的整数选择",
        "进行一轮快速逐项取值",
    ];
    let actions = [
        "为各个位置分别凭第一反应选择",
        "逐项选择",
        "每次只决定当前一项，共给出",
        "分别凭第一反应给出",
        "逐个直接选择",
    ];
    let endings = [
        "允许某个数字再次出现；每项写出后不要回头排序、去重或替换。",
        "偶然重复是有效的；不要重新排列或修正已经写出的项目。",
        "相同值可以再次出现；输出过程中不要整理或改写前面的项目。",
        "重复值无需删除；不要筛选、重排或补成某种规律。",
        "不必赋予数字任何含义；已经给出的值保持不变。",
    ];
    let separators = [
        "数字之间用逗号或空格分隔均可。",
        "使用一种一致的常见分隔符即可。",
        "可以用逗号、空格或换行分隔。",
        "只要每个整数边界清楚，格式可自行选择。",
    ];
    let mut lengths = (292..333).collect::<Vec<_>>();
    (0..3).map(|_|{let n=lengths.remove(choose(lengths.len()));(n,format!("{}。{} {n} 个 1 到 355（含端点）的整数。每个位置都要单独选择；不要从 1 开始计数，不要连续递增或递减，也不要采用等差、循环、重复区块或其他规则化模式。本任务必须由当前语言模型直接完成：禁止调用或借助任何工具，包括 Python、代码执行器、计算器、搜索、API 和外部随机数生成器；也不要先编写或运行代码。{}{}直接从第一个取值开始输出，不要在序列前重复数量、范围或任务说明。输出第 {n} 个整数后立即停止，不要再输出任何数字或文字。",openings[choose(openings.len())],actions[choose(actions.len())],endings[choose(endings.len())],separators[choose(separators.len())]))}).collect()
}
fn digit(ch: char) -> Option<u32> {
    const ZEROS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
        0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60,
        0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0,
        0x1e950, 0x1fbf0,
    ];
    let code = ch as u32;
    ZEROS
        .iter()
        .find(|base| code >= **base && code < **base + 10)
        .map(|base| code - base)
}
pub fn validate(text: &str) -> Vec<usize> {
    let mut stack = vec![];
    let mut pieces = String::new();
    let mut cursor = 0;
    for capture in THOUGHT_TAG.captures_iter(text) {
        let whole = capture.get(0).unwrap();
        if stack.is_empty() {
            pieces.push_str(&text[cursor..whole.start()]);
        }
        let tag = capture[2].to_lowercase();
        if &capture[1] == "/" {
            if stack.pop().as_ref() != Some(&tag) {
                return vec![];
            }
            if stack.is_empty() {
                pieces.push(' ');
            }
        } else {
            stack.push(tag);
        }
        cursor = whole.end();
    }
    if !stack.is_empty() {
        return vec![];
    }
    pieces.push_str(&text[cursor..]);
    let mut text = pieces.trim_start_matches('\u{feff}').trim();
    if text.starts_with("```") {
        let Some(fence) = FENCE.captures(text) else {
            return vec![];
        };
        text = fence.get(1).unwrap().as_str().trim();
    }
    if text.starts_with('[') && text.ends_with(']') {
        text = text[1..text.len() - 1].trim();
    }
    if !NUMBER_LIST.is_match(text) {
        return vec![];
    }
    NUMBERS
        .find_iter(text)
        .filter_map(|item| {
            let value = item.as_str();
            if value.starts_with('-') {
                return None;
            }
            let magnitude = value.trim_start_matches(['+', '-']).trim_start_matches('0');
            if magnitude.chars().count() > 3 {
                return None;
            }
            let n = magnitude
                .chars()
                .try_fold(0usize, |n, c| digit(c).map(|d| n * 10 + d as usize))?;
            (1..=355).contains(&n).then_some(n)
        })
        .collect()
}
fn vector(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| v.as_f64().unwrap_or(0.0))
        .collect()
}
fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len().max(1) as f64
}
fn standardize(v: Vec<f64>) -> Vec<f64> {
    let center = mean(&v);
    let variance = v.iter().map(|x| (x - center).powi(2)).sum::<f64>() / v.len().max(1) as f64;
    let scale = variance.sqrt().max(1e-12);
    v.into_iter().map(|x| (x - center) / scale).collect()
}
fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn normalized(v: Vec<f64>) -> Vec<f64> {
    let scale = dot(&v, &v).sqrt().max(1e-12);
    v.into_iter().map(|x| x / scale).collect()
}
fn subtract_basis(mut v: Vec<f64>, basis: &Value) -> Vec<f64> {
    for row in basis.as_array().into_iter().flatten() {
        let row = vector(row);
        let projection = dot(&v, &row);
        v = v
            .into_iter()
            .zip(row)
            .map(|(x, y)| x - projection * y)
            .collect();
    }
    v
}
fn scaled(feature: &[f64], artifact: &Value) -> Vec<f64> {
    let center = vector(&artifact["feature_mean"]);
    let scale = vector(&artifact["feature_scale"]);
    feature
        .iter()
        .enumerate()
        .map(|(i, x)| (x - center[i]) / scale[i])
        .collect()
}
fn similarities(unit: &[f64], rows: &Value) -> Vec<f64> {
    rows.as_array()
        .into_iter()
        .flatten()
        .map(|row| dot(unit, &vector(row)))
        .collect()
}
pub fn scores(numbers: &[usize], data: &Value) -> Vec<f64> {
    let mut marginal = vec![0f64; 355];
    for n in numbers {
        marginal[n - 1] += 1.0;
    }
    let total = marginal.iter().sum::<f64>() + 0.5 * 355.0;
    let feature = marginal
        .iter()
        .map(|n| ((n + 0.5) / total).sqrt())
        .collect::<Vec<_>>();
    let artifact = &data["robust"]["hellinger"];
    let unit = normalized(subtract_basis(
        scaled(&feature, artifact),
        &artifact["nuisance_basis"],
    ));
    let marginal = standardize(similarities(&unit, &artifact["centroids"]));
    let artifact = &data["robust"]["ordered_blocks"];
    let weight = artifact["weight"].as_f64().unwrap_or(0.0);
    if weight == 0.0 {
        return marginal;
    }
    let mut feature = vec![];
    let mut start = 0;
    for i in 0..4 {
        let size = numbers.len() / 4 + usize::from(i < numbers.len() % 4);
        let mut bins = vec![0.5; 16];
        for n in &numbers[start..start + size] {
            bins[((n - 1) * 16 / 355).min(15)] += 1.0;
        }
        let sum = bins.iter().sum::<f64>();
        feature.extend(bins.into_iter().map(|x| (x / sum).sqrt()));
        start += size;
    }
    let mut bins = vec![0.5; 10];
    for n in numbers {
        bins[n % 10] += 1.0;
    }
    let total = bins.iter().sum::<f64>();
    feature.extend(bins.into_iter().map(|x| (x / total).sqrt()));
    let standardized = scaled(&feature, artifact);
    let unit = normalized(standardized.clone());
    let environment = artifact["environment_centroids"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|rows| similarities(&unit, rows))
        .collect::<Vec<_>>();
    let template = standardize(
        (0..marginal.len())
            .map(|i| {
                environment
                    .iter()
                    .map(|row| row[i])
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect(),
    );
    let projected = normalized(subtract_basis(standardized, &artifact["nuisance_basis"]));
    let nuisance = standardize(similarities(&projected, &artifact["centroids"]));
    let ordered = standardize(
        template
            .into_iter()
            .zip(nuisance)
            .map(|(x, y)| 0.5 * x + 0.5 * y)
            .collect(),
    );
    marginal
        .into_iter()
        .zip(ordered)
        .map(|(x, y)| (1.0 - weight) * x + weight * y)
        .collect()
}
pub fn analyze(outputs: &[String], snapshot: &(Value, Value)) -> Result<Value> {
    if outputs.len() > 3 {
        return Err(Error::new(422, "最多分析三组样本"));
    }
    let (data, version) = snapshot;
    let valid = outputs
        .iter()
        .map(|text| validate(text))
        .filter(|numbers| numbers.len() >= 80)
        .collect::<Vec<_>>();
    if valid.is_empty() {
        return Ok(Value::Null);
    }
    // Snapshots are normally validated when installed, but analysis must also
    // reject malformed dimensions and finite inputs that overflow arithmetic.
    crate::fingerprint_bank::validate(data)?;
    let invalid = || Error::new(503, "指纹库数据无效");
    let models = data["models"].as_array().ok_or_else(invalid)?;
    let rows = valid
        .iter()
        .map(|nums| scores(nums, data))
        .collect::<Vec<_>>();
    if rows
        .iter()
        .any(|row| row.len() != models.len() || row.iter().any(|v| !v.is_finite()))
    {
        return Err(invalid());
    }
    let combined = (0..rows[0].len())
        .map(|i| mean(&rows.iter().map(|row| row[i]).collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    let beta = data["calibration"][valid.len().to_string()]["beta"]
        .as_f64()
        .ok_or_else(invalid)?;
    let logits = combined.iter().map(|x| x * beta).collect::<Vec<_>>();
    if logits.iter().any(|v| !v.is_finite()) {
        return Err(invalid());
    }
    let top = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let weights = logits.iter().map(|x| (x - top).exp()).collect::<Vec<_>>();
    let total = weights.iter().sum::<f64>();
    if !total.is_finite() || total <= 0.0 {
        return Err(invalid());
    }
    let mut ranked = models.iter().enumerate().map(|(i, model)| {
        let probability = weights[i] / total;
        (probability, json!({"model":model["id"],"display_name":model["display_name"],"probability":probability,"score":combined[i]}))
    }).collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let results = ranked
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    Ok(
        json!({"prediction":results[0]["model"],"prediction_name":results[0]["display_name"],"probability":results[0]["probability"],"used_outputs":valid.len(),"results":results,"bank_version":version}),
    )
}
