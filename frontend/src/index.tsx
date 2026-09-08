import { render } from "solid-js/web";
import { ErrorBoundary } from "solid-js";
import App from "./App";
import "./styles.css";
render(
  () => (
    <ErrorBoundary
      fallback={() => (
        <main class="fatal">
          <h1>Waveform needs a refresh</h1>
          <p>Your session is kept on the server. Reload to reconnect.</p>
          <button onClick={() => location.reload()}>Reload Waveform</button>
        </main>
      )}
    >
      <App />
    </ErrorBoundary>
  ),
  document.getElementById("root")!,
);
