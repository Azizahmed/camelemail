// Horizontal swipes on the phone calendar (the week strip, the day view, the
// month picker). The element should have `touch-action: pan-y` so vertical
// scrolling stays the browser's while horizontal drags come here.

import type { Action } from "svelte/action";
import { swipeDir } from "./model";

export interface SwipeOptions {
  /** A finished swipe: 1 = next period, -1 = previous (mirrored in RTL). */
  onstep: (dir: -1 | 1) => void;
  /** The finger's horizontal offset while dragging sideways; 0 when released. */
  ondrag?: (dx: number) => void;
}

export const swipe: Action<HTMLElement, SwipeOptions> = (node, initial) => {
  let opts = initial;
  let x0 = 0;
  let y0 = 0;
  let dx = 0;
  let dy = 0;
  /** null until the gesture shows its direction. */
  let horizontal: boolean | null = null;
  let tracking = false;
  /** Swallow the click a swipe may end with. */
  let swiped = false;

  const start = (e: TouchEvent) => {
    if (e.touches.length !== 1) {
      tracking = false;
      return;
    }
    tracking = true;
    horizontal = null;
    swiped = false;
    x0 = e.touches[0].clientX;
    y0 = e.touches[0].clientY;
    dx = dy = 0;
  };
  const move = (e: TouchEvent) => {
    if (!tracking) return;
    dx = e.touches[0].clientX - x0;
    dy = e.touches[0].clientY - y0;
    if (horizontal === null && (Math.abs(dx) > 10 || Math.abs(dy) > 10)) horizontal = Math.abs(dx) > Math.abs(dy) * 1.2;
    if (horizontal) opts.ondrag?.(dx);
  };
  const end = () => {
    if (!tracking) return;
    tracking = false;
    if (!horizontal) return;
    opts.ondrag?.(0);
    const dir = swipeDir(dx, dy, getComputedStyle(node).direction === "rtl");
    if (dir) {
      swiped = true;
      setTimeout(() => (swiped = false), 350);
      opts.onstep(dir);
    }
  };
  const click = (e: MouseEvent) => {
    if (!swiped) return;
    swiped = false;
    e.stopPropagation();
    e.preventDefault();
  };

  node.addEventListener("touchstart", start, { passive: true });
  node.addEventListener("touchmove", move, { passive: true });
  node.addEventListener("touchend", end);
  node.addEventListener("touchcancel", end);
  node.addEventListener("click", click, true);
  return {
    update(o) {
      opts = o;
    },
    destroy() {
      node.removeEventListener("touchstart", start);
      node.removeEventListener("touchmove", move);
      node.removeEventListener("touchend", end);
      node.removeEventListener("touchcancel", end);
      node.removeEventListener("click", click, true);
    },
  };
};
