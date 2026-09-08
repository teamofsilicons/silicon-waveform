import {
  createResource,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import {
  acceptSession,
  api,
  contextVersion,
  session,
  sessionAction,
  type Session,
} from "./api";
import { Brand, Heading, Icon, Modal, Notice } from "./ui";
import Speech from "./Speech";
import History from "./History";
import Settings from "./Settings";
import Environments from "./Environments";
const navigation = [
  { id: "speech", name: "Text to speech", icon: "speech" },
  { id: "transcribe", name: "Speech to text", icon: "text" },
  { id: "history", name: "Job history", icon: "history" },
  { id: "settings", name: "Provider settings", icon: "settings" },
  { id: "environments", name: "Testing environments", icon: "test" },
];
const readPage = () =>
  navigation.some((n) => n.id === location.hash.slice(1)) ||
  location.hash === "#account"
    ? location.hash.slice(1)
    : "speech";
export default function App() {
  const [page, setPage] = createSignal(readPage()),
    [menu, setMenu] = createSignal(false),
    [login, setLogin] = createSignal(false),
    [connect, setConnect] = createSignal(false);
  const [error, setError] = createSignal<unknown>(),
    [busy, setBusy] = createSignal(false),
    [slt, setSlt] = createSignal(""),
    [org, setOrg] = createSignal("tos"),
    [key, setKey] = createSignal("");
  const [ready, setReady] = createSignal(false);
  const [health, { refetch: refreshHealth }] = createResource(async () => {
    try {
      await api("/health/ready");
      return true;
    } catch {
      return false;
    }
  });
  const initialError = new URL(location.href).searchParams.get("auth_error");
  onMount(async () => {
    const hash = () => {
      if (location.hash === "#main-content") return;
      setPage(readPage());
      setMenu(false);
    };
    window.addEventListener("hashchange", hash);
    onCleanup(() => window.removeEventListener("hashchange", hash));
    if (initialError) {
      setError(new Error(initialError));
      history.replaceState(null, "", location.pathname + location.hash);
      setLogin(true);
    }
    try {
      acceptSession(await api<Session>("/api/session"));
    } catch (err) {
      setError(err);
    } finally {
      setReady(true);
    }
  });
  function signin() {
    setSlt("");
    setOrg(session()?.org || "tos");
    setError();
    setLogin(true);
  }
  async function signIn(e: SubmitEvent) {
    e.preventDefault();
    setBusy(true);
    setError();
    try {
      await sessionAction("login", { slt: slt().trim() });
      setSlt("");
      setLogin(false);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  async function connectEnvironment(e: SubmitEvent) {
    e.preventDefault();
    setBusy(true);
    setError();
    try {
      await sessionAction("environment", {
        key: key().trim(),
        org: org().trim(),
      });
      setKey("");
      setConnect(false);
      location.hash = "environments";
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  async function logout() {
    setBusy(true);
    setError();
    try {
      await sessionAction("logout");
      location.hash = "speech";
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <a class="skip-link" href="#main-content">
        Skip to content
      </a>
      <div class="app-shell">
        <aside class="sidebar" classList={{ open: menu() }}>
          <Brand />
          <div class="workspace-context">
            <span class="eyebrow">ORGANIZATION</span>
            <button
              class="organization-button"
              onClick={() => {
                location.hash = "account";
                setMenu(false);
              }}
            >
              <span>
                {session()?.org === "tos"
                  ? "Team of Silicons"
                  : session()?.org || "Your workspace"}
              </span>
              <Icon name="down" size={14} />
            </button>
          </div>
          <nav aria-label="Main navigation">
            <For each={navigation}>
              {(item) => (
                <a
                  href={`#${item.id}`}
                  classList={{ selected: page() === item.id }}
                  aria-current={page() === item.id ? "page" : undefined}
                >
                  <Icon name={item.icon} />
                  <span>{item.name}</span>
                </a>
              )}
            </For>
          </nav>
          <div class="sidebar-bottom">
            <a
              href="https://briefcase.teamofsilicons.com"
              target="_blank"
              rel="noreferrer"
              class="sidebar-external"
            >
              Open Briefcase <Icon name="external" size={12} />
            </a>
            <button
              class="account-button"
              onClick={() =>
                session()?.authenticated
                  ? (location.hash = "account")
                  : signin()
              }
            >
              <span class="avatar">
                {session()?.user?.public_id?.slice(0, 1).toUpperCase() || "W"}
              </span>
              <span>
                <strong>{session()?.user?.public_id || "Your account"}</strong>
                <small>
                  {session()?.user?.actor_type || "Sign in with Silicon IAM"}
                </small>
              </span>
            </button>
          </div>
        </aside>
        <div class="workspace">
          <header class="topbar">
            <button
              class="icon-button mobile-menu"
              aria-label="Toggle navigation"
              aria-expanded={menu()}
              onClick={() => setMenu(!menu())}
            >
              <Icon name="settings" />
            </button>
            <span class="breadcrumb">
              Silicon <span>/</span> Waveform
            </span>
            <button
              class={`plane-button ${session()?.plane === "test" ? "test" : ""}`}
              onClick={() => {
                location.hash = "environments";
              }}
            >
              <span class="status-dot" />
              {session()?.plane === "test"
                ? `Test · ${session()?.environment?.name}`
                : "Production"}
            </button>
            <div class="topbar-right">
              <Show when={session()?.plane === "test"}>
                <button
                  class="text-button"
                  onClick={() =>
                    void sessionAction("switch", { plane: "production" }).catch(
                      setError,
                    )
                  }
                >
                  Exit test
                </button>
              </Show>
              <button
                class="text-button"
                disabled={busy()}
                onClick={() =>
                  session()?.authenticated ? void logout() : signin()
                }
              >
                {session()?.authenticated ? "Sign out" : "Sign in"}
              </button>
            </div>
          </header>
          <main id="main-content" tabindex="-1">
            <Show when={!login() && !connect()}>
              <Notice error={error()} />
            </Show>
            <Show
              when={ready()}
              fallback={
                <div class="empty" role="status">
                  <span class="spinner" />
                  Loading your workspace…
                </div>
              }
            >
              <Show when={contextVersion() + 1} keyed>
                {(_version) => (
                  <>
                    <div hidden={page() !== "speech"}>
                      <Speech operation="tts" signin={signin} />
                    </div>
                    <div hidden={page() !== "transcribe"}>
                      <Speech operation="stt" signin={signin} />
                    </div>
                    <Show when={page() === "history"}>
                      <History signin={signin} />
                    </Show>
                    <Show when={page() === "settings"}>
                      <Settings signin={signin} />
                    </Show>
                    <Show when={page() === "environments"}>
                      <Environments
                        signin={signin}
                        connect={() => {
                          setError();
                          setKey("");
                          setOrg(session()?.org || "tos");
                          setConnect(true);
                        }}
                      />
                    </Show>
                    <Show when={page() === "account"}>
                      <Heading
                        eyebrow="YOUR WORKSPACE"
                        title="Account & session"
                      >
                        Connected through Silicon IAM.
                      </Heading>
                      <section class="panel settings-panel">
                        <div class="stack">
                          <dl class="details">
                            <dt>Identity</dt>
                            <dd>
                              {session()?.user?.public_id || "Not signed in"}
                            </dd>
                            <dt>Account type</dt>
                            <dd>{session()?.user?.actor_type || "—"}</dd>
                            <dt>Organization</dt>
                            <dd>{session()?.org}</dd>
                            <dt>Role</dt>
                            <dd>
                              {session()?.user?.org_role ||
                                "Not disclosed by IAM"}
                            </dd>
                            <dt>Environment</dt>
                            <dd>
                              {session()?.plane === "test"
                                ? session()?.environment?.name
                                : "Production"}
                            </dd>
                          </dl>
                          <div class="actions">
                            <button class="button primary" onClick={signin}>
                              {session()?.authenticated
                                ? "Switch identity or organization"
                                : "Sign in with IAM"}
                            </button>
                            <Show when={session()?.authenticated}>
                              <button
                                class="button"
                                onClick={() =>
                                  void sessionAction("refresh").catch(setError)
                                }
                              >
                                Refresh session
                              </button>
                              <button
                                class="button"
                                disabled={busy()}
                                onClick={() => void logout()}
                              >
                                Sign out
                              </button>
                            </Show>
                            <Show
                              when={
                                session()?.testAvailable &&
                                session()?.plane === "production"
                              }
                            >
                              <button
                                class="button"
                                onClick={() =>
                                  void sessionAction("switch", {
                                    plane: "test",
                                  }).catch(setError)
                                }
                              >
                                Return to test environment
                              </button>
                            </Show>
                          </div>
                          <a
                            class="text-link"
                            href="https://iam.teamofsilicons.com/account"
                            target="_blank"
                            rel="noreferrer"
                          >
                            Manage your identity in IAM{" "}
                            <Icon name="external" size={14} />
                          </a>
                        </div>
                      </section>
                    </Show>
                  </>
                )}
              </Show>
            </Show>
          </main>
          <footer class="workspace-footer">
            <span>
              Silicon Waveform <span class="footer-separator">/</span> For
              agents and humans.
            </span>
            <button
              class="health-button"
              onClick={() => void refreshHealth()}
              aria-label="Refresh service status"
            >
              <span
                class="status-dot"
                classList={{ offline: health() === false }}
              />
              {health.loading
                ? "Checking service…"
                : health()
                  ? "Service operational"
                  : "Service unavailable · Retry"}
            </button>
          </footer>
        </div>
      </div>
      <Show when={login()}>
        <Modal
          title={
            session()?.plane === "test"
              ? "Sign in to your test environment"
              : "Welcome to Waveform"
          }
          close={() => {
            if (!busy()) {
              setLogin(false);
              setSlt("");
              setError();
            }
          }}
        >
          <div class="stack">
            <p class="muted">
              {session()?.plane === "test"
                ? "Use a short-lived code issued to Waveform in the linked IAM test environment."
                : "Sign in or create your account securely with Silicon IAM."}
            </p>
            <Show
              when={session()?.plane !== "test"}
              fallback={
                <form class="stack" onSubmit={signIn}>
                  <label>
                    Short-lived IAM code
                    <input
                      required
                      type="password"
                      autocomplete="off"
                      placeholder="oac_…"
                      value={slt()}
                      onInput={(e) => setSlt(e.currentTarget.value)}
                    />
                  </label>
                  <button class="button" disabled={busy() || !slt().trim()}>
                    {busy() ? "Signing in…" : "Sign in with code"}
                  </button>
                </form>
              }
            >
              <a class="button primary" href="/auth/start">
                Continue with IAM <Icon name="arrow" />
              </a>
            </Show>
            <Notice error={error()} />
            <p class="hint">
              Access tokens stay on the frontend server. Waveform never asks for
              your IAM password.
            </p>
          </div>
        </Modal>
      </Show>
      <Show when={connect()}>
        <Modal
          title="Connect a test environment"
          close={() => {
            if (!busy()) {
              setConnect(false);
              setKey("");
              setError();
            }
          }}
        >
          <form class="stack" onSubmit={connectEnvironment}>
            <p class="muted">
              Use an existing Waveform environment key. After connecting, sign
              in with an IAM code from that test environment.
            </p>
            <label>
              Organization handle
              <input
                required
                value={org()}
                onInput={(e) => setOrg(e.currentTarget.value)}
              />
            </label>
            <label>
              Waveform environment key
              <input
                required
                type="password"
                autocomplete="off"
                pattern="[A-Za-z0-9]{32}"
                placeholder="32-character key"
                value={key()}
                onInput={(e) => setKey(e.currentTarget.value)}
              />
            </label>
            <Notice error={error()} />
            <button class="button primary" disabled={busy()}>
              {busy() ? "Connecting…" : "Connect environment"}
            </button>
          </form>
        </Modal>
      </Show>
    </>
  );
}
