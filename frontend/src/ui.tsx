import {
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  type JSX,
} from "solid-js";
import {
  ApiError,
  models,
  providerNames,
  type Operation,
  type Provider,
} from "./api";
export function Icon(props: { name: string; size?: number }) {
  const paths: Record<string, JSX.Element> = {
    speech: (
      <>
        <path d="M3 10v4m4-8v12m5-16v20m5-17v14m4-10v4" />
      </>
    ),
    text: (
      <>
        <path d="M8 3h13M8 8h10M8 13h13M8 18h7M3 3v18" />
      </>
    ),
    history: (
      <>
        <path d="M3 11a9 9 0 1 1 2.4 7M3 4v7h7M12 7v5l3 2" />
      </>
    ),
    settings: (
      <>
        <path d="M4 6h16M4 12h16M4 18h16M8 3v6m8 0v6m-6 0v6" />
      </>
    ),
    test: (
      <>
        <path d="m9 3 6 0m-5 0v6l-6 10q-1 2 2 2h12q3 0 2-2L14 9V3M7 15h10" />
      </>
    ),
    arrow: (
      <>
        <path d="M4 12h16m-6-6 6 6-6 6" />
      </>
    ),
    external: (
      <>
        <path d="M14 3h7v7m0-7L10 14M10 3H3v18h18v-7" />
      </>
    ),
    chevron: <path d="m8 4 8 8-8 8" />,
    up: <path d="m6 14 6-6 6 6" />,
    down: <path d="m6 10 6 6 6-6" />,
    close: <path d="m6 6 12 12M6 18 18 6" />,
    check: <path d="m5 12 4 4L19 6" />,
  };
  return (
    <svg
      aria-hidden="true"
      width={props.size || 18}
      height={props.size || 18}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      {paths[props.name] || paths.speech}
    </svg>
  );
}
export function Brand() {
  return (
    <a class="brand" href="#speech">
      <img src="/brand/mark.svg" alt="" />
      silicon<span>WAVEFORM</span>
    </a>
  );
}
export function Notice(props: { error?: unknown; message?: string }) {
  return (
    <Show when={props.error || props.message}>
      <div
        role={props.error ? "alert" : "status"}
        class={`notice ${props.error ? "error" : "success"}`}
      >
        <p>
          {props.error instanceof Error
            ? props.error.message
            : props.error
              ? String(props.error)
              : props.message}
        </p>
        <Show when={props.error instanceof ApiError && props.error.requestId}>
          <small class="mono">
            Request {(props.error as ApiError).requestId}
          </small>
        </Show>
        <Show when={props.error instanceof ApiError && props.error.retryAfter}>
          <small>
            Try again after {(props.error as ApiError).retryAfter} seconds.
          </small>
        </Show>
      </div>
    </Show>
  );
}
export function Empty(props: {
  icon?: string;
  title: string;
  children?: JSX.Element;
}) {
  return (
    <div class="empty">
      <div class="empty-icon">
        <Icon name={props.icon || "speech"} size={25} />
      </div>
      <h3>{props.title}</h3>
      <p>{props.children}</p>
    </div>
  );
}
export function Copy(props: { value: string; label?: string }) {
  const [copied, setCopied] = createSignal(false),
    [error, setError] = createSignal(false);
  return (
    <>
      <button
        type="button"
        class="button"
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(props.value);
            setCopied(true);
            setError(false);
          } catch {
            setError(true);
          }
        }}
      >
        {copied() ? "Copied" : props.label || "Copy"}
      </button>
      <Show when={error()}>
        <small role="alert">
          Clipboard unavailable. Select and copy the text manually.
        </small>
      </Show>
    </>
  );
}
export function Modal(props: {
  title: string;
  close: () => void;
  children: JSX.Element;
  wide?: boolean;
}) {
  let dialog!: HTMLDialogElement;
  const previous = document.activeElement as HTMLElement | null;
  onMount(() => dialog.showModal());
  onCleanup(() => {
    dialog.close();
    previous?.focus();
  });
  return (
    <dialog
      ref={dialog}
      classList={{ wide: props.wide }}
      onCancel={(e) => {
        e.preventDefault();
        props.close();
      }}
      onClick={(e) => {
        if (e.target === dialog) {
          const r = dialog.getBoundingClientRect();
          if (
            e.clientX < r.left ||
            e.clientX > r.right ||
            e.clientY < r.top ||
            e.clientY > r.bottom
          )
            props.close();
        }
      }}
      aria-label={props.title}
    >
      <div class="modal-head">
        <h2>{props.title}</h2>
        <button
          class="icon-button"
          aria-label="Close dialog"
          onClick={props.close}
        >
          <Icon name="close" />
        </button>
      </div>
      {props.children}
    </dialog>
  );
}
export function Order(props: {
  operation: Operation;
  value: Provider[];
  change: (value: Provider[]) => void;
  disabled?: boolean;
}) {
  function move(index: number, by: number) {
    const next = [...props.value];
    [next[index], next[index + by]] = [next[index + by], next[index]];
    props.change(next);
  }
  return (
    <ol class="order-list">
      <For each={props.value}>
        {(provider, index) => (
          <li>
            <span class="order-number mono">0{index() + 1}</span>
            <div class="grow">
              <strong>{providerNames[provider]}</strong>
              <small>{models[props.operation][provider]}</small>
            </div>
            <span class="order-controls">
              <button
                type="button"
                class="icon-button"
                disabled={props.disabled || index() === 0}
                aria-label={`Move ${providerNames[provider]} earlier`}
                onClick={() => move(index(), -1)}
              >
                <Icon name="up" size={16} />
              </button>
              <button
                type="button"
                class="icon-button"
                disabled={props.disabled || index() === props.value.length - 1}
                aria-label={`Move ${providerNames[provider]} later`}
                onClick={() => move(index(), 1)}
              >
                <Icon name="down" size={16} />
              </button>
            </span>
          </li>
        )}
      </For>
    </ol>
  );
}
export function Heading(props: {
  eyebrow: string;
  title: string;
  children: JSX.Element;
  action?: JSX.Element;
}) {
  return (
    <header class="page-heading">
      <div>
        <p class="eyebrow">{props.eyebrow}</p>
        <h1>{props.title}</h1>
        <p class="muted">{props.children}</p>
      </div>
      {props.action}
    </header>
  );
}
