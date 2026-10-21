export type AccountSort = "recent:desc" | "recent:asc" | "priority:asc" | "priority:desc";
const storageKey = "sub2ops-account-sort";

export function isAccountSort(value: unknown): value is AccountSort {
  return value === "recent:desc" || value === "recent:asc" ||
    value === "priority:asc" || value === "priority:desc";
}

export function readAccountSort(): AccountSort {
  try {
    const value = localStorage.getItem(storageKey);
    if (isAccountSort(value)) return value;
  } catch { /* Sorting remains available when local storage is unavailable. */ }
  return "recent:desc";
}

export function saveAccountSort(value: AccountSort): void {
  try { localStorage.setItem(storageKey, value); } catch { /* Keep the session choice. */ }
}
