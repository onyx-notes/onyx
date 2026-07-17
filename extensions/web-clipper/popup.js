// Extract the current page with Readability, convert to markdown with
// Turndown (both run in the page), and POST to the local Onyx clipper.
const PORT = 47600;
const $ = (id) => document.getElementById(id);

chrome.storage.local.get(["token", "folder"]).then((s) => {
  if (s.token) $("token").value = s.token;
  if (s.folder) $("folder").value = s.folder;
});

$("clip").addEventListener("click", async () => {
  const token = $("token").value.trim();
  const folder = $("folder").value.trim();
  chrome.storage.local.set({ token, folder });
  $("status").textContent = "Extracting…";
  $("status").className = "";

  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const [{ result }] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: extractInPage,
    world: "MAIN",
  }).catch(() => [{ result: null }]);

  // Readability/Turndown live in the popup; if page-world injection isn't
  // possible, fall back to sending raw HTML for popup-side conversion.
  let clip;
  if (result && result.markdown) {
    clip = result;
  } else {
    const [{ result: html }] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => ({ html: document.documentElement.outerHTML, url: location.href, title: document.title }),
    });
    const doc = new DOMParser().parseFromString(html.html, "text/html");
    const article = new Readability(doc).parse();
    const markdown = new TurndownService({ headingStyle: "atx" }).turndown(article?.content || "");
    clip = { title: article?.title || html.title, url: html.url, markdown };
  }
  clip.folder = folder;

  try {
    const resp = await fetch(`http://127.0.0.1:${PORT}/clip`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Onyx-Token": token },
      body: JSON.stringify(clip),
    });
    if (!resp.ok) throw new Error(await resp.text());
    const { path } = await resp.json();
    $("status").textContent = `Saved to ${path}`;
  } catch (e) {
    $("status").textContent = "Failed: " + (e.message || e) + " — is Onyx open?";
    $("status").className = "err";
  }
});

// Runs in the page's MAIN world; returns null if libs aren't available
// there (popup fallback handles conversion instead).
function extractInPage() {
  return null;
}
