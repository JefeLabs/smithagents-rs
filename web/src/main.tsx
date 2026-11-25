import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import { Chat } from "./Chat";
import { loadChatProps, type ChatProps } from "./load";

function App() {
  const [state, setState] = useState<{ props?: ChatProps; error?: string }>({});
  useEffect(() => {
    let live = true;
    loadChatProps().then(
      (props) => live && setState({ props }),
      (e: unknown) => live && setState({ error: e instanceof Error ? e.message : "Unknown error." }),
    );
    return () => {
      live = false;
    };
  }, []);
  if (state.error !== undefined) {
    return (
      <main role="alert" className="bg-background text-foreground min-h-screen p-6">
        <h1 className="text-status-error mb-2 text-lg font-semibold">Could not open this chat</h1>
        <p>{state.error}</p>
      </main>
    );
  }
  if (!state.props) return <main className="bg-background text-muted-foreground min-h-screen p-6">Loading...</main>;
  return <Chat {...state.props} />;
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
