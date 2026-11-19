import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import App from "./App";

if (import.meta.env.DEV) void import("./dev/contrastAudit");
// Dev builds, or a production build made with VITE_FROSTCAP_SELFTEST=1, include the self-test harness;
// it still runs only when the backend reports `launch_options().selftest`. Otherwise the bundler drops it.
if (import.meta.env.DEV || import.meta.env.VITE_FROSTCAP_SELFTEST === "1") void import("./selftest").then((m) => m.maybeRunSelfTest());

// Dev only: lets the screenshot script drive the editor store.
if (import.meta.env.DEV) void import("./store/editor").then((m) => ((window as unknown as { __editor: unknown }).__editor = m.useEditor));

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
