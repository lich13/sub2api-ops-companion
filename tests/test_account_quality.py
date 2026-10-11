import asyncio
import json
import threading
import time
import unittest
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import MagicMock, Mock, patch

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient
from app.account_quality import (ERROR_CURVE, PERF_CURVE, QualityCache, build_baselines, calculate,
    failure_cause, failures, interpolate, metric_result, performance, quantile, read_evidence)
from app.desktop_api import DesktopService, install_desktop_api

NOW = datetime(2026, 9, 26, 8, tzinfo=timezone.utc)
ACCOUNTS = [{"id": i, "platform": "openai", "type": "oauth" if i == 1 else "apikey"} for i in (1, 2, 3)]


def usage(account=1, count=30, *, first=2000, tps=40, hours=2, **overrides):
    return [{"id": account * 10000 + n, "account_id": account, "created_at": NOW - timedelta(hours=hours, seconds=n),
        "model": "requested", "upstream_model": "gpt-6-sol", "stream": True, "first_token_ms": first,
        "duration_ms": first + 1000000 / tps, "output_tokens": 1000, "input_tokens": 4096,
        "cache_read_tokens": 0, "cache_creation_tokens": 0,
        "reasoning_effort": "max", "service_tier": "default", **overrides} for n in range(count)]


def error(account=1, idx=1, minutes=3, **overrides):
    return {"id": idx, "account_id": account, "request_id": f"request-{idx}", "created_at": NOW - timedelta(minutes=minutes),
        "error_owner": "provider", "error_phase": "upstream", "upstream_status_code": 502,
        "error_message": "upstream failed", **overrides}


def result(rows=None, errors=None, accounts=ACCOUNTS):
    return calculate(accounts, rows if rows is not None else sum((usage(i) for i in (1, 2, 3)), []), errors or [], NOW)[0]


