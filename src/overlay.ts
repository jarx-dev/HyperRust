// Recording indicator: reacts to state and level events from the Rust side.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type OverlayState =
  | { state: "hidden" }
  | { state: "recording"; mode: string; switchable: boolean }
  | { state: "processing"; stage: string }
  | { state: "done"; message: string }
  | { state: "error"; message: string };

const BARS = 13;
const CENTER = (BARS - 1) / 2;
// Fade-outs must end before overlay.rs hides the window (Done 600 ms, Error 3500 ms).
const LEAVE_DELAY_MS = { done: 400, error: 3260 } as const;
const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");

const root = document.getElementById("root") as HTMLDivElement;
const shell = document.getElementById("shell") as HTMLDivElement;
const pill = document.getElementById("pill") as HTMLDivElement;
const label = document.getElementById("label") as HTMLSpanElement;
const modeButton = document.getElementById("mode") as HTMLButtonElement;
const modeName = document.getElementById("mode-name") as HTMLSpanElement;
const wave = document.getElementById("wave") as HTMLSpanElement;
const bars = Array.from({ length: BARS }, (_, i) => {
  const bar = document.createElement("i");
  bar.style.setProperty("--i", String(i));
  wave.append(bar);
  return bar;
});
// Newest level first: it drives the middle bar, older ones move outwards.
const history: number[] = new Array(CENTER + 1).fill(0);
let current: OverlayState["state"] = "hidden";
let leaving: Animation | undefined;

function enter(): void {
  const lift = reduceMotion.matches ? "none" : "translateY(14px) scale(0.9)";
  shell.animate([{ opacity: 0, transform: lift }, { opacity: 1, transform: "none" }], {
    duration: 420,
    easing: "cubic-bezier(0.2, 1.35, 0.35, 1)",
  });
}

function leave(delay: number): void {
  const drop = reduceMotion.matches ? "none" : "translateY(8px) scale(0.96)";
  leaving = shell.animate([{ opacity: 1, transform: "none" }, { opacity: 0, transform: drop }], {
    duration: 200,
    delay,
    easing: "ease-in",
    fill: "forwards",
  });
}

function drawLevels(): void {
  bars.forEach((bar, i) => {
    const distance = Math.abs(i - CENTER);
    const level = history[distance] * (1 - distance * 0.07);
    bar.style.transform = `scaleY(${(0.14 + 0.86 * level).toFixed(3)})`;
  });
  root.style.setProperty("--lvl", history[0].toFixed(3));
}

function render(s: OverlayState): void {
  const previous = current;
  current = s.state;
  leaving?.cancel();
  leaving = undefined;
  root.className = `root ${s.state}`;
  modeButton.hidden = true;
  if (s.state === "hidden") {
    shell.getAnimations().forEach((a) => a.cancel());
    // Flush styles while hidden so the next show restarts the CSS animations
    // instead of painting the previous state.
    void root.offsetWidth;
    return;
  }
  if (previous === "hidden" || previous === "done" || previous === "error") {
    enter();
  }
  switch (s.state) {
    case "recording":
      label.textContent = s.switchable ? "" : s.mode;
      modeName.textContent = s.mode;
      modeButton.hidden = !s.switchable;
      history.fill(0);
      drawLevels();
      break;
    case "processing":
      label.textContent = `${s.stage}…`;
      break;
    case "done":
    case "error":
      label.textContent = s.message;
      leave(LEAVE_DELAY_MS[s.state]);
      break;
  }
}

void listen<OverlayState>("overlay-state", (e) => render(e.payload));
// Like the shortcut's stop press. In other states overlay.rs lets clicks
// through to the window below.
pill.addEventListener("click", () => {
  if (current === "recording") void invoke("finish_dictation");
});
// The mode name switches to the next mode instead (also for this dictation).
modeButton.addEventListener("click", (e) => {
  e.stopPropagation();
  if (current !== "recording") return;
  void invoke<string | null>("cycle_mode").then((name) => {
    if (!name || current !== "recording") return;
    modeName.textContent = name;
    if (!reduceMotion.matches) {
      modeButton.animate([{ transform: "scale(0.9)" }, { transform: "none" }], { duration: 220, easing: "cubic-bezier(0.2, 1.35, 0.35, 1)" });
    }
  });
});
void listen<number>("overlay-level", (e) => {
  if (current !== "recording") return;
  const level = Math.min(1, Math.max(0, e.payload));
  history.pop();
  // Soften the low end so background noise barely moves the bars.
  history.unshift(level ** 1.4);
  drawLevels();
});
