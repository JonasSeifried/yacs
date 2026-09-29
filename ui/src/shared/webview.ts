/**
 * The webview's own right-click menu (Back, Reload, Save as, Print…) belongs
 * to a browser, not to YACS. Text fields and selected text keep theirs, for
 * cutting, copying and pasting.
 */
export function limitContextMenu() {
  document.addEventListener("contextmenu", (e) => {
    const editable = e.target instanceof Element && e.target.closest("input, textarea, [contenteditable]");
    if (!editable && !window.getSelection()?.toString()) e.preventDefault();
  });
}
