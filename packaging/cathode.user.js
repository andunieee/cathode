// ==UserScript==
// @name         Open in Cathode
// @description  Open the YouTube video, playlist or channel you are on in Cathode (Alt+C, or the userscript menu)
// @match        https://www.youtube.com/*
// @match        https://m.youtube.com/*
// @grant        GM_registerMenuCommand
// ==/UserScript==

(function () {
  "use strict";

  function openInCathode() {
    // The player pauses so the video isn't playing twice.
    document.querySelector("video")?.pause();
    location.href = "cathode://play/?url=" + encodeURIComponent(location.href);
  }

  GM_registerMenuCommand("Open in Cathode", openInCathode);
  document.addEventListener("keydown", (event) => {
    if (event.altKey && event.key.toLowerCase() === "c" && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      openInCathode();
    }
  });
})();
