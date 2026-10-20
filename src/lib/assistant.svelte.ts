// Chat with the AI assistant (lives as long as the main window).

import { api, errorText, newRequestId } from "./api";
import type { ChatEvent, ThreadRef } from "./types";

export type ChatItem =
  | { kind: "user"; text: string; context: string | null }
  | { kind: "assistant"; text: string }
  | {
      kind: "tool";
      id: string;
      name: string;
      title: string;
      input: unknown;
      server: string | null;
      status: "running" | "approval" | "ok" | "error" | "denied";
      summary: string;
      /** Why the assistant asks first although this server may run tools without asking. */
      reason?: string;
    }
  | { kind: "error"; text: string };

const newConversation = () => `chat-${Date.now().toString(36)}`;

export const chat = $state({
  conversationId: newConversation(),
  items: [] as ChatItem[],
  running: false,
  requestId: "",
});

function onEvent(e: ChatEvent) {
  const last = chat.items[chat.items.length - 1];
  switch (e.type) {
    case "text":
      if (last?.kind === "assistant") last.text += e.delta;
      else chat.items.push({ kind: "assistant", text: e.delta });
      break;
    case "toolStart":
      chat.items.push({ kind: "tool", id: e.id, name: e.name, title: e.title, input: e.input, server: e.server, status: "running", summary: "" });
      break;
    case "approval": {
      const t = chat.items.findLast((i) => i.kind === "tool" && i.id === e.id);
      if (t && t.kind === "tool") {
        t.status = "approval";
        t.server = e.server;
        t.input = e.input;
        t.reason = e.reason;
      }
      break;
    }
    case "toolEnd": {
      const t = chat.items.findLast((i) => i.kind === "tool" && i.id === e.id);
      if (t && t.kind === "tool") {
        if (t.status !== "denied") t.status = e.ok ? "ok" : "error";
        t.summary = e.summary;
      }
      break;
    }
    case "error":
      if (e.message !== "Stopped") chat.items.push({ kind: "error", text: e.message });
      break;
    case "done":
      break;
  }
}

export async function ask(text: string, context: ThreadRef | null, contextLabel: string | null) {
  const t = text.trim();
  if (!t || chat.running) return;
  chat.items.push({ kind: "user", text: t, context: contextLabel });
  chat.running = true;
  const id = newRequestId();
  chat.requestId = id;
  // After "New chat" this request's late events belong to nothing on screen.
  const current = () => chat.requestId === id;
  try {
    await api.aiChat(chat.conversationId, id, t, context, (e) => {
      if (current()) onEvent(e);
    });
  } catch (e) {
    if (!current()) return;
    const msg = errorText(e);
    const last = chat.items[chat.items.length - 1];
    if (msg !== "Stopped" && !(last?.kind === "error" && last.text === msg)) chat.items.push({ kind: "error", text: msg });
  } finally {
    if (current()) {
      chat.running = false;
      // Anything still waiting is abandoned.
      for (const i of chat.items) if (i.kind === "tool" && (i.status === "running" || i.status === "approval")) i.status = "error";
    }
  }
}

export function stop() {
  if (chat.running) void api.aiCancel(chat.requestId);
}

export async function approve(id: string, allow: boolean) {
  const t = chat.items.findLast((i) => i.kind === "tool" && i.id === id);
  if (t && t.kind === "tool") t.status = allow ? "running" : "denied";
  await api.aiApprove(id, allow);
}

export function newChat() {
  stop();
  void api.aiChatReset(chat.conversationId);
  chat.conversationId = newConversation();
  chat.items = [];
  chat.requestId = "";
  chat.running = false;
}
