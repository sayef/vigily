"""Versioned datasets: items with an input, an expected output and metadata.

Every commit creates an immutable version; committing the same items again does not. Eval runs record
the dataset name, version and digest, so their results trace back to exactly the items they saw.
"""

from __future__ import annotations

import csv
import json
import os
from collections.abc import Iterable, Iterator, Mapping
from dataclasses import dataclass, field
from typing import Any

from vigily import _jsonable, _open


@dataclass(frozen=True)
class Item:
    """One dataset item. ``hash`` covers the input, expected output and metadata."""

    id: str
    input: Any
    expected: Any = None
    metadata: dict[str, Any] = field(default_factory=dict)
    hash: str = ""


def _item_json(x: Any) -> dict[str, Any]:
    if isinstance(x, Item):
        return {
            "id": x.id,
            "input": _jsonable(x.input),
            "expected": _jsonable(x.expected),
            "metadata": _jsonable(x.metadata),
        }
    if isinstance(x, Mapping):
        if "input" not in x:
            raise ValueError(f"an item needs an 'input': {dict(x)!r}"[:200])
        return {
            "id": None if x.get("id") is None else str(x["id"]),
            "input": _jsonable(x["input"]),
            "expected": _jsonable(x.get("expected")),
            "metadata": _jsonable(x.get("metadata") or {}),
        }
    if isinstance(x, tuple) and len(x) in (2, 3):
        return {
            "id": None,
            "input": _jsonable(x[0]),
            "expected": _jsonable(x[1]),
            "metadata": _jsonable(x[2] if len(x) == 3 else {}),
        }
    raise TypeError(
        "items are dicts with 'input' (and 'expected', 'metadata', 'id'), "
        "Item objects or (input, expected) tuples"
    )


class Dataset:
    """A version of a dataset: ``Dataset(name)`` is the latest, ``Dataset(name, version=3)`` one version.

    .. code-block:: python

        ds = Dataset.create("intents", [{"input": "where is my parcel", "expected": "shipping"}, ...])
        ds = Dataset.from_jsonl("intents", "intents.jsonl")
        ds = ds.add([{"id": "q-17", "input": "...", "expected": "billing"}], note="add billing cases")
    """

    def __init__(
        self, name: str, version: int | None = None, *, repo: str | os.PathLike[str] | None = None
    ) -> None:
        self._repo = _open(repo)
        self._repo_arg = repo
        self._meta: dict[str, Any] = json.loads(self._repo.dataset_version(name, version))
        self._items: list[Item] | None = None

    @property
    def name(self) -> str:
        return self._meta["name"]

    @property
    def version(self) -> int:
        return self._meta["version"]

    @property
    def digest(self) -> str:
        """A hash of the version's item ids and contents."""
        return self._meta["digest"]

    @property
    def note(self) -> str | None:
        return self._meta.get("note")

    @property
    def changes(self) -> dict[str, int]:
        """Items added, updated and removed by this version."""
        return dict(self._meta["changes"])

    @property
    def items(self) -> list[Item]:
        if self._items is None:
            rows = json.loads(self._repo.dataset_items(self.name, self.version))
            self._items = [
                Item(r["id"], r["input"], r["expected"], r["metadata"] or {}, r["hash"]) for r in rows
            ]
        return self._items

    def __iter__(self) -> Iterator[Item]:
        return iter(self.items)

    def __len__(self) -> int:
        return len(self._meta["items"])

    def __repr__(self) -> str:
        return f"<vigily.Dataset {self.name} v{self.version}, {len(self)} items>"

    def versions(self) -> list[dict[str, Any]]:
        """Every version: number, time, note, digest and changes."""
        rows = json.loads(self._repo.dataset_versions(self.name))
        return [{k: v for k, v in r.items() if k != "items"} | {"items": len(r["items"])} for r in rows]

    def add(self, items: Iterable[Any], *, note: str | None = None) -> Dataset:
        """Adds items, or replaces items with the same id, as a new version."""
        return Dataset._commit(self.name, items, "upsert", note, None, "sdk", self._repo_arg)

    @staticmethod
    def _commit(name, items, mode, note, description, source, repo) -> Dataset:
        h = _open(repo)
        payload = json.dumps([_item_json(i) for i in items])
        meta = json.loads(h.commit_dataset(name, payload, mode, note, description, source))
        return Dataset(name, meta["version"], repo=repo)

    @classmethod
    def create(
        cls,
        name: str,
        items: Iterable[Any],
        *,
        description: str | None = None,
        note: str | None = None,
        repo: str | os.PathLike[str] | None = None,
    ) -> Dataset:
        """Commits ``items`` as the dataset's whole content: the first version, or a new one."""
        return cls._commit(name, items, "replace", note, description, "sdk", repo)

    @classmethod
    def from_jsonl(
        cls,
        name: str,
        path: str | os.PathLike[str],
        *,
        input: str = "input",
        expected: str = "expected",
        id: str | None = "id",
        description: str | None = None,
        note: str | None = None,
        repo: str | os.PathLike[str] | None = None,
    ) -> Dataset:
        """Imports a JSON Lines file. Keys other than the input, expected and id keys become metadata."""
        rows = []
        with open(path, encoding="utf-8") as f:
            for n, line in enumerate(f, 1):
                if line.strip():
                    try:
                        rows.append(json.loads(line))
                    except json.JSONDecodeError as e:
                        raise ValueError(f"{path}:{n}: not JSON ({e.msg})") from None
        items = [_from_row(r, input, expected, id) for r in rows]
        return cls._commit(name, items, "replace", note, description, f"jsonl:{os.path.basename(path)}", repo)

    @classmethod
    def from_csv(
        cls,
        name: str,
        path: str | os.PathLike[str],
        *,
        input: str = "input",
        expected: str = "expected",
        id: str | None = "id",
        description: str | None = None,
        note: str | None = None,
        repo: str | os.PathLike[str] | None = None,
    ) -> Dataset:
        """Imports a CSV file with a header row. Other columns become metadata (as text)."""
        with open(path, encoding="utf-8", newline="") as f:
            rows = list(csv.DictReader(f))
        items = [_from_row(r, input, expected, id) for r in rows]
        return cls._commit(name, items, "replace", note, description, f"csv:{os.path.basename(path)}", repo)


def _from_row(r: Mapping[str, Any], input: str, expected: str, id: str | None) -> dict[str, Any]:
    if input not in r:
        raise ValueError(f"a row has no {input!r} column or key: {dict(r)!r}"[:200])
    meta = {k: v for k, v in r.items() if k not in (input, expected, id)}
    return {"id": r.get(id) if id else None, "input": r[input], "expected": r.get(expected), "metadata": meta}


def datasets(repo: str | os.PathLike[str] | None = None) -> list[dict[str, Any]]:
    """Every dataset in the repository: name, description, latest version, item count."""
    return json.loads(_open(repo).datasets())
