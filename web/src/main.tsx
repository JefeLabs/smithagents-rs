import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <main className="bg-background text-foreground min-h-screen p-4">SmithAgents chat</main>
  </StrictMode>,
);
