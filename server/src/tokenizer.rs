//! Tokenizer posterior and adaptive probe selection, ported from Ikaleio/lm-detector (MIT).
//! The source revision and hashes are recorded in resources/lm-detector/manifest.json.
use crate::error::{Error, Result};
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

#[derive(Deserialize)]
pub struct Probe {
    pub id: String,
    pub text: String,
}
#[derive(Deserialize)]
pub struct Class {
    pub id: String,
    pub lab_name: String,
    pub series_zh: String,
    pub counts: Vec<i64>,
    pub aliases: Vec<String>,
    #[serde(default)]
    pub alternatives: BTreeMap<String, Vec<i64>>,
}
#[derive(Deserialize)]
pub struct ApiModel {
    pattern: String,
    vendor: String,
    class: Option<String>,
}
#[derive(Deserialize)]
pub struct Wrapper {
    prefix: String,
    suffix: String,
}
#[derive(Deserialize)]
struct RawBank {
    schema: String,
    pub generated_at: String,
    pub wrapper: Wrapper,
    pub probes: Vec<Probe>,
    pub classes: Vec<Class>,
    api_models: Vec<ApiModel>,
}
pub struct Bank {
    raw: RawBank,
    values: Vec<Vec<Vec<f64>>>,
    scales: Vec<f64>,
    aliases: Vec<Vec<Regex>>,
    api_patterns: Vec<Regex>,
    roots: Mutex<BTreeMap<(usize, usize, u8), Vec<f64>>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Observation {
    pub probe: Option<String>,
    pub tokens: i64,
}
#[derive(Clone, Serialize)]
pub struct Score {
    pub id: String,
    pub exact: f64,
    pub related: f64,
    pub matched: usize,
    pub compared: usize,
    pub overhead: f64,
}
#[derive(Clone, Serialize)]
pub struct Posterior {
    pub classes: Vec<Score>,
    pub unknown: f64,
    pub alien: f64,
    pub answered: usize,
}
fn invalid() -> Error {
    Error::new(503, "分词器库不兼容或数据无效")
}
fn regex(pattern: &str) -> Result<Regex> {
    fancy_regex::RegexBuilder::new(&format!("(?i){pattern}"))
        .backtrack_limit(10_000)
        .build()
        .map_err(|_| invalid())
}
impl Bank {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let raw: RawBank = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let n = raw.probes.len();
        let ids: BTreeSet<_> = raw.classes.iter().map(|c| &c.id).collect();
        if raw.schema != "tokenizer-bank-v1"
            || !(1..=256).contains(&n)
            || !(2..=256).contains(&raw.classes.len())
            || ids.len() != raw.classes.len()
            || raw
                .probes
                .iter()
                .map(|p| &p.id)
                .collect::<BTreeSet<_>>()
                .len()
                != n
            || raw
                .probes
                .iter()
                .any(|p| p.id.is_empty() || p.text.len() > 65536)
            || raw.classes.iter().any(|c| {
                c.counts.len() != n
                    || c.counts.iter().any(|v| !(0..=65536).contains(v))
                    || c.alternatives.iter().any(|(i, v)| {
                        i.parse::<usize>().map_or(true, |i| i >= n)
                            || v.is_empty()
                            || v.len() > 256
                            || v.iter().any(|v| !(0..=65536).contains(v))
                    })
            })
            || raw
                .api_models
                .iter()
                .any(|m| m.class.as_ref().is_some_and(|id| !ids.contains(id)))
        {
            return Err(invalid());
        }
        let aliases = raw
            .classes
            .iter()
            .map(|c| c.aliases.iter().map(|s| regex(s)).collect())
            .collect::<Result<Vec<Vec<_>>>>()?;
        let api_patterns = raw
            .api_models
            .iter()
            .map(|m| regex(&m.pattern))
            .collect::<Result<Vec<_>>>()?;
        let values = raw
            .classes
            .iter()
            .map(|c| {
                c.counts
                    .iter()
                    .enumerate()
                    .map(|(j, v)| {
                        c.alternatives
                            .get(&j.to_string())
                            .cloned()
                            .unwrap_or_else(|| vec![*v])
                            .into_iter()
                            .map(|v| v as f64)
                            .collect()
                    })
                    .collect()
            })
            .collect();
        let scales = (0..n)
            .map(|j| {
                let mut values = raw.classes.iter().map(|c| c.counts[j]).collect::<Vec<_>>();
                values.sort();
                (-1.0 / (0.15 * values[values.len() / 2] as f64).max(1.5)).exp()
            })
            .collect();
        Ok(Self {
            raw,
            values,
            scales,
            aliases,
            api_patterns,
            roots: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn text(&self, probe: Option<&str>) -> Result<String> {
        let text = match probe {
            None => "",
            Some(id) => {
                &self
                    .raw
                    .probes
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or_else(invalid)?
                    .text
            }
        };
        Ok(format!(
            "{}{text}{}",
            self.raw.wrapper.prefix, self.raw.wrapper.suffix
        ))
    }
    pub fn built_at(&self) -> &str {
        &self.raw.generated_at
    }
    pub fn posterior(&self, observations: &[Observation]) -> Option<Posterior> {
        let baseline = observations.iter().find(|o| o.probe.is_none())?.tokens as f64;
        let rows = observations
            .iter()
            .map(|o| {
                Some((
                    match o.probe.as_ref() {
                        None => None,
                        Some(id) => Some(self.raw.probes.iter().position(|p| &p.id == id)?),
                    },
                    o.tokens as f64,
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        let count = self.raw.classes.len();
        let known = (0..count)
            .map(|c| self.evidence(&rows, c, &[(0.02, 0.4), (0.06, 0.4), (0.15, 0.2)], baseline))
            .collect::<Vec<_>>();
        let related = (0..count)
            .map(|c| self.evidence(&rows, c, &[(0.2, 0.3), (0.4, 0.4), (0.7, 0.3)], baseline))
            .collect::<Vec<_>>();
        let kl = known
            .iter()
            .map(|(v, _)| (0.8 / count as f64).ln() + v)
            .collect::<Vec<_>>();
        let rl = related
            .iter()
            .map(|(v, _)| (0.1 / count as f64).ln() + v)
            .collect::<Vec<_>>();
        let al = 0.1_f64.ln()
            + lse(&(-3..=3)
                .map(|off| {
                    -7.0_f64.ln()
                        + rows
                            .iter()
                            .map(|(j, t)| match j {
                                None => residual(t - baseline - off as f64, 0.02),
                                Some(j) => {
                                    let density = (0..count)
                                        .map(|c| {
                                            geometric(
                                                nearest(
                                                    t - baseline - off as f64,
                                                    &self.values[c][*j],
                                                ),
                                                self.scales[*j],
                                            )
                                            .exp()
                                        })
                                        .sum::<f64>()
                                        / count as f64;
                                    (density + 1e-6).ln()
                                }
                            })
                            .sum::<f64>()
                })
                .collect::<Vec<_>>());
        let total = lse(&kl
            .iter()
            .chain(&rl)
            .copied()
            .chain([al])
            .collect::<Vec<_>>());
        let answered = rows.iter().filter(|(j, _)| j.is_some()).count();
        let mut classes = self
            .raw
            .classes
            .iter()
            .enumerate()
            .map(|(c, item)| Score {
                id: item.id.clone(),
                exact: (kl[c] - total).exp(),
                related: (rl[c] - total).exp(),
                matched: rows
                    .iter()
                    .filter(|(j, t)| {
                        j.is_some_and(|j| nearest(t - known[c].1, &self.values[c][j]) == 0.0)
                    })
                    .count(),
                compared: answered,
                overhead: known[c].1,
            })
            .collect::<Vec<_>>();
        classes.sort_by(|a, b| (b.exact + b.related).total_cmp(&(a.exact + a.related)));
        let alien = (al - total).exp();
        Some(Posterior {
            unknown: alien + classes.iter().map(|s| s.related).sum::<f64>(),
            classes,
            alien,
            answered,
        })
    }
    fn evidence(
        &self,
        rows: &[(Option<usize>, f64)],
        c: usize,
        grid: &[(f64, f64)],
        baseline: f64,
    ) -> (f64, f64) {
        let mut terms = vec![];
        let mut best = (f64::NEG_INFINITY, baseline);
        for off in -3..=3 {
            let offset = baseline + off as f64;
            for &(rate, weight) in grid {
                let log = rows
                    .iter()
                    .map(|(j, t)| {
                        residual(
                            match j {
                                Some(j) => nearest(t - offset, &self.values[c][*j]),
                                None => t - offset,
                            },
                            if j.is_none() { 0.02 } else { rate },
                        )
                    })
                    .sum::<f64>();
                terms.push(-7.0_f64.ln() + weight.ln() + log);
                if log > best.0 {
                    best = (log, offset);
                }
            }
        }
        (lse(&terms), best.1)
    }
    pub fn next(
        &self,
        posterior: Option<&Posterior>,
        asked: &BTreeSet<String>,
        count: usize,
    ) -> Vec<String> {
        // (class index, hypothesis kind, weight, distinct comparison group).
        let mut atoms: Vec<(usize, u8, f64, usize)> = vec![];
        if let Some(p) = posterior {
            for score in p.classes.iter().take(24) {
                let c = self
                    .raw
                    .classes
                    .iter()
                    .position(|c| c.id == score.id)
                    .unwrap();
                atoms.push((c, 0, score.exact, c));
                if score.related > 0.0 {
                    atoms.push((c, 1, score.related, usize::MAX));
                }
            }
            if p.alien > 0.0 {
                atoms.push((0, 2, p.alien, usize::MAX));
            }
        } else {
            for c in 0..self.raw.classes.len() {
                atoms.push((c, 0, 1.0, c));
            }
        }
        let total = atoms
            .iter()
            .map(|a| a.2)
            .sum::<f64>()
            .max(f64::MIN_POSITIVE);
        let mut pairs = vec![];
        for a in 0..atoms.len() {
            for b in a + 1..atoms.len() {
                if atoms[a].3 != atoms[b].3 {
                    pairs.push((a, b, (atoms[a].2 * atoms[b].2).sqrt() / total));
                }
            }
        }
        let candidates = self
            .raw
            .probes
            .iter()
            .enumerate()
            .filter(|(_, p)| !asked.contains(&p.id))
            .map(|(j, p)| {
                let roots = atoms
                    .iter()
                    .map(|(c, k, _, _)| self.roots(j, *c, *k))
                    .collect::<Vec<_>>();
                (
                    p.id.clone(),
                    pairs
                        .iter()
                        .map(|(a, b, _)| {
                            roots[*a]
                                .iter()
                                .zip(&roots[*b])
                                .map(|(a, b)| a * b)
                                .sum::<f64>()
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let mut remaining = vec![1.0; pairs.len()];
        let mut chosen = vec![];
        while chosen.len() < count {
            let mut best: Option<(&String, &Vec<f64>, f64)> = None;
            for (id, bc) in &candidates {
                if chosen.contains(id) {
                    continue;
                }
                let gain = pairs
                    .iter()
                    .enumerate()
                    .map(|(t, (_, _, w))| w * remaining[t] * (1.0 - bc[t]))
                    .sum::<f64>();
                if best.is_none_or(|(_, _, old)| gain > old) {
                    best = Some((id, bc, gain));
                }
            }
            let Some((id, bc, gain)) = best else {
                break;
            };
            if gain < 1e-4 {
                break;
            }
            chosen.push(id.clone());
            for (r, c) in remaining.iter_mut().zip(bc) {
                *r *= c;
            }
        }
        chosen
    }
    fn roots(&self, j: usize, c: usize, kind: u8) -> Vec<f64> {
        let key = (j, c, kind);
        let mut cache = self.roots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = cache.get(&key) {
            return v.clone();
        }
        let low = self.values.iter().map(|v| v[j][0] as i64).min().unwrap() - 24;
        let high = self.values.iter().map(|v| v[j][0] as i64).max().unwrap() + 24;
        let mut raw = (low..=high)
            .map(|y| {
                if kind == 2 {
                    self.values
                        .iter()
                        .map(|v| geometric(y as f64 - v[j][0], self.scales[j]).exp())
                        .sum::<f64>()
                } else {
                    residual(
                        (y as f64 - self.values[c][j][0]).abs().min(2047.0),
                        if kind == 0 { 0.06 } else { 0.4 },
                    )
                    .exp()
                }
            })
            .collect::<Vec<_>>();
        let norm = raw.iter().sum::<f64>();
        for v in &mut raw {
            *v = (*v / norm).sqrt();
        }
        cache.insert(key, raw.clone());
        raw
    }
    pub fn report(&self, p: &Posterior, model: &str) -> Value {
        let best = |related: bool| {
            p.classes
                .iter()
                .reduce(|a, b| {
                    if if related {
                        a.related >= b.related
                    } else {
                        a.exact >= b.exact
                    } {
                        a
                    } else {
                        b
                    }
                })
                .unwrap()
        };
        let exact = best(false);
        let related = best(true);
        let (kind, top, confidence) = if exact.exact >= p.unknown {
            ("exact", exact, exact.exact)
        } else if related.related >= p.alien {
            ("related", related, related.related)
        } else {
            ("unknown", &p.classes[0], p.unknown)
        };
        let model = model.trim().to_ascii_lowercase();
        let model = model
            .rsplit('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
        let expected = self
            .raw
            .api_models
            .iter()
            .zip(&self.api_patterns)
            .find(|(_, re)| re.is_match(model).unwrap_or(false))
            .map(|(m, _)| {
                (
                    m.class.iter().cloned().collect::<Vec<_>>(),
                    m.vendor.clone(),
                    true,
                )
            })
            .or_else(|| {
                let found = self
                    .raw
                    .classes
                    .iter()
                    .zip(&self.aliases)
                    .filter(|(_, rs)| rs.iter().any(|r| r.is_match(model).unwrap_or(false)))
                    .map(|(c, _)| c)
                    .collect::<Vec<_>>();
                found.first().map(|first| {
                    (
                        found.iter().map(|c| c.id.clone()).collect(),
                        first.lab_name.clone(),
                        false,
                    )
                })
            });
        let claim=expected.map(|(ids,vendor,first)|{let probability=if ids.is_empty(){p.unknown}else{p.classes.iter().filter(|c|ids.contains(&c.id)).map(|c|c.exact+if first{0.0}else{c.related}).sum()};
            json!({"expected":ids,"vendor":vendor,"probability":probability,"status":if probability>=0.9{"consistent"}else if probability<=0.1{"inconsistent"}else{"uncertain"}})});
        let candidates=p.classes.iter().take(6).map(|s|{let c=self.raw.classes.iter().find(|c|c.id==s.id).unwrap();json!({"id":s.id,"name":c.series_zh,"lab":c.lab_name,"probability":s.exact+s.related})}).collect::<Vec<_>>();
        let c = self.raw.classes.iter().find(|c| c.id == top.id).unwrap();
        json!({"kind":kind,"name":c.series_zh,"lab":c.lab_name,"confidence":confidence,"claim":claim,"candidates":candidates,"unknown":p.unknown,"answered":p.answered})
    }
}
pub fn done(p: &Posterior) -> bool {
    p.answered >= 20 || p.answered >= 4 && p.classes[0].exact.max(p.unknown) >= 0.99
}
fn lse(values: &[f64]) -> f64 {
    let top = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    top + values.iter().map(|v| (v - top).exp()).sum::<f64>().ln()
}
fn geometric(d: f64, p: f64) -> f64 {
    ((1.0 - p) / (1.0 + p)).ln() + d.abs() * p.ln()
}
fn residual(d: f64, rate: f64) -> f64 {
    lse(&[
        (1.0 - rate).ln() + geometric(d, 0.02),
        rate.ln() + geometric(d, 0.85),
        1e-6_f64.ln(),
    ])
}
fn nearest(delta: f64, values: &[f64]) -> f64 {
    values.iter().fold(f64::INFINITY, |best, v| {
        if (delta - v).abs() < best.abs() {
            delta - v
        } else {
            best
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn close(actual: f64, expected: &Value) {
        assert!(
            (actual - expected.as_f64().unwrap()).abs() < 1e-9,
            "posterior mismatch"
        );
    }
    #[test]
    fn pinned_typescript_posterior_adaptive_probes_and_verdicts_match() {
        let bank = Bank::parse(include_bytes!(
            "../resources/lm-detector/tokenizer_bank.json"
        ))
        .unwrap();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/contracts/lm-detector/tokenizer.json"
        ))
        .unwrap();
        assert_eq!(
            json!(bank.next(None, &BTreeSet::new(), 2)),
            fixture["initial"]
        );
        for case in fixture["cases"].as_array().unwrap() {
            let observations: Vec<Observation> =
                serde_json::from_value(case["observations"].clone()).unwrap();
            let actual = bank.posterior(&observations);
            let asked = observations
                .iter()
                .filter_map(|o| o.probe.clone())
                .collect();
            assert_eq!(
                json!(bank.next(actual.as_ref(), &asked, 3)),
                case["next"],
                "{}",
                case["name"]
            );
            let Some(actual) = actual else {
                assert!(case["posterior"].is_null());
                continue;
            };
            close(actual.unknown, &case["posterior"]["unknown"]);
            close(actual.alien, &case["posterior"]["alien"]);
            assert_eq!(json!(done(&actual)), case["done"]);
            for (a, b) in actual
                .classes
                .iter()
                .zip(case["posterior"]["classes"].as_array().unwrap())
            {
                assert_eq!(a.id, b["id"]);
                close(a.exact, &b["exact"]);
                close(a.related, &b["related"]);
                close(a.overhead, &b["overhead"]);
                assert_eq!(a.matched as u64, b["matched"]);
                assert_eq!(a.compared as u64, b["compared"]);
            }
            let report = bank.report(&actual, "openai/gpt-4o:test");
            assert_eq!(report["kind"], case["verdict"]["kind"]);
            assert_eq!(
                report["claim"]["status"],
                case["verdict"]["claim"]["status"]
            );
            close(
                report["confidence"].as_f64().unwrap(),
                &case["verdict"]["confidence"],
            );
        }
    }
    #[test]
    fn malformed_bank_is_rejected_before_requests() {
        let mut raw: Value = serde_json::from_slice(include_bytes!(
            "../resources/lm-detector/tokenizer_bank.json"
        ))
        .unwrap();
        raw["classes"][0]["counts"] = json!([1]);
        assert!(Bank::parse(&serde_json::to_vec(&raw).unwrap()).is_err());
    }
}
