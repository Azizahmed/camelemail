// Small Svelte actions shared by the forms.

/** Focus the element once it's on screen (the first field of a form). */
export function autofocus(node: HTMLElement, enabled: boolean = true) {
  if (enabled) queueMicrotask(() => node.focus());
}
