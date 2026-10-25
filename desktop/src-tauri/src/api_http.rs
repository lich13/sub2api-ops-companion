use serde_json::Value;
use std::{io::Write, path::Path, time::Duration};

const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_OPERATION_JSON_BYTES: usize = 128 * 1024 * 1024;

fn recoveries_valid(data: &Value) -> bool {
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

fn credit_state_valid(data: &Value) -> bool {
    data.is_null()
        || ["stage", "label", "error"]
            .iter()
            .all(|k| data[*k].is_string())
            && [
                "trigger_at",
                "consume_deadline_at",
                "verification_deadline_at",
                "completed_at",
                "attempt_at",
                "next_at",
            ]
            .iter()
            .all(|k| data[*k].is_null() || data[*k].is_string())
            && (data["outcome"].is_null()
                || matches!(
                    data["outcome"].as_str(),
                    Some(
                        "restored" | "not_executed" | "consume_failed" | "unrecovered" | "unknown"
                    )
                ))
            && (data["notification_status"].is_null()
                || matches!(
                    data["notification_status"].as_str(),
                    Some("pending" | "delivered" | "failed" | "suppressed")
                ))
            && (data["notification_attempts"].is_null()
                || data["notification_attempts"]
                    .as_u64()
                    .is_some_and(|n| n <= 5))
}

fn snapshot_valid(data: &Value) -> bool {
    data.get("observed_at")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
        && data
            .get("accounts")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.iter().all(|a| {
                    a["id"].as_i64().is_some()
                        && (a["parent_account_id"].is_null()
                            || a["parent_account_id"].as_i64().is_some_and(|id| id > 0))
                        && ["name", "platform", "type", "version"]
                            .iter()
                            .all(|k| a[*k].is_string())
                        && ["group_ids", "usage_windows", "blockers"]
                            .iter()
                            .all(|k| a[*k].is_array())
                        && a["schedulable"].is_boolean()
                        && a["available"].is_boolean()
                        && a["priority"].as_i64().is_some()
                        && credit_state_valid(&a["auto_reset_credit"])
                        && (a["concurrency"].is_null() || a["concurrency"].as_u64().is_some())
                        && (a["load_factor"].is_null()
                            || a["load_factor"]
                                .as_u64()
                                .is_some_and(|n| (1..=10000).contains(&n)))
                        && (a["usage"].is_null() || usage_valid(&a["usage"]))
                        && (a["recovery_monitor"].is_null()
                            || ["status", "label", "reason"]
                                .iter()
                                .all(|k| a["recovery_monitor"][*k].is_string())
                                && (a["recovery_monitor"]["next_at"].is_null()
                                    || a["recovery_monitor"]["next_at"].is_string()))
                })
            })
        && data
            .get("groups")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.iter().all(|g| {
                    g["id"].as_i64().is_some()
                        && g["name"].is_string()
                        && g["recent_accounts"].is_array()
                })
            })
        && data["errors"].is_array()
        && recoveries_valid(&data["recoveries"])
}

fn usage_valid(data: &Value) -> bool {
    let stats = |v: &Value| {
        v.is_null()
            || ["requests", "tokens", "cost", "standard_cost", "user_cost"]
                .iter()
                .all(|k| v[*k].as_f64().is_some_and(|n| n.is_finite() && n >= 0.0))
    };
    (data["key_quota"].is_null() || crate::key_quota::valid(&data["key_quota"]))
        && data["branch"].is_string()
        && data["actions"].is_array()
        && stats(&data["today"])
        && data["windows"].as_array().is_some_and(|windows| {
            windows.iter().all(|w| {
                w["key"].is_string()
                    && w["label"].is_string()
                    && w["source"].is_string()
                    && matches!(
                        w["status"].as_str(),
                        Some("known" | "unknown" | "stale" | "error")
                    )
                    && (w["used_percent"].is_null()
                        || w["used_percent"]
                            .as_f64()
                            .is_some_and(|v| v.is_finite() && v >= 0.0))
                    && stats(&w["stats"])
                    && ["reset_at", "observed_at"]
                        .iter()
                        .all(|k| w[*k].is_null() || w[*k].is_string())
                    && (w["estimated"].is_null() || w["estimated"].is_boolean())
            })
        })
        && (data["balances"].is_null()
            || data["balances"].as_array().is_some_and(|items| {
                items.iter().all(|v| {
                    v["currency"].is_string()
                        && v["balance"].as_f64().is_some_and(f64::is_finite)
                        && (v["status"].is_null()
                            || matches!(
                                v["status"].as_str(),
                                Some("known" | "unknown" | "stale" | "error")
                            ))
                        && (v["observed_at"].is_null() || v["observed_at"].is_string())
                })
            }))
        && (data["native_read"].is_null() || data["native_read"].is_boolean())
        && (data["status"].is_null()
            || matches!(
                data["status"].as_str(),
                Some("known" | "unavailable" | "error" | "authorization_required")
            ))
        && (data["message"].is_null() || data["message"].is_string())
}

fn scheduled_at_valid(value: &Value) -> bool {
    let Some(s) = value.as_str() else {
        return false;
    };
    let b = s.as_bytes();
    if b.len() != 25
        || !s.ends_with("+00:00")
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
        && s[17..19].parse::<u32>().unwrap() < 60
        && s[11..13].parse::<u32>().unwrap() < 24
        && s[14..16].parse::<u32>().unwrap() < 60
}

