"""Field-scoped optimistic versions. Passive observations never invalidate edits."""
import hashlib
import json
from contextvars import ContextVar

operation_context = ContextVar('desktop_operation_context', default=None)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, default=str, ensure_ascii=False).encode()).hexdigest()


def versions(row, managed=False):
    identity = {k: row.get(k) for k in ('id', 'platform', 'type', 'parent_account_id', 'credential_version', 'proxy_id')}
    groups = sorted(row.get('group_ids') or [])
    limits = {k: row.get(k) for k in ('status', 'error_message', 'rate_limit_reset_at', 'overload_until', 'temp_unschedulable_until')}
    extra = row.get('extra') or {}
    return {name: digest([identity, value]) for name, value in {
        'schedulable': [row.get('schedulable'), managed],
        'priority': row.get('priority', 0), 'groups': groups,
        'recover': limits,
        'delete': [row.get('name'), row.get('status'), row.get('schedulable'), groups, managed],
        'usage': None,
        'reset_quota': [limits, {k: v for k, v in extra.items() if 'reset_credit' in k or 'reset_quota' in k}],
        'test': [groups, row.get('model_catalog_version')],
        'model_test': [groups, row.get('model_catalog_version')],
    }.items()}
