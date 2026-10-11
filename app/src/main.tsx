import { createRoot } from "react-dom/client";
import { App } from "./App";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";

// StrictMode は開発時にマウントを2回行い、デーモンへの接続も2回になるので使わない。
createRoot(document.getElementById("root")!).render(<App />);
