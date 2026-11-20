// Library page of the web UI slice: renders the same rows the native
// Library page holds, with the same tab and Quick Open behaviour.
const data = window.__MUSE_LIBRARY__ || { artifacts: [], total: 0 };
const state = { tab: "All", query: "" };
const TABS = ["All", "Spaces", "Files"];

const isSpace = (a) => a.target_kind === "space";

function esc(value) {
  return String(value == null ? "" : value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function relative(ms) {
  if (!ms) return "";
  const days = Math.floor((Date.now() - ms) / 86400000);
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return days + " days ago";
  const months = Math.floor(days / 30);
  if (months < 12) return months + (months === 1 ? " month ago" : " months ago");
  const years = Math.floor(months / 12);
  return years + (years === 1 ? " year ago" : " years ago");
}

function visible(a) {
  const q = state.query.trim().toLowerCase();
  if (q && !String(a.title || "").toLowerCase().includes(q)) return false;
  if (state.tab === "Spaces") return isSpace(a);
  if (state.tab === "Files") return !isSpace(a);
  return true;
}

function card(a) {
  const label = isSpace(a) ? "space" : a.format || a.kind || "file";
  const badge = isSpace(a) ? "space" : "";
  const when = relative(a.updated_at_ms);
  return '<div class="card"><h3>' + esc(a.title) + "</h3>" +
    '<div class="meta">' +
    (badge ? '<span class="badge ' + badge + '">' + esc(label) + "</span>" : "") +
    (a.pinned ? '<span class="badge">pinned</span>' : "") +
    "<span>" + esc(when) + "</span>" +
    "</div></div>";
}

function section(title, rows) {
  if (!rows.length) return "";
  return '<div class="section">' + esc(title) + '</div><div class="grid">' +
    rows.map(card).join("") + "</div>";
}

function render() {
  const rows = (data.artifacts || []).filter(visible);
  const pinned = rows.filter((a) => a.pinned);
  const rest = rows.filter((a) => !a.pinned);
  const html = pinned.length || rest.length
    ? section("Pinned", pinned) + section("All", rest)
    : '<div class="empty">Nothing in the Library yet for this filter.</div>';
  document.getElementById("body").innerHTML = html;
  document.getElementById("count").textContent =
    (data.artifacts || []).length + (data.total && data.total > (data.artifacts || []).length
      ? " of " + data.total
      : "") + " items";
}

function tabs() {
  const holder = document.getElementById("tabs");
  holder.innerHTML = "";
  TABS.forEach((name) => {
    const button = document.createElement("button");
    button.className = "tab" + (state.tab === name ? " active" : "");
    button.textContent = name;
    button.addEventListener("click", () => {
      state.tab = name;
      tabs();
      render();
    });
    holder.appendChild(button);
  });
}

document.getElementById("search").addEventListener("input", (event) => {
  state.query = event.target.value;
  render();
});

tabs();
render();
