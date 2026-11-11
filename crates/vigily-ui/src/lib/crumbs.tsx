import { createContext, useContext, useEffect, useState } from "react";

/** One step of the top bar's breadcrumb trail; the last one is the current page. */
export interface Crumb {
  label: string;
  to?: string;
  mono?: boolean;
}

const CrumbsContext = createContext<{ crumbs: Crumb[] | null; set: (c: Crumb[] | null) => void }>({ crumbs: null, set: () => {} });

export function CrumbsProvider({ children }: { children: React.ReactNode }) {
  const [crumbs, set] = useState<Crumb[] | null>(null);
  return <CrumbsContext.Provider value={{ crumbs, set }}>{children}</CrumbsContext.Provider>;
}

export const useCrumbTrail = () => useContext(CrumbsContext).crumbs;

/** Sets the breadcrumb trail while the calling page is shown. Pages without one get their nav label. */
export function useCrumbs(crumbs: Crumb[]) {
  const { set } = useContext(CrumbsContext);
  const key = JSON.stringify(crumbs);
  useEffect(() => {
    set(JSON.parse(key) as Crumb[]);
    return () => set(null);
  }, [key, set]);
}
