import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import AgentEditor from "./AgentEditor";
import "./App.css";

// 同じ index.html を 2 用途で使う。?agent=<id> 付きで開かれたら設定ウインドウ、
// 無ければメインウインドウ(App.tsx が別ウインドウとして開く)。
const agentId = new URLSearchParams(location.search).get("agent");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{agentId ? <AgentEditor agentId={agentId} /> : <App />}</React.StrictMode>,
);
