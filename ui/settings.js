// Settings window. Talks to the core through the commands in core/src/settings.rs.
const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

let saved = null;     // settings as loaded / last saved
let shortcut = "";    // shortcut being edited
let shortcutOk = true;
let slices = [];      // [{ id, label, pack, enabled, applies }] in display order
let theme = "";       // selected theme id
let recording = false;

const STYLE_NAMES = { orbit: "Buttons on an orbit", pie: "Pie", ring: "Slim ring" };

function renderThemes(view) {
  const box = $("themes");
  box.replaceChildren();
  for (const t of view.themes) {
    const label = document.createElement("label");
    label.className = "theme";

    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = "theme";
    radio.value = t.id;
    radio.checked = t.id === theme;
    radio.addEventListener("change", () => { theme = t.id; refreshSave(); });

    const text = document.createElement("div");
    text.className = "grow";
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = t.name;
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = STYLE_NAMES[t.style] || t.style;
    text.append(name, meta);

    const swatches = document.createElement("div");
    swatches.className = "swatches";
    swatches.setAttribute("aria-hidden", "true");
    for (const color of t.swatches) {
      const dot = document.createElement("span");
      dot.style.background = color;
      swatches.append(dot);
    }

    label.append(radio, text, swatches);
    box.append(label);
  }
  const errors = $("theme-errors");
  errors.replaceChildren();
  for (const error of view.theme_errors) {
    const li = document.createElement("li");
    li.textContent = `${error.file}: ${error.message}`;
    errors.append(li);
  }
}

// KeyboardEvent.code -> the key names core/src/hotkey.rs understands.
function keyName(code) {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  const named = {
    Space: "Space", Tab: "Tab", Enter: "Enter", PageUp: "PageUp", PageDown: "PageDown",
    Home: "Home", End: "End", Insert: "Insert", Delete: "Delete", Backquote: "`",
    ArrowLeft: "Left", ArrowRight: "Right", ArrowUp: "Up", ArrowDown: "Down",
  };
  return named[code] || null;
}

// Fallback for key events without a scan code (some remote desktop and macro tools send
// those), where KeyboardEvent.code is empty but KeyboardEvent.key is set.
function keyNameFromKey(key) {
  if (/^[a-z0-9]$/i.test(key)) return key.toUpperCase();
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(key)) return key;
  const named = { " ": "Space", Tab: "Tab", Enter: "Enter", PageUp: "PageUp", PageDown: "PageDown",
    Home: "Home", End: "End", Insert: "Insert", Delete: "Delete", "`": "`",
    ArrowLeft: "Left", ArrowRight: "Right", ArrowUp: "Up", ArrowDown: "Down" };
  return named[key] || null;
}

function setStatus(el, text, kind) {
  el.textContent = text;
  el.className = "status" + (kind ? " " + kind : "");
}

function dirty() {
  if (!saved) return false;
  const ids = (list) => list.map((s) => s.id).join("|");
  const off = (list) => list.filter((s) => !s.enabled).map((s) => s.id).join("|");
  return shortcut !== saved.shortcut
    || theme !== saved.theme
    || ids(slices) !== ids(saved.slices)
    || off(slices) !== off(saved.slices);
}

function refreshSave() {
  $("save").disabled = !dirty() || !shortcutOk || recording;
}

function renderShortcut() {
  $("recorder").textContent = recording ? "Press a shortcut…  (Esc to cancel)" : shortcut;
  $("recorder").classList.toggle("recording", recording);
}

function button(text, label, disabled, onClick) {
  const b = document.createElement("button");
  b.type = "button";
  b.textContent = text;
  b.title = label;
  b.setAttribute("aria-label", label);
  b.disabled = disabled;
  b.addEventListener("click", onClick);
  return b;
}

function renderSlices() {
  const list = $("slices");
  list.replaceChildren();
  $("no-slices").hidden = slices.length > 0;
  slices.forEach((slice, i) => {
    const li = document.createElement("li");

    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = slice.enabled;
    box.id = "slice-" + i;
    box.addEventListener("change", () => { slice.enabled = box.checked; refreshSave(); });

    const text = document.createElement("label");
    text.className = "grow";
    text.htmlFor = box.id;
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = slice.label;
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = `${slice.pack} · ${slice.applies}`;
    text.append(name, meta);

    li.append(
      box,
      text,
      button("↑", `Move ${slice.label} up`, i === 0, () => move(i, -1)),
      button("↓", `Move ${slice.label} down`, i === slices.length - 1, () => move(i, 1)),
    );
    list.append(li);
  });
}

