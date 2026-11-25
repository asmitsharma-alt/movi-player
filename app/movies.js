const searchForm = document.querySelector("#search");
const query = document.querySelector("#query");
const movieList = document.querySelector("#movies");
const searchStatus = document.querySelector("#search-status");
const details = document.querySelector("#details");
const movieTitle = document.querySelector("#movie-title");
const releaseList = document.querySelector("#releases");
const releaseStatus = document.querySelector("#release-status");
const playback = document.querySelector("#playback");
const playStatus = document.querySelector("#play-status");
const stage = document.querySelector("#player-stage");
const lighterButton = document.querySelector("#lighter-release");

let searchTimer;
let searchRequest;
let detailRequest;
let playerRequest;
let warmRequest;
let warmRelease;
let hoverTimer;
let activeReleases = [];
let playerModule;
let selectedPoster;
let playNonce = 0;

async function api(path, options) {
  const response = await fetch(path, options);
  if (response.status === 401) {
    location.assign("/sign-in");
    throw new Error("Sign in to continue.");
  }
  const result = await response.json().catch(() => null);
  if (!response.ok) throw new Error(result?.error ?? "Something went wrong. Please try again.");
  return result;
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
}

function preloadPlayer() {
  playerModule ??= import("/player/element.js").catch((error) => {
    playerModule = undefined;
    throw error;
  });
  return playerModule;
}

function getPlayback(releaseId, signal, previous) {
  return api("/api/playback", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ releaseId, previous }),
    signal,
  });
}

function displaySearch(show) {
  searchForm.hidden = !show;
  movieList.hidden = !show;
  searchStatus.hidden = !show;
  details.hidden = show;
}

async function search() {
  searchRequest?.abort();
  const value = query.value.trim();
  movieList.replaceChildren();
  if (value.length < 2) {
    searchStatus.textContent = value ? "Enter at least two characters." : "";
    return;
  }
  const request = new AbortController();
  searchRequest = request;
  searchStatus.textContent = "Searching…";

  try {
    const movies = await api(`/api/movies?q=${encodeURIComponent(value)}`, { signal: request.signal });
    if (request.signal.aborted) return;
    searchStatus.textContent = movies.length ? "" : "No matching movies.";
    for (const movie of movies) {
      const card = element("button", "poster");
      const art = element("span", "poster-image");
      if (movie.posterUrl) {
        const image = element("img");
        image.src = movie.posterUrl;
        image.alt = "";
        image.loading = "lazy";
        image.addEventListener("error", () => art.replaceChildren("No poster"), { once: true });
        art.append(image);
      } else {
        art.textContent = "No poster";
      }
      const title = element("span", "poster-title", movie.title);
      if (movie.year) title.append(element("small", "", movie.year));
      card.append(art, title);
      card.addEventListener("click", () => openMovie(movie, card));
      const item = element("li");
      item.append(card);
      movieList.append(item);
    }
  } catch (error) {
    if (!request.signal.aborted) searchStatus.textContent = error.message;
  }
}

searchForm.addEventListener("submit", (event) => {
  event.preventDefault();
  clearTimeout(searchTimer);
  search();
});
query.addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchRequest?.abort();
  searchTimer = setTimeout(search, 250);
});

function warm(release) {
  if (warmRelease?.id === release.id && warmRelease.expiresAt > Date.now()) return;
  warmRequest?.abort();
  warmRequest = new AbortController();
  const entry = { id: release.id, expiresAt: Infinity };
  warmRelease = entry;
  entry.result = getPlayback(release.id, warmRequest.signal).then((source) => {
    entry.expiresAt = Date.now() + source.expiresIn * 1000;
    return source;
  }).catch(() => {
    entry.expiresAt = 0;
    return null;
  });
}