fn operation_valid(data: &Value) -> bool {
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

fn connection_valid(data: &Value) -> bool {
    [
        "ok",
        "network_unreachable",
        "auth_rejected",
        "route_not_found",
        "protocol_mismatch",
        "upstream_error",
    ]
    .contains(&data["state"].as_str().unwrap_or(""))
        && ["verify", "public", "none"].contains(&data["endpoint"].as_str().unwrap_or(""))
        && (data["http_status"].is_null()
            || data["http_status"]
                .as_u64()
                .is_some_and(|n| (100..=599).contains(&n)))
        && data["message"].is_string()
        && data["retryable"].is_boolean()
        && (data["checked_at"].is_null() || data["checked_at"].is_string())
}

fn templates_valid(data: &Value) -> bool {
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

fn batch_valid(data: &Value) -> bool {
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

fn proxy_batch_valid(data: &Value) -> bool {
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

fn settings_valid(data: &Value) -> bool {
    let actions = [
        "priority",
        "concurrency",
        "load_factor",
        "model_config",
        "proxy",
        "sync_models",
    ];
    data["account_id"].as_u64().is_some_and(|n| n > 0)
        && data["priority"].as_u64().is_some()
        && (data["concurrency"].is_null() || data["concurrency"].as_u64().is_some())
        && (data["load_factor"].is_null()
            || data["load_factor"]
                .as_u64()
                .is_some_and(|n| (1..=10000).contains(&n)))
        && data["effective_load_factor"]
            .as_u64()
            .is_some_and(|n| n > 0)
        && (data["proxy_id"].is_null() || data["proxy_id"].as_u64().is_some_and(|n| n > 0))
        && data["model_config"]["whitelist"]
            .as_array()
            .is_some_and(|v| v.len() <= 300 && v.iter().all(Value::is_string))
        && data["model_config"]["mappings"]
            .as_array()
            .is_some_and(|v| {
                v.len() <= 300
                    && v.iter()
                        .all(|r| r["source"].is_string() && r["target"].is_string())
            })
        && actions.iter().all(|action| {
            data["capabilities"][*action]["editable"].is_boolean()
                && data["capabilities"][*action]["reason"].is_string()
                && data["versions"][*action]
                    .as_str()
                    .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        })
        && data.as_object().is_some_and(|map| {
            map.keys().all(|k| {
                matches!(
                    k.as_str(),
                    "account_id"
                        | "priority"
                        | "concurrency"
                        | "load_factor"
                        | "effective_load_factor"
                        | "proxy_id"
                        | "model_config"
                        | "capabilities"
                        | "versions"
                )
            })
        })
}

fn proxies_valid(data: &Value) -> bool {
    data.as_array().is_some_and(|rows| {
        rows.len() <= 10000
            && rows.iter().all(|row| {
                row["id"].as_u64().is_some_and(|n| n > 0)
                    && row["name"].is_string()
                    && matches!(
                        row["protocol"].as_str(),
                        Some("http" | "https" | "socks5" | "socks5h" | "unknown")
                    )
                    && matches!(
                        row["status"].as_str(),
                        Some("active" | "inactive" | "error" | "unknown")
                    )
                    && row.as_object().is_some_and(|map| {
                        map.keys()
                            .all(|k| matches!(k.as_str(), "id" | "name" | "protocol" | "status"))
                    })
            })
    })
}

fn proxy_diagnostic_valid(path: &str, data: &Value) -> bool {
    let text = |key: &str| data[key].is_string();
    let number = |key: &str| {
        data[key].is_null()
            || data[key]
                .as_f64()
                .is_some_and(|n| n.is_finite() && n >= 0.0)
    };
    let keys = if path.ends_with("/test") {
        &[
            "success",
            "message",
            "latency_ms",
            "ip_address",
            "city",
            "region",
            "country",
            "country_code",
        ][..]
    } else {
        &[
            "score",
            "grade",
            "summary",
            "exit_ip",
            "country",
            "country_code",
            "base_latency_ms",
            "passed_count",
            "warn_count",
            "failed_count",
            "challenge_count",
            "checked_at",
            "items",
        ][..]
    };
    if !data
        .as_object()
        .is_some_and(|map| map.keys().all(|k| keys.contains(&k.as_str())))
    {
        return false;
    }
    if path.ends_with("/test") {
        return data["success"].is_boolean()
            && text("message")
            && text("ip_address")
            && number("latency_ms")
            && ["city", "region", "country", "country_code"]
                .iter()
                .all(|k| text(k));
    }
    data["score"]
        .as_f64()
        .is_some_and(|n| (0.0..=100.0).contains(&n))
        && text("grade")
        && text("summary")
        && text("exit_ip")
        && text("country")
        && text("country_code")
        && [
            "base_latency_ms",
            "passed_count",
            "warn_count",
            "failed_count",
            "challenge_count",
            "checked_at",
        ]
        .iter()
        .all(|k| number(k))
        && data["items"].as_array().is_some_and(|items| {
            items.len() <= 32
                && items.iter().all(|item| {
                    matches!(
                        item["status"].as_str(),
                        Some("pass" | "warn" | "fail" | "challenge")
                    ) && item["target"].is_string()
                        && item["message"].is_string()
                        && ["latency_ms", "http_status"].iter().all(|k| {
                            item[*k].is_null()
                                || item[*k].as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
                        })
                        && item.as_object().is_some_and(|m| {
                            m.keys().all(|k| {
                                matches!(
                                    k.as_str(),
                                    "target" | "status" | "http_status" | "latency_ms" | "message"
                                )
                            })
                        })
                })
        })
}

fn detection_logs_valid(data: &Value) -> bool {
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

fn detection_platforms_valid(value: &Value) -> bool {
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

fn shape_valid(path: &str, data: &Value) -> bool {
    if path == "/sub2api-config" {
        return data["available"].is_boolean()
            && data["editable"].is_boolean()
            && data["reason"].is_string()
            && (data["available"] == false
                || data["healthy"].is_boolean()
                    && data["catalog"]["version"].is_string()
                    && data["catalog"]["revision"].is_string()
                    && data["catalog"]["groups"].as_array().is_some_and(|a| {
                        a.iter()
                            .all(|g| g["id"].is_string() && g["label"].is_string())
                    })
                    && data["catalog"]["fields"].as_array().is_some_and(|a| {
                        a.iter().all(|f| {
                            [
                                "path",
                                "group",
                                "type",
                                "type_label",
                                "description",
                                "range",
                                "default_text",
                                "status",
                                "source_url",
                            ]
                            .iter()
                            .all(|k| f[*k].is_string())
                                && ["sensitive", "internal", "editable", "has_default"]
                                    .iter()
                                    .all(|k| f[*k].is_boolean())
                                && f["env"]
                                    .as_array()
                                    .is_some_and(|a| a.iter().all(Value::is_string))
                                && (f["choices"].is_null()
                                    || f["choices"]
                                        .as_array()
                                        .is_some_and(|a| a.iter().all(Value::is_string)))
                                && (f["internal"] == true
                                    || data["values"][f["path"].as_str().unwrap_or("")].is_object())
                        })
                    })
                    && data["values"].as_object().is_some_and(|m| {
                        m.values().all(|v| {
                            v["configured"].is_boolean()
                                && v["editable"].is_boolean()
                                && v["source"].is_string()
                                && (v["environment"].is_null() || v["environment"].is_string())
                        })
                    })
                    && data["unknown"].as_array().is_some_and(|a| {
                        a.iter()
                            .all(|v| v["path"].is_string() && v["message"].is_string())
                    })
                    && ["version", "commit", "revision", "runtime_revision"]
                        .iter()
                        .all(|k| data[*k].is_string()));
    }
    if path == "/sub2api-config/validate" {
        return data["valid"].is_boolean()
            && data["errors"].as_array().is_some_and(|a| {
                a.iter()
                    .all(|e| e["path"].is_string() && e["message"].is_string())
            });
    }
    if matches!(path, "/sub2api-config/apply" | "/sub2api-config/operation") {
        return matches!(
            data["status"].as_str(),
            Some(
                "idle"
                    | "saving"
                    | "restarting"
                    | "restoring"
                    | "succeeded"
                    | "failed"
                    | "restored"
                    | "restore_failed"
                    | "conflict"
            )
        ) && (data["message"].is_null() || data["message"].is_string());
    }

    if path == "/account-creation"
        || path.starts_with("/account-creation/")
        || path == "/account-creation?sync_models=true"
    {
        return crate::account_creation::valid(path, data);
    }
    if path
        .split('?')
        .next()
        .is_some_and(|p| p == "/proxy-management" || p.starts_with("/proxy-management/"))
    {
        return crate::proxy_management::valid(path, data);
    }
    match path.split('?').next().unwrap_or("") {
        "/snapshot" => snapshot_valid(data),
        "/config/key-quota" => crate::key_quota::settings_valid(data, None),
        p if p.ends_with("/key-quota-settings/access-token") => p
            .strip_prefix("/accounts/")
            .and_then(|p| p.strip_suffix("/key-quota-settings/access-token"))
            .and_then(|id| id.parse::<u64>().ok())
            .is_some_and(|id| {
                data["account_id"].as_u64() == Some(id)
                    && data["access_token"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 8192)
                    && data.as_object().is_some_and(|o| o.len() == 2)
            }),
        p if p.ends_with("/key-quota-settings") => p
            .strip_prefix("/accounts/")
            .and_then(|p| p.strip_suffix("/key-quota-settings"))
            .and_then(|id| id.parse::<u64>().ok())
            .is_some_and(|id| crate::key_quota::settings_valid(data, Some(id))),
        p if p.starts_with("/accounts/") && p.ends_with("/usage") => usage_valid(data),
        "/config" => {
            ["oauth", "bark"]
                .iter()
                .all(|k| data[*k]["revision"].is_string())
                && data["oauth"]
                    .get("model_detection_model_id")
                    .is_none_or(Value::is_string)
                && data["oauth"]
                    .get("model_detection_platforms")
                    .is_none_or(detection_platforms_valid)
        }
        "/capabilities" => {
            data["api_version"].as_u64() == Some(1)
                && data["features"]
                    .get("account_priority_ordering")
                    .is_none_or(Value::is_boolean)
                && data["features"]
                    .get("account_priority_per_platform")
                    .is_none_or(Value::is_boolean)
                && (data["upstream_connection"].is_null()
                    || connection_valid(&data["upstream_connection"]))
        }
        "/connection-status" => connection_valid(data),
        "/proxies" => proxies_valid(data),
        p if p.starts_with("/proxies/") => proxy_diagnostic_valid(p, data),
        p if p.ends_with("/settings") => settings_valid(data),
        "/detection-logs" => detection_logs_valid(data),
        "/account-priorities" => {
            if crate::priority_api::is_v2(path) {
                crate::priority_api::list_v2(data)
            } else {
                crate::priority_api::list(data)
            }
        }
        "/account-priority-orders" => {
            if crate::priority_api::is_v2(path) {
                crate::priority_api::task_v2(data, true)
            } else {
                crate::priority_api::task(data, true)
            }
        }
        p if p.starts_with("/account-priority-orders/") => {
            (if crate::priority_api::is_v2(path) {
                crate::priority_api::task_v2(data, !crate::priority_api::is_summary(path))
            } else {
                crate::priority_api::task(data, true)
            }) && p.split('/').nth(2) == data["id"].as_str()
        }
        "/errors" => data["items"].is_array(),
        "/recoveries" => recoveries_valid(&data["items"]),
        "/account-operations" => {
            if path.contains("?batch_id=") {
                batch_valid(data)
                    && data["items"].as_array().is_some_and(|items| {
                        items.iter().all(|item| {
                            (item["action"] != "recover" || recovery_operation_valid(item))
                                && (item["action"] != "test"
                                    || item["result"].is_null()
                                    || connection_test_result_valid(&item["result"]))
                        })
                    })
            } else {
                data["pending"].is_u64()
                    && data["items"]
                        .as_array()
                        .is_some_and(|rows| rows.iter().all(operation_valid))
            }
        }
        "/account-operations/batch" => proxy_batch_valid(data),
        p if p.starts_with("/account-operations/batches/") => proxy_batch_valid(data),
        p if p.starts_with("/account-operations/") || p.ends_with("/operations") => {
            operation_valid(data)
        }
        p if p.ends_with("/model-tests/latest") && data.is_null() => true,
        p if p.starts_with("/model-tests/")
            || p.ends_with("/model-tests")
            || p.ends_with("/model-tests/latest") =>
        {
            data["id"].is_string() && data["status"].is_string() && data["account_id"].is_u64()
        }
        "/account-templates/apply" => batch_valid(data),
        p if p == "/account-templates" || p.starts_with("/account-templates/") => {
            templates_valid(data)
        }
        p if p.ends_with("/model-detection") => {
            data["account_id"].is_u64()
                && data["version"].is_string()
                && data["enabled"].is_boolean()
                && data["interval_minutes"].as_u64().is_some_and(|v| v > 0)
                && data
                    .get("schedule_mode")
                    .is_none_or(|v| v == "fixed" || v == "smart")
                && data
                    .get("min_interval_minutes")
                    .is_none_or(|v| v.as_u64().is_some_and(|n| (5..=30).contains(&n)))
                && (data["effective_interval_minutes"].is_null()
                    || data["effective_interval_minutes"]
                        .as_u64()
                        .is_some_and(|n| {
                            n > 0 && (data["schedule_mode"] != "smart" || (5..=30).contains(&n))
                        }))
                && (data["recent_request_count"].is_null()
                    || data["recent_request_count"].as_u64().is_some())
                && data["model_id"].is_string()
                && (data["enabled"] != true
                    || data["model_id"].as_str().is_some_and(|v| !v.is_empty()))
                && data
                    .get("engine")
                    .is_none_or(|v| v == "modeltrace" || v == "lm_detector")
                && data
                    .get("sample_count")
                    .is_none_or(|v| matches!(v.as_u64(), Some(1 | 3)))
                && data
                    .get("model_source")
                    .is_none_or(|v| v == "global" || v == "account")
                && (data["model_override"].is_null() || data["model_override"].is_string())
                && data.get("global_model_id").is_none_or(Value::is_string)
                && data["status"].is_string()
                && (data["last_automatic_started_at"].is_null()
                    || data["last_automatic_started_at"].is_string())
        }
        p if p.ends_with("/keepalive") => {
            let account_id = p
                .strip_prefix("/accounts/")
                .and_then(|p| p.strip_suffix("/keepalive"))
                .and_then(|id| id.parse::<u64>().ok());
            account_id.is_some_and(|id| id > 0 && data["account_id"].as_u64() == Some(id))
                && data["enabled"].is_boolean()
                && data["read_only"].is_boolean()
                && data["generation"]
                    .as_u64()
                    .is_some_and(|v| v <= 9_007_199_254_740_991)
                && data["version"]
                    .as_str()
                    .is_some_and(|v| v.len() == 64 && v.bytes().all(|c| c.is_ascii_hexdigit()))
                && data["status"].as_str().is_some_and(|v| {
                    matches!(
                        v,
                        "disabled" | "waiting" | "paused" | "running" | "needs_confirmation"
                    )
                })
                && ["reason", "paused_reason"]
                    .iter()
                    .all(|key| data[*key].is_string())
                && data["restore_count"].as_u64().is_some_and(|count| {
                    count <= 5
                        && data["budget_used"]
                            .as_u64()
                            .is_some_and(|used| (count..=5).contains(&used))
                })
                && ["last_restore_at", "next_allowed_at"].iter().all(|key| {
                    data.get(*key)
                        .is_some_and(|v| v.is_null() || v.as_str().is_some_and(|v| !v.is_empty()))
                })
        }
        "/modeltrace/fingerprint-bank" | "/modeltrace/fingerprint-bank/sync" => {
            data["version"]["revision"].is_string()
                && data["version"]["sha256"].is_string()
                && data["version"]["analyzer_version"].is_u64()
                && data["source"].is_string()
                && data["status"].is_string()
        }
        "/quota-refresh" => {
            data["status"].is_string()
                && data["items"].as_array().is_some_and(|items| {
                    items
                        .iter()
                        .all(|item| item["usage"].is_null() || usage_valid(&item["usage"]))
                })
        }
        p if p.ends_with("/models") => data
            .as_array()
            .is_some_and(|m| m.iter().all(|v| v["id"].is_string())),
        _ => data.is_object(),
    }
}

fn response_kind(value: &str) -> &'static str {
    match value.split(';').next().unwrap_or("").trim() {
        "application/json" => "json",
        "text/html" => "html",
        "text/plain" => "text",
        "" => "missing",
        _ => "other",
    }
}

fn diagnostic(
    file: &Path,
    method: &str,
    path: &str,
    status: u16,
    kind: &str,
    stage: &str,
    request_id: &str,
) {
    use std::os::unix::fs::OpenOptionsExt;
    let Some(parent) = file.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let truncate = std::fs::metadata(file).is_ok_and(|m| m.len() > 64 * 1024);
    let route = path
        .split('?')
        .next()
        .unwrap_or("")
        .split('/')
        .map(|s| {
            if s.parse::<u64>().is_ok() {
                ":id"
            } else if s
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c == b'-' || c == b'_')
            {
                s
            } else {
                "unknown"
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(!truncate)
        .truncate(truncate)
        .mode(0o600)
        .open(file)
    {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let _ = writeln!(f, "{stamp} method={method} path={route} status={status} type={kind} stage={stage} request_id={request_id}");
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HttpFailure {
    pub message: String,
    pub connection: Option<Value>,
}
impl From<String> for HttpFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            connection: None,
        }
    }
}
impl From<&str> for HttpFailure {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}
impl From<HttpFailure> for String {
    fn from(error: HttpFailure) -> Self {
        error.message
    }
}

pub(crate) async fn http(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    log: &Path,
) -> Result<Value, String> {
    http_detailed(client, base, key, method, path, body, log)
        .await
        .map_err(String::from)
}

fn model_directory_request(method: &str, path: &str) -> bool {
    let Some(account_path) = path.strip_prefix("/accounts/") else {
        return false;
    };
    let Some((id, operation)) = account_path.split_once('/') else {
        return false;
    };
    if id.parse::<u64>().ok().filter(|id| *id > 0).is_none() {
        return false;
    }
    matches!(
        (method, operation),
        ("GET", "models?purpose=model_test") | ("POST", "model-tests") | ("PUT", "model-detection")
    )
}

pub(crate) async fn http_detailed(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    log: &Path,
) -> Result<Value, HttpFailure> {
    let parsed_method =
        reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "请求方式无效")?;
    let mut req = client
        .request(parsed_method, format!("{base}/api/desktop/v1{path}"))
        .header("x-api-key", key)
        .header("Accept", "application/json");
    if path.starts_with("/sub2api-config") {
        req = req.timeout(Duration::from_secs(65));
    }
    if path.starts_with("/proxies/") || path.starts_with("/proxy-management") {
        req = req.timeout(Duration::from_secs(105));
    }
    if path == "/account-creation"
        || path.starts_with("/account-creation/")
        || path == "/account-creation?sync_models=true"
    {
        req = req.timeout(Duration::from_secs(390));
    }
    if path.ends_with("/usage-action") || path.ends_with("/usage") {
        req = req.timeout(Duration::from_secs(105));
    }
    if method == "DELETE" || path.ends_with("/recover-state") {
        req = req.timeout(Duration::from_secs(20));
    }
    if path.starts_with("/model-groups/") {
        req = req.timeout(Duration::from_secs(65));
    }
    if model_directory_request(method, path) {
        req = req.timeout(Duration::from_secs(20));
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let mut response = req.send().await.map_err(|e| {
        diagnostic(
            log,
            method,
            path,
            0,
            "missing",
            if e.is_timeout() {
                "headers_timeout"
            } else {
                "connect"
            },
            "-",
        );
        if e.is_timeout() {
            "连接超时，未重放操作"
        } else {
            "连接暂时中断"
        }
        .to_string()
    })?;
    let status = response.status().as_u16();
    let kind = response_kind(
        response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or(""),
    );
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .filter(|s| s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
        .unwrap_or("-")
        .to_string();
    let fail = |stage: &str, message: &str| {
        diagnostic(log, method, path, status, kind, stage, &request_id);
        HttpFailure::from(message)
    };
    let limit = if path.starts_with("/account-operations/") {
        MAX_OPERATION_JSON_BYTES
    } else {
        MAX_JSON_BYTES
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| {
        fail(
            if e.is_timeout() {
                "body_timeout"
            } else {
                "body_interrupted"
            },
            if e.is_timeout() {
                "响应读取超时，未重放操作"
            } else {
                "响应读取中断"
            },
        )
    })? {
        if bytes.len() + chunk.len() > limit {
            return Err(fail("size", "服务响应超过大小限制"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let parsed = serde_json::from_slice::<Value>(&bytes);
    if !(200..300).contains(&status) {
        let fallback = match status {
            401 | 403 => "管理员 API Key 被拒绝，请重新连接",
            429 => "请求过于频繁",
            502..=504 => "网关暂不可用",
            500..=599 => "服务暂不可用",
            _ => "请求失败",
        };
        let detail = parsed.as_ref().ok().and_then(|d| d.get("detail"));
        let message = detail
            .and_then(Value::as_str)
            .or_else(|| detail.and_then(|d| d["message"].as_str()))
            .unwrap_or(fallback);
        let mut error = fail(
            if status == 401 || status == 403 {
                "auth"
            } else {
                "http"
            },
            &format!(
                "{} [{}]",
                if path.starts_with("/proxies/") || path.starts_with("/sub2api-config") {
                    message.to_string()
                } else {
                    message.chars().take(240).collect::<String>()
                },
                status
            ),
        );
        error.connection = detail
            .and_then(|d| d.get("connection"))
            .filter(|v| connection_valid(v))
            .cloned();
        return Err(error);
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(fail("empty", "服务返回空响应"));
    }
    if kind != "json" {
        return Err(fail("content_type", "服务返回了非 JSON 响应"));
    }
    let data = parsed.map_err(|_| fail("json", "服务 JSON 响应不完整或格式无效"))?;
    if !shape_valid(path, &data) {
        return Err(fail("schema", "服务数据结构不兼容，请检查客户端与服务版本"));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    #[test]
    fn access_token_reveal_requires_the_requested_id_and_a_nonempty_token() {
        let path = "/accounts/7/key-quota-settings/access-token";
        assert!(super::shape_valid(
            path,
            &serde_json::json!({"account_id":7,"access_token":"fixture-token"})
        ));
        for value in [
            serde_json::json!({"account_id":8,"access_token":"fixture-token"}),
            serde_json::json!({"account_id":7,"access_token":""}),
            serde_json::json!({"account_id":7,"access_token":"fixture-token","extra":"unexpected"}),
        ] {
            assert!(!super::shape_valid(path, &value));
        }
    }
    use super::*;
    use std::{io::Read, net::TcpListener};

    #[test]
    fn native_usage_summary_validates_balances_windows_and_missing_values() {
        let data = serde_json::json!({"branch":"native","native_read":true,"status":"known","actions":["query_usage"],"today":null,
            "windows":[{"key":"shared","label":"共享 1d","source":"local_estimate","estimated":true,"status":"known","used_percent":0,"observed_at":null,"reset_at":null}],
            "balances":[{"currency":"CNY","balance":0,"status":"stale","observed_at":null}]});
        assert!(shape_valid("/accounts/1/usage", &data));
        assert!(shape_valid(
            "/quota-refresh",
            &serde_json::json!({"status":"completed","items":[{"usage":data}]})
        ));
        assert!(!shape_valid(
            "/quota-refresh",
            &serde_json::json!({"status":"completed","items":[{"usage":{"windows":[]}}]})
        ));
        for (field, value) in [
            (
                "balances",
                serde_json::json!([{"currency":"USD","balance":"unknown"}]),
            ),
            (
                "windows",
                serde_json::json!([{"key":"x","label":"x","source":"passive","status":"known","used_percent":-1}]),
            ),
            ("today", serde_json::json!({"requests":1})),
        ] {
            let mut invalid = data.clone();
            invalid[field] = value;
            assert!(!shape_valid("/accounts/1/usage", &invalid));
        }
        for status in ["error", "unavailable", "authorization_required"] {
            let mut data = data.clone();
            data["status"] = status.into();
            assert!(shape_valid("/accounts/1/usage", &data));
        }
    }

    #[test]
    fn capabilities_accepts_existing_and_android_clients() {
        for platforms in [
            serde_json::json!(["macos"]),
            serde_json::json!(["macos", "android"]),
        ] {
            assert!(shape_valid(
                "/capabilities",
                &serde_json::json!({
                    "api_version":1, "platforms":platforms
                })
            ));
        }
    }

    #[tokio::test]
    async fn response_limits_cover_exact_boundaries_and_chunked_overflow() {
        for (path, limit) in [
            ("/snapshot", MAX_JSON_BYTES),
            ("/account-operations/fixture", MAX_OPERATION_JSON_BYTES),
        ] {
            for (extra, chunked) in [(0, false), (0, true), (1, false), (1, true)] {
                let length = limit + extra;
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let base = format!("http://{}", listener.local_addr().unwrap());
                let server = std::thread::spawn(move || {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(20)))
                        .unwrap();
                    let mut request = [0; 4096];
                    let _ = socket.read(&mut request);
                    let framing = if chunked {
                        "Transfer-Encoding: chunked".to_owned()
                    } else {
                        format!("Content-Length: {length}")
                    };
                    // An HTTP failure is intentional: it proves that a body at
                    // the boundary reaches ordinary status handling, not size rejection.
                    let headers = format!("HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\n{framing}\r\nConnection: close\r\n\r\n");
                    if socket.write_all(headers.as_bytes()).is_err() {
                        return;
                    }
                    let mut block = vec![b' '; 65_536];
                    let mut sent = 0;
                    while sent < length {
                        if sent == 0 {
                            block[..2].copy_from_slice(b"{}");
                        } else {
                            block[..2].fill(b' ');
                        }
                        let size = block.len().min(length - sent);
                        if chunked && write!(socket, "{size:x}\r\n").is_err() {
                            return;
                        }
                        if socket.write_all(&block[..size]).is_err() {
                            return;
                        }
                        if chunked && socket.write_all(b"\r\n").is_err() {
                            return;
                        }
                        sent += size;
                    }
                    if chunked {
                        let _ = socket.write_all(b"0\r\n\r\n");
                    }
                });
                let directory = tempfile::tempdir().unwrap();
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .unwrap();
                let error = http(
                    &client,
                    &base,
                    "fixture-admin-key",
                    "GET",
                    path,
                    None,
                    &directory.path().join("network.log"),
                )
                .await
                .unwrap_err();
                server.join().unwrap();
                if extra == 0 {
                    assert!(
                        error.contains("网关暂不可用 [502]"),
                        "at-limit body must reach HTTP status handling"
                    );
                } else {
                    assert!(
                        error.contains("服务响应超过大小限制"),
                        "oversize body must be rejected"
                    );
                }
                assert!(
                    !std::fs::read_to_string(directory.path().join("network.log"))
                        .unwrap_or_default()
                        .contains("fixture-admin-key")
                );
            }
        }
    }

    async fn serve(
        status: &str,
        headers: &str,
        body: &str,
        delay: bool,
    ) -> (Result<Value, String>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let reply = format!("HTTP/1.1 {status}\r\n{headers}\r\nx-request-id: test-request\r\nConnection: close\r\n\r\n{body}");
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(reply.as_bytes());
            if delay {
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("network.log");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let result = http(
            &client,
            &base,
            "secret-admin-key",
            "GET",
            "/snapshot",
            None,
            &log,
        )
        .await;
        thread.join().unwrap();
        (result, std::fs::read_to_string(log).unwrap_or_default())
    }

    #[tokio::test]
    async fn classifies_failures_without_leaking_body_or_key() {
        for (status, headers, body, expected, stage, delay) in [
            (
                "502 Bad Gateway",
                "Content-Type: text/html",
                "<html>secret-body</html>",
                "网关暂不可用 [502]",
                "http",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json",
                "",
                "空响应",
                "empty",
                false,
            ),
            (
                "200 OK",
                "Content-Type: text/html",
                "secret-body",
                "非 JSON",
                "content_type",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json",
                "{",
                "JSON 响应不完整",
                "json",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json",
                "{}",
                "数据结构不兼容",
                "schema",
                false,
            ),
            (
                "401 Unauthorized",
                "Content-Type: text/html",
                "secret-body",
                "API Key 被拒绝",
                "auth",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json\r\nContent-Length: 1000",
                "{",
                "读取中断",
                "body_interrupted",
                false,
            ),
            (
                "200 OK",
                "Content-Type: application/json\r\nContent-Length: 1000",
                "{",
                "读取超时",
                "body_timeout",
                true,
            ),
        ] {
            let (result, log) = serve(status, headers, body, delay).await;
            assert!(result.unwrap_err().contains(expected));
            assert!(log.contains(&format!("stage={stage}")));
            assert!(!log.contains("secret"));
        }
    }

    #[tokio::test]
    async fn structured_upstream_failure_survives_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let connection = serde_json::json!({"state":"auth_rejected","endpoint":"verify","http_status":403,
            "message":"管理员 Key 被拒绝","retryable":false,"checked_at":"2026-10-07T00:00:00Z"});
        let body = serde_json::json!({"detail":{"code":"auth_rejected","message":"管理员 Key 被拒绝","connection":connection}}).to_string();
        let reply = format!("HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(reply.as_bytes());
        });
        let temp = tempfile::tempdir().unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = http_detailed(
            &client,
            &base,
            "fixture-key",
            "GET",
            "/snapshot",
            None,
            &temp.path().join("network.log"),
        )
        .await
        .unwrap_err();
        thread.join().unwrap();
        assert_eq!(error.connection, Some(connection));
        assert!(error.message.contains("管理员 Key 被拒绝"));
        assert!(!std::fs::read_to_string(temp.path().join("network.log"))
            .unwrap()
            .contains("fixture-key"));
    }

    #[tokio::test]
    async fn valid_snapshot_and_structure_guard() {
        let body = r#"{"observed_at":"2026-09-25T00:00:00Z","accounts":[],"groups":[],"errors":[],"recoveries":[]}"#;
        let (result, log) = serve("200 OK", "Content-Type: application/json", body, false).await;
        assert!(result.is_ok());
        assert!(log.is_empty());
        assert!(!snapshot_valid(&serde_json::json!({"accounts":null})));
    }

    #[tokio::test]
    async fn proxy_diagnostics_use_the_extended_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let body = serde_json::json!({
            "success": true, "message": "fixture ok", "latency_ms": 31,
            "ip_address": "203.0.113.7", "city": "Taipei", "region": "Taiwan",
            "country": "Taiwan", "country_code": "TW"
        })
        .to_string();
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            std::thread::sleep(Duration::from_millis(200));
            let _ = stream.write_all(reply.as_bytes());
        });
        let temp = tempfile::tempdir().unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let result = http_detailed(
            &client,
            &base,
            "fixture-key",
            "POST",
            "/proxies/7/test",
            Some(serde_json::json!({})),
            &temp.path().join("network.log"),
        )
        .await;
        thread.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(!temp.path().join("network.log").exists());
    }

    #[tokio::test]
    async fn model_directory_routes_extend_timeout_without_replaying_writes() {
        for (method, path) in [
            ("GET", "/accounts/7/models?purpose=model_test"),
            ("POST", "/accounts/7/model-tests"),
            ("PUT", "/accounts/7/model-detection"),
        ] {
            assert!(model_directory_request(method, path));
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let expected_method = method.to_owned();
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let size = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..size]).starts_with(&expected_method));
                std::thread::sleep(Duration::from_millis(200));
                stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
                listener.set_nonblocking(true).unwrap();
                std::thread::sleep(Duration::from_millis(50));
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            });
            let directory = tempfile::tempdir().unwrap();
            let client = reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .timeout(Duration::from_millis(100))
                .build()
                .unwrap();
            let error = http_detailed(
                &client,
                &base,
                "fixture-admin-key",
                method,
                path,
                (method != "GET").then(|| serde_json::json!({})),
                &directory.path().join("network.log"),
            )
            .await
            .unwrap_err();
            assert!(
                error.message.contains("503"),
                "extended timeout must receive the HTTP response"
            );
            thread.join().unwrap();
        }
        for (method, path) in [
            ("GET", "/accounts/7/models"),
            ("GET", "/accounts/7/model-detection"),
            ("POST", "/accounts/7/model-detection"),
            ("GET", "/snapshot"),
            ("GET", "/accounts/0/models?purpose=model_test"),
        ] {
            assert!(!model_directory_request(method, path));
        }
    }

    #[test]
    fn proxy_diagnostic_responses_require_safe_known_shapes() {
        let test = serde_json::json!({
            "success": true, "message": format!("https://example.invalid/?token=fixture\n  password=fixture\n{}", "原始错误".repeat(1000)), "latency_ms": 31,
            "ip_address": "203.0.113.7", "city": "Taipei", "region": "Taiwan",
            "country": "Taiwan", "country_code": "TW"
        });
        assert!(shape_valid("/proxies/7/test", &test));
        for (key, value) in [
            ("success", serde_json::json!("true")),
            ("latency_ms", serde_json::json!(-1)),
            ("ip_address", serde_json::json!(7)),
            ("api_key", serde_json::json!("fixture-secret")),
        ] {
            let mut bad = test.clone();
            bad[key] = value;
            assert!(!shape_valid("/proxies/7/test", &bad), "{key}");
        }

        let quality = serde_json::json!({
            "score": 88, "grade": "yellow", "summary": "完整质量报告".repeat(1000),
            "exit_ip": "203.0.113.8", "country": "Taiwan", "country_code": "TW",
            "base_latency_ms": 20, "passed_count": 1, "warn_count": 0,
            "failed_count": 0, "challenge_count": 0, "checked_at": null,
            "items": [{"target": "https://example.invalid", "status": "pass",
                "http_status": 200, "latency_ms": 20, "message": "ok"}]
        });
        assert!(shape_valid("/proxies/7/quality-check", &quality));
        let mut bad = quality.clone();
        bad["score"] = serde_json::json!(101);
        assert!(!shape_valid("/proxies/7/quality-check", &bad));
        let mut bad = quality.clone();
        bad["items"][0]["status"] = serde_json::json!("unknown");
        assert!(!shape_valid("/proxies/7/quality-check", &bad));
        let mut bad = quality;
        bad["items"][0]["password"] = serde_json::json!("fixture-secret");
        assert!(!shape_valid("/proxies/7/quality-check", &bad));
    }

    #[test]
    fn recovery_response_accepts_legacy_and_complete_card_recovery_metadata() {
        let legacy = serde_json::json!([{"id":1,"account_id":1,"legacy":true}]);
        let ordinary =
            serde_json::json!([{"id":2,"account_id":1,"kind":"quota_recovery","legacy":false}]);
        let connection_test = serde_json::json!([{"id":4,"account_id":1,"kind":"connection_test","result_status":"completed","legacy":false}]);
        let card = serde_json::json!([{"id":3,"account_id":1,"kind":"reset_credit","legacy":false,
            "test_completed_at":"2026-10-01T01:11:00Z","recovered_at":"2026-10-01T01:12:00Z",
            "reset_credit":{"completed_at":"2026-10-01T01:10:00Z","verification_method":"connection"}}]);

        assert!(recoveries_valid(&legacy));
        assert!(recoveries_valid(&ordinary));
        assert!(recoveries_valid(&connection_test));
        assert!(recoveries_valid(&card));
        assert!(shape_valid(
            "/recoveries",
            &serde_json::json!({"items":card})
        ));
    }

    #[test]
    fn recovery_response_rejects_partial_or_invalid_card_recovery_metadata() {
        let valid = serde_json::json!({"id":1,"account_id":1,"kind":"reset_credit","legacy":false,
            "test_completed_at":"2026-10-01T01:11:00Z","recovered_at":"2026-10-01T01:12:00Z",
            "reset_credit":{"completed_at":"2026-10-01T01:10:00Z","verification_method":"model"}});
        let mut invalid = Vec::new();
        let mut row = valid.clone();
        row["reset_credit"] = serde_json::Value::Null;
        invalid.push(row);
        let mut row = valid.clone();
        row["reset_credit"]["completed_at"] = serde_json::json!("");
        invalid.push(row);
        let mut row = valid.clone();
        row["reset_credit"]["verification_method"] = serde_json::json!("unknown");
        invalid.push(row);
        let mut row = valid.clone();
        row["reset_credit"]["consumed"] = serde_json::json!(false);
        invalid.push(row);
        let mut row = valid.clone();
        row["legacy"] = serde_json::json!(true);
        invalid.push(row);
        let mut row = valid.clone();
        row["legacy"] = serde_json::Value::Null;
        invalid.push(row);
        let mut row = valid.clone();
        row["test_completed_at"] = serde_json::json!("");
        invalid.push(row);
        let mut row = valid.clone();
        row["recovered_at"] = serde_json::Value::Null;
        invalid.push(row);
        let mut row = valid;
        row["kind"] = serde_json::json!("unknown");
        invalid.push(row);

        for row in invalid {
            assert!(!recoveries_valid(&serde_json::json!([row])));
        }
        assert!(!recoveries_valid(&serde_json::json!({"items":[]})));
    }

    #[test]
    fn detection_platforms_accept_legacy_and_four_platform_configuration() {
        use serde_json::json;
        let mut config = json!({"oauth":{"revision":"fixture","model_detection_platforms":{"openai":{"engine":"modeltrace","sample_count":1}}},"bark":{"revision":"fixture"}});
        assert!(shape_valid("/config", &config));
        for platform in ["anthropic", "grok", "deepseek"] {
            config["oauth"]["model_detection_platforms"][platform] =
                json!({"engine":"lm_detector","sample_count":3});
        }
        assert!(shape_valid("/config", &config));
        config["oauth"]["model_detection_platforms"]["anthropic"]["engine"] = "modeltrace".into();
        assert!(!shape_valid("/config", &config));
        let mut detection = json!({"account_id":1,"version":"fixture","enabled":false,"interval_minutes":15,"model_id":"","model_source":"account","global_model_id":"","model_override":null,"status":"disabled","engine":"lm_detector","sample_count":1});
        assert!(shape_valid("/accounts/1/model-detection", &detection));
        detection["enabled"] = true.into();
        assert!(!shape_valid("/accounts/1/model-detection", &detection));
        detection["model_id"] = "claude-opus-4-6".into();
        assert!(shape_valid("/accounts/1/model-detection", &detection));
        detection["sample_count"] = 2.into();
        assert!(!shape_valid("/accounts/1/model-detection", &detection));
    }

    #[test]
    fn detection_templates_and_bank_response_guards() {
        let detection = serde_json::json!({"account_id":1,"version":"fixture","enabled":false,"interval_minutes":15,"model_id":"gpt-6-luna","status":"disabled"});
        assert!(shape_valid("/accounts/1/model-detection", &detection));
        let mut smart_detection = detection.clone();
        smart_detection["schedule_mode"] = serde_json::json!("smart");
        smart_detection["min_interval_minutes"] = serde_json::json!(5);
        smart_detection["effective_interval_minutes"] = serde_json::json!(10);
        smart_detection["recent_request_count"] = serde_json::json!(77);
        assert!(shape_valid("/accounts/1/model-detection", &smart_detection));
        for (field, value) in [
            ("schedule_mode", serde_json::json!("other")),
            ("min_interval_minutes", serde_json::json!(4)),
            ("min_interval_minutes", serde_json::json!(5.5)),
            ("effective_interval_minutes", serde_json::json!(31)),
            ("recent_request_count", serde_json::json!(-1)),
        ] {
            let mut invalid = smart_detection.clone();
            invalid[field] = value;
            assert!(!shape_valid("/accounts/1/model-detection", &invalid));
        }
        smart_detection["effective_interval_minutes"] = serde_json::Value::Null;
        smart_detection["recent_request_count"] = serde_json::Value::Null;
        assert!(shape_valid("/accounts/1/model-detection", &smart_detection));
        let mut global_detection = detection.clone();
        global_detection["model_source"] = serde_json::json!("global");
        assert!(shape_valid(
            "/accounts/1/model-detection",
            &global_detection
        ));
        let mut account_detection = global_detection.clone();
        account_detection["model_source"] = serde_json::json!("account");
        account_detection["model_override"] = serde_json::json!("fixture-provider-model");
        account_detection["global_model_id"] = serde_json::json!("gpt-6-luna");
        assert!(shape_valid(
            "/accounts/1/model-detection",
            &account_detection
        ));
        account_detection["model_override"] = serde_json::json!(false);
        assert!(!shape_valid(
            "/accounts/1/model-detection",
            &account_detection
        ));
        for invalid_source in [serde_json::json!("other"), serde_json::json!(true)] {
            let mut invalid = detection.clone();
            invalid["model_source"] = invalid_source;
            assert!(!shape_valid("/accounts/1/model-detection", &invalid));
        }
        let config = serde_json::json!({
            "oauth":{"revision":"fixture","model_detection_model_id":"gpt-6-luna"},
            "bark":{"revision":"fixture"}
        });
        assert!(shape_valid("/config", &config));
        let mut invalid_config = config.clone();
        invalid_config["oauth"]["model_detection_model_id"] = serde_json::json!(7);
        assert!(!shape_valid("/config", &invalid_config));
        for at in [
            serde_json::Value::Null,
            serde_json::json!("2026-10-01T02:30:00Z"),
        ] {
            let mut timed = detection.clone();
            timed["last_automatic_started_at"] = at;
            assert!(shape_valid("/accounts/1/model-detection", &timed));
        }
        let mut bad = detection.clone();
        bad["last_automatic_started_at"] = serde_json::json!(123);
        assert!(!shape_valid("/accounts/1/model-detection", &bad));
        let mut bad = detection.clone();
        bad["interval_minutes"] = serde_json::json!(0);
        assert!(!shape_valid("/accounts/1/model-detection", &bad));

        let profile = serde_json::json!({"whitelist":[],"mappings":[]});
        let templates = serde_json::json!({"version":"fixture","configured":false,"templates":{"full":profile,"degraded":profile,"takeover":profile}});
        assert!(shape_valid("/account-templates", &templates));
        assert!(!shape_valid("/account-templates", &serde_json::json!({})));
        let bank = serde_json::json!({"version":{"revision":"fixture","sha256":"fixture","analyzer_version":1},"source":"bundled","status":"idle"});
        assert!(shape_valid("/modeltrace/fingerprint-bank", &bank));
        assert!(!shape_valid(
            "/modeltrace/fingerprint-bank/sync",
            &serde_json::json!({})
        ));
    }

    #[test]
    fn detection_log_response_guard_accepts_the_projection_and_rejects_bad_fields() {
        let valid = serde_json::json!({
            "items":[{
                "job_id":"0123456789abcdef0123456789abcdef",
                "account_id":7,
                "account_name":"fixture account",
                "model_id":"gpt-6-sol",
                "status":"completed",
                "created_at":"2026-10-20T12:00:00Z",
                "started_at":null,
                "completed_at":"2026-10-20T12:00:00Z",
                "triggers":["scheduled"],
                "error":"",
                "details_available":false,
                "summary":{
                    "outcome":"unavailable",
                    "analysis_available":false,
                    "first_prediction":"",
                    "prediction":"",
                    "probability":null,
                    "first_probability":null,
                    "used_outputs":null,
                    "first_downgrade":null,
                    "aggregate_downgrade":null,
                    "disposition":null,
                    "recovery":null,
                    "verification":null,
                    "restoration":null
                },
                "notification":{
                    "status":"delivered",
                    "merged":false,
                    "delivered_at":"2026-10-20T12:00:01Z",
                    "reason":""
                }
            }],
            "next_cursor":"eyJmaXh0dXJlIjoxfQ"
        });
        assert!(shape_valid("/detection-logs", &valid));

        let mut malformed = Vec::new();
        let mut row = valid.clone();
        row["items"][0]["status"] = serde_json::json!("running");
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["summary"]["analysis_available"] = serde_json::json!("false");
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["summary"]["probability"] = serde_json::json!(1.01);
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["notification"]["merged"] = serde_json::json!("false");
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["summary"]["recovery"] = serde_json::json!({
            "status":"completed","reason":"fixture","completed_at":123,
            "marked":true,"schedule_verified":true,"mark_cleared":true
        });
        malformed.push(row);
        let mut row = valid.clone();
        row["items"][0]["job_id"] = serde_json::json!("invalid");
        malformed.push(row);
        let mut row = valid.clone();
        row["next_cursor"] = serde_json::json!("");
        malformed.push(row);

        for response in malformed {
            assert!(
                !shape_valid("/detection-logs", &response),
                "accepted malformed detection log response: {response}"
            );
        }
    }

    #[test]
    fn keepalive_response_shape_matches_account_and_bounds() {
        let valid = serde_json::json!({
            "account_id":7,
            "enabled":true,
            "read_only":false,
            "generation":3,
            "version":"a".repeat(64),
            "status":"waiting",
            "reason":"",
            "paused_reason":"",
            "last_restore_at":"2026-10-15T12:30:00Z",
            "restore_count":2,
            "budget_used":3,
            "next_allowed_at":null
        });
        assert!(shape_valid("/accounts/7/keepalive", &valid));
        assert!(!shape_valid("/accounts/8/keepalive", &valid));

        for status in [
            "disabled",
            "waiting",
            "paused",
            "running",
            "needs_confirmation",
        ] {
            let mut response = valid.clone();
            response["status"] = serde_json::json!(status);
            assert!(shape_valid("/accounts/7/keepalive", &response), "{status}");
        }
        for (restore_count, budget_used) in [(0, 0), (5, 5)] {
            let mut response = valid.clone();
            response["restore_count"] = serde_json::json!(restore_count);
            response["budget_used"] = serde_json::json!(budget_used);
            assert!(shape_valid("/accounts/7/keepalive", &response));
        }

        let mut malformed = Vec::new();
        let mut response = valid.clone();
        response["account_id"] = serde_json::json!(8);
        malformed.push(response);
        let mut response = valid.clone();
        response["enabled"] = serde_json::json!("true");
        malformed.push(response);
        let mut response = valid.clone();
        response["version"] = serde_json::json!("not-a-version");
        malformed.push(response);
        let mut response = valid.clone();
        response["generation"] = serde_json::json!(9_007_199_254_740_992_u64);
        malformed.push(response);
        let mut response = valid.clone();
        response["status"] = serde_json::json!("unknown");
        malformed.push(response);
        let mut response = valid.clone();
        response["restore_count"] = serde_json::json!(6);
        response["budget_used"] = serde_json::json!(6);
        malformed.push(response);
        let mut response = valid.clone();
        response["restore_count"] = serde_json::json!(2);
        response["budget_used"] = serde_json::json!(1);
        malformed.push(response);
        let mut response = valid.clone();
        response["next_allowed_at"] = serde_json::json!(123);
        malformed.push(response);
        let mut response = valid.clone();
        response["last_restore_at"] = serde_json::json!("");
        malformed.push(response);

        for response in malformed {
            assert!(
                !shape_valid("/accounts/7/keepalive", &response),
                "{response}"
            );
        }
    }

    #[test]
    fn connection_diagnostics_require_redacted_known_shape() {
        let status = serde_json::json!({"state":"auth_rejected","endpoint":"verify","http_status":403,
            "message":"管理员 API Key 被上游拒绝，请重新连接","retryable":false,"checked_at":"2026-10-07T00:00:00Z"});
        assert!(connection_valid(&status));
        assert!(shape_valid("/connection-status", &status));
        let mut invalid = status.clone();
        invalid["endpoint"] = serde_json::json!("https://private.invalid");
        assert!(!connection_valid(&invalid));
        invalid = status;
        invalid["state"] = serde_json::json!("unknown");
        assert!(!connection_valid(&invalid));
    }

    #[test]
    fn operation_response_requires_identity_and_known_state() {
        let job = serde_json::json!({"id":"0123456789abcdef0123456789abcdef", "account_id":421,
            "action":"priority", "status":"queued", "requested":{"priority":2}});
        assert!(shape_valid("/accounts/421/operations", &job));
        assert!(shape_valid(
            "/account-operations",
            &serde_json::json!({"pending":1,"items":[job.clone()]})
        ));
        let mut scheduled = job.clone();
        scheduled["action"] = "test".into();
        scheduled["scheduled_at"] = "2030-01-02T01:31:00+00:00".into();
        assert!(operation_valid(&scheduled));
        scheduled["scheduled_at"] = "2030-01-02T01:31:38+00:00".into();
        assert!(operation_valid(&scheduled));
        for invalid in [
            "2030-02-30T01:31:00+00:00",
            "2030-01-02T01:31:60+00:00",
            "2030-01-02T01:31:38.001+00:00",
            "invalid",
        ] {
            scheduled["scheduled_at"] = invalid.into();
            assert!(!operation_valid(&scheduled));
        }
        let mut wrong = job.clone();
        wrong["status"] = serde_json::json!("unknown");
        assert!(!shape_valid("/accounts/421/operations", &wrong));
        wrong = job.clone();
        wrong["account_id"] = serde_json::json!("421");
        assert!(!shape_valid("/accounts/421/operations", &wrong));
        assert!(!shape_valid(
            "/account-operations",
            &serde_json::json!({"items":[job]})
        ));
    }

    #[test]
    fn account_settings_and_proxies_reject_unsafe_or_malformed_fields() {
        let mut settings = serde_json::json!({"account_id":7,"priority":0,"concurrency":2,
            "load_factor":null,"effective_load_factor":2,"proxy_id":null,
            "model_config":{"whitelist":["fixture-text"],"mappings":[{"source":"fixture-*","target":"fixture-text"}]},
            "capabilities":{},"versions":{}});
        for action in [
            "priority",
            "concurrency",
            "load_factor",
            "model_config",
            "proxy",
            "sync_models",
        ] {
            settings["capabilities"][action] = serde_json::json!({"editable":true,"reason":""});
            settings["versions"][action] = serde_json::json!("a".repeat(64));
        }
        assert!(shape_valid("/accounts/7/settings", &settings));
        for (field, value) in [
            ("load_factor", serde_json::json!(10001)),
            ("proxy_id", serde_json::json!(-1)),
            ("priority", serde_json::json!("2")),
            ("credentials", serde_json::json!({"api_key":"fixture"})),
        ] {
            let mut bad = settings.clone();
            bad[field] = value;
            assert!(!shape_valid("/accounts/7/settings", &bad), "{field}");
        }
        let mut bad = settings.clone();
        bad["versions"]["proxy"] = serde_json::json!("unverified");
        assert!(!shape_valid("/accounts/7/settings", &bad));
        bad = settings.clone();
        bad["model_config"]["mappings"][0]["target"] = serde_json::json!(3);
        assert!(!shape_valid("/accounts/7/settings", &bad));
        let proxies = serde_json::json!([{"id":7,"name":"fixture proxy","protocol":"socks5","status":"active"}]);
        assert!(shape_valid("/proxies", &proxies));
        assert!(shape_valid("/proxies", &serde_json::json!([])));
        for key in ["password", "username", "url", "host", "port"] {
            let mut unsafe_proxy = proxies.clone();
            unsafe_proxy[0][key] = serde_json::json!("fixture");
            assert!(!shape_valid("/proxies", &unsafe_proxy), "{key}");
        }
    }

    #[test]
    fn empty_templates_and_full_batch_responses_are_validated() {
        let empty = serde_json::json!({"version":"fixture", "configured":true, "templates":{},
            "template_order":[],"template_meta":{},"template_versions":{}});
        assert!(shape_valid("/account-templates/full", &empty));
        let mut bad = empty.clone();
        bad["template_order"] = serde_json::json!(["full"]);
        assert!(!shape_valid("/account-templates", &bad));
        let id = "0123456789abcdef0123456789abcdef";
        let job = serde_json::json!({"id":id,"batch_id":id,"account_id":1,"action":"account_template","status":"completed","requested":{}});
        let mut batch = serde_json::json!({"batch_id":id,"pending":0,"items":vec![job; 40]});
        assert!(shape_valid("/account-templates/apply", &batch));
        assert!(shape_valid(
            &format!("/account-operations?batch_id={id}"),
            &batch
        ));
        batch["items"][0]["batch_id"] = serde_json::json!("ffffffffffffffffffffffffffffffff");
        assert!(!shape_valid("/account-templates/apply", &batch));
        assert!(!shape_valid("/account-templates/apply", &empty));
    }
    #[test]
    fn proxy_batch_response_requires_batch_not_single_operation() {
        let id = "0123456789abcdef0123456789abcdef";
        let job = serde_json::json!({"id":id,"batch_id":id,"account_id":1,"action":"proxy",
            "status":"queued","requested":{"proxy_id":null}});
        let batch = serde_json::json!({"batch_id":id,"pending":1,"items":[job.clone()]});
        assert!(shape_valid("/account-operations/batch", &batch));
        assert!(!shape_valid("/account-operations/batch", &job));
        let mut bad = batch;
        bad["items"][0]["batch_id"] = serde_json::Value::Null;
        assert!(!shape_valid("/account-operations/batch", &bad));
    }

    #[test]
    fn proxy_batch_response_rejects_wrong_action_and_proxy_payload_shape() {
        let id = "0123456789abcdef0123456789abcdef";
        let job = serde_json::json!({"id":id,"batch_id":id,"account_id":1,"action":"proxy",
            "status":"queued","requested":{"proxy_id":null}});
        let batch = serde_json::json!({"batch_id":id,"pending":1,"items":[job]});

        let mut wrong_action = batch.clone();
        wrong_action["items"][0]["action"] = serde_json::json!("account_template");
        assert!(!shape_valid("/account-operations/batch", &wrong_action));
        for value in [
            serde_json::json!(-1),
            serde_json::json!(0),
            serde_json::json!("2"),
        ] {
            let mut bad_proxy = batch.clone();
            bad_proxy["items"][0]["requested"]["proxy_id"] = value;
            assert!(!shape_valid("/account-operations/batch", &bad_proxy));
        }
        let mut missing_requested = batch.clone();
        missing_requested["items"][0]
            .as_object_mut()
            .unwrap()
            .remove("requested");
        assert!(!shape_valid(
            "/account-operations/batch",
            &missing_requested
        ));
        let mut missing_proxy_id = batch.clone();
        missing_proxy_id["items"][0]["requested"] = serde_json::json!({});
        assert!(!shape_valid("/account-operations/batch", &missing_proxy_id));
        let mut extra_proxy_field = batch;
        extra_proxy_field["items"][0]["requested"]["extra"] = serde_json::json!(true);
        assert!(!shape_valid(
            "/account-operations/batch",
            &extra_proxy_field
        ));
    }

    #[test]
    fn account_batch_response_validates_delete_and_group_payloads() {
        let id = "0123456789abcdef0123456789abcdef";
        let item = |action: &str, requested: Value| {
            serde_json::json!({
                "id": "abcdefabcdefabcdefabcdefabcdefab",
                "batch_id": id,
                "account_id": 7,
                "action": action,
                "status": "completed",
                "requested": requested,
            })
        };
        let batch = |job: Value| serde_json::json!({"batch_id": id, "pending": 0, "items": [job]});

        let delete = batch(item("delete", serde_json::json!({})));
        assert!(shape_valid("/account-operations/batch", &delete));
        let mut bad_delete = delete.clone();
        bad_delete["items"][0]["requested"]["unexpected"] = serde_json::json!(true);
        assert!(!shape_valid("/account-operations/batch", &bad_delete));
        let mut extra_delete = delete.clone();
        extra_delete["items"][0]["requested"]["extra"] = serde_json::json!(false);
        assert!(!shape_valid("/account-operations/batch", &extra_delete));

        let groups = batch(item(
            "groups",
            serde_json::json!({"scope_group_ids": [1, 2, 9], "group_ids": [1, 9]}),
        ));
        assert!(shape_valid("/account-operations/batch", &groups));
        let mut outside_scope = groups.clone();
        outside_scope["items"][0]["requested"]["group_ids"] = serde_json::json!([3]);
        assert!(!shape_valid("/account-operations/batch", &outside_scope));
        let mut empty_scope = groups.clone();
        empty_scope["items"][0]["requested"]["scope_group_ids"] = serde_json::json!([]);
        assert!(!shape_valid("/account-operations/batch", &empty_scope));
        let mut bad_id = groups.clone();
        bad_id["items"][0]["requested"]["scope_group_ids"] = serde_json::json!([0, 1]);
        assert!(!shape_valid("/account-operations/batch", &bad_id));
        let mut too_many = groups.clone();
        too_many["items"][0]["requested"]["scope_group_ids"] =
            serde_json::Value::Array((1..=101).map(|value| serde_json::json!(value)).collect());
        assert!(!shape_valid("/account-operations/batch", &too_many));
    }

    fn connection_test_batch_fixture() -> Value {
        serde_json::json!({
            "batch_id": "0123456789abcdef0123456789abcdef",
            "pending": 0,
            "items": [{
                "id": "abcdefabcdefabcdefabcdefabcdefab",
                "batch_id": "0123456789abcdef0123456789abcdef",
                "account_id": 7,
                "action": "test",
                "status": "completed",
                "requested": {"confirmed": true, "mode": "text", "model_id": "fixture-model"},
                "result": {
                    "success": true,
                    "requested_model": "fixture-model",
                    "actual_model": "fixture-mapped-model",
                    "duration_ms": 12,
                    "completed_at": "2026-10-13T00:00:00Z",
                    "error": null,
                    "uncertain": false
                }
            }]
        })
    }

    #[test]
    fn connection_batch_responses_accept_queue_results_and_cancellation() {
        let batch = connection_test_batch_fixture();
        for path in [
            "/account-operations/batch",
            "/account-operations?batch_id=0123456789abcdef0123456789abcdef",
            "/account-operations/batches/0123456789abcdef0123456789abcdef/cancel",
        ] {
            assert!(shape_valid(path, &batch), "{path}");
            for status in ["queued", "running", "cancelled"] {
                let mut pending = batch.clone();
                pending["items"][0]["status"] = serde_json::json!(status);
                pending["items"][0]["result"] = Value::Null;
                assert!(shape_valid(path, &pending), "{path} {status}");
            }
            let mut failed = batch.clone();
            failed["items"][0]["status"] = serde_json::json!("failed");
            failed["items"][0]["result"]["success"] = serde_json::json!(false);
            failed["items"][0]["result"]["actual_model"] = Value::Null;
            failed["items"][0]["result"]["error"] = serde_json::json!("fixture failure");
            failed["items"][0]["result"]["uncertain"] = serde_json::json!(true);
            assert!(shape_valid(path, &failed), "{path}");
        }
        let mut default_mode = batch;
        default_mode["items"][0]["requested"]["mode"] = serde_json::json!("default");
        default_mode["items"][0]["requested"]["model_id"] = serde_json::json!("");
        default_mode["items"][0]["result"]["requested_model"] = serde_json::json!("");
        assert!(shape_valid("/account-operations/batch", &default_mode));
    }

    #[test]
    fn recovery_batch_accepts_empty_requests_and_typed_results() {
        let mut batch = connection_test_batch_fixture();
        batch["items"][0]["action"] = serde_json::json!("recover");
        batch["items"][0]["requested"] = serde_json::json!({});
        batch["items"][0]["result"] = serde_json::json!({"verified":true,"unchanged":true});
        for path in [
            "/account-operations/batch",
            "/account-operations?batch_id=0123456789abcdef0123456789abcdef",
        ] {
            assert!(shape_valid(path, &batch));
            for invalid in [
                serde_json::json!({"verified":"true"}),
                serde_json::json!({"verified":true,"unchanged":"true"}),
            ] {
                let mut bad = batch.clone();
                bad["items"][0]["result"] = invalid;
                assert!(!shape_valid(path, &bad));
            }
            let mut bad = batch.clone();
            bad["items"][0]["requested"] = serde_json::json!({"schedulable":true});
            assert!(!shape_valid(path, &bad));
            let mut queued = batch.clone();
            queued["items"][0]["status"] = serde_json::json!("queued");
            queued["items"][0]["result"] = Value::Null;
            assert!(shape_valid(path, &queued));
        }
    }

    #[test]
    fn connection_batch_responses_reject_invalid_request_and_batch_shapes() {
        let batch = connection_test_batch_fixture();
        for path in [
            "/account-operations/batch",
            "/account-operations/batches/0123456789abcdef0123456789abcdef/cancel",
        ] {
            assert!(!shape_valid(path, &batch["items"][0]), "{path}");
            for requested in [
                serde_json::json!({"confirmed": false, "mode": "text", "model_id": "fixture-model"}),
                serde_json::json!({"confirmed": "true", "mode": "text", "model_id": "fixture-model"}),
                serde_json::json!({"confirmed": true, "mode": "image", "model_id": "fixture-model"}),
                serde_json::json!({"confirmed": true, "mode": "text", "model_id": 7}),
                serde_json::json!({"confirmed": true, "mode": "text", "model_id": "x".repeat(201)}),
                serde_json::json!({"confirmed": true, "mode": "text"}),
                serde_json::json!({"confirmed": true, "mode": "text", "model_id": "fixture-model", "extra": true}),
            ] {
                let mut bad = batch.clone();
                bad["items"][0]["requested"] = requested;
                assert!(!shape_valid(path, &bad), "{path}");
            }
            let mut wrong_batch = batch.clone();
            wrong_batch["items"][0]["batch_id"] =
                serde_json::json!("ffffffffffffffffffffffffffffffff");
            assert!(!shape_valid(path, &wrong_batch), "{path}");
        }
    }

    #[test]
    fn connection_batch_responses_reject_malformed_result_fields() {
        let batch = connection_test_batch_fixture();
        for path in [
            "/account-operations/batch",
            "/account-operations?batch_id=0123456789abcdef0123456789abcdef",
            "/account-operations/batches/0123456789abcdef0123456789abcdef/cancel",
        ] {
            for (field, value) in [
                ("success", serde_json::json!("true")),
                ("requested_model", serde_json::json!(7)),
                ("requested_model", serde_json::json!("x".repeat(201))),
                ("actual_model", serde_json::json!(7)),
                ("actual_model", serde_json::json!("x".repeat(201))),
                ("duration_ms", serde_json::json!(-1)),
                ("duration_ms", serde_json::json!(1.5)),
                ("completed_at", Value::Null),
                ("error", serde_json::json!({"message": "fixture failure"})),
                ("error", serde_json::json!("x".repeat(1025))),
                ("uncertain", serde_json::json!("true")),
            ] {
                let mut bad = batch.clone();
                bad["items"][0]["result"][field] = value;
                assert!(!shape_valid(path, &bad), "{path} {field}");
            }
            for field in ["success", "requested_model", "duration_ms", "completed_at"] {
                let mut missing = batch.clone();
                missing["items"][0]["result"]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                assert!(!shape_valid(path, &missing), "{path} {field}");
            }
            let mut wrong_result = batch.clone();
            wrong_result["items"][0]["result"] = serde_json::json!([]);
            assert!(!shape_valid(path, &wrong_result), "{path}");
        }
    }

    #[test]
    fn snapshot_recovery_monitor_is_optional_but_typed_when_present() {
        let account = serde_json::json!({
            "id": 1, "name": "fixture", "platform": "openai", "type": "oauth", "version": "v1",
            "group_ids": [], "usage_windows": [], "blockers": [], "schedulable": true,
            "available": true, "priority": 0
        });
        let mut snapshot = serde_json::json!({
            "observed_at": "2026-10-11T00:00:00Z", "accounts": [account], "groups": [],
            "errors": [], "recoveries": []
        });
        assert!(snapshot_valid(&snapshot));
        snapshot["accounts"][0]["recovery_monitor"] = serde_json::Value::Null;
        assert!(snapshot_valid(&snapshot));
        snapshot["accounts"][0]["recovery_monitor"] = serde_json::json!({
            "status": "scheduled", "label": "额度恢复监控", "reason": "等待下次检查", "next_at": null
        });
        assert!(snapshot_valid(&snapshot));
        snapshot["accounts"][0]["recovery_monitor"]["next_at"] =
            serde_json::json!("2026-10-11T01:00:00Z");
        assert!(snapshot_valid(&snapshot));
        snapshot["accounts"][0]["recovery_monitor"]["next_at"] = serde_json::json!(3600);
        assert!(!snapshot_valid(&snapshot));
    }
}
