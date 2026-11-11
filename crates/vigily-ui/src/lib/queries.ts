import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";

/** Repository metadata: counts, experiments, metric and parameter keys. Polled while runs are live. */
export function useMeta() {
  return useQuery({
    queryKey: ["meta"],
    queryFn: ({ signal }) => api.meta(signal),
    refetchInterval: (q) => (q.state.data?.running ? 5_000 : 30_000),
  });
}
