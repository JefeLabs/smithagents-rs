import { useEffect, useState } from "react";
import { Chat } from "./Chat";
import { loadChatProps, type ChatProps } from "./load";

export function App({ load = loadChatProps }: { load?: () => Promise<ChatProps> }) {
  const [state, setState] = useState<{ props?: ChatProps; error?: string }>({});
  useEffect(() => {
    let live = true;
    load().then(
      (props) => live && setState({ props }),
      (e: unknown) => live && setState({ error: e instanceof Error ? e.message : "Unknown error." }),
    );
    return () => {
      live = false;
    };
  }, [load]);
  if (state.error !== undefined) {
    return (
      <main role="alert" className="bg-center-pane text-center-pane-foreground min-h-screen p-6">
        <h1 className="text-status-error mb-2 text-lg font-semibold">Could not open this chat</h1>
        <p>{state.error}</p>
      </main>
    );
  }
  if (!state.props) return <main className="bg-center-pane text-muted-foreground min-h-screen p-6">Loading...</main>;
  return <Chat {...state.props} />;
}
