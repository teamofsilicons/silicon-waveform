/** One ordinary sign-in owns the busy state; older completions cannot reset it. */
export function createLoginLifecycle(setBusy: (value: boolean) => void) {
  let generation = 0, mounted = true, active: AbortController | undefined;
  const fence = () => { const value = generation; return { current: () => mounted && value === generation }; };
  return {
    begin() {
      generation++; active?.abort(); active = new AbortController();
      const attempt = { ...fence(), signal: active.signal };
      setBusy(true);
      return { ...attempt, finish: () => { if (attempt.current()) setBusy(false); } };
    },
    cancel() {
      generation++; active?.abort(); active = undefined; setBusy(false);
      return fence();
    },
    dispose() { mounted = false; generation++; active?.abort(); active = undefined; },
  };
}

/** A failed cancellation retains its nonce. Concurrent UI intents share the RPC;
 * its restored session is adopted only while the original local context is selected. */
export function createLoginCancellation<T extends { context: string }>(
  currentContext: () => string | undefined,
  cancel: (nonce: string, context: string | undefined) => Promise<T>,
  accept: (value: T) => void,
) {
  let nonce: string | undefined, inFlight: Promise<string | undefined> | undefined;
  return {
    remember(value: string) { nonce = value; },
    forget() { nonce = undefined; },
    cancel(): Promise<string | undefined> {
      if (inFlight) return inFlight;
      if (!nonce) return Promise.resolve(currentContext());
      const captured = nonce, before = currentContext();
      const operation = Promise.resolve().then(() => cancel(captured, before)).then(value => {
        if (nonce === captured) nonce = undefined;
        if (currentContext() === before) accept(value);
        return value.context;
      });
      inFlight = operation.finally(() => { inFlight = undefined; });
      return inFlight;
    },
  };
}
