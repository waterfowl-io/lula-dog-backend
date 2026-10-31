import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { configure } from "@lula/sdk";
import App from "./App";
import "./index.css";

configure({
  baseUrl: import.meta.env.VITE_LULA_API_URL ?? "",
  mode: import.meta.env.VITE_LULA_API_URL ? "http" : "mock",
});

const root = document.getElementById("root");
if (!root) throw new Error("Missing #root element");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
