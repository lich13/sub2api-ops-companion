//! Relations between fields that individual Serde records cannot express.
use serde_json::Value;
pub fn recoveries_valid(data: &Value) -> bool {
    data.as_array().is_some_and(|rows| {
        rows.iter().all(|row| {
            let kind = &row["kind"];
            let receipt = &row["reset_credit"];
            (kind.is_null()
                || matches!(
                    kind.as_str(),
                    Some("quota_recovery" | "reset_credit" | "connection_test")
                ))
                && (receipt.is_null()
                    || receipt.is_object()
                        && receipt["completed_at"]
                            .as_str()
                            .is_some_and(|time| !time.is_empty())
                        && matches!(
                            receipt["verification_method"].as_str(),
                            Some("connection" | "model")
                        )
                        && (receipt["consumed"].is_null()
                            || receipt["consumed"].as_bool() == Some(true)))
                && (kind.as_str() != Some("reset_credit")
                    || !receipt.is_null()
                        && row["legacy"].as_bool() == Some(false)
                        && ["test_completed_at", "recovered_at"]
                            .iter()
                            .all(|key| row[*key].as_str().is_some_and(|time| !time.is_empty())))
        })
    })
}

fn date_ymd_valid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || ![0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
            .into_iter()
            .all(|i| b[i].is_ascii_digit())
    {
        return false;
    }
    let year = s[..4].parse::<u32>().unwrap();
    let month = s[5..7].parse::<u32>().unwrap();
    let day = s[8..10].parse::<u32>().unwrap();
    let days = match month {
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) {
                29
            } else {
                28
            }
        }
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    day > 0
        && day <= days
        && s[11..13].parse::<u32>().unwrap() < 24
        && s[14..16].parse::<u32>().unwrap() < 60
        && s[17..19].parse::<u32>().unwrap() < 60
}

fn scheduled_at_valid(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.len() == 25 && s.ends_with("+00:00") && date_ymd_valid(s))
}

fn rfc3339_valid(value: &Value) -> bool {
    let Some(s) = value.as_str() else {
        return false;
    };
    if !date_ymd_valid(s) {
        return false;
    }
    let mut rest = &s.as_bytes()[19..];
    if let Some(after_dot) = rest.strip_prefix(b".") {
        let digits = after_dot
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 {
            return false;
        }
        rest = &after_dot[digits..];
    }
    match rest {
        b"Z" | b"z" => true,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2]
            if h1.is_ascii_digit()
                && h2.is_ascii_digit()
                && m1.is_ascii_digit()
                && m2.is_ascii_digit() =>
        {
            let _ = sign;
            let hh = (h1 - b'0') * 10 + (h2 - b'0');
            let mm = (m1 - b'0') * 10 + (m2 - b'0');
            hh < 24 && mm < 60
        }
        _ => false,
    }
}

