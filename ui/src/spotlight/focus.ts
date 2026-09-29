/**
 * Calls `onLeft` when the window loses focus to something else. A click into
 * the HTML preview blurs the window too, but only moves focus into its
 * iframe, where our keys don't arrive: that takes focus straight back
 * instead. Only known a moment after the blur. Returns a function that stops
 * listening.
 */
export function onFocusLeft(onLeft: () => void): () => void {
  let pending: ReturnType<typeof setTimeout> | undefined;
  const onBlur = () => {
    clearTimeout(pending);
    pending = setTimeout(() => {
      if (document.activeElement instanceof HTMLIFrameElement) {
        document.activeElement.blur();
        window.focus();
      } else {
        onLeft();
      }
    });
  };
  window.addEventListener("blur", onBlur);
  return () => {
    clearTimeout(pending);
    window.removeEventListener("blur", onBlur);
  };
}
