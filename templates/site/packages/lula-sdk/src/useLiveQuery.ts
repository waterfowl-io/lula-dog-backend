import { useCallback, useEffect, useRef, useState } from "react";

export interface UseLiveQueryResult<T> {
  rows: T[];
  loading: boolean;
  error: Error | null;
  refetch: () => Promise<void>;
}

/**
 * Runs `queryFn` on mount and then on a fixed interval, re-running whenever
 * `deps` change. This is a polling stand-in: it does not push updates
 * server-side, it just re-fetches on a timer.
 *
 * TODO: swap the polling interval for a real live-query subscription
 * (websocket/SSE push from the Lula backend) once that channel exists.
 */
export function useLiveQuery<T>(
  queryFn: () => Promise<T[]>,
  deps: unknown[] = [],
  intervalMs = 5000,
): UseLiveQueryResult<T> {
  const [rows, setRows] = useState<T[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);
  const queryFnRef = useRef(queryFn);
  queryFnRef.current = queryFn;

  // eslint-disable-next-line react-hooks/exhaustive-deps
  const refetch = useCallback(async () => {
    try {
      const result = await queryFnRef.current();
      setRows(result);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setLoading(false);
    }
  }, deps);

  useEffect(() => {
    void refetch();

    if (intervalMs <= 0) return;
    const id = setInterval(() => void refetch(), intervalMs);
    return () => clearInterval(id);
  }, [refetch, intervalMs]);

  return { rows, loading, error, refetch };
}
