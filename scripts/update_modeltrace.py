#!/usr/bin/env python3
"""Verify or import a fixed, reviewed upstream data revision (never remote runtime code)."""
import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from app.fingerprint_bank import validate_fingerprint_bank, git_blob_digest
from app.modeltrace import analyze


def download(revision, path):
    url = f'https://raw.githubusercontent.com/Hanmo123/ModelTrace/{revision}/{path}'
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    with urllib.request.build_opener(NoRedirect).open(url, timeout=15) as response:
        raw = response.read(8 * 1024 * 1024 + 1)
    if len(raw) > 8 * 1024 * 1024:
        raise ValueError('文件超过 8 MiB')
    return raw


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--revision', help='Reviewed full Git commit SHA; defaults to the bundled revision')
    parser.add_argument('--write', action='store_true', help='Replace data and manifest after all checks pass')
    args = parser.parse_args()
    folder = ROOT / 'app/modeltrace_data'
    manifest = json.loads((folder / 'manifest.json').read_text())
    revision = args.revision or manifest['revision']
    if not re.fullmatch('[0-9a-f]{40}', revision):
        raise ValueError('需要完整提交 SHA')
    files = {name: download(revision, info['path']) for name, info in manifest['files'].items()}
    for name in ('core', 'challenge'):
        if hashlib.sha256(files[name]).hexdigest() != manifest['files'][name]['sha256'] or git_blob_digest(files[name]) != manifest['files'][name]['gitBlob']:
            raise ValueError('评分器或挑战文件已变化，需要人工升级兼容契约')
    value = json.loads(files['bank'])
    validate_fingerprint_bank(value)
    # Execute the already hash-verified reference scorer only in this developer
    # verification script. Companion runtime downloads and loads data only.
    samples = [' '.join(str(((i * stride + seed) % 355) + 1) for i in range(320)) for stride, seed in ((71, 3), (53, 9), (97, 17))]
    with tempfile.TemporaryDirectory(prefix='modeltrace-verify-') as temporary:
        directory = Path(temporary)
        (directory / 'core.mjs').write_bytes(files['core'])
        (directory / 'input.json').write_text(json.dumps({'bank': value, 'samples': samples}))
        (directory / 'verify.mjs').write_text("import fs from 'node:fs'; import {analyzeGlobalOutputs} from './core.mjs'; const x=JSON.parse(fs.readFileSync(new URL('./input.json',import.meta.url))); console.log(JSON.stringify([1,2,3].map(n=>analyzeGlobalOutputs(x.samples.slice(0,n).map(text=>({text})),x.bank))));")
        reference = json.loads(subprocess.check_output(['node', str(directory / 'verify.mjs')], text=True, timeout=15))
    for count, expected in enumerate(reference, 1):
        actual = analyze(samples[:count], snapshot=(value, {}))
        if actual['prediction'] != expected['prediction'] or abs(actual['probability'] - expected['probability']) > 1e-10:
            raise ValueError(f'{count} 组评分不一致')
    manifest['revision'] = revision
    for name, raw in files.items():
        manifest['files'][name].update(sha256=hashlib.sha256(raw).hexdigest(), gitBlob=git_blob_digest(raw))
    if args.write:
        (folder / 'unified_bank.json').write_bytes(files['bank'])
        (folder / 'manifest.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
    else:
        if (folder / 'unified_bank.json').read_bytes() != files['bank'] or json.loads((folder / 'manifest.json').read_text()) != manifest:
            raise ValueError('内置库或 manifest 与固定提交不一致')
    print(json.dumps({'revision': revision, 'sha256': manifest['files']['bank']['sha256'], 'models': len(value['models']), 'scoring_groups_verified': [1, 2, 3], 'written': args.write}))

if __name__ == '__main__':
    main()
