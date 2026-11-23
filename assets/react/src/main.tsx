import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import { applyTheme, initialTheme } from "./theme";
import "./index.css";

// Apply the stored (or OS) theme before first paint so the shell never
// flashes the wrong theme.
applyTheme(initialTheme(), false);

const root = document.getElementById("root");
if (!root) throw new Error("React mount point #root is missing");

createRoot(root).render(
  <StrictMode>
    <BrowserRouter basename="/react">
      <App />
    </BrowserRouter>
  </StrictMode>,
);
