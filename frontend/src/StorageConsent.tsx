import { createSignal, onCleanup, Show } from "solid-js";
import { bindApi, ApiError } from "./api";
import {
  storageAuthorization,
  type StorageAuthorization as Authorization,
} from "./storage-authorization";
import { Notice } from "./ui";

/** Explicit feature approval. The browser receives only a single-use code. */
export default function StorageConsent(props: {
  approved: () => void;
  cancel: () => void;
}) {
  const api = bindApi();
  const [request, setRequest] = createSignal<Authorization>();
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<unknown>();
  let retryKey = crypto.randomUUID(),
    completionKey = crypto.randomUUID();
  let mounted = true;
  onCleanup(() => {
    mounted = false;
    setCode("");
  });
  function reset() {
    setRequest();
    setCode("");
    retryKey = crypto.randomUUID();
    completionKey = crypto.randomUUID();
  }
  function failed(error: unknown) {
    if (error instanceof ApiError && error.status === 412) reset();
    setError(error);
  }
  async function start() {
    if (busy()) return;
    setBusy(true);
    setError();
    try {
      const value = await api<Authorization>("/api/v1/storage-authorizations", {
        method: "POST",
        body: {},
        headers: { "idempotency-key": retryKey },
      });
      if (mounted) {
        const checked = storageAuthorization(value);
        if (checked.status === "completed") props.approved();
        else setRequest(checked);
      }
    } catch (error) {
      if (mounted) failed(error);
    } finally {
      if (mounted) setBusy(false);
    }
  }
  async function complete() {
    if (busy() || !request() || !code().trim()) return;
    setBusy(true);
    setError();
    try {
      const value = await api<Authorization>(
        `/api/v1/storage-authorizations/${request()!.authorization_id}/complete`,
        {
          method: "POST",
          body: { code: code().trim(), state: request()!.state },
          headers: { "idempotency-key": completionKey },
        },
      );
      if (mounted) {
        const checked = storageAuthorization(value, request());
        if (checked.status !== "completed")
          throw new Error(
            "Approval is still pending. Complete the review in IAM and retry.",
          );
        setCode("");
        props.approved();
      }
    } catch (error) {
      if (mounted) failed(error);
    } finally {
      if (mounted) setBusy(false);
    }
  }
  return (
    <section class="storage-consent" aria-label="Briefcase approval">
      <h3>Connect Briefcase</h3>
      <p>
        Approve access to read your source files and store generated audio. Use
        the same account and organization as this Waveform workspace in IAM.
      </p>
      <Show
        when={request()}
        fallback={
          <button
            type="button"
            class="button primary"
            onClick={start}
            disabled={busy()}
          >
            {busy() ? "Preparing approval…" : "Review Briefcase access"}
          </button>
        }
      >
        <p>
          <a
            class="text-link"
            href={request()!.consent_url || undefined}
            target="_blank"
            rel="noopener noreferrer"
          >
            Open approval in IAM ↗
          </a>
        </p>
        <label>
          Approval code from IAM
          <input
            type="password"
            autocomplete="off"
            spellcheck={false}
            value={code()}
            onInput={(event) => setCode(event.currentTarget.value)}
            disabled={busy()}
            placeholder="Paste your approval code"
          />
        </label>
        <p class="hint">
          This request expires{" "}
          {new Date(request()!.expires_at).toLocaleTimeString()}. Approval alone
          does not generate speech.
        </p>
        <button
          type="button"
          class="button primary"
          onClick={complete}
          disabled={busy() || !code().trim()}
        >
          {busy() ? "Saving approval…" : "Save approval"}
        </button>
      </Show>
      <Show when={request() || error()}>
        <button
          type="button"
          class="button"
          onClick={() => {
            reset();
            void start();
          }}
          disabled={busy()}
        >
          Start a new permission review
        </button>
      </Show>
      <button
        type="button"
        class="button"
        onClick={props.cancel}
        disabled={busy()}
      >
        Cancel
      </button>
      <Notice error={error()} />
    </section>
  );
}
