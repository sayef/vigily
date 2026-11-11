import { createContext, useCallback, useContext, useMemo, useState } from "react";

/** The runs picked for comparison. Shared across pages and kept for the browser tab's session. */
interface Selection {
  ids: string[];
  has: (id: string) => boolean;
  toggle: (id: string) => void;
  set: (ids: string[]) => void;
  clear: () => void;
}

const SelectionContext = createContext<Selection | null>(null);
const KEY = "vigily.selection";

function load(): string[] {
  try {
    const v = JSON.parse(sessionStorage.getItem(KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function SelectionProvider({ children }: { children: React.ReactNode }) {
  const [ids, setIds] = useState<string[]>(load);
  const save = useCallback((next: string[]) => {
    setIds(next);
    try {
      sessionStorage.setItem(KEY, JSON.stringify(next));
    } catch {
      // storage may be unavailable
    }
  }, []);
  const value = useMemo<Selection>(() => {
    const set = new Set(ids);
    return {
      ids,
      has: (id) => set.has(id),
      toggle: (id) => save(set.has(id) ? ids.filter((x) => x !== id) : [...ids, id]),
      set: save,
      clear: () => save([]),
    };
  }, [ids, save]);
  return <SelectionContext.Provider value={value}>{children}</SelectionContext.Provider>;
}

export function useSelection(): Selection {
  const s = useContext(SelectionContext);
  if (!s) throw new Error("useSelection outside SelectionProvider");
  return s;
}

export const LETTERS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/** The run's letter in the selection (A for the first picked), carried through to Compare. */
export function letterOf(ids: string[], id: string): string | null {
  const i = ids.indexOf(id);
  return i >= 0 && i < LETTERS.length ? LETTERS[i] : null;
}