function move(i, step) {
  const j = i + step;
  [slices[i], slices[j]] = [slices[j], slices[i]];
  renderSlices();
  refreshSave();
}

function renderPacks(view) {
  const list = $("packs");
  list.replaceChildren();
  if (view.packs.length === 0) {
    const li = document.createElement("li");
    li.className = "muted";
    li.textContent = "No packs installed.";
    list.append(li);
  }
  for (const pack of view.packs) {
    const li = document.createElement("li");
    const text = document.createElement("div");
    text.className = "grow";
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = `${pack.name} ${pack.version}`;
    const meta = document.createElement("div");
    meta.className = "meta";
    const actions = pack.slices === 1 ? "1 action" : `${pack.slices} actions`;
    meta.textContent = `${pack.id} · ${actions} · ${pack.license}`;
    text.append(name, meta);
    li.append(text);
    list.append(li);
  }
  const errors = $("pack-errors");
  errors.replaceChildren();
  for (const error of view.errors) {
    const li = document.createElement("li");
    li.textContent = `${error.dir}: ${error.message}`;
    errors.append(li);
  }
  $("packs-dir").textContent = view.packs_dir || "(not found)";
  $("config-path").textContent = view.config_path || "(not available)";
}

async function load() {
  const view = await invoke("get_settings");
  saved = structuredClone(view);
  shortcut = view.shortcut;
  shortcutOk = true;
  slices = view.slices.map((s) => ({ ...s }));
  theme = view.theme;
  $("version").textContent = "v" + view.version;
  renderShortcut();
  renderSlices();
  renderPacks(view);
  renderThemes(view);
  if (view.shortcut_active) {
    setStatus($("shortcut-status"), "");
  } else {
    setStatus($("shortcut-status"),
      `${view.shortcut} is not working: another app is using it. Choose a different shortcut.`,
      "warn");
  }
  refreshSave();
}

async function startRecording() {
  if (recording) return;
  recording = true;
  renderShortcut();
  refreshSave();
  await invoke("pause_hotkey", { paused: true }).catch(() => {});
}

async function stopRecording() {
  if (!recording) return;
  recording = false;
  renderShortcut();
  refreshSave();
  await invoke("pause_hotkey", { paused: false }).catch(() => {});
}

async function useShortcut(text) {
  try {
    shortcut = await invoke("check_shortcut", { shortcut: text });
    shortcutOk = true;
    setStatus($("shortcut-status"),
      shortcut === saved.shortcut ? "" : `${shortcut} is free. Save to use it.`, "ok");
  } catch (e) {
    shortcut = text;
    shortcutOk = false;
    setStatus($("shortcut-status"), String(e), "bad");
  }
  renderShortcut();
  refreshSave();
}

$("recorder").addEventListener("click", startRecording);
$("recorder").addEventListener("blur", stopRecording);
$("recorder").addEventListener("keydown", async (e) => {
  if (!recording) return; // Enter and Space click the button, which starts recording
  e.preventDefault();
  if (e.code === "Escape" || e.key === "Escape") { await stopRecording(); return; }
  const key = keyName(e.code) || (e.code ? null : keyNameFromKey(e.key));
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Win"]
    .filter(Boolean);
  if (!key) {
    // Only modifiers so far: show them while the user is still pressing.
    if (mods.length) $("recorder").textContent = mods.join("+") + "+…";
    return;
  }
  if (!mods.length) {
    setStatus($("shortcut-status"), "Add Ctrl, Alt, Shift or Win to the key.", "bad");
    return;
  }
  await stopRecording();
  await useShortcut([...mods, key].join("+"));
});

$("reset-shortcut").addEventListener("click", () => useShortcut(saved.default_shortcut));

$("save").addEventListener("click", async () => {
  $("save").disabled = true;
  try {
    await invoke("save_settings", {
      shortcut,
      order: slices.map((s) => s.id),
      disabled: slices.filter((s) => !s.enabled).map((s) => s.id),
      theme,
    });
    await load();
    setStatus($("save-status"), "Saved.", "ok");
  } catch (e) {
    setStatus($("save-status"), String(e), "bad");
    refreshSave();
  }
});

$("close").addEventListener("click", () => invoke("close_settings"));

load().catch((e) => setStatus($("save-status"), "Could not load settings: " + e, "bad"));
