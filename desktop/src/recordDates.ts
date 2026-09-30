export const datePresets = [
  ["today", "今天"],
  ["yesterday", "昨天"],
  ["24h", "近24小时"],
  ["7d", "近7天"],
  ["14d", "近14天"],
  ["30d", "近30天"],
  ["month", "本月"],
  ["lastMonth", "上月"],
] as const;
export type DatePreset = (typeof datePresets)[number][0];
export type RecordDateRange = {
  preset: DatePreset | null;
  start: string;
  end: string;
};
const DAY = 86400000;
export function beijingDate(now = Date.now()) {
  return new Date(now + 8 * 3600000).toISOString().slice(0, 10);
}
export function shiftDate(value: string, days: number) {
  return new Date(Date.parse(`${value}T00:00:00Z`) + days * DAY)
    .toISOString()
    .slice(0, 10);
}
export function validDateRange(start: string, end: string) {
  return (
    [start, end].every(
      (value) =>
        /^\d{4}-\d{2}-\d{2}$/.test(value) &&
        Number.isFinite(Date.parse(`${value}T00:00:00Z`)) &&
        new Date(`${value}T00:00:00Z`).toISOString().slice(0, 10) === value,
    ) &&
    start <= end &&
    end < "9999-12-31"
  );
}
export function presetRange(
  preset: DatePreset,
  now = Date.now(),
): RecordDateRange {
  const today = beijingDate(now);
  let start = today,
    end = today;
  if (preset === "yesterday") start = end = shiftDate(today, -1);
  // Match Sub2API's date-based preset, including the entire preceding day.
  if (preset === "24h") start = shiftDate(today, -1);
  if (preset === "7d" || preset === "14d" || preset === "30d")
    start = shiftDate(today, 1 - parseInt(preset));
  if (preset === "month") start = `${today.slice(0, 7)}-01`;
  if (preset === "lastMonth") {
    end = shiftDate(`${today.slice(0, 7)}-01`, -1);
    start = `${end.slice(0, 7)}-01`;
  }
  return { preset, start, end };
}
export function dateRangeLabel(value: RecordDateRange) {
  return (
    datePresets.find(([key]) => key === value.preset)?.[1] ??
    (value.start === value.end ? value.start : `${value.start} — ${value.end}`)
  );
}
