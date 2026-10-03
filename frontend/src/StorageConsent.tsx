import { createSignal, onCleanup } from "solid-js";
import { api } from "./api";
import { Notice } from "./ui";
import { openIamPopup } from "./iam-popup";

/** Browser callbacks exchange the one-use code on the gateway, never in messages. */
export default function StorageConsent(props: { approved: () => void; cancel: () => void }) {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<unknown>();
  let mounted = true;
  onCleanup(() => { mounted = false; });
  async function start() {
    if (busy()) return;
    setBusy(true); setError();
    try {
      await openIamPopup(async nonce => {
        const value = await api<{ redirect_url: string }>("/api/session/storage/start", { method: "POST", body: { popup_nonce: nonce } });
        return value.redirect_url;
      });
      if (mounted) props.approved();
    } catch (error) { if (mounted) setError(error); }
    finally { if (mounted) setBusy(false); }
  }
  return <section class="storage-consent" aria-label="Briefcase approval">
    <h3>Connect Briefcase</h3>
    <p>Approve access to read your source files and store generated audio. In IAM, choose the account and organization you want to use.</p>
    <button type="button" class="button primary" onClick={start} disabled={busy()}>{busy() ? "Waiting for your approval…" : "Review Briefcase access"}</button>
    <p class="hint">Approval opens in a secure popup and returns here automatically. No speech is generated until you request it.</p>
    <button type="button" class="button" onClick={props.cancel} disabled={busy()}>Cancel</button>
    <Notice error={error()} />
  </section>;
}