class EvidenceTests(unittest.TestCase):
    def test_account_attribution_retry_dedup_and_recovered_success(self):
        events = [{"account_id": a, "kind": "retry", "upstream_status_code": 502} for a in (1, 1, 2)]
        rows = [error(3, upstream_errors=events), error(3, upstream_errors=events)]
        output = result(errors=rows)
        self.assertEqual([output[i]["reliability"]["failures"] for i in (1, 2, 3)], [1, 1, 0])
        self.assertEqual(output[1]["reliability"]["successes"], 30)
        self.assertAlmostEqual(output[1]["reliability"]["rate"], 1 / 31)

    def test_detail_array_without_account_never_falls_back_to_envelope(self):
        self.assertEqual(failures([error(upstream_errors=[{"kind": "failover"}])], {1: ACCOUNTS[0]}, NOW), [])

    def test_exclusions_are_specific_not_all_429_or_all_400(self):
        excluded = [(429, "The usage limit has been reached"), (429, "insufficient_quota"),
            (429, "Client Key 限定范围中的可用账号额度等待恢复"), (429, "客户端 API Key 已超过 RPM 限制"),
            (400, "invalid_request_error"), (403, "content_policy_violation"), (499, "cancelled"), (500, "context canceled")]
        for code, message in excluded:
            with self.subTest(message=message): self.assertIsNone(failure_cause({"status_code": code, "message": message}))
        for code, message, cause in [(429,"Rate limit exceeded","rate"),(400,"provider exploded","upstream"),
            (401,"token invalid","auth"),(504,"gateway timeout","timeout"),(502,"stream_failed","stream"),(500,"connection reset","network")]:
            with self.subTest(message=message): self.assertEqual(failure_cause({"status_code": code, "message": message}), cause)

    def test_platform_errors_deleted_accounts_and_out_of_range_are_excluded(self):
        output = result(errors=[error(error_owner="platform"), error(account=None), error(account=999), error(minutes=8*24*60)])
        self.assertEqual(output[1]["reliability"]["failures"], 0)
        self.assertNotIn(3, result(accounts=[*ACCOUNTS[:2], {**ACCOUNTS[2], "deleted_at": NOW}]))

    def test_detail_event_timestamp_and_request_namespace(self):
        event = {"account_id": 1, "at_unix_ms": (NOW-timedelta(seconds=10)).timestamp()*1000, "upstream_status_code":401}
        found = failures([error(minutes=30,upstream_errors=[event])], {1:ACCOUNTS[0]}, NOW)
        self.assertEqual(found[0]["at"], NOW-timedelta(seconds=10))

    def test_tps_uses_total_duration_and_rejects_invalid_or_media(self):
        row = usage()[0]
        expected_tps = row["output_tokens"] / (row["duration_ms"] / 1000)
        self.assertAlmostEqual(performance(row,ACCOUNTS[0])["tps"],expected_tps)
        missing_ttft = performance({**row,"first_token_ms":None},ACCOUNTS[0])
        self.assertIsNotNone(missing_ttft)
        self.assertIsNone(missing_ttft["ttft"])
        self.assertAlmostEqual(missing_ttft["tps"],expected_tps)
        for first in (0,-1,True,float('nan'),'invalid'):
            with self.subTest(first_token_ms=first):
                invalid_ttft=performance({**row,"first_token_ms":first},ACCOUNTS[0])
                self.assertIsNotNone(invalid_ttft)
                self.assertIsNone(invalid_ttft["ttft"])
                self.assertAlmostEqual(invalid_ttft["tps"],expected_tps)
        equal_timing=performance({**row,"duration_ms":row["first_token_ms"]},ACCOUNTS[0])
        self.assertIsNotNone(equal_timing)
        self.assertIsNone(equal_timing["ttft"])
        self.assertAlmostEqual(equal_timing["tps"],row["output_tokens"]/(row["first_token_ms"]/1000))
        for change in ({"stream":False},{"duration_ms":0},{"duration_ms":True},{"duration_ms":float('nan')},{"duration_ms":"invalid"},
                       {"output_tokens":0},{"input_tokens":None},{"image_count":1},{"video_count":1},
                       {"inbound_endpoint":"/v1/audio/transcriptions"},{"upstream_model":"grok-tts"},
                       {"billing_mode":"image"}):
            with self.subTest(change=change): self.assertIsNone(performance({**row,**change},ACCOUNTS[0]))
        for change in ({"output_tokens":31},{"duration_ms":499}):
            self.assertIsNone(performance({**row,**change},ACCOUNTS[0])["tps"])
        self.assertIsNotNone(performance({**row,"output_tokens":32,"duration_ms":500},ACCOUNTS[0])["tps"])

    def test_duplicate_usage_ids_and_invalid_dates(self):
        rows = usage()
        output = result(rows + rows + usage(2,created_at='bad'))[1]
        self.assertEqual(output['reliability']['successes'],30)

    def test_context_buckets_include_cached_input(self):
        row=usage()[0]
        self.assertEqual(performance({**row,'cache_read_tokens':50000},ACCOUNTS[0])['cohort'][-1],'32–128K')
        for invalid in (-1, "invalid", True, float("nan")):
            with self.subTest(cache_creation_tokens=invalid):
                self.assertIsNone(performance({**row,'cache_creation_tokens':invalid},ACCOUNTS[0]))

    def test_missing_or_null_optional_cache_counts_keep_timing_and_mark_unknown_input(self):
        row = usage()[0]
        expected_tps = row["output_tokens"] / (row["duration_ms"] / 1000)
        variants = [
            {key: value for key, value in row.items() if key != "cache_read_tokens"},
            {**row, "cache_read_tokens": None},
            {key: value for key, value in row.items() if key != "cache_creation_tokens"},
            {**row, "cache_creation_tokens": None},
            {**row, "cache_read_tokens": None, "cache_creation_tokens": None},
        ]
        for variant in variants:
            with self.subTest(cache_fields={key: variant.get(key) for key in ("cache_read_tokens", "cache_creation_tokens")}):
                sample = performance(variant, ACCOUNTS[0])
                self.assertIsNotNone(sample)
                self.assertAlmostEqual(sample["ttft"], row["first_token_ms"] / 1000)
                self.assertAlmostEqual(sample["tps"], expected_tps)
                self.assertEqual(sample["cohort"][-1], "未知")


