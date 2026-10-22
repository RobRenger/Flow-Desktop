import { useEffect } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { Loader2, RefreshCw, Speaker, Users } from "lucide-react";

import { useCastStore } from "../../store/useCastStore";
import { getString } from "../../lib/i18n/index";

export function CastPopover({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { devices, searching, phase, device, search, castTo, stop } = useCastStore();

  useEffect(() => {
    if (open) void search();
  }, [open, search]);

  const status =
    phase === "connecting" && device ? getString("music_cast_connecting", device.name)
    : phase === "connected" && device ? getString("music_cast_playing_on", device.name)
    : phase === "failed" && device ? getString("music_cast_failed", device.name)
    : null;

  return (
    <AnimatePresence>
      {open && (
        <motion.div
          key="cast-popover"
          initial={{ opacity: 0, y: 10, scale: 0.97 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          exit={{ opacity: 0, y: 10, scale: 0.97 }}
          transition={{ type: "spring", stiffness: 420, damping: 34, mass: 0.8 }}
          style={{ transformOrigin: "bottom center" }}
          className="flex w-72 flex-col gap-1 rounded-2xl border border-chrome-neutral-800 bg-surface-container p-2"
        >
          <div className="flex items-center justify-between px-2 py-1">
            <span className="text-sm font-medium text-chrome-neutral-100">{getString("music_cast")}</span>
            <button
              type="button"
              onClick={() => void search()}
              aria-label={getString("music_cast_refresh")}
              className="grid h-7 w-7 place-items-center rounded-full text-chrome-neutral-400 transition-colors duration-200 ease-out hover:text-chrome-neutral-100"
            >
              {searching ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCw className="h-4 w-4" />}
            </button>
          </div>
          {status && <p className="px-2 pb-1 text-xs text-chrome-neutral-400">{status}</p>}
          {devices.length === 0 ? (
            <p className="px-2 py-2 text-xs text-chrome-neutral-400">
              {getString(searching ? "music_cast_searching" : "music_cast_none")}
            </p>
          ) : (
            <ul className="flex max-h-72 flex-col overflow-y-auto">
              {devices.map((d) => {
                const active = device?.id === d.id && (phase === "connected" || phase === "connecting");
                const Icon = d.isGroup ? Users : Speaker;
                return (
                  <li key={d.id}>
                    <button
                      type="button"
                      onClick={() => {
                        void castTo(d);
                        onClose();
                      }}
                      className={`flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left transition-colors duration-200 ease-out hover:bg-chrome-neutral-800 ${active ? "text-accent" : "text-chrome-neutral-200"}`}
                    >
                      <Icon className="h-4 w-4 shrink-0" />
                      <span className="flex min-w-0 flex-col">
                        <span className="truncate text-sm">{d.name}</span>
                        {d.isGroup && <span className="text-xs text-chrome-neutral-500">{getString("music_cast_group")}</span>}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
          {(phase === "connected" || phase === "connecting") && (
            <button
              type="button"
              onClick={() => {
                void stop();
                onClose();
              }}
              className="mt-1 rounded-full border border-chrome-neutral-800 px-3 py-1.5 text-sm text-chrome-neutral-200 transition-colors duration-200 ease-out hover:bg-chrome-neutral-800"
            >
              {getString("music_cast_stop")}
            </button>
          )}
        </motion.div>
      )}
    </AnimatePresence>
  );
}
