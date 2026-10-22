import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface CastDevice {
  id: string;
  name: string;
  model: string;
  host: string;
  port: number;
  isGroup: boolean;
}

export interface CastMediaStatus {
  session: number;
  playerState: string;
  idleReason: string | null;
  currentTime: number;
  duration: number | null;
  mediaSessionId: number;
}

export interface CastLoad {
  url: string;
  contentType: string;
  title: string | null;
  artist: string | null;
  album: string | null;
  artworkUrl: string | null;
  startSeconds: number;
  autoplay: boolean;
}

export const discoverCastDevices = () => invoke<CastDevice[]>("cast_discover");
/** `session` tags every event of this connection, see `listenToCast`. */
export const connectCast = (device: CastDevice, session: number) => invoke<void>("cast_connect", { device, session });
/** Without `session` any cast stops; with it, only that one (if still current). */
export const disconnectCast = (session?: number) => invoke<void>("cast_disconnect", { session: session ?? null });
export const loadOnCast = (request: CastLoad) => invoke<void>("cast_load", { request });
export const controlCast = (action: "play" | "pause" | "seek" | "volume", value?: number) =>
  invoke<void>("cast_control", { action, value: value ?? null });

/**
 * Events of one connection only. A session that is being replaced still
 * reports its own close, which must not end the cast that replaced it.
 */
export function listenToCast(
  session: number,
  handlers: {
    onReady: () => void;
    onStatus: (status: CastMediaStatus) => void;
    onClosed: (error: string | null) => void;
  },
): Promise<UnlistenFn[]> {
  return Promise.all([
    listen<{ session: number }>("cast://ready", (e) => {
      if (e.payload.session === session) handlers.onReady();
    }),
    listen<CastMediaStatus>("cast://status", (e) => {
      if (e.payload.session === session) handlers.onStatus(e.payload);
    }),
    listen<{ session: number; error: string | null }>("cast://closed", (e) => {
      if (e.payload.session === session) handlers.onClosed(e.payload.error);
    }),
  ]);
}
