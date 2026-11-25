let libraryRequest;

export function preloadShaka() {
  libraryRequest ??= new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = "/player/shaka-player.experimental.js";
    script.async = true;
    script.onload = () => {
      const shaka = globalThis.shaka;
      if (!shaka?.Player) {
        reject(new Error("Shaka Player failed to initialize."));
        return;
      }
      shaka.polyfill.installAll();
      resolve(shaka);
    };
    script.onerror = () => {
      script.remove();
      reject(new Error("Shaka Player could not load."));
    };
    document.head.append(script);
  }).catch((error) => {
    libraryRequest = undefined;
    throw error;
  });
  return libraryRequest;
}

const languageNames = new Intl.DisplayNames(["en"], { type: "language" });

function audioLabel(track, index) {
  let language;
  try {
    language = track.language && track.language !== "und" ? languageNames.of(track.language) : "";
  } catch {
    language = track.language;
  }
  const name = track.label || language || `Track ${index + 1}`;
  const channels = track.channelsCount;
  return channels ? `${name} · ${channels} ${channels === 1 ? "channel" : "channels"}` : name;
}

function playbackError(shaka, value) {
  const code = value?.code;
  const known = shaka.util.Error.Code;
  const incompatible = typeof code === "number" && [
    known.CONTENT_UNSUPPORTED_BY_BROWSER, known.NO_VARIANTS, known.HLS_COULD_NOT_GUESS_CODECS,
    known.UNABLE_TO_GUESS_MANIFEST_TYPE, known.VIDEO_ERROR, known.MKV_MISSING_CUES,
  ].includes(code);
  const error = new Error(
    incompatible
      ? "Shaka cannot open this release on this device. Try Movi Player or a different release."
      : "Shaka could not load this mirror. Try a different release.",
  );
  error.incompatible = incompatible;
  return error;
}

function sourceMime(filename) {
  if (/\.(mkv|mka)$/i.test(filename)) return "video/x-matroska";
  if (/\.mp4$/i.test(filename)) return "video/mp4";
  return undefined;
}

export function shakaSession(shaka, video, audioControl, onError) {
  if (!shaka.Player.isBrowserSupported()) {
    const error = new Error("Shaka is unavailable in this browser. Choose Movi Player.");
    error.incompatible = true;
    throw error;
  }

  const player = new shaka.Player();
  let disposed = false;
  let audioTracks = [];
  player.configure({
    streaming: { bufferingGoal: 24, rebufferingGoal: 2, bufferBehind: 30 },
  });

  const updateTracks = () => {
    if (disposed) return;
    audioTracks = player.getAudioTracks();
    audioControl.input.replaceChildren(...audioTracks.map((track, index) => {
      const option = new Option(audioLabel(track, index), String(index));
      option.selected = track.active;
      return option;
    }));
    audioControl.container.hidden = audioTracks.length < 2;
  };
  const selectTrack = () => {
    const track = audioTracks[audioControl.input.selectedIndex];
    if (track && !track.active) player.selectAudioTrack(track, 1);
  };
  audioControl.input.addEventListener("change", selectTrack);
  player.addEventListener("trackschanged", updateTracks);
  player.addEventListener("audiotrackchanged", updateTracks);
  player.addEventListener("error", (event) => {
    if (!disposed && event.detail?.severity === shaka.util.Error.Severity.CRITICAL) {
      onError(playbackError(shaka, event.detail));
    }
  });

  return {
    async load(url, startAt, filename) {
      try {
        await player.attach(video);
        if (disposed) return;
        await player.load(new URL(url, location.href).href, Math.max(0, startAt), sourceMime(filename));
        updateTracks();
      } catch (error) {
        if (!disposed) throw playbackError(shaka, error);
      }
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      audioControl.input.removeEventListener("change", selectTrack);
      audioControl.container.hidden = true;
      audioControl.input.replaceChildren();
      void player.destroy().catch(() => {});
    },
  };
}
