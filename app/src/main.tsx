import React from "react";
import ReactDOM from "react-dom/client";
import { HashRouter, Route, Routes } from "react-router-dom";
import App from "./App";
import TimelinePage from "./pages/Timeline";
import PlannerPage from "./pages/Planner";
import SessionsPage from "./pages/Sessions";
import SettingsPage from "./pages/Settings";
import ReportsPage from "./pages/Reports";
import SearchPage from "./pages/Search";
import "./index.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <HashRouter>
      <Routes>
        <Route path="/" element={<App />} />
        <Route path="/timeline" element={<TimelinePage />} />
        <Route path="/planner" element={<PlannerPage />} />
        <Route path="/sessions" element={<SessionsPage />} />
        <Route path="/settings" element={<SettingsPage />} />
        <Route path="/reports" element={<ReportsPage />} />
        <Route path="/search" element={<SearchPage />} />
      </Routes>
    </HashRouter>
  </React.StrictMode>,
);