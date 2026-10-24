// Alt+C sends the page you are on to Cathode.
document.addEventListener("keydown", (event) => {
  if (event.altKey && event.key.toLowerCase() === "c" && !event.ctrlKey && !event.metaKey) {
    event.preventDefault();
    chrome.runtime.sendMessage({ open: location.href });
  }
});
