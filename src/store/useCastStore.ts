import { create } from "zustand";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { musicAudioEngine, type RemoteOutput } from "../lib/audio/musicAudioEngine";
import {
  connectCast,
  controlCast,
  disconnectCast,
  discoverCastDevices,
  listenToCast,
  loadOnCast,
  type CastDevice,
  type CastMediaStatus,
} from "../lib/cast/castBackend";
import { upgradeMusicImageUrl } from "../lib/thumbnails";
import { useMusicPlayerStore } from "./useMusicPlayerStore";

export type CastPhase = "off" | "connecting" | "connected" | "failed";

interface CastStore {
  devices: CastDevice[];
  searching: boolean;
  phase: CastPhase;
  device: CastDevice | null;
  search: () => Promise<void>;
  castTo: (device: CastDevice) => Promise<void>;
  stop: () => Promise<void>;
}

/**
 * The speaker side of music playback. The queue stays in the music player
 * store: the speaker gets one track at a time, and its status is fed back
 * through the same internal hooks the <audio> element would call.
 */
class CastRemote implements RemoteOutput {
  private position = 0;
  private positionAt = performance.now();
  private duration = 0;
  private playing = false;
  private wantPlaying = true;
  private pendingSeek: number | null = null;
  private sessionId = 0;
  private staleSessionId: number | null = null;
  private finishedSessionId: number | null = null;
  private ticker: ReturnType<typeof setInterval> | null = null;

  async load(url: string, mimeType: string | null): Promise<void> {
    const track = useMusicPlayerStore.getState().currentTrack;
    this.staleSessionId = this.sessionId;
    this.pendingSeek = null;
    this.setPosition(0);
    this.duration = 0;
    useMusicPlayerStore.getState()._setBuffering(true);
    await loadOnCast({
      url,
      contentType: (mimeType ?? "audio/mp4").split(";")[0]?.trim() || "audio/mp4",
      title: track?.title ?? null,
      artist: track ? track.artists.map((a) => a.name).join(", ") : null,
      album: track?.album?.name ?? null,
      artworkUrl: track ? upgradeMusicImageUrl(track.thumbnail, 512) ?? null : null,
      startSeconds: 0,
      autoplay: this.wantPlaying,
    });
  }

  play(): void {
    this.wantPlaying = true;
    void controlCast("play");
  }

  pause(): void {
    this.wantPlaying = false;
    this.position = this.getCurrentTime();
    void controlCast("pause");
  }

  seek(seconds: number): void {
    this.setPosition(seconds);
    if (this.staleSessionId !== null) this.pendingSeek = seconds;
    else void controlCast("seek", seconds);
  }

  setVolume(volume: number): void {
    void controlCast("volume", volume);
  }

  getCurrentTime(): number {
    if (!this.playing) return this.position;
    const t = this.position + (performance.now() - this.positionAt) / 1000;
    return this.duration > 0 ? Math.min(t, this.duration) : t;
  }

  getDuration(): number {
    return this.duration;
  }

  stop(): void {
    this.wantPlaying = false;
    void controlCast("pause");
  }

  dispose(): void {
    if (this.ticker) clearInterval(this.ticker);
    this.ticker = null;
  }

