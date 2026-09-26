"""Disposable PostgreSQL/Sub2API verification; never targets production."""
import json
import os
import time
from datetime import datetime, timedelta, timezone
from unittest.mock import patch
from urllib.parse import urlparse

import psycopg
from psycopg.rows import dict_row
from psycopg.types.json import Jsonb

from app.account_quality import QualityCache, calculate, read_evidence
from app.db import Database
from scripts.desktop_integration import DB, ops

if os.environ.get("DESKTOP_QA_DISPOSABLE") != "sub2ops-desktop" or urlparse(DB).hostname != "pg":
    raise SystemExit("Refusing: disposable QA database required")


def main():
    now = datetime.now(timezone.utc)
    with psycopg.connect(DB, row_factory=dict_row) as conn:
        ids = [r['id'] for r in conn.execute("SELECT id FROM accounts WHERE name IN ('QA Shared','QA Secondary','QA Third') ORDER BY id")]
        assert len(ids) == 3
        row = conn.execute("SELECT user_id,api_key_id,group_id FROM usage_logs LIMIT 1").fetchone()
        before = conn.execute("SELECT id,priority,schedulable,updated_at FROM accounts ORDER BY id").fetchall()
        seeded = conn.execute("SELECT count(*) AS n FROM usage_logs WHERE upstream_model='gpt-6-sol'").fetchone()['n']
        assert seeded in (0,90), 'Unexpected QA data; do not overwrite it'
        for account_id in ids:
            for n in range(0 if seeded else 30):
                conn.execute("""INSERT INTO usage_logs(user_id,api_key_id,account_id,group_id,model,upstream_model,
                    stream,input_tokens,output_tokens,duration_ms,first_token_ms,reasoning_effort,created_at)
                    VALUES (%s,%s,%s,%s,'requested','gpt-6-sol',true,4096,1000,27000,2000,'max',%s)""",
                    (row['user_id'],row['api_key_id'],account_id,row['group_id'],now-timedelta(hours=2,seconds=n)))
        details = [{'account_id':ids[1],'upstream_status_code':502,'kind':'failover','message':'upstream failure',
                    'request_body':'PRIVATE_PROMPT','credentials':{'token':'PRIVATE_TOKEN'}}] * 3
        if not seeded:
            conn.execute("""INSERT INTO ops_error_logs(account_id,platform,error_phase,error_owner,error_type,status_code,
                error_message,request_id,upstream_errors,created_at) VALUES (%s,'openai','upstream','provider','upstream_error',200,
                'recovered failure','quality-qa-retry',%s,%s)""",(ids[0],Jsonb(details),now-timedelta(minutes=5)))
    db = Database(DB);db.open()
    try:
        with patch('urllib.request.urlopen', side_effect=AssertionError('upstream request forbidden')):
            evidence = read_evidence(db,now)
            results,_ = calculate(*evidence,now)
            assert results[ids[1]]['reliability']['failures'] == 1
            assert results[ids[1]]['reliability']['successes'] >= 30
            assert results[ids[1]]['score'] <= 84
            assert results[ids[2]]['score'] == 93
            assert 'PRIVATE_' not in json.dumps(results,default=str)
            with db.connection() as conn, conn.transaction():
                conn.execute('SET TRANSACTION READ ONLY')
                try:
                    with conn.transaction():conn.execute('UPDATE accounts SET priority=999')
                    raise AssertionError('readonly transaction permitted writing')
                except psycopg.errors.ReadOnlySqlTransaction:pass
            # A real PostgreSQL timeout is isolated to the quality worker.
            def slow_reader(database, _now):
                with database.connection() as conn, conn.transaction():
                    conn.execute('SET TRANSACTION READ ONLY')
                    conn.execute("SET LOCAL statement_timeout='150ms'")
                    conn.execute('SELECT pg_sleep(1)')
            cache=QualityCache(db,reader=slow_reader)
            start=time.monotonic();cache.get(ids)
            assert time.monotonic()-start < .1
            assert db.fetch_one('SELECT 1 AS ok')['ok']==1
            cache._thread.join(2)
            assert cache.get(ids)[ids[0]]['data_status']=='delayed'
            cache.close()
        # Authenticated HTTP route returns cached evidence; pending is explicit.
        status,detail=ops(f'/accounts/{ids[2]}/quality');assert status==200
        for _ in range(180):
            if detail.get('reliability',{}).get('successes') == results[ids[2]]['reliability']['successes']:break
            time.sleep(.2)
            status,detail=ops(f'/accounts/{ids[2]}/quality')
        assert detail['reliability']['successes']==results[ids[2]]['reliability']['successes']
        # The first snapshot may have built an empty 5-minute baseline before
        # fixture insertion. It must remain explicitly uncovered, not score 100.
        if detail['coverage'] >= .6:
            assert detail['score']==93
        else:
            assert detail['score'] is None and detail['sample_status']=='insufficient'
        assert ops(f'/accounts/{ids[0]}/quality',key='invalid')[0]==401
        assert ops('/accounts/9999999/quality')[0]==404
        with psycopg.connect(DB,row_factory=dict_row) as conn:
            assert conn.execute('SELECT id,priority,schedulable,updated_at FROM accounts ORDER BY id').fetchall()==before
        print(json.dumps({'quality_integration':'passed','retry_attribution':True,'account_writes':0,
            'upstream_calls':0,'readonly_enforced':True,'real_sql_timeout_isolated':True,'detail_auth':True,'score':detail['score']}))
    finally:db.close()


if __name__ == '__main__':main()
