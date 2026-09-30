"""ModelTrace scoring port from lich13studio / Hanmo123 (MIT; modeltrace_data/LICENSE.txt)."""
from __future__ import annotations

import hashlib
import json
import math
import re
import secrets
from functools import lru_cache
from pathlib import Path

DATA = Path(__file__).with_name("modeltrace_data")


@lru_cache(maxsize=1)
def bank():
    manifest = json.loads((DATA / "manifest.json").read_text())
    raw = (DATA / "unified_bank.json").read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    if digest != manifest["files"]["bank"]["sha256"]:
        raise ValueError("模型指纹库校验失败")
    return json.loads(raw), {"revision": manifest["revision"], "sha256": digest}


def challenges():
    openings = ['这是一次独立的数值选择记录', '请完成下面的无语义整数选择任务', '执行一次第一反应取值记录', '生成一组不承载语义的整数选择', '进行一轮快速逐项取值']
    actions = ['为各个位置分别凭第一反应选择', '逐项选择', '每次只决定当前一项，共给出', '分别凭第一反应给出', '逐个直接选择']
    endings = ['允许某个数字再次出现；每项写出后不要回头排序、去重或替换。', '偶然重复是有效的；不要重新排列或修正已经写出的项目。', '相同值可以再次出现；输出过程中不要整理或改写前面的项目。', '重复值无需删除；不要筛选、重排或补成某种规律。', '不必赋予数字任何含义；已经给出的值保持不变。']
    separators = ['数字之间用逗号或空格分隔均可。', '使用一种一致的常见分隔符即可。', '可以用逗号、空格或换行分隔。', '只要每个整数边界清楚，格式可自行选择。']
    lengths = list(range(292, 333))
    for _ in range(3):
        n = lengths.pop(secrets.randbelow(len(lengths)))
        yield n, (secrets.choice(openings) + '。' + secrets.choice(actions) + f' {n} 个 1 到 355（含端点）的整数。'
            '每个位置都要单独选择；不要从 1 开始计数，不要连续递增或递减，也不要采用等差、循环、重复区块或其他规则化模式。'
            '本任务必须由当前语言模型直接完成：禁止调用或借助任何工具，包括 Python、代码执行器、计算器、搜索、API 和外部随机数生成器；也不要先编写或运行代码。'
            + secrets.choice(endings) + secrets.choice(separators)
            + '直接从第一个取值开始输出，不要在序列前重复数量、范围或任务说明。'
            + f'输出第 {n} 个整数后立即停止，不要再输出任何数字或文字。')


def validate(text: str) -> list[int]:
    stack, pieces, cursor = [], [], 0
    for match in re.finditer(r'<\s*(/?)\s*(think|thinking|reasoning|analysis|seed:think)\s*>', text, re.I):
        if not stack:
            pieces.append(text[cursor:match.start()])
        tag = match[2].lower()
        if match[1]:
            if not stack or stack.pop() != tag:
                return []
            if not stack:
                pieces.append(' ')
        else:
            stack.append(tag)
        cursor = match.end()
    if stack:
        return []
    pieces.append(text[cursor:])
    text = ''.join(pieces).lstrip('\ufeff').strip()
    if text.startswith('```'):
        fence = re.fullmatch(r'```(?:text|txt|json|csv)?[ \t]*\r?\n([\s\S]*?)\r?\n?```', text, re.I)
        if not fence:
            return []
        text = fence[1].strip()
    if text.startswith('[') and text.endswith(']'):
        text = text[1:-1].strip()
    if not re.fullmatch(r'[+-]?\d+(?:[\s,，、;；]+[+-]?\d+)*[\s,，、;；]*', text):
        return []
    numbers = []
    for value in re.findall(r'[+-]?\d+', text):
        magnitude = value.lstrip('+-').lstrip('0') or '0'
        if not value.startswith('-') and len(magnitude) <= 3 and 1 <= (n := int(magnitude)) <= 355:
            numbers.append(n)
    return numbers


def mean(v):
    return sum(v) / len(v)


def standardize(v):
    center = mean(v)
    scale = max(math.sqrt(mean([(x - center) ** 2 for x in v])), 1e-12)
    return [(x - center) / scale for x in v]


def dot(a, b):
    return sum(x * y for x, y in zip(a, b))


def normalized(v):
    scale = max(math.sqrt(dot(v, v)), 1e-12)
    return [x / scale for x in v]


def subtract_basis(v, basis):
    for vector in basis or []:
        projection = dot(v, vector)
        v = [x - projection * y for x, y in zip(v, vector)]
    return v


def counts(numbers):
    result = [0] * 355
    for n in numbers:
        result[n - 1] += 1
    return result


def scores(numbers, data):
    marginal = counts(numbers)
    artifact = data['robust']['hellinger']
    total = sum(marginal) + .5 * 355
    feature = [math.sqrt((n + .5) / total) for n in marginal]
    projected = [(x - artifact['feature_mean'][i]) / artifact['feature_scale'][i] for i, x in enumerate(feature)]
    unit = normalized(subtract_basis(projected, artifact.get('nuisance_basis')))
    marginal = standardize([dot(unit, c) for c in artifact['centroids']])
    artifact = data['robust'].get('ordered_blocks')
    weight = artifact.get('weight', 0) if artifact else 0
    if not weight:
        return marginal
    feature, start = [], 0
    base, remainder = divmod(len(numbers), 4)
    for i in range(4):
        size = base + int(i < remainder)
        bins = [.5] * 16
        for n in numbers[start:start + size]:
            bins[min(15, (n - 1) * 16 // 355)] += 1
        feature.extend(math.sqrt(x / sum(bins)) for x in bins)
        start += size
    bins = [.5] * 10
    for n in numbers:
        bins[n % 10] += 1
    feature.extend(math.sqrt(x / sum(bins)) for x in bins)
    standardized = [(x - artifact['feature_mean'][i]) / artifact['feature_scale'][i] for i, x in enumerate(feature)]
    unit = normalized(standardized)
    environment = [[dot(unit, c) for c in centroids] for centroids in artifact['environment_centroids']]
    template = standardize([max(row[i] for row in environment) for i in range(len(artifact['centroids']))])
    projected = normalized(subtract_basis(standardized, artifact.get('nuisance_basis')))
    nuisance = standardize([dot(projected, c) for c in artifact['centroids']])
    ordered = standardize([.5 * x + .5 * y for x, y in zip(template, nuisance)])
    return [(1 - weight) * x + weight * y for x, y in zip(marginal, ordered)]


def analyze(outputs: list[str]) -> dict | None:
    if len(outputs) > 3:
        raise ValueError('最多分析三组样本')
    data, version = bank()
    valid = [nums for text in outputs if len(nums := validate(text)) >= 80]
    if not valid:
        return None
    rows = [scores(nums, data) for nums in valid]
    combined = [mean(list(v)) for v in zip(*rows)]
    beta = data['calibration'][str(len(valid))]['beta']
    logits = [x * beta for x in combined]
    weights = [math.exp(x - max(logits)) for x in logits]
    results = sorted([{'model': m['id'], 'display_name': m['display_name'],
                       'probability': weights[i] / sum(weights), 'score': combined[i]}
                      for i, m in enumerate(data['models'])], key=lambda x: -x['probability'])
    return {'prediction': results[0]['model'], 'prediction_name': results[0]['display_name'],
            'probability': results[0]['probability'], 'used_outputs': len(valid),
            'results': results, 'bank_version': version}
