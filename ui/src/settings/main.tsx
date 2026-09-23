import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { guessOs } from "../shared/hotkey";
import "../shared/theme.css";
import "./settings.css";
import { Settings } from "./Settings";

document.documentElement.dataset.os = guessOs();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Settings />
  </StrictMode>,
);