function releaseRow(release, movie) {
  const row = element("li", "release");
  const text = element("div");
  text.append(element("div", "release-name", release.filename));
  const size = release.sizeBytes
    ? `${(release.sizeBytes / 1024 ** 3).toFixed(2)} GiB`
    : undefined;
  const metadata = [release.quality, release.codec, release.language, size, release.recommended && "Recommended"]
    .filter(Boolean).join(" · ");
  if (metadata) text.append(element("p", "release-info", metadata));
  const button = element("button", release.recommended ? "primary" : "", "Play");
  button.setAttribute("aria-label", `Play ${release.filename}`);
  const deferWarm = () => {
    clearTimeout(hoverTimer);
    hoverTimer = setTimeout(() => warm(release), 300);
  };
  button.addEventListener("pointerenter", deferWarm);
  button.addEventListener("pointerleave", () => clearTimeout(hoverTimer));
  button.addEventListener("focus", deferWarm);
  button.addEventListener("blur", () => clearTimeout(hoverTimer));
  button.addEventListener("click", () => {
    clearTimeout(hoverTimer);
    play(movie, release);
  });
  row.append(text, button);
  return row;
}

async function openMovie(movie, poster) {
  detailRequest?.abort();
  warmRequest?.abort();
  clearTimeout(hoverTimer);
  activeReleases = [];
  const request = new AbortController();
  detailRequest = request;
  selectedPoster = poster;
  displaySearch(false);
  movieTitle.textContent = movie.title;
  movieTitle.focus();
  releaseList.replaceChildren();
  releaseStatus.textContent = "Loading releases…";
  preloadPlayer().then((module) => module.loadWasmModule?.()).catch(() => {});
  try {
    const releases = await api(`/api/movies/${encodeURIComponent(movie.id)}/releases`, { signal: request.signal });
    if (request.signal.aborted) return;
    activeReleases = releases;
    releaseStatus.textContent = releases.length ? "" : "No video releases are currently available.";
    releaseList.replaceChildren(...releases.map((release) => releaseRow(release, movie)));
    const recommended = releases.find((release) => release.recommended);
    if (recommended) warm(recommended);
  } catch (error) {
    if (!request.signal.aborted) {
      releaseStatus.textContent = error.message;
      const retry = element("button", "quiet", "Retry");
      retry.addEventListener("click", () => openMovie(movie, poster));
      releaseList.replaceChildren(element("li"));
      releaseList.firstChild.append(retry);
    }
  }
}

document.querySelector("#back").addEventListener("click", () => {
  detailRequest?.abort();
  playerRequest?.abort();
  warmRequest?.abort();
  clearTimeout(hoverTimer);
  playNonce++;
  stage.replaceChildren();
  playback.hidden = true;
  displaySearch(true);
  selectedPoster?.focus();
});

function height(release) {
  return Number.parseInt(release.quality, 10) || 0;
}

function lighterRelease(current) {
  return activeReleases
    .filter((candidate) => candidate.id !== current.id && (
      (height(candidate) > 0 && height(candidate) < height(current)) ||
      (candidate.sizeBytes && current.sizeBytes && candidate.sizeBytes < current.sizeBytes * 0.75)
    ))
    .sort((a, b) => (
      Math.abs(Math.min(height(current), 1080) - height(a)) -
      Math.abs(Math.min(height(current), 1080) - height(b)) ||
      (a.sizeBytes ?? Infinity) - (b.sizeBytes ?? Infinity)
    ))[0];
}

