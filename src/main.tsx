import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import AgentEditor from "./AgentEditor";
import AgentSummary from "./AgentSummary";
import "./App.css";

// 同じ index.html を 3 用途で使う。?agent=<id> なら設定ウインドウ、?summary=<id> ならサマリ
// ウインドウ、無ければメインウインドウ(App.tsx が別ウインドウとして開く)。
const params = new URLSearchParams(location.search);
const agentId = params.get("agent");
const summaryId = params.get("summary");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {agentId ? <AgentEditor agentId={agentId} /> : summaryId ? <AgentSummary agentId={summaryId} /> : <App />}
  </React.StrictMode>,
);