  onStatus(status: CastMediaStatus): void {
    // Media session ids only grow; id 0 is the backend reporting a refused load.
    const loadRefused = status.mediaSessionId === 0;
    if (!loadRefused && this.staleSessionId !== null && status.mediaSessionId <= this.staleSessionId) return;
    if (status.mediaSessionId > this.sessionId) this.sessionId = status.mediaSessionId;
    const player = useMusicPlayerStore.getState();
    switch (status.playerState) {
      case "PLAYING":
      case "PAUSED": {
        const firstForTrack = this.staleSessionId !== null;
        this.staleSessionId = null;
        this.playing = status.playerState === "PLAYING";
        if (status.duration) this.duration = status.duration;
        this.setPosition(status.currentTime);
        if (firstForTrack) {
          if (this.pendingSeek !== null) void controlCast("seek", this.pendingSeek);
          this.pendingSeek = null;
          if (this.wantPlaying !== this.playing) void controlCast(this.wantPlaying ? "play" : "pause");
        } else {
          this.wantPlaying = this.playing;
        }
        player._setBuffering(false);
        player._reflectPlaying(this.playing);
        player._syncTime(this.getCurrentTime(), this.duration);
        this.tick();
        break;
      }
      case "BUFFERING":
      case "LOADING":
        player._setBuffering(true);
        break;
      case "IDLE":
        if (status.idleReason === "FINISHED" && status.mediaSessionId !== this.finishedSessionId) {
          this.finishedSessionId = status.mediaSessionId;
          this.playing = false;
          player.handleEnded();
        } else if (status.idleReason === "ERROR") {
          this.playing = false;
          player._onPlaybackError();
        }
        break;
    }
  }

  private setPosition(seconds: number): void {
    this.position = seconds;
    this.positionAt = performance.now();
  }

  private tick(): void {
    if (this.ticker) return;
    this.ticker = setInterval(() => {
      if (this.playing) useMusicPlayerStore.getState()._syncTime(this.getCurrentTime(), this.duration);
    }, 500);
  }
}

let remote: CastRemote | null = null;
let unlisten: UnlistenFn[] = [];
/** Tags each connection; starts from the clock so a reloaded window never reuses one. */
let lastSession = Date.now();

/** Reloads the current track on whichever output the engine now uses. */
async function resumeCurrentTrack(play: boolean): Promise<void> {
  const player = useMusicPlayerStore.getState();
  if (!player.currentTrack) return;
  const at = player.progress;
  await player._loadIndex(player.currentIndex, { resumeAt: at });
  if (!play) useMusicPlayerStore.getState().pause();
}

async function backToComputer(): Promise<void> {
  remote?.dispose();
  remote = null;
  unlisten.forEach((u) => u());
  unlisten = [];
  musicAudioEngine.setRemote(null);
  await resumeCurrentTrack(false);
}

export const useCastStore = create<CastStore>((set, get) => ({
  devices: [],
  searching: false,
  phase: "off",
  device: null,

  search: async () => {
    if (get().searching) return;
    set({ searching: true });
    try {
      set({ devices: await discoverCastDevices() });
    } catch (e) {
      console.error("[cast] discovery failed", e);
    } finally {
      set({ searching: false });
    }
  },

  castTo: async (device) => {
    if (remote) await get().stop();
    set({ phase: "connecting", device });
    const castRemote = new CastRemote();
    const session = ++lastSession;
    remote = castRemote;
    const listeners = await listenToCast(session, {
      onReady: () => {
        if (remote !== castRemote) return;
        set({ phase: "connected" });
        const wasPlaying = useMusicPlayerStore.getState().isPlaying;
        musicAudioEngine.setRemote(castRemote);
        void resumeCurrentTrack(wasPlaying);
      },
      onStatus: (status) => {
        if (remote === castRemote) castRemote.onStatus(status);
      },
      onClosed: (error) => {
        if (remote !== castRemote) return;
        console.warn("[cast] session ended", error);
        set({ phase: "failed" });
        void backToComputer();
        // Lets the backend drop the session and close its LAN relay.
        void disconnectCast(session).catch(() => undefined);
      },
    });
    if (remote !== castRemote) {
      // Stopped or switched while the listeners were being set up.
      listeners.forEach((u) => u());
      return;
    }
    unlisten = listeners;
    try {
      await connectCast(device, session);
    } catch (e) {
      // A cancelled connect belongs to a cast the user already left.
      if (remote !== castRemote) return;
      console.error("[cast] connect failed", e);
      set({ phase: "failed" });
      await backToComputer();
    }
  },

  stop: async () => {
    set({ phase: "off", device: null });
    if (remote) await backToComputer();
    await disconnectCast().catch(() => undefined);
  },
}));
