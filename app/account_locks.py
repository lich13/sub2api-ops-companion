"""Shared account-operation locks, including the credential-owning parent."""
from __future__ import annotations

import threading
from typing import Any

_guard = threading.Lock()


def control_lock(db: Any, account_id: int) -> Any:
    """Short scheduling mutations never wait on a model/quota operation lease."""
    with _guard:
        locks = vars(db).setdefault("_companion_control_locks", {})
        return locks.setdefault(int(account_id), threading.RLock())


def account_lock(db: Any, account_id: int) -> Any:
    with _guard:
        locks = vars(db).get("_companion_account_locks")
        if locks is None:
            locks = {}
            setattr(db, "_companion_account_locks", locks)
        return locks.setdefault(int(account_id), threading.Lock())


class AccountLease:
    def __init__(self, db: Any, row: dict, *, include_account: bool = True):
        ids = {int(row["id"])} if include_account else set()
        parent = int(row.get("parent_account_id") or 0)
        if parent and parent != int(row["id"]):
            ids.add(parent)
        self.locks = [account_lock(db, aid) for aid in sorted(ids)]
        self.held: list[Any] = []

    def acquire(self, blocking: bool = False) -> bool:
        for lock in self.locks:
            if not lock.acquire(blocking=blocking):
                self.release()
                return False
            self.held.append(lock)
        return True

    def release(self) -> None:
        for lock in reversed(self.held):
            lock.release()
        self.held.clear()