class ScoringTests(unittest.TestCase):
    def test_curves_percentiles_and_boundaries(self):
        self.assertEqual(quantile([1,2,3,4],.5),2.5)
        for anchors in (ERROR_CURVE,PERF_CURVE):
            for x,y in anchors: self.assertAlmostEqual(interpolate(x,anchors),y)
        self.assertAlmostEqual(interpolate(.02,ERROR_CURVE),82.5)
        self.assertEqual(interpolate(.9,ERROR_CURVE),0)
        self.assertEqual(interpolate(10,PERF_CURVE),100)

    def test_equal_performance_scores_and_oauth_key_same_standard(self):
        output = result()
        self.assertEqual([output[i]['score'] for i in (1,2,3)],[93,93,93])
        self.assertTrue(all(r['grade']=='green' for r in output.values()))

    def test_fast_errors_cannot_hide_failure_ratio(self):
        rows = usage(1,tps=1000,first=50) + usage(2) + usage(3)
        for count,cap in ((1,84),(4,59)):
            out=result(rows,[error(idx=i,minutes=60+i) for i in range(count)])[1]
            self.assertLessEqual(out['score'],cap)
            self.assertIn('错误偏多',out['reasons'])

    def test_three_distinct_failures_force_red_with_insufficient_samples(self):
        errors=[error(idx=i,minutes=6-i,upstream_status_code=401) for i in range(3)]
        out=result([],errors)[1]
        self.assertIsNone(out['score']);self.assertEqual(out['grade'],'red')
        self.assertEqual(out['reasons'],['连续失败','认证失败'])
        out=result(usage(count=1,hours=0),errors)[1]
        self.assertEqual(out['grade'],'yellow')
        self.assertEqual(out['reliability']['failures'],3)
        out=result([],errors[:1]*3)[1]
        self.assertEqual(out['consecutive_failures'],1)
        out=result([],[{**e,'request_id':None} for e in errors])[1]
        self.assertEqual(out['consecutive_failures'],0)
        self.assertEqual(out['reliability']['failures'],3)

    def test_absolute_severe_performance_caps_without_a_baseline(self):
        for first,tps,label in ((61000,40,'首字偏慢'),(2000,1.5,'输出偏慢')):
            out=result(usage(first=first,tps=tps))[1]
            self.assertIsNone(out['score']);self.assertEqual(out['grade'],'red')
            self.assertIn(label,out['reasons'])
        rows=usage(1,first=31000)+usage(2,first=31000)+usage(3,first=31000)
        self.assertEqual(result(rows)[1]['score'],79)

    def test_sample_gates_and_coverage(self):
        empty=result([])[1]
        self.assertEqual(empty['reasons'],['调用不足'])
        self.assertEqual(empty['reliability']['missing_reason'],'insufficient_calls')
        insufficient=result(usage(count=19))[1]
        self.assertEqual(insufficient['sample_status'],'insufficient')
        self.assertIsNone(insufficient['score'])
        self.assertEqual(insufficient['reliability']['missing_reason'],'insufficient_calls')
        rows=usage(1,count=9)+usage(2)+usage(3)
        self.assertIsNone(result(rows)[1]['score'])
        rows=usage(1,count=30)+usage(2)+usage(3)+usage(1,count=30,upstream_model='unique',id=999999)
        # One unique ID contributes once, not 30 times.
        self.assertAlmostEqual(result(rows)[1]['coverage'],30/31)
        unique=[{**r,'id':r['id']+900000} for r in usage(1,count=30,upstream_model='unique')]
        out=result(usage(1)+usage(2)+usage(3)+unique)[1]
        self.assertEqual(out['coverage'],.5);self.assertIsNone(out['score'])

    def test_model_effort_service_transport_and_platform_never_cross(self):
        for field,value in [('upstream_model','different'),('reasoning_effort','low'),('service_tier','priority'),('openai_ws_mode',True)]:
            with self.subTest(field=field):
                out=result(usage(1)+usage(2,**{field:value})+usage(3,**{field:value}))[1]
                self.assertEqual(out['coverage'],0)
        out=result(accounts=[ACCOUNTS[0],{**ACCOUNTS[1],'platform':'grok'},{**ACCOUNTS[2],'platform':'grok'}])[1]
        self.assertEqual(out['coverage'],0)

    def test_only_input_bucket_fallback_and_equal_account_weight_baseline(self):
        rows=usage(1,count=100,first=1000)+usage(2,count=10,first=10000,input_tokens=64000)+usage(3,count=10,first=20000,input_tokens=64000)
        out=result(rows)[1]
        cohort=out['ttft']['all']['cohorts'][0]
        self.assertEqual(cohort['input_bucket'],'全部输入规模')
        self.assertEqual(cohort['baseline_p50'],10)
        self.assertEqual(cohort['baseline_accounts'],3)
        self.assertEqual(cohort['baseline_samples'],120)

    def test_heavy_and_light_models_normalize_independently(self):
        accounts=ACCOUNTS + [{**a,'id':a['id']+3} for a in ACCOUNTS]
        rows=sum((usage(i,first=15000,tps=10,upstream_model='heavy') for i in (1,2,3)),[])
        rows+=sum((usage(i,first=1000,tps=100,upstream_model='light') for i in (4,5,6)),[])
        out=result(rows,accounts=accounts)
        self.assertEqual(out[1]['score'],out[4]['score'])

    def test_recent_weight_and_insufficient_recent_fall_back(self):
        recent=usage(1,count=20,first=1000)
        history=[{**r,'id':r['id']+100000} for r in usage(1,count=30,hours=48,first=10000)]
        peers=usage(2,count=50)+usage(3,count=50)
        out=result(recent+history+peers, [error(idx=i,minutes=180) for i in range(5)])[1]
        rel=out['reliability']
        self.assertEqual(rel['mode'],'70/30');self.assertAlmostEqual(rel['effective_rate'],.7*.2)
        self.assertEqual(out['ttft']['mode'],'70/30')
        out=result(recent[:5]+history+peers)[1]
        self.assertEqual(out['reliability']['mode'],'7d');self.assertEqual(out['ttft']['mode'],'7d')

    def test_normal_quota_does_not_lower_quality(self):
        out=result(errors=[error(idx=i,upstream_status_code=429,error_message='The usage limit has been reached') for i in range(20)])[1]
        self.assertEqual(out['score'],93)

    def test_recent_unknown_model_is_not_hidden_by_comparable_old_history(self):
        old=[{**r,'id':r['id']+100000} for r in usage(1,hours=48)]
        out=result(usage(1,upstream_model='unknown-new')+old+usage(2)+usage(3))[1]
        self.assertEqual(out['ttft']['mode'],'24h')
        self.assertEqual(out['coverage'],0)
        self.assertIsNone(out['score'])
        self.assertEqual(out['ttft']['missing_reason'],'no_comparable_baseline')
        self.assertEqual(out['throughput']['missing_reason'],'no_comparable_baseline')
        self.assertEqual(out['reasons'],['暂无可比基准'])

    def test_missing_ttft_does_not_become_fast_zero_or_drop_tps(self):
        out=result(sum((usage(i,first_token_ms=None) for i in (1,2,3)),[]))[1]
        self.assertIsNone(out['score']);self.assertIsNone(out['ttft']['p50'])
        self.assertEqual(out['ttft']['samples'],0)
        self.assertEqual(out['ttft']['missing_reason'],'first_token_missing')
        self.assertEqual(out['throughput']['samples'],30)
        self.assertGreater(out['throughput']['p50'],0)
        self.assertIsNone(out['throughput']['missing_reason'])
        self.assertEqual(out['reasons'],['首字未记录'])

    def test_unavailable_throughput_is_reported_without_fabricating_a_score(self):
        out=result(sum((usage(i,output_tokens=31) for i in (1,2,3)),[]))[1]
        self.assertIsNone(out['score'])
        self.assertEqual(out['ttft']['samples'],30)
        self.assertEqual(out['throughput']['samples'],0)
        self.assertEqual(out['throughput']['compared'],0)
        self.assertEqual(out['throughput']['missing_reason'],'throughput_unavailable')

    def test_quality_score_keeps_50_25_25_weights(self):
        reliability_result={"total":20,"successes":20,"failures":0,"rate":0,"score":80,
            "effective_rate":0,"causes":{},"recent":{},"history":{},"mode":"7d"}
        metric_result_value={"samples":10,"compared":10,"coverage":1,"score":60,"p50":10,"tail":10,
            "cohorts":[],"recent":{},"history":{},"all":{},"mode":"7d"}
        with patch('app.account_quality.reliability',return_value=reliability_result), \
             patch('app.account_quality.metric_result',side_effect=[metric_result_value, {**metric_result_value,'score':40}]):
            out=calculate([ACCOUNTS[0]],usage(1,count=20),[],NOW,baselines={})[0][1]
        self.assertEqual(out['score'],65)


