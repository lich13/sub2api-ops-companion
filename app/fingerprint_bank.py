"""Verified ModelTrace data updates; downloaded code is never executed."""
from __future__ import annotations
import asyncio
import copy
import fcntl
import hashlib
import json
import math
import re
import threading
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from pathlib import Path
from urllib.parse import urljoin, urlsplit
from typing import Any, Callable
import httpx
from .atomic_config import write_json
from .audit import write_audit

FINGERPRINT_SYNC_INTERVAL_SECONDS = 3600
FINGERPRINT_FAILURE_COOLDOWN_SECONDS = 300
REQUEST_TIMEOUT = 15
ROUND_TIMEOUT = 60
MAX_BYTES = 8 * 1024 * 1024
SHA1 = re.compile(r'^[0-9a-f]{40}$')
SHA256 = re.compile(r'^[0-9a-f]{64}$')
HOSTS = {'api.github.com', 'github.com', 'raw.githubusercontent.com'}

class IncompatibleFingerprintBankError(ValueError):
    pass

class InvalidFingerprintBankError(ValueError):
    pass

class SyncFailure(Exception):
    def __init__(self, code, retry_at=None):
        super().__init__(code)
        self.code, self.retry_at = code, retry_at

def fingerprint_digest(raw):
    return hashlib.sha256(raw).hexdigest()

def git_blob_digest(raw):
    return hashlib.sha1(f'blob {len(raw)}\0'.encode() + raw).hexdigest()

def _parse_time(value):
    try:
        result = datetime.fromisoformat(value.replace('Z','+00:00'))
        return result if result.tzinfo is not None else None
    except (ValueError, TypeError, AttributeError):
        return None

def finite(value):
    return type(value) in (int,float) and math.isfinite(value)

def validate_fingerprint_bank(value):
    def check(valid):
        if not valid:
            raise InvalidFingerprintBankError('指纹库数据无效')
    def vector(v, n, predicate=lambda x: True):
        return isinstance(v,list) and len(v)==n and all(finite(x) and predicate(x) for x in v)
    def matrix(v, n, dim):
        return isinstance(v,list) and len(v)==n and all(vector(row,dim) for row in v)
    check(isinstance(value,dict))
    models, method, robust = value.get('models'), value.get('method') or {}, value.get('robust') or {}
    check(value.get('schema')=='robust-number-fingerprint-bank')
    if _parse_time(value.get('built_at')) is None:
        raise InvalidFingerprintBankError('构建时间无效')
    check(value.get('minimum_valid_numbers')==80 and value.get('recommended_queries')==3)
    check(method.get('range')==[1,355] and method.get('alpha')==.5 and method.get('ordered_block_weight')==.25)
    check(isinstance(models,list) and 0<len(models)<=256 and robust.get('robust_ready') is True)
    check(all(isinstance(m,dict) for m in models))
    ids=[m.get('id') for m in models]
    check(all(isinstance(x,str) and 0<len(x)<=256 for x in ids))
    check(len(set(ids))==len(ids) and robust.get('model_order')==ids)
    for model in models:
        check(all(isinstance(model.get(k),str) and 0<len(model[k])<=256 for k in ('display_name','family','family_name')))
        check(vector(model.get('counts'),355,lambda n: n>=0 and n<=2**53-1 and int(n)==n))
    for key, dim in (('hellinger',355),('ordered_blocks',74)):
        feature=robust.get(key) or {}
        check(vector(feature.get('feature_mean'),dim) and vector(feature.get('feature_scale'),dim,lambda n:n>0))
        basis=feature.get('nuisance_basis')
        check(isinstance(basis,list) and len(basis)<=dim and matrix(basis,len(basis),dim))
        check(matrix(feature.get('centroids'),len(ids),dim))
    ordered=robust['ordered_blocks']
    env=ordered.get('environment_centroids')
    check(ordered.get('weight')==.25 and isinstance(env,list) and 0<len(env)<=256)
    check(all(matrix(e,len(ids),74) for e in env))
    for count in ('1','2','3'):
        c=(value.get('calibration') or {}).get(count) or {}
        check(finite(c.get('beta')) and c['beta']>0 and finite(c.get('cv_accuracy')) and 0<=c['cv_accuracy']<=1)

def parse_git_refs(raw, branch):
    offset, found = 0, []
    pattern = re.compile(r'^([0-9a-f]{40}) refs/heads/' + re.escape(branch) + r'(?:\x00|[ \t\r\n]|$)')
    try:
        while offset<len(raw):
            header=raw[offset:offset+4]
            if len(header)!=4 or not re.fullmatch(b'[0-9a-fA-F]{4}',header):
                raise ValueError()
            size=int(header,16)
            if size==0:
                offset+=4
                continue
            if size<4 or offset+size>len(raw):
                raise ValueError()
            line=raw[offset+4:offset+size].decode('utf-8')
            match=pattern.match(line)
            if match:
                found.append(match[1])
            offset+=size
        if len(found)!=1:
            raise ValueError()
        return found[0]
    except (ValueError,UnicodeError):
        raise SyncFailure('invalid-data') from None

