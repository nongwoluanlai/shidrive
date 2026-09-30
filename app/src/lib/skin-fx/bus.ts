// Skin event bus: app code announces *what happened*; the active skin decides how (or whether)
// to react. Emitting is fire-and-forget and never throws into the caller.

/**
 * - message.send     the user sent a message
 * - agent.streaming  the assistant started replying (visible chat only)
 * - agent.done       the reply finished
 * - agent.error      the turn failed
 * - skin.enter       the skin was just activated
 * - character.click  the user clicked the character (opaque pixels, not over a control)
 */
export const SKIN_EVENTS = ["message.send", "agent.streaming", "agent.done", "agent.error", "skin.enter", "character.click"] as const;
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