class CacheTests(unittest.TestCase):
    def test_single_inflight_nonblocking_30s_refresh_and_5min_baselines(self):
        entered,release=threading.Event(),threading.Event()
        tick=[0.0]
        def reader(*_): entered.set();release.wait(2);return ACCOUNTS,sum((usage(i) for i in (1,2,3)),[]),[]
        read=Mock(side_effect=reader)
        cache=QualityCache(None,reader=read,clock=lambda:tick[0],utcnow=lambda:NOW)
        self.addCleanup(cache.close)
        self.assertEqual(cache.get([1])[1]['sample_status'],'pending')
        self.assertTrue(entered.wait(1))
        for _ in range(20):cache.get([1])
        self.assertEqual(read.call_count,1)
        release.set();cache._thread.join(2)
        baseline=cache._baselines
        self.assertEqual(cache.get([1])[1]['score'],93)
        tick[0]=31;cache.get([1]);cache._thread.join(2)
        self.assertIs(cache._baselines,baseline)
        tick[0]=301;cache.get([1]);cache._thread.join(2)
        self.assertIsNot(cache._baselines,baseline)

    def test_query_failure_retains_results_then_expires_and_recovers(self):
        tick=[0.0]
        read=Mock(return_value=(ACCOUNTS,sum((usage(i) for i in (1,2,3)),[]),[]))
        cache=QualityCache(None,reader=read,clock=lambda:tick[0],utcnow=lambda:NOW)
        self.addCleanup(cache.close)
        cache.get([1]);cache._thread.join(2)
        previous=cache.get([1],detail=True)[1]
        read.side_effect=TimeoutError('PRIVATE SQL marker')
        tick[0]=31;cache.get([1]);cache._thread.join(2)
        out=cache.get([1],detail=True)[1]
        self.assertEqual(out['score'],previous['score'])
        self.assertEqual(out['computed_at'],previous['computed_at'])
        self.assertEqual(out['reliability'],previous['reliability'])
        self.assertEqual(out['ttft'],previous['ttft'])
        self.assertEqual(out['throughput'],previous['throughput'])
        self.assertEqual(out['data_status'],'delayed')
        self.assertEqual(out['reasons'],['统计更新失败'])
        self.assertEqual(out['diagnostic']['stage'],'usage')
        self.assertEqual(out['diagnostic']['kind'],'query_timeout')
        self.assertEqual(out['diagnostic']['checked_at'],NOW.isoformat())
        tick[0]=301
        out=cache.get([1])[1];cache._thread.join(2)
        self.assertEqual(out['score'],previous['score'])
        self.assertEqual(out['computed_at'],previous['computed_at'])
        self.assertEqual(out['data_status'],'stale')
        self.assertEqual(out['reasons'],['统计更新失败'])
        read.side_effect=None;tick[0]=332;cache.get([1]);cache._thread.join(2)
        self.assertEqual(cache.get([1])[1]['score'],93)

    def test_read_transaction_is_readonly_bounded_and_whitelisted(self):
        db=MagicMock();conn=db.connection.return_value.__enter__.return_value
        def execute(sql, params=None):
            cursor=MagicMock()
            cursor.fetchall.return_value=(
                [{"attname": name} for name in ("upstream_model", "duration_ms")]
                if "pg_attribute" in sql else [])
            return cursor
        conn.execute.side_effect=execute
        self.assertEqual(read_evidence(db,NOW),([],[],[]))
        sql='\n'.join(call.args[0] for call in conn.execute.call_args_list)
        self.assertIn('READ ONLY',sql);self.assertIn('statement_timeout',sql)
        self.assertIn("pg_attribute",sql)
        self.assertIn("u.upstream_model",sql)
        self.assertIn("NULL AS cache_read_tokens",sql)
        self.assertIn("NULL AS video_count",sql)
        for hidden in ('credentials','request_body','error_body','upstream_error_detail'):
            self.assertNotIn(hidden,sql)
        self.assertNotIn('to_jsonb(u)',sql)
        for forbidden in ('UPDATE ','DELETE ','INSERT '):self.assertNotIn(forbidden,sql)

    def test_detail_auth_deleted_filter_and_no_upstream_scoring(self):
        db=Mock();db.fetch_one.return_value={'id':1}
        runtime=SimpleNamespace(db=db,APP_VERSION='qa')
        app=FastAPI();service=install_desktop_api(app,runtime)
        self.addCleanup(service.quality.close)
        with patch.object(service,'authenticate') as auth,patch.object(service.quality,'get',return_value={1:{'score':None,'reasons':['样本不足']}}):
            client=TestClient(app)
            self.assertEqual(client.get('/api/desktop/v1/accounts/1/quality',headers={'x-api-key':'qa'}).json()['score'],None)
            auth.assert_called_with('qa',fresh=False)
            self.assertIn('deleted_at IS NULL',db.fetch_one.call_args.args[0])
            db.fetch_one.return_value=None
            self.assertEqual(client.get('/api/desktop/v1/accounts/1/quality').status_code,404)
            auth.side_effect=HTTPException(401,'invalid')
            self.assertEqual(client.get('/api/desktop/v1/accounts/1/quality').status_code,401)
