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
export const connectCast = (device: CastDevice) => invoke<void>("cast_connect", { device });
export const disconnectCast = () => invoke<void>("cast_disconnect");
export const loadOnCast = (request: CastLoad) => invoke<void>("cast_load", { request });
export const controlCast = (action: "play" | "pause" | "seek" | "volume", value?: number) =>
  invoke<void>("cast_control", { action, value: value ?? null });

export function listenToCast(handlers: {
  onReady: () => void;
  onStatus: (status: CastMediaStatus) => void;
  onClosed: (error: string | null) => void;
}): Promise<UnlistenFn[]> {
  return Promise.all([
    listen("cast://ready", () => handlers.onReady()),
    listen<CastMediaStatus>("cast://status", (e) => handlers.onStatus(e.payload)),
    listen<{ error: string | null }>("cast://closed", (e) => handlers.onClosed(e.payload.error)),
  ]);
}
