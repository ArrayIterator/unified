// Small data-loading hook: one loader, three states (loading /
// ready / error), plus a manual reload. Pages render a skeleton
// while loading and an honest error note on failure.

import { useCallback, useEffect, useState } from "react";

export type LoadState<T> =
  | { status: "loading"; data: null; error: null }
  | { status: "ready"; data: T; error: null }
  | { status: "error"; data: null; error: Error };

export function useLoad<T>(
  loader: () => Promise<T>,
  deps: unknown[],
): LoadState<T> & { reload: () => void } {
  const [nonce, setNonce] = useState(0);
  const [state, setState] = useState<LoadState<T>>({
    status: "loading",
    data: null,
    error: null,
  });

  // The loader identity changes with its deps; eslint-style exhaustive
  // deps are approximated here with a JSON key so callers can pass
  // inline closures without an infinite refetch loop.
  const key = JSON.stringify(deps);
  const reload = useCallback(() => setNonce((value) => value + 1), []);

  useEffect(() => {
    let cancelled = false;
    setState({ status: "loading", data: null, error: null });
    void loader()
      .then((data) => {
        if (!cancelled) setState({ status: "ready", data, error: null });
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setState({
            status: "error",
            data: null,
            error: error instanceof Error ? error : new Error(String(error)),
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [key, nonce]);

  return { ...state, reload };
}
