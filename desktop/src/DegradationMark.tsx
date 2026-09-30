import { useEffect, useRef, useState } from "react";
import { BellOff, LoaderCircle } from "lucide-react";
import { api, command } from "./bridge";
import type { Account, DegradationMark } from "./types";

export function DegradationBadge({ account }: { account: Account }) {
  return account.degradation_mark?.marked ? <span className="degradation-badge"><BellOff size={12}/>降智</span> : null;
}

export default function DegradationAction({ account, online, report }: { account: Account; online: boolean; report: (error: unknown) => void }) {
  const [busy, setBusy] = useState(false), [saved, setSaved] = useState<DegradationMark | undefined>();
  const alive = useRef(true);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  useEffect(() => setSaved(undefined), [account.degradation_mark?.version, account.id]);
  if (account.platform !== "openai" || account.type !== "oauth") return null;
  const mark = saved ?? account.degradation_mark;
  async function change() {
    if (!mark?.version || busy) return;
    setBusy(true);
    try {
      const result = await api<{ verified: boolean; degradation_mark: DegradationMark }>("PUT", `/accounts/${account.id}/degradation-mark`, { marked: !mark.marked, expected_mark_version: mark.version });
      if (!alive.current) return;
      if (!result.verified) throw new Error("降智标记保存未确认");
      setSaved(result.degradation_mark);
      await command("refresh");
    } catch (error) { if (alive.current) report(error); } finally { if (alive.current) setBusy(false); }
  }
  return <button className="degradation-action" title={mark?.error} disabled={!online || busy || !mark?.version} onClick={() => void change()}>
    {busy ? <LoaderCircle size={14} className="spin"/> : <BellOff size={14}/>}{mark?.error ? "标记读取失败" : mark?.marked ? "取消标记" : "标记降智"}
  </button>;
}
