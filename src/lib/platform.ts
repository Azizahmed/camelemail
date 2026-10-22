// Which shell this window runs: the desktop app (windows, tray, keyboard) or
// the phone app (one window of touch screens, src/mobile). Known before any
// Tauri call, so the first render already picks the right layout.

export const isMobile = typeof navigator !== "undefined" && /Android|iPhone|iPad|iPod/i.test(navigator.userAgent);

// Phone styles (src/mobile/mobile.css keys on it) from the first screen, sign-in included.
if (isMobile && typeof document !== "undefined") document.documentElement.classList.add("mobile");

export const isAndroid = typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent);

/** Android's WebView can leave its text menu (Copy, Share…) on screen with
 *  nothing selected: after a screen closes, or when another app or a system
 *  dialog gave the screen back. Close it then, unless text is selected here. */
export function dismissStaleTextMenu() {
  if (!isAndroid || getSelection()?.type === "Range") return;
  void import("@tauri-apps/api/core").then(({ invoke }) => invoke("dismiss_text_menu")).catch(() => {});
}

/** Files the user picks in the phone's own picker, read as attachments. */
export function pickFilesInPage(images: boolean): Promise<{ filename: string; mimeType: string; dataBase64: string }[]> {
  return new Promise((resolve, reject) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    if (images) input.accept = "image/*";
    input.style.display = "none";
    document.body.appendChild(input);
    const done = () => input.remove();
    input.addEventListener("cancel", () => {
      done();
      resolve([]);
    });
    input.addEventListener("change", async () => {
      try {
        const files = Array.from(input.files ?? []);
        if (files.reduce((n, f) => n + f.size, 0) > 25 * 1024 * 1024) throw new Error("Attachments are limited to 25 MB in total");
        const out = await Promise.all(
          files.map(async (f) => ({ filename: f.name, mimeType: f.type || "application/octet-stream", dataBase64: await toBase64(f) })),
        );
        resolve(out);
      } catch (e) {
        reject(e instanceof Error ? e.message : String(e));
      } finally {
        done();
      }
    });
    input.click();
  });
}

function toBase64(f: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).replace(/^data:[^,]*,/, ""));
    r.onerror = () => reject(r.error?.message ?? "Couldn't read the file");
    r.readAsDataURL(f);
  });
}