async function play(movie, release, startAt = 0) {
  playerRequest?.abort();
  const request = new AbortController();
  playerRequest = request;
  const run = ++playNonce;
  const started = performance.now();
  let firstFrame = false;
  let sameSourceRelayUsed = false;
  let alternateUsed = false;
  let refreshed = false;
  const lighter = lighterRelease(release);

  playback.hidden = false;
  lighterButton.hidden = true;
  stage.replaceChildren();
  playStatus.textContent = "Finding a playable mirror…";
  playback.scrollIntoView({ block: "nearest" });

  async function attach(source, url = source.url, resumeAt = 0) {
    await preloadPlayer();
    if (request.signal.aborted) return;
    const player = element("movi-player");
    player.setAttribute("controls", "");
    player.setAttribute("autoplay", "");
    player.setAttribute("playsinline", "");
    player.setAttribute("title", movie.title);
    player.setAttribute("smoothwarning", "");
    if (/\.mp4$/i.test(release.filename) && /^h\.?264$/i.test(release.codec ?? "")
      && release.language && !/[,/+|]|multi|dual/i.test(release.language)) {
      player.setAttribute("engine", "native wasm");
    }
    let bufferingTimer;
    const offerLighter = () => {
      if (!lighter || request.signal.aborted || !player.isConnected) return;
      lighterButton.textContent = lighter.quality
        ? `Switch to a lighter ${lighter.quality} release`
        : "Switch to a lighter release";
      lighterButton.hidden = false;
      lighterButton.onclick = () => play(movie, lighter, Number(player.currentTime) || resumeAt);
    };
    player.addEventListener("smoothwarning", (event) => {
      if (!lighter) return;
      event.preventDefault();
      offerLighter();
    });
    player.addEventListener("statechange", (event) => {
      clearTimeout(bufferingTimer);
      const state = event.detail?.state ?? event.detail;
      if (state === "buffering") bufferingTimer = setTimeout(offerLighter, 3000);
    });
    request.signal.addEventListener("abort", () => clearTimeout(bufferingTimer), { once: true });
    if (resumeAt > 0) player.setAttribute("startat", String(resumeAt));
    const readyTimer = setTimeout(() => {
      if (player.isConnected && !request.signal.aborted) {
        playStatus.textContent = "Still opening this video. You can choose another release below.";
      }
    }, 10000);
    request.signal.addEventListener("abort", () => clearTimeout(readyTimer), { once: true });
    player.addEventListener("loadeddata", () => {
      clearTimeout(readyTimer);
      if (request.signal.aborted) return;
      if (!firstFrame) {
        firstFrame = true;
        performance.measure(`movi:play-to-first-frame:${run}`, { start: started, end: performance.now() });
      }
      playStatus.textContent = "";
      queueMicrotask(() => {
        if (!player.isConnected || request.signal.aborted || !player.paused) return;
        Promise.resolve(player.play()).catch(() => {
          if (player.isConnected && !request.signal.aborted) playStatus.textContent = "Press Play to start.";
        });
      });
    }, { once: true });
    player.addEventListener("play", () => {
      if (player.isConnected && !request.signal.aborted) playStatus.textContent = "";
    });
    player.addEventListener("error", async () => {
      clearTimeout(readyTimer);
      if (request.signal.aborted || !player.isConnected) return;
      const position = Number(player.currentTime) || resumeAt;
      try {
        playStatus.textContent = "Retrying this release…";
        if (url !== source.relayUrl && !sameSourceRelayUsed) {
          sameSourceRelayUsed = true;
          await attach(source, source.relayUrl, position);
        } else if (!refreshed && Date.now() - source.receivedAt >= source.expiresIn * 1000) {
          refreshed = true;
          const fresh = await getPlayback(release.id, request.signal);
          await attach({ ...fresh, receivedAt: Date.now() }, fresh.url, position);
        } else if (!alternateUsed) {
          alternateUsed = true;
          sameSourceRelayUsed = false;
          const alternate = await getPlayback(release.id, request.signal, source.token);
          await attach({ ...alternate, receivedAt: Date.now() }, alternate.url, position);
        } else {
          playStatus.textContent = "This release could not play in your browser. Try another release below.";
        }
      } catch (error) {
        if (!request.signal.aborted) playStatus.textContent = error.message;
      }
    }, { once: true });
    playStatus.textContent = "Opening video…";
    player.setAttribute("src", url);
    stage.replaceChildren(player);
  }

  try {
    const candidate = warmRelease?.id === release.id ? warmRelease : null;
    const warmed = candidate && candidate.expiresAt > Date.now() ? await candidate.result : null;
    if (request.signal.aborted) return;
    const reusable = warmed && candidate.expiresAt > Date.now();
    const source = reusable ? warmed : await getPlayback(release.id, request.signal);
    const receivedAt = reusable ? candidate.expiresAt - source.expiresIn * 1000 : Date.now();
    await attach({ ...source, receivedAt }, source.url, startAt);
  } catch (error) {
    if (!request.signal.aborted) playStatus.textContent = error.message;
  }
}
