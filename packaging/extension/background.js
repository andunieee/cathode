const MENU = "open-in-cathode";

function openInCathode(tabId, url) {
  // Chrome asks once whether to let this site open Cathode.
  chrome.tabs.update(tabId, { url: "cathode://play/?url=" + encodeURIComponent(url) });
}

// The player pauses so the video isn't playing twice.
function pause(tabId) {
  chrome.scripting
    .executeScript({ target: { tabId }, func: () => document.querySelector("video")?.pause() })
    .catch(() => {});
}

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: MENU,
    title: "Open in Cathode",
    contexts: ["link", "page"],
    documentUrlPatterns: ["https://www.youtube.com/*", "https://m.youtube.com/*"],
  });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId !== MENU || !tab) return;
  pause(tab.id);
  openInCathode(tab.id, info.linkUrl || info.pageUrl);
});

chrome.action.onClicked.addListener((tab) => {
  if (!/^https:\/\/(www|m)\.youtube\.com\//.test(tab.url || "")) return;
  pause(tab.id);
  openInCathode(tab.id, tab.url);
});

chrome.runtime.onMessage.addListener((message, sender) => {
  if (!message.open || !sender.tab) return;
  pause(sender.tab.id);
  openInCathode(sender.tab.id, message.open);
});