class FingerprintBankService:
    def __init__(self,state_path,audit_path=None,*,now=None,fetcher=None):
        self.path=Path(state_path)
        self.audit_path=str(audit_path) if audit_path else ''
        self.now=now or time.time
        self.fetcher=fetcher
        self._lock=threading.RLock()
        self._sync_lock=threading.Lock()
        self._initialized=False
        self._active=None
        self._state={}
        self._status='idle'
        self._deadline=0
        self._task=None

    @staticmethod
    def _manifest():
        from .modeltrace import DATA
        value=json.loads((DATA/'manifest.json').read_text())
        if value.get('repository')!='Hanmo123/ModelTrace' or value.get('branch')!='hanmo':
            raise ValueError('指纹库来源无效')
        return value

    @staticmethod
    def _bundled():
        from .modeltrace import bundled_bank
        bank,version=bundled_bank()
        validate_fingerprint_bank(bank)
        return bank,version

    def _audit(self,action,**fields):
        if self.audit_path:
            write_audit(self.audit_path,'fingerprint_bank_'+action,fields)

    @contextmanager
    def _state_lock(self):
        self.path.parent.mkdir(parents=True,exist_ok=True)
        with self.path.with_suffix('.lock').open('a+') as lock:
            self.path.with_suffix('.lock').chmod(0o600)
            fcntl.flock(lock,fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock,fcntl.LOCK_UN)

    def _persist(self,state):
        with self._state_lock():
            write_json(self.path,state)

    def initialize(self):
        with self._lock:
            if self._initialized:
                return
            bank,version=self._bundled()
            from .modeltrace import DATA
            manifest=self._manifest()
            provenance={**version,'core_blob':manifest['files']['core']['gitBlob'], 'challenge_blob':manifest['files']['challenge']['gitBlob']}
            self._active=(bank,version)
            self._state={'version':1,'active':{'raw_bank':(DATA/'unified_bank.json').read_text(),'version':provenance},
                         'source':'bundled','checked_at':0,'synced_at':0,'cooldown_until':None,'last_error':None,'result':None}
            try:
                cached=json.loads(self.path.read_text())
                active=cached['active']; v=active['version']; raw=active['raw_bank'].encode()
                if cached.get('version')!=1 or len(raw)>MAX_BYTES or not SHA1.fullmatch(v['revision']) or not SHA256.fullmatch(v['sha256']):
                    raise ValueError()
                if v.get('analyzer_version')!=manifest['analyzerVersion'] or any(v.get(k+'_blob')!=manifest['files'][k]['gitBlob'] for k in ('core','challenge')):
                    raise ValueError()
                if any(not finite(cached.get(k)) or cached[k]<0 for k in ('checked_at','synced_at')):
                    raise ValueError()
                if cached.get('cooldown_until') is not None and not finite(cached['cooldown_until']):
                    raise ValueError()
                if fingerprint_digest(raw)!=v['sha256']:
                    raise ValueError()
                parsed=json.loads(raw); validate_fingerprint_bank(parsed)
                if v.get('built_at')!=parsed['built_at'] or _parse_time(parsed['built_at'])<_parse_time(bank['built_at']):
                    raise ValueError()
                self._active=(parsed,{k:v[k] for k in ('revision','sha256','built_at','analyzer_version')})
                self._state=cached | {'source':'cache'}
                if cached.get('last_error'):
                    self._status='upgrade-required' if self._error_code(cached)=='incompatible' else 'error'
            except FileNotFoundError:
                pass
            except (OSError,ValueError,TypeError,KeyError,UnicodeError):
                self._state.update(last_error={'code':'cache'},result='failed')
                self._status='error'
                self._audit('cache_error')
            self._initialized=True

    @staticmethod
    def _error_code(state):
        error=state.get('last_error') or {}
        return error.get('code',error.get('stage'))

    def snapshot(self):
        self.initialize()
        with self._lock:
            return copy.deepcopy(self._active)

    def capture(self):
        return self.snapshot()

    def public(self):
        self.initialize()
        with self._lock:
            return {'version':dict(self._active[1]),'source':self._state['source'],'status':self._status,
                    **{k:self._state.get(k) for k in ('checked_at','synced_at','cooldown_until','result','retrieval')},
                    'error':self._error_code(self._state)}

    def request_sync(self):
        if self._task is None or self._task.done():
            self._task=asyncio.create_task(asyncio.to_thread(self.sync,True))
        return self.public()

    def _url(self,url):
        p=urlsplit(url)
        if p.scheme!='https' or p.hostname not in HOSTS or p.username or p.password or p.port not in (None,443):
            raise SyncFailure('invalid-data')

    def _status_error(self,status,headers):
        if 200<=status<300:
            return
        headers={k.lower():v for k,v in headers.items()}
        if status in (403,429):
            retry=None
            value=headers.get('retry-after')
            try:
                retry=self.now()+max(0,float(value)) if value is not None else None
            except (ValueError,TypeError):
                try:
                    retry=parsedate_to_datetime(value).timestamp()
                except (ValueError,TypeError):
                    pass
            if retry is None:
                try:
                    retry=float(headers.get('x-ratelimit-reset',''))
                except ValueError:
                    pass
            raise SyncFailure('rate-limit',retry if retry is not None and math.isfinite(retry) else None)
        raise SyncFailure('timeout' if status in (408,504) else 'network' if status>=500 else 'invalid-data')

    def _read_remote(self,url,accept='application/vnd.github+json'):
        self._url(url)
        remaining=self._deadline-time.monotonic()
        if remaining<=0:
            raise SyncFailure('timeout')
        try:
            if self.fetcher:
                status,headers,body=self.fetcher(url)
                self._status_error(status,headers)
                if len(body)>MAX_BYTES:
                    raise SyncFailure('invalid-data')
                if time.monotonic()>self._deadline:
                    raise SyncFailure('timeout')
                return body
            async def read():
                async with asyncio.timeout(min(REQUEST_TIMEOUT,remaining)):
                    async with httpx.AsyncClient(timeout=httpx.Timeout(15,connect=10),follow_redirects=False,trust_env=False,
                        headers={'Accept':accept,'User-Agent':'sub2ops-modeltrace'}) as client:
                        current=url
                        for _ in range(4):
                            self._url(current)
                            async with client.stream('GET',current) as response:
                                if response.status_code in (301,302,303,307,308):
                                    current=urljoin(current,response.headers.get('location',''))
                                    continue
                                self._status_error(response.status_code,response.headers)
                                length=response.headers.get('content-length','0')
                                if not length.isdigit() or int(length)>MAX_BYTES:
                                    raise SyncFailure('invalid-data')
                                chunks=[]; total=0
                                async for part in response.aiter_bytes():
                                    total+=len(part)
                                    if total>MAX_BYTES:
                                        raise SyncFailure('invalid-data')
                                    chunks.append(part)
                                return b''.join(chunks)
                        raise SyncFailure('invalid-data')
            return asyncio.run(read())
        except (TimeoutError,httpx.TimeoutException):
            raise SyncFailure('timeout') from None
        except (OSError,httpx.HTTPError):
            raise SyncFailure('network') from None

    def _read_retry(self,url,accept='application/vnd.github+json'):
        for attempt in range(2):
            try:
                return self._read_remote(url,accept)
            except SyncFailure as exc:
                if attempt or exc.code not in {'network','timeout'}:
                    raise

    def _pinned_file(self,revision,path):
        repo=self._manifest()['repository']
        for index,url in enumerate((f'https://raw.githubusercontent.com/{repo}/{revision}/{path}',f'https://github.com/{repo}/raw/{revision}/{path}')):
            try:
                return self._read_retry(url,'text/plain')
            except SyncFailure as exc:
                if index or exc.code not in {'network','timeout'}:
                    raise

    def _fallback_files(self,revision):
        m=self._manifest()
        for name in ('core','challenge'):
            raw=self._pinned_file(revision,m['files'][name]['path'])
            if git_blob_digest(raw)!=m['files'][name]['gitBlob'] or fingerprint_digest(raw)!=m['files'][name]['sha256']:
                raise IncompatibleFingerprintBankError()
        return self._pinned_file(revision,m['files']['bank']['path'])

    def sync(self,force=False):
        self.initialize()
        if not self._sync_lock.acquire(blocking=False):
            return
        try:
            now=self.now()
            with self._lock:
                last=float(self._state.get('checked_at') or 0)
                cooldown=self._state.get('cooldown_until') or 0
                if now < (self._state.get('rate_limit_until') or 0):
                    return
                error=self._error_code(self._state)
                if now<cooldown and (not force or error=='rate-limit'):
                    return
                interval=FINGERPRINT_FAILURE_COOLDOWN_SECONDS if error in {'network','timeout','rate-limit','cache'} else FINGERPRINT_SYNC_INTERVAL_SECONDS
                if not force and last>0 and 0<=now-last<interval:
                    return
                self._status='checking'
            self._deadline=time.monotonic()+ROUND_TIMEOUT
            manifest=self._manifest(); repo=manifest['repository']; base=f'https://api.github.com/repos/{repo}'
            retrieval='api_tree'
            rate_limit_until=None
            try:
                try:
                    head=json.loads(self._read_retry(f'{base}/commits/{manifest["branch"]}'))
                    revision=head.get('sha')
                    if not isinstance(revision,str) or not SHA1.fullmatch(revision):
                        raise SyncFailure('invalid-data')
                except SyncFailure as exc:
                    if exc.code not in {'network','timeout','rate-limit'}:
                        raise
                    if exc.code=='rate-limit':
                        rate_limit_until=exc.retry_at or now+FINGERPRINT_FAILURE_COOLDOWN_SECONDS
                    revision=parse_git_refs(self._read_retry(f'https://github.com/{repo}.git/info/refs?service=git-upload-pack',
                                            'application/x-git-upload-pack-advertisement'),manifest['branch'])
                    retrieval='git_refs'
                if revision==self._active[1]['revision']:
                    next_state=self._state | {'checked_at':now,'cooldown_until':None,'last_error':None,'result':'up-to-date'}
                else:
                    expected=None
                    if retrieval=='api_tree':
                        try:
                            tree=json.loads(self._read_retry(f'{base}/git/trees/{revision}?recursive=1'))
                            if not isinstance(tree,dict) or tree.get('truncated') or not isinstance(tree.get('tree'),list):
                                raise SyncFailure('invalid-data')
                            entries={e.get('path'):e for e in tree['tree'] if isinstance(e,dict)}
                            for name in ('core','challenge','bank'):
                                entry=entries.get(manifest['files'][name]['path'],{})
                                if entry.get('type')!='blob' or entry.get('mode')!='100644' or not SHA1.fullmatch(str(entry.get('sha',''))):
                                    raise SyncFailure('invalid-data')
                                if name!='bank' and entry['sha']!=manifest['files'][name]['gitBlob']:
                                    raise IncompatibleFingerprintBankError()
                                if name=='bank':
                                    expected=entry['sha']
                            raw=self._pinned_file(revision,manifest['files']['bank']['path'])
                        except SyncFailure as exc:
                            if exc.code not in {'network','timeout','rate-limit'}:
                                raise
                            if exc.code=='rate-limit':
                                rate_limit_until=exc.retry_at or now+FINGERPRINT_FAILURE_COOLDOWN_SECONDS
                            raw=self._fallback_files(revision); retrieval='git_refs'
                    else:
                        raw=self._fallback_files(revision)
                    if expected and git_blob_digest(raw)!=expected:
                        raise SyncFailure('invalid-data')
                    bank=json.loads(raw); validate_fingerprint_bank(bank)
                    if _parse_time(bank['built_at'])<_parse_time(self._active[0]['built_at']):
                        raise IncompatibleFingerprintBankError()
                    version={'revision':revision,'sha256':fingerprint_digest(raw),'built_at':bank['built_at'],'analyzer_version':manifest['analyzerVersion']}
                    next_state={'version':1,'active':{'raw_bank':raw.decode('utf-8'),'version':version | {
                        'core_blob':manifest['files']['core']['gitBlob'],'challenge_blob':manifest['files']['challenge']['gitBlob']}},
                        'checked_at':now,'synced_at':self.now(),'source':'remote','last_error':None,'cooldown_until':None,
                        'result':'updated','retrieval':retrieval}
                next_state['rate_limit_until']=rate_limit_until
                if rate_limit_until:
                    next_state['cooldown_until']=rate_limit_until
                try:
                    self._persist(next_state)
                except OSError:
                    raise SyncFailure('cache') from None
                with self._lock:
                    self._state=next_state
                    if next_state['result']=='updated':
                        self._active=(bank,version)
                    self._status='idle'
                self._audit(next_state['result'],revision=revision)
            except Exception as exc:
                failure=exc if isinstance(exc,SyncFailure) else SyncFailure('incompatible' if isinstance(exc,IncompatibleFingerprintBankError) else 'invalid-data')
                cooldown=max(self.now()+300,failure.retry_at or 0) if failure.code in {'network','timeout','rate-limit','cache'} else None
                failed=self._state | {'checked_at':self.now(),'last_error':{'code':failure.code},'cooldown_until':cooldown,'result':'failed'}
                if rate_limit_until:
                    failed['rate_limit_until']=rate_limit_until
                    failed['cooldown_until']=max(failed.get('cooldown_until') or 0,rate_limit_until)
                try:
                    self._persist(failed)
                except (OSError,ValueError):
                    pass
                with self._lock:
                    self._state=failed
                    self._status='upgrade-required' if failure.code=='incompatible' else 'error'
                self._audit('sync_error',code=failure.code)
        finally:
            self._deadline=0
            self._sync_lock.release()

    def start(self):
        self.sync()