pub fn keepalive_valid(data: &Value) -> bool {
    let Some(generation) = data["generation"].as_u64() else {
        return false;
    };
    let Some(restore_count) = data["restore_count"].as_u64() else {
        return false;
    };
    let Some(budget_used) = data["budget_used"].as_u64() else {
        return false;
    };
    generation <= 9_007_199_254_740_991
        && data["version"].as_str().is_some_and(|v| {
            v.len() == 64
                && v.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
        && matches!(
            data["status"].as_str(),
            Some("disabled" | "waiting" | "paused" | "running" | "needs_confirmation")
        )
        && restore_count <= 5
        && budget_used >= restore_count
        && budget_used <= 5
        && ["last_restore_at", "next_allowed_at"]
            .iter()
            .all(|k| data[*k].is_null() || rfc3339_valid(&data[*k]))
}

pub fn operation_valid(data: &Value) -> bool {
    data["id"]
        .as_str()
        .is_some_and(|s| s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        && data["account_id"].as_u64().is_some_and(|id| id > 0)
        && data["action"].is_string()
        && data["status"].as_str().is_some_and(|s| {
            matches!(
                s,
                "queued"
                    | "running"
                    | "checking"
                    | "completed"
                    | "needs_confirmation"
                    | "failed"
                    | "cancelled"
                    | "superseded"
            )
        })
        && data["requested"].is_object()
        && data
            .get("scheduled_at")
            .is_none_or(|v| data["action"] == "test" && scheduled_at_valid(v))
}

pub fn templates_valid(data: &Value) -> bool {
    let Some(templates) = data["templates"].as_object() else {
        return false;
    };
    data["configured"].is_boolean()
        && data["version"].is_string()
        && templates.len() <= 32
        && templates.iter().all(|(id, value)| {
            let builtin = matches!(id.as_str(), "full" | "degraded" | "takeover");
            let custom = id.strip_prefix("custom-").is_some_and(|v| {
                v.len() == 24
                    && v.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            });
            (builtin || custom)
                && value["whitelist"]
                    .as_array()
                    .is_some_and(|v| v.iter().all(Value::is_string))
                && value["mappings"].as_array().is_some_and(|v| {
                    v.iter()
                        .all(|m| m["source"].is_string() && m["target"].is_string())
                })
        })
        && (data["template_order"].is_null()
            || data["template_order"].as_array().is_some_and(|values| {
                let ids = values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<std::collections::HashSet<_>>();
                ids.len() == values.len()
                    && ids.len() == templates.len()
                    && ids.iter().all(|id| templates.contains_key(*id))
            }))
        && (data["template_meta"].is_null()
            || data["template_meta"].as_object().is_some_and(|meta| {
                meta.len() == templates.len()
                    && templates.keys().all(|id| {
                        meta.get(id).is_some_and(|value| {
                            value["name"]
                                .as_str()
                                .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 40)
                        })
                    })
            }))
        && (data["template_versions"].is_null()
            || data["template_versions"]
                .as_object()
                .is_some_and(|versions| {
                    versions.len() == templates.len()
                        && templates.keys().all(|id| {
                            versions.get(id).and_then(Value::as_str).is_some_and(|s| {
                                s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
                            })
                        })
                }))
}

pub fn batch_valid(data: &Value) -> bool {
    data["batch_id"].as_str().is_some_and(|s| {
        s.len() == 32
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    }) && data["pending"].is_u64()
        && data["items"].as_array().is_some_and(|rows| {
            rows.iter()
                .all(|row| operation_valid(row) && row["batch_id"] == data["batch_id"])
        })
}

pub fn proxy_batch_valid(data: &Value) -> bool {
    batch_valid(data)
        && data["items"].as_array().is_some_and(|rows| {
            rows.iter().all(|row| {
                row["requested"]
                    .as_object()
                    .is_some_and(|fields| match row["action"].as_str() {
                        Some("proxy") => {
                            fields.len() == 1
                                && fields.get("proxy_id").is_some_and(|id| {
                                    id.is_null() || id.as_u64().is_some_and(|value| value > 0)
                                })
                        }
                        Some("delete") => fields.is_empty(),
                        Some("test") => {
                            fields.len() == 3
                                && fields.get("confirmed").and_then(Value::as_bool) == Some(true)
                                && fields
                                    .get("mode")
                                    .and_then(Value::as_str)
                                    .is_some_and(|mode| matches!(mode, "default" | "text"))
                                && fields
                                    .get("model_id")
                                    .and_then(Value::as_str)
                                    .is_some_and(|id| id.len() <= 200)
                                && (row["result"].is_null()
                                    || connection_test_result_valid(&row["result"]))
                        }
                        Some("groups") => {
                            let valid_ids = |v: &Value| {
                                v.as_array().is_some_and(|ids| {
                                    ids.len() <= 100
                                        && ids.iter().all(|id| id.as_u64().is_some_and(|n| n > 0))
                                })
                            };
                            fields.len() == 2
                                && fields.get("scope_group_ids").is_some_and(|v| {
                                    valid_ids(v) && v.as_array().is_some_and(|ids| !ids.is_empty())
                                })
                                && fields.get("group_ids").is_some_and(valid_ids)
                                && row["requested"]["group_ids"].as_array().is_some_and(|ids| {
                                    ids.iter().all(|id| {
                                        row["requested"]["scope_group_ids"]
                                            .as_array()
                                            .is_some_and(|scope| scope.contains(id))
                                    })
                                })
                        }
                        Some("recover") => recovery_operation_valid(row),
                        _ => false,
                    })
            })
        })
}

fn recovery_operation_valid(row: &Value) -> bool {
    row["requested"]
        .as_object()
        .is_some_and(|fields| fields.is_empty())
        && (row["result"].is_null()
            || row["result"].is_object()
                && row["result"]["verified"].is_boolean()
                && row["result"].get("unchanged").is_none_or(Value::is_boolean))
}

fn connection_test_result_valid(data: &Value) -> bool {
    data.is_object()
        && data["success"].is_boolean()
        && data["requested_model"]
            .as_str()
            .is_some_and(|s| s.len() <= 200)
        && (data["actual_model"].is_null()
            || data["actual_model"]
                .as_str()
                .is_some_and(|s| s.len() <= 200))
        && data["duration_ms"].as_u64().is_some()
        && data["completed_at"].is_string()
        && (data["error"].is_null() || data["error"].as_str().is_some_and(|s| s.len() <= 1024))
        && (data["uncertain"].is_null() || data["uncertain"].is_boolean())
}

pub fn detection_logs_valid(data: &Value) -> bool {
    let optional_text = |v: &Value| v.is_null() || v.is_string();
    let receipt = |v: &Value| {
        v.is_null()
            || v.is_object()
                && v["status"].is_string()
                && v["reason"].is_string()
                && optional_text(&v["completed_at"])
                && (v["basis"].is_null()
                    || matches!(v["basis"].as_str(), Some("first_group" | "aggregate")))
                && ["marked", "schedule_verified", "mark_cleared"]
                    .iter()
                    .all(|k| v[*k].is_null() || v[*k].is_boolean())
    };
    data.is_object()
        && data.get("next_cursor").is_some_and(|v| {
            v.is_null() || v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 512)
        })
        && data["items"].as_array().is_some_and(|items| {
            items.len() <= 100
                && items.iter().all(|item| {
                    item["job_id"].as_str().is_some_and(|id| {
                        id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit())
                    }) && item["account_id"].as_u64().is_some_and(|id| id > 0)
                        && ["account_name", "model_id", "created_at", "error"]
                            .iter()
                            .all(|key| item[*key].is_string())
                        && matches!(
                            item["status"].as_str(),
                            Some(
                                "completed"
                                    | "failed"
                                    | "cancelled"
                                    | "interrupted"
                                    | "needs_confirmation"
                            )
                        )
                        && ["started_at", "completed_at"]
                            .iter()
                            .all(|key| optional_text(&item[*key]))
                        && item["triggers"]
                            .as_array()
                            .is_some_and(|values| values.iter().all(Value::is_string))
                        && item["details_available"].is_boolean()
                        && item["summary"]["outcome"].is_string()
                        && item["summary"]["analysis_available"].is_boolean()
                        && ["first_prediction", "prediction"]
                            .iter()
                            .all(|key| item["summary"][*key].is_string())
                        && (item["summary"]["probability"].is_null()
                            || item["summary"]["probability"]
                                .as_f64()
                                .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v)))
                        && (item["summary"]["first_probability"].is_null()
                            || item["summary"]["first_probability"]
                                .as_f64()
                                .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v)))
                        && (item["summary"]["used_outputs"].is_null()
                            || item["summary"]["used_outputs"].is_u64())
                        && ["first_downgrade", "aggregate_downgrade"]
                            .iter()
                            .all(|key| {
                                item["summary"][*key].is_null()
                                    || item["summary"][*key].is_boolean()
                            })
                        && ["disposition", "recovery", "verification", "restoration"]
                            .iter()
                            .all(|key| receipt(&item["summary"][*key]))
                        && (item["notification"].is_null()
                            || item["notification"].is_object()
                                && item["notification"]["status"].is_string()
                                && item["notification"]["merged"].is_boolean()
                                && item["notification"]["reason"].is_string()
                                && optional_text(&item["notification"]["delivered_at"]))
                })
        })
}

pub fn detection_platforms_valid(value: &Value) -> bool {
    value.as_object().is_some_and(|platforms| {
        platforms.iter().all(|(platform, selection)| {
            matches!(
                platform.as_str(),
                "openai" | "anthropic" | "grok" | "deepseek"
            ) && (selection["engine"] == "lm_detector"
                || platform == "openai" && selection["engine"] == "modeltrace")
                && matches!(selection["sample_count"].as_u64(), Some(1 | 3))
        })
    })
}

pub fn batch_results(data: &Value) -> bool {
    batch_valid(data)
        && data["items"].as_array().is_some_and(|rows| {
            rows.iter().all(|row| {
                (row["action"] != "recover" || recovery_operation_valid(row))
                    && (row["action"] != "test"
                        || row["result"].is_null()
                        || connection_test_result_valid(&row["result"]))
            })
        })
}
