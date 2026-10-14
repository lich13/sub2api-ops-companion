import { CircleAlert } from 'lucide-react';
import type { AccountActionItem } from './AccountActions';
import type { Account } from './types';

export function accountRestriction(account?: Account): string {
  if (!account || (account.available && !account.blockers.length && !account.recoverable)) return '';
  const reasons = account.blockers.map(blocker => blocker.label).filter(Boolean);
  if (!reasons.length) reasons.push(account.recoverable ? '存在可恢复的状态限制' : '暂不可调度');
  return reasons.join(' · ');
}

export function QuickRestriction({ reason }: { reason: string }) {
  return reason ? <CircleAlert className="quick-restriction" size={11} role="img" aria-label={`调度受限：${reason}`} /> : null;
}

export function QuickRecover({ account, action }: { account: Account; action?: AccountActionItem }) {
  if (!action) return null;
  const label = `恢复状态：${account.name}`;
  return <button type="button" className="quick-recover" title={label} aria-label={label}
    aria-busy={action.pending || undefined} disabled={action.disabled} onClick={action.run}>{action.icon}</button>;
}
