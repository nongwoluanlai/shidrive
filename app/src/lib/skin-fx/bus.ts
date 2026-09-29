// Skin event bus: app code announces *what happened*; the active skin decides how (or whether)
// to react. Emitting is fire-and-forget and never throws into the caller.

export const SKIN_EVENTS = ["message.send", "agent.streaming", "agent.done", "agent.error", "skin.enter"] as const;
export type SkinEvent = (typeof SKIN_EVENTS)[number];

type Handler = (event: SkinEvent) => void;
const handlers = new Set<Handler>();

export function onSkinEvent(handler: Handler): () => void {
  handlers.add(handler);
  return () => handlers.delete(handler);
}

export function emitSkinEvent(event: SkinEvent) {
  for (const handler of handlers) {
    try { handler(event); } catch (error) { console.warn("[skin-fx]", error); }
  }
}

export const isSkinEvent = (value: unknown): value is SkinEvent => SKIN_EVENTS.includes(value as SkinEvent);
