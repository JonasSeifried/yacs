import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { guessOs } from "../shared/hotkey";
import "../shared/theme.css";
import "./spotlight.css";
import { Spotlight } from "./Spotlight";

document.documentElement.dataset.os = guessOs();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Spotlight />
  </StrictMode>,
);
