// Settings window. Plain DOM: loads the snapshot from Rust, edits a copy of
// the settings and saves them in one call.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { type Key, type Lang, locale, plural, setLang, t, translatePage } from "./i18n";

type SttProvider = "elevenlabs" | "xai" | "openai" | "openrouter";
type LlmBackend = "open_ai_compatible" | "open_router" | "claude_cli" | "codex_cli";
type SecretKey = "eleven_labs" | "eleven_labs_backup" | "xai" | "open_ai" | "open_router" | "llm_endpoint";
type Preset = "agent" | "cleanup";

interface Mode {
  id: string;
  name: string;
  post_process: boolean;
  /** Own system prompt; empty means the preset's built-in one. */
  prompt: string;
  preset: Preset | null;
  /** Model on the active backend; empty means the backend's model. */
  llm_model: string;
}

interface DictionaryEntry {
  term: string;
  heard_as: string[];
}

/** One entry per line: `term = form, form`; a line without "=" is a term alone. */
const dictionaryText = (entries: DictionaryEntry[]) =>
  entries.map((e) => (e.heard_as.length ? `${e.term} = ${e.heard_as.join(", ")}` : e.term)).join("\n");

function parseDictionary(text: string): DictionaryEntry[] {
  return text.split("\n").flatMap((line) => {
    const [term, forms = ""] = line.split("=", 2).map((s) => s.trim());
    const heard_as = forms.split(",").map((f) => f.trim()).filter(Boolean);
    return term ? [{ term, heard_as }] : [];
  });
}

interface Settings {
  ui_language: Lang;
  hotkey: string;
  hotkey_mode: "auto" | "hold" | "toggle";
  active_mode: string;
  modes: Mode[];
  stt: {
    provider: SttProvider;
    language: string;
    streaming: boolean;
    models: Record<string, string>;
    final_timeout_ms: number;
    dictionary: DictionaryEntry[];
  };
  llm: {
    backend: LlmBackend;
    base_url: string;
    model: string;
    openrouter_model: string;
    cli_model: string;
    reasoning_effort: string;
    timeout_ms: number;
  };
  audio: { keep_warm: boolean; device: string; preroll_ms: number; min_duration_ms: number; cues: boolean };
  paste: { auto_paste: boolean; restore_clipboard: boolean; restore_delay_ms: number };
  updates: { enabled: boolean; url: string; interval_s: number };
  history: { enabled: boolean; upload_url: string };
  keep_connections_warm: boolean;
  autostart: boolean;
}

interface UpdateStatus {
  state: string;
  message: string;
  latest_build: string | null;
  checked_at: number | null;
}

interface Snapshot {
  settings: Settings;
  builtin_prompts: Record<Preset, string>;
  build: { version: string; build: string; built_at: number };
  /** The build time as `v26.1111.1215`. */
  version: string;
  /** A build that never updates itself. */
  local_build: boolean;
  secrets: Record<SecretKey, boolean>;
  logs_dir: string;
  update: UpdateStatus;
}

interface Timing {
  outcome: string;
  mode: string;
  stt: string;
  streamed: boolean;
  llm: string;
  chars: number;
  audio_ms: number | null;
  release_to_stt_ms: number | null;
  llm_ms: number | null;
  release_to_paste_ms: number | null;
  /** Missing in entries made before costs were recorded. */
  stt_cost_usd?: number | null;
  stt_cost_estimated?: boolean;
  llm_cost_usd?: number | null;
}

/** One line of `history.jsonl`: the dictation and its timing. */
interface HistoryItem extends Timing {
  id: string;
  ts_ms: number;
  transcript: string;
  output: string;
}

type Fetched<T> = { state: "ok"; data: T } | { state: "no_key" } | { state: "unused" } | { state: "error"; message: string };

interface ElevenLabsBalance {
  tier: string;
  used: number;
  limit: number;
  remaining: number;
  reset_unix: number | null;
  credits_per_min_realtime: number;
  credits_per_min_batch: number;
  per_day: number | null;
  runs_out_unix: number | null;
}

/** Amounts in USD. */
interface OpenRouterBalance {
  account_total: number | null;
  account_remaining: number | null;
  key_limit: number | null;
  key_remaining: number | null;
  usage_daily: number;
  usage_weekly: number;
  usage_monthly: number;
  available: number | null;
  per_day: number;
  days_left: number | null;
}

interface UsageRow {
  ts: number;
  audio_ms: number;
  stt: string;
  streamed: boolean;
  llm: string;
  outcome: string;
  release_to_paste_ms: number | null;
}

interface Overview {
  elevenlabs: Fetched<ElevenLabsBalance>;
  /** The second account dictation moves to when the first fails. */
  elevenlabs_backup: Fetched<ElevenLabsBalance>;
  openrouter: Fetched<OpenRouterBalance>;
  usage: UsageRow[];
  fetched_at: number;
}

/** A model of a catalog (`hyperrust_core::catalog`). */
interface CatalogModel {
  id: string;
  name: string;
  /** Unix seconds the catalog added it. */
  created: number | null;
  owner: string;
  price: Price | null;
}

/** USD per hour of audio, or per million input and output tokens. */
type Price = { unit: "hour"; usd: number } | { unit: "tokens"; input: number; output: number };

interface SttModelList {
  batch: CatalogModel[];
  realtime: CatalogModel[];
}

interface NewModel extends CatalogModel {
  newer_than_yours: boolean;
}

interface ModelNews {
  days: number;
  stt: NewModel[];
  stt_error: string | null;
  llm_source: string;
  llm: NewModel[];
  llm_error: string | null;
}

/** `hyperrust_core::ranking::RankedModel`. */
interface RankedModel {
  model: string;
  name: string;
  use_model: string;
  effort: string;
  score: number | null;
  good: number;
  cases: number;
  errors: number;
  tested_at: number;
  median_ms: number | null;
  p90_ms: number | null;
  timed_at: number;
  cost_usd: number | null;
  earlier: { at: number; median_ms: number }[];
  other_prompt: boolean;
  available: boolean;
  issues: string[];
  note: string;
}

/** `ranking.json` from the update server or GitHub, with what this build adds. */
interface RankingView {
  generated_at: number;
  prompt_sha: string;
  judge: string;
  cases: number;
  models: RankedModel[];
  app_prompt_sha: string;
  age_days: number;
}

const SECRETS = (): { key: SecretKey; label: string; use: string }[] => [
  { key: "eleven_labs", label: "ElevenLabs", use: t("secretElevenLabsUse") },
  { key: "eleven_labs_backup", label: t("secretBackupLabel"), use: t("secretBackupUse") },
  { key: "xai", label: "xAI", use: t("secretXaiUse") },
  { key: "open_ai", label: "OpenAI", use: t("secretOpenAiUse") },
  { key: "open_router", label: "OpenRouter", use: t("secretOpenRouterUse") },
  { key: "llm_endpoint", label: "CLIProxy", use: t("secretCliProxyUse") },
];

const MODEL_KEYS = ["elevenlabs", "elevenlabs_realtime", "xai", "openai", "openai_realtime", "openrouter"];
/** Keys the overview reads balances with. */
const BALANCE_KEYS: SecretKey[] = ["eleven_labs", "eleven_labs_backup", "open_router"];

const STT_LABELS: Record<SttProvider, string> = {
  elevenlabs: "ElevenLabs",
  xai: "xAI",
  openai: "OpenAI",
  openrouter: "OpenRouter",
};

const STT_SECRET: Record<SttProvider, SecretKey> = {
  elevenlabs: "eleven_labs",
  xai: "xai",
  openai: "open_ai",
  openrouter: "open_router",
};

const LLM_LABELS: Record<LlmBackend, string> = {
  open_ai_compatible: "CLIProxy",
  open_router: "OpenRouter",
  claude_cli: "claude -p",
  codex_cli: "codex exec",
};

const LLM_SECRET: Partial<Record<LlmBackend, SecretKey>> = {
  open_ai_compatible: "llm_endpoint",
  open_router: "open_router",
};

type ModeKind = "none" | Preset;

const MODE_KINDS: [ModeKind, Key][] = [
  ["none", "kindNone"],
  ["cleanup", "kindCleanup"],
  ["agent", "kindAgent"],
];

const OUTCOMES: Record<string, Key> = {
  llm_error_raw: "outcomeLlmErrorRaw",
  llm_empty_raw: "outcomeLlmEmptyRaw",
  paste_error: "outcomePasteError",
  stt_error: "outcomeSttError",
};

const $ = <T extends HTMLElement = HTMLInputElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`brak elementu #${id}`);
  return el as T;
};

let settings: Settings;
let builtinPrompts: Partial<Record<Preset, string>> = {};
let secrets: Partial<Record<SecretKey, boolean>> = {};
/** The saved settings as the form reads them, to tell unsaved edits. */
let baseline = "";
let snapshot: Snapshot | undefined;

/** Number and date formats of the UI language. */
function formats() {
  const l = locale();
  const usd = (o: Intl.NumberFormatOptions = {}) =>
    new Intl.NumberFormat(l, { style: "currency", currency: "USD", currencyDisplay: "narrowSymbol", ...o });
  return {
    nf0: new Intl.NumberFormat(l, { maximumFractionDigits: 0 }),
    nf1: new Intl.NumberFormat(l, { maximumFractionDigits: 1 }),
    nf2: new Intl.NumberFormat(l, { maximumFractionDigits: 2 }),
    score: new Intl.NumberFormat(l, { minimumFractionDigits: 1, maximumFractionDigits: 1 }),
    usd: usd(),
    usd2: usd({ maximumFractionDigits: 2 }),
    usdPrice: usd({ minimumFractionDigits: 2, maximumFractionDigits: 3 }),
    usdCost: usd({ minimumSignificantDigits: 2, maximumSignificantDigits: 2 }),
    day: new Intl.DateTimeFormat(l, { day: "numeric", month: "short" }),
    weekday: new Intl.DateTimeFormat(l, { weekday: "long", day: "numeric", month: "long" }),
    clock: new Intl.DateTimeFormat(l, { hour: "2-digit", minute: "2-digit" }),
  };
}

let fmt = formats();

/** The language of the last window, so a Polish UI does not start in English before the settings load. */
function rememberedLang(): Lang {
  try {
    return localStorage.getItem("lang") === "pl" ? "pl" : "en";
  } catch {
    return "en";
  }
}

/** Switch the window's texts and formats; dynamic parts follow on their next render (`rerender`). */
function applyLanguage(l: Lang): void {
  setLang(l);
  fmt = formats();
  translatePage();
  try {
    localStorage.setItem("lang", l);
  } catch {
    // only a convenience for the next start
  }
  if (snapshot) {
    const version = snapshot.local_build ? `${snapshot.version} ${t("versionLocal")}` : snapshot.version;
    $<HTMLElement>("build").textContent = t("versionLabel", { version });
    $<HTMLElement>("build").title = t("versionTitle", { build: snapshot.build.build });
  }
}

/** Redraw everything built in code after a language switch, keeping unsaved edits. */
function rerender(): void {
  if (!settings) return;
  renderSttInUse();
  renderKeyBadges();
  renderModelPick();
  renderSttPicks();
  renderModes();
  renderTiles();
  renderSecrets();
  void loadDevices($<HTMLSelectElement>("audio_device").value);
  void loadTimings();
  void loadOverview();
  pageShown(currentPage);
}

function status(id: string, text: string, isError = false): void {
  const el = $<HTMLElement>(id);
  el.textContent = text;
  el.classList.toggle("error", isError);
}

function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = ""): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text) e.textContent = text;
  return e;
}

function button(text: string, cls = ""): HTMLButtonElement {
  const b = el("button", cls, text);
  b.type = "button";
  return b;
}

function modeName(id: string): string {
  return settings?.modes.find((m) => m.id === id)?.name || id;
}

/** The STT model dictations use, as `SttSettings::realtime_model` / `batch_model` pick it in core. */
function sttInUse(stt: Settings["stt"]): { model: string; live: boolean } {
  const live = stt.streaming && stt.provider !== "openrouter";
  const realtimeKey = stt.provider === "elevenlabs" || stt.provider === "openai" ? `${stt.provider}_realtime` : stt.provider;
  return { model: stt.models[live ? realtimeKey : stt.provider] || "?", live };
}

function renderSttInUse(): void {
  const { model, live } = sttInUse(readForm().stt);
  $<HTMLElement>("stt-in-use").textContent = t(live ? "sttNowLive" : "sttNowBatch", { model });
}

// ---------- form <-> settings ----------

function bindValue(id: string, value: string | number): void {
  $(id).value = String(value);
}

function bindCheck(id: string, value: boolean): void {
  $(id).checked = value;
}

function num(id: string): number {
  const v = Number($(id).value);
  return Number.isFinite(v) ? v : 0;
}

function fillForm(): void {
  const s = settings;
  $<HTMLSelectElement>("ui_language").value = s.ui_language;
  bindValue("hotkey", s.hotkey);
  $<HTMLSelectElement>("hotkey_mode").value = s.hotkey_mode;
  $<HTMLSelectElement>("stt_provider").value = s.stt.provider;
  bindValue("stt_language", s.stt.language);
  bindCheck("stt_streaming", s.stt.streaming);
  for (const k of MODEL_KEYS) bindValue(`m_${k}`, s.stt.models[k] ?? "");
  bindValue("stt_final_timeout_ms", s.stt.final_timeout_ms);
  bindValue("stt_dictionary", dictionaryText(s.stt.dictionary));
  $<HTMLSelectElement>("llm_backend").value = s.llm.backend;
  bindValue("llm_base_url", s.llm.base_url);
  bindValue("llm_model", s.llm.model);
  bindValue("llm_openrouter_model", s.llm.openrouter_model);
  bindValue("llm_cli_model", s.llm.cli_model);
  bindValue("llm_reasoning_effort", s.llm.reasoning_effort);
  bindValue("llm_timeout_ms", s.llm.timeout_ms);
  bindCheck("audio_keep_warm", s.audio.keep_warm);
  bindCheck("audio_cues", s.audio.cues);
  bindValue("audio_preroll_ms", s.audio.preroll_ms);
  bindValue("audio_min_duration_ms", s.audio.min_duration_ms);
  bindCheck("paste_auto_paste", s.paste.auto_paste);
  bindCheck("paste_restore_clipboard", s.paste.restore_clipboard);
  bindValue("paste_restore_delay_ms", s.paste.restore_delay_ms);
  bindCheck("upd_enabled", s.updates.enabled);
  bindValue("upd_url", s.updates.url);
  bindValue("upd_interval_s", s.updates.interval_s);
  bindCheck("hist_enabled", s.history.enabled);
  bindValue("hist_upload_url", s.history.upload_url);
  bindCheck("autostart", s.autostart);
  bindCheck("keep_connections_warm", s.keep_connections_warm);
  showProviderFields();
  renderModelPick();
  renderSttPicks();
  renderModes();
  renderSttInUse();
}

/** The form's settings; modes and the active mode are edited in `settings` itself. */
function readForm(): Settings {
  const s = structuredClone(settings);
  s.ui_language = $<HTMLSelectElement>("ui_language").value as Lang;
  s.hotkey = $("hotkey").value.trim();
  s.hotkey_mode = $<HTMLSelectElement>("hotkey_mode").value as Settings["hotkey_mode"];
  s.stt.provider = $<HTMLSelectElement>("stt_provider").value as SttProvider;
  s.stt.language = $("stt_language").value.trim();
  s.stt.streaming = $("stt_streaming").checked;
  for (const k of MODEL_KEYS) s.stt.models[k] = $(`m_${k}`).value.trim();
  s.stt.final_timeout_ms = num("stt_final_timeout_ms");
  s.stt.dictionary = parseDictionary($("stt_dictionary").value);
  s.llm.backend = $<HTMLSelectElement>("llm_backend").value as LlmBackend;
  s.llm.base_url = $("llm_base_url").value.trim();
  s.llm.model = $("llm_model").value.trim();
  s.llm.openrouter_model = $("llm_openrouter_model").value.trim();
  s.llm.cli_model = $("llm_cli_model").value.trim();
  s.llm.reasoning_effort = $("llm_reasoning_effort").value.trim();
  s.llm.timeout_ms = num("llm_timeout_ms");
  s.audio.device = $<HTMLSelectElement>("audio_device").value;
  s.audio.keep_warm = $("audio_keep_warm").checked;
  s.audio.cues = $("audio_cues").checked;
  s.audio.preroll_ms = num("audio_preroll_ms");
  s.audio.min_duration_ms = num("audio_min_duration_ms");
  s.paste.auto_paste = $("paste_auto_paste").checked;
  s.paste.restore_clipboard = $("paste_restore_clipboard").checked;
  s.paste.restore_delay_ms = num("paste_restore_delay_ms");
  s.updates.enabled = $("upd_enabled").checked;
  s.updates.url = $("upd_url").value.trim();
  s.updates.interval_s = num("upd_interval_s");
  s.history.enabled = $("hist_enabled").checked;
  s.history.upload_url = $("hist_upload_url").value.trim();
  s.autostart = $("autostart").checked;
  s.keep_connections_warm = $("keep_connections_warm").checked;
  return s;
}

/** Show only the fields of the selected STT provider and LLM backend. */
function showProviderFields(): void {
  const provider = $<HTMLSelectElement>("stt_provider").value;
  document.querySelectorAll<HTMLElement>("[data-stt]").forEach((row) => (row.hidden = row.dataset.stt !== provider));
  const backend = $<HTMLSelectElement>("llm_backend").value;
  document.querySelectorAll<HTMLElement>("[data-backend]").forEach((row) => {
    row.hidden = !(row.dataset.backend ?? "").split(" ").includes(backend);
  });
  renderKeyBadges();
}

function keyBadge(id: string, key: SecretKey | undefined): void {
  const badge = $<HTMLButtonElement>(id);
  badge.hidden = !key;
  if (!key) return;
  const ok = secrets[key] === true;
  badge.className = ok ? "badge ok" : "badge missing";
  badge.textContent = t(ok ? "keySaved" : "keyMissing");
  badge.title = t(ok ? "keySavedTitle" : "keyMissingTitle");
}

function renderKeyBadges(): void {
  keyBadge("stt-key", STT_SECRET[$<HTMLSelectElement>("stt_provider").value as SttProvider]);
  keyBadge("llm-key", LLM_SECRET[$<HTMLSelectElement>("llm_backend").value as LlmBackend]);
}

for (const id of ["stt-key", "llm-key"]) $<HTMLButtonElement>(id).addEventListener("click", () => showPage("keys"));

// ---------- model lists ----------

type ModelBackend = "open_ai_compatible" | "open_router";

const CUSTOM_MODEL = "__custom__";
/** Defaults in `config.rs`: the fastest of the newest models (README). */
const RECOMMENDED: Record<ModelBackend, string> = {
  open_ai_compatible: "claude-haiku-5-5",
  open_router: "anthropic/claude-haiku-5.5",
};
/** Models that cannot rewrite text (images, speech, embeddings). */
const NOT_CHAT = /image|embed|tts|transcri|moderation/i;
const FAMILIES: [RegExp, string][] = [
  [/^(anthropic\/|claude)/, "Claude (Anthropic)"],
  [/^(google\/|gemini)/, "Gemini (Google)"],
  [/^(openai\/|gpt)/, "GPT (OpenAI)"],
];
/** Fetched model lists per backend and endpoint, for this window. */
const modelLists = new Map<string, string[]>();
/** Prices of the listed models (OpenRouter has them, CLIProxy does not). */
const modelPrices = new Map<string, Price>();
let modelsLoading = "";

function priceText(p: Price | null | undefined): string {
  if (!p) return "";
  if (p.unit === "hour") return `${fmt.usdPrice.format(p.usd)}/h`;
  if (p.input === 0 && p.output === 0) return t("priceFree");
  return t("pricePerTokens", { input: fmt.usdPrice.format(p.input), output: fmt.usdPrice.format(p.output) });
}

/** Option text: the parts that apply, joined with a middle dot. */
const optionText = (...parts: (string | false)[]) => parts.filter(Boolean).join(" · ");

function modelBackend(): ModelBackend | null {
  const b = $<HTMLSelectElement>("llm_backend").value;
  return b === "open_ai_compatible" || b === "open_router" ? b : null;
}

/** Hidden input with the saved model of `backend`; the picker edits it. */
function modelField(backend: ModelBackend): HTMLInputElement {
  return $(backend === "open_router" ? "llm_openrouter_model" : "llm_model");
}

/** The model a mode without its own model uses, as the form has it now. */
function defaultModel(): string {
  const backend = modelBackend();
  return (backend ? modelField(backend).value : $("llm_cli_model").value).trim();
}

function listKey(backend: ModelBackend): string {
  return backend === "open_router" ? backend : `${backend} ${$("llm_base_url").value.trim()}`;
}

function family(id: string): string {
  for (const [re, name] of FAMILIES) if (re.test(id)) return name;
  const slash = id.indexOf("/");
  return slash > 0 ? id.slice(0, slash) : t("familyOther");
}

function familyRank(name: string): number {
  const i = FAMILIES.findIndex(([, n]) => n === name);
  return i >= 0 ? i : name === t("familyOther") ? 99 : 50;
}

/** Options for a model picker: `current` first when the list lacks it, then the list by family. */
function modelItems(backend: ModelBackend, current: string): HTMLElement[] {
  const models = (modelLists.get(listKey(backend)) ?? []).filter((m) => !NOT_CHAT.test(m));
  const label = (m: string) => optionText(m, priceText(modelPrices.get(m)), m === RECOMMENDED[backend] && t("recommended"));
  const groups = new Map<string, string[]>();
  for (const m of models) groups.set(family(m), [...(groups.get(family(m)) ?? []), m]);
  const items: HTMLElement[] = [];
  if (current && !models.includes(current)) items.push(new Option(label(current), current));
  for (const name of [...groups.keys()].sort((a, b) => familyRank(a) - familyRank(b) || a.localeCompare(b))) {
    const group = el("optgroup");
    group.label = name;
    group.append(...(groups.get(name) ?? []).sort().map((m) => new Option(label(m), m)));
    items.push(group);
  }
  return items;
}

function showCustomModel(on: boolean, backend: ModelBackend): void {
  $("llm_model_custom_row").hidden = !on;
  if (on) $("llm_model_custom").value = modelField(backend).value;
}

/** Rebuild the picker for the active backend; the saved model is always shown. */
function renderModelPick(): void {
  const backend = modelBackend();
  if (!backend) {
    $("llm_model_custom_row").hidden = true;
    return;
  }
  const current = modelField(backend).value.trim();
  const pick = $<HTMLSelectElement>("llm_model_pick");
  pick.replaceChildren(...modelItems(backend, current), new Option(t("customModel"), CUSTOM_MODEL));
  // Keep the name field open while the user is typing in it.
  const typing = !$("llm_model_custom_row").hidden;
  pick.value = typing || !current ? CUSTOM_MODEL : current;
  showCustomModel(pick.value === CUSTOM_MODEL, backend);
}

/** Fetch the model list of the active backend once per window (or on demand). */
async function loadModels(force = false): Promise<void> {
  const backend = modelBackend();
  if (!backend) return;
  const key = listKey(backend);
  if ((!force && modelLists.has(key)) || modelsLoading === key) return;
  modelsLoading = key;
  status("models-status", t("loadingModels"));
  try {
    const models = await invoke<CatalogModel[]>("list_llm_models", { backend, baseUrl: $("llm_base_url").value.trim() });
    modelLists.set(key, models.map((m) => m.id));
    for (const m of models) if (m.price) modelPrices.set(m.id, m.price);
    status("models-status", "");
  } catch (e) {
    status("models-status", t("modelsFailed", { error: errorText(e) }), true);
  } finally {
    modelsLoading = "";
  }
  if (modelBackend() === backend) {
    renderModelPick();
    refreshModeModels();
  }
}

$<HTMLSelectElement>("llm_model_pick").addEventListener("change", (e) => {
  const backend = modelBackend();
  if (!backend) return;
  const model = (e.target as HTMLSelectElement).value;
  if (model === CUSTOM_MODEL) {
    showCustomModel(true, backend);
    $("llm_model_custom").focus();
  } else {
    modelField(backend).value = model;
    showCustomModel(false, backend);
  }
});
$("llm_model_custom").addEventListener("input", (e) => {
  const backend = modelBackend();
  if (backend) modelField(backend).value = (e.target as HTMLInputElement).value.trim();
});
$<HTMLButtonElement>("models-refresh").addEventListener("click", () => void loadModels(true));
$<HTMLSelectElement>("llm_backend").addEventListener("change", () => {
  showProviderFields();
  $("llm_model_custom_row").hidden = true;
  renderModelPick();
  refreshModeModels();
  void loadModels();
});
$("llm_base_url").addEventListener("change", () => void loadModels());
// The "yours" mark in the ranking follows the model the form has now.
for (const id of ["llm_model_pick", "llm_backend", "llm_model_custom"]) $(id).addEventListener("change", () => void loadRanking());

const sttPage = document.querySelector<HTMLElement>('section[data-page="stt"]')!;
sttPage.addEventListener("input", renderSttInUse);
sttPage.addEventListener("change", renderSttInUse);
$<HTMLSelectElement>("stt_provider").addEventListener("change", () => {
  showProviderFields();
  void loadSttModels();
});

// ---------- speech-to-text model lists ----------

/** Defaults in `config.rs`. */
const STT_RECOMMENDED: Record<string, string> = {
  elevenlabs: "scribe_v2",
  elevenlabs_realtime: "scribe_v2_realtime",
  xai: "grok-voice-transcribe-2.0",
  openai: "gpt-transcribe",
  openai_realtime: "gpt-live-transcribe",
  openrouter: "openai/gpt-transcribe",
};
/** Models the catalog added within this many days are marked new (`catalog::NEW_DAYS`). */
const NEW_DAYS = 45;
/** Fetched lists per provider, for this window. */
const sttModelLists = new Map<SttProvider, SttModelList>();
let sttModelsLoading = "";

const sttProviderOf = (key: string) => key.replace(/_realtime$/, "") as SttProvider;
const isNew = (m: CatalogModel) => m.created !== null && Date.now() / 1000 - m.created < NEW_DAYS * 86_400;

/** Options for the picker of `m_<key>`: the saved model first when the list lacks it. */
function sttOptions(key: string, current: string): HTMLOptionElement[] {
  const list = sttModelLists.get(sttProviderOf(key));
  const models = (key.endsWith("_realtime") ? list?.realtime : list?.batch) ?? [];
  const label = (m: CatalogModel) =>
    optionText(
      m.name,
      priceText(m.price),
      m.id === STT_RECOMMENDED[key] && t("recommended"),
      isNew(m) && t("newModel", { date: fmt.day.format(m.created! * 1000) }),
    );
  const items = models.map((m) => new Option(label(m), m.id));
  if (current && !models.some((m) => m.id === current)) items.unshift(new Option(current, current));
  return items;
}

function renderSttPick(key: string): void {
  const input = $(`m_${key}`);
  const pick = $<HTMLSelectElement>(`m_${key}_pick`);
  const current = input.value.trim();
  pick.replaceChildren(...sttOptions(key, current), new Option(t("customModel"), CUSTOM_MODEL));
  // Keep the name field open while the user is typing in it.
  pick.value = !input.hidden || !current ? CUSTOM_MODEL : current;
  input.hidden = pick.value !== CUSTOM_MODEL;
}

function renderSttPicks(): void {
  for (const k of MODEL_KEYS) renderSttPick(k);
}

for (const k of MODEL_KEYS) {
  const pick = $<HTMLSelectElement>(`m_${k}_pick`);
  const input = $(`m_${k}`);
  pick.addEventListener("change", () => {
    input.hidden = pick.value !== CUSTOM_MODEL;
    if (input.hidden) input.value = pick.value;
    else input.focus();
  });
}

/** Where the prices in the lists come from (`catalog::builtin_stt` or the catalog). */
const STT_PRICE_NOTE: Record<SttProvider, Key> = {
  elevenlabs: "priceNoteElevenLabs",
  xai: "priceNoteXai",
  openai: "priceNoteOpenAi",
  openrouter: "priceNoteOpenRouter",
};

/** Fetch the selected provider's STT models once per window. */
async function loadSttModels(): Promise<void> {
  const provider = $<HTMLSelectElement>("stt_provider").value as SttProvider;
  if (sttModelLists.has(provider)) status("stt-models-status", t(STT_PRICE_NOTE[provider]));
  if (sttModelLists.has(provider) || sttModelsLoading === provider) return;
  sttModelsLoading = provider;
  status("stt-models-status", t("loadingModels"));
  try {
    sttModelLists.set(provider, await invoke<SttModelList>("list_stt_models", { provider }));
    status("stt-models-status", t(STT_PRICE_NOTE[provider]));
  } catch (e) {
    status("stt-models-status", t("modelsFailed", { error: errorText(e) }), true);
  } finally {
    sttModelsLoading = "";
  }
  renderSttPicks();
}

// ---------- modes ----------

const TRASH =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14M10 11v5M14 11v5" /></svg>';

/** Redraws each mode's model picker after the backend or its model list changed. */
let modeModelRefreshers: (() => void)[] = [];

function refreshModeModels(): void {
  for (const refresh of modeModelRefreshers) refresh();
}

function modeKind(mode: Mode): ModeKind {
  return mode.post_process ? (mode.preset ?? "cleanup") : "none";
}

function markActiveMode(): void {
  document.querySelectorAll<HTMLElement>(".mode").forEach((card) => {
    const active = card.dataset.id === settings.active_mode;
    card.classList.toggle("active", active);
    const radio = card.querySelector<HTMLInputElement>('input[type="radio"]');
    if (radio) radio.checked = active;
  });
}

/** "Model AI" row of a mode: the active backend's list, or a name field for the CLIs. */
function modeModelRow(mode: Mode): HTMLElement {
  const id = `mode-model-${mode.id}`;
  const row = el("div", "set");
  const text = el("label", "set-text");
  text.htmlFor = id;
  const sub = el("small");
  text.append(el("b", "", t("modeModel")), sub);
  const ctl = el("div", "ctl");
  row.append(text, ctl);

  const custom = el("input");
  custom.hidden = true;
  custom.spellcheck = false;
  custom.placeholder = t("modelNamePlaceholder");
  custom.setAttribute("aria-label", t("modeModelNameAria", { mode: mode.name }));
  custom.addEventListener("input", () => (mode.llm_model = custom.value.trim()));

  const refresh = () => {
    const backend = $<HTMLSelectElement>("llm_backend").value as LlmBackend;
    sub.textContent = t("forBackend", { backend: LLM_LABELS[backend] });
    const fallback = t("defaultModel", { model: defaultModel() || t("cliModelFallback") });
    const http = modelBackend();
    if (!http) {
      custom.hidden = true;
      const input = el("input");
      input.id = id;
      input.spellcheck = false;
      input.value = mode.llm_model;
      input.placeholder = fallback;
      input.addEventListener("input", () => (mode.llm_model = input.value.trim()));
      ctl.replaceChildren(input);
      return;
    }
    const pick = el("select");
    pick.id = id;
    pick.append(new Option(fallback, ""), ...modelItems(http, mode.llm_model), new Option(t("customModel"), CUSTOM_MODEL));
    pick.value = custom.hidden ? mode.llm_model : CUSTOM_MODEL;
    pick.addEventListener("change", () => {
      custom.hidden = pick.value !== CUSTOM_MODEL;
      if (custom.hidden) {
        mode.llm_model = pick.value;
      } else {
        custom.value = mode.llm_model;
        custom.focus();
      }
    });
    ctl.replaceChildren(custom, pick);
  };
  modeModelRefreshers.push(refresh);
  refresh();
  return row;
}

function modeCard(mode: Mode, index: number): HTMLElement {
  const card = el("div", "card mode");
  card.dataset.id = mode.id;

  const radio = el("input");
  radio.type = "radio";
  radio.name = "active-mode";
  radio.value = mode.id;
  radio.title = t("activeMode");
  radio.setAttribute("aria-label", t("activeModeAria", { mode: mode.name }));
  radio.addEventListener("change", () => {
    settings.active_mode = mode.id;
    markActiveMode();
  });

  const name = el("input", "mode-name");
  name.value = mode.name;
  name.setAttribute("aria-label", t("modeNameAria"));
  name.addEventListener("input", () => {
    mode.name = name.value;
    radio.setAttribute("aria-label", t("activeModeAria", { mode: mode.name }));
  });

  const kind = el("select");
  kind.setAttribute("aria-label", t("modeKindAria"));
  kind.append(...MODE_KINDS.map(([value, label]) => new Option(t(label), value)));
  kind.value = modeKind(mode);

  const del = button("", "icon ghost");
  del.innerHTML = TRASH;
  del.title = t("deleteMode");
  del.setAttribute("aria-label", t("deleteModeAria", { mode: mode.name }));
  del.disabled = settings.modes.length <= 1;
  del.addEventListener("click", () => {
    settings.modes.splice(index, 1);
    if (!settings.modes.some((m) => m.id === settings.active_mode)) settings.active_mode = settings.modes[0].id;
    renderModes();
    updateDirty();
  });

  const head = el("div", "mode-head");
  head.append(radio, name, kind, del);

  const prompt = el("textarea");
  prompt.rows = 8;
  prompt.value = mode.prompt;
  prompt.setAttribute("aria-label", t("modePromptAria", { mode: mode.name }));
  const summary = el("summary");
  const showPrompt = () => {
    prompt.placeholder = builtinPrompts[mode.preset ?? "cleanup"] ?? "";
    summary.textContent = t(mode.prompt.trim() ? "promptOwn" : "promptBuiltin");
  };
  prompt.addEventListener("input", () => {
    mode.prompt = prompt.value;
    showPrompt();
  });
  const details = el("details");
  details.append(
    summary,
    prompt,
    el("p", "hint", t("promptHint")),
  );

  const body = el("div", "mode-body");
  body.append(modeModelRow(mode), details);
  body.hidden = !mode.post_process;
  showPrompt();

  kind.addEventListener("change", () => {
    const value = kind.value as ModeKind;
    mode.post_process = value !== "none";
    if (value !== "none") mode.preset = value;
    body.hidden = !mode.post_process;
    showPrompt();
  });

  card.append(head, body);
  return card;
}

function renderModes(): void {
  modeModelRefreshers = [];
  $<HTMLDivElement>("modes").replaceChildren(...settings.modes.map(modeCard));
  markActiveMode();
}

$<HTMLButtonElement>("add-mode").addEventListener("click", () => {
  let n = settings.modes.length + 1;
  while (settings.modes.some((m) => m.id === `tryb-${n}`)) n++;
  settings.modes.push({ id: `tryb-${n}`, name: t("newModeName", { n }), post_process: true, prompt: "", preset: "cleanup", llm_model: "" });
  renderModes();
  updateDirty();
  const name = document.querySelector<HTMLInputElement>(`.mode[data-id="tryb-${n}"] .mode-name`);
  name?.focus();
  name?.select();
});

// ---------- history ----------

let historyItems: HistoryItem[] = [];

function dayLabel(ts: number): string {
  const d = new Date(ts);
  d.setHours(0, 0, 0, 0);
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const diff = Math.round((today.getTime() - d.getTime()) / DAY_MS);
  return diff === 0 ? t("today") : diff === 1 ? t("yesterday") : fmt.weekday.format(d);
}

/** Copy to the clipboard; the textarea route covers a webview without clipboard access. */
async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const area = el("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.append(area);
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    return ok;
  }
}

function copyButton(label: string, text: string): HTMLButtonElement {
  const b = button(label, "ghost");
  b.addEventListener("click", async () => {
    b.textContent = t((await copyText(text)) ? "copied" : "copyFailed");
    window.setTimeout(() => (b.textContent = label), 1200);
  });
  return b;
}

function historyRow(h: HistoryItem): HTMLElement {
  const row = el("div", "hitem");
  const meta = el("div", "hmeta");
  meta.append(el("span", "", fmt.clock.format(h.ts_ms)), el("span", "", modeName(h.mode)));
  if (h.release_to_paste_ms !== null && h.release_to_paste_ms !== undefined) {
    meta.append(el("span", "", `${fmt.nf1.format(h.release_to_paste_ms / 1000)} s`));
  }
  const cost = costOf(h);
  if (cost.text !== "–") {
    const span = el("span", "", cost.text);
    span.title = cost.detail;
    meta.append(span);
  }
  if (h.outcome !== "ok") meta.append(el("span", "badge missing", OUTCOMES[h.outcome] ? t(OUTCOMES[h.outcome]) : h.outcome));
  const changed = h.transcript.trim() !== h.output.trim();
  const actions = el("span", "hactions");
  actions.append(copyButton(t("copy"), h.output));
  if (changed) actions.append(copyButton(t("copyTranscript"), h.transcript));
  meta.append(actions);
  row.append(meta, el("p", "htext", h.output));
  if (changed) {
    const transcript = el("p", "htrans", h.transcript);
    transcript.dataset.label = t("transcriptLabel");
    row.append(transcript);
  }
  return row;
}

function renderHistory(): void {
  const query = $("hist-search").value.trim().toLowerCase();
  const items = query
    ? historyItems.filter((h) => h.output.toLowerCase().includes(query) || h.transcript.toLowerCase().includes(query))
    : historyItems;

  const line = $<HTMLElement>("hist-status");
  line.classList.remove("error");
  const total = historyItems.length;
  const count = t(query ? "histCountFiltered" : "histCount", { n: items.length, total, dictations: plural(total, "dictations") });
  line.replaceChildren(count);
  if (settings && !settings.history.enabled) {
    const go = button(t("histEnableInGeneral"), "link");
    go.addEventListener("click", () => showPage("general"));
    line.append(t("histOff"), go);
  }

  const host = $<HTMLDivElement>("history-list");
  if (items.length === 0) {
    host.replaceChildren(el("div", "card empty-note", t(query ? "histNoMatch" : "histEmpty")));
    return;
  }
  const nodes: HTMLElement[] = [];
  let card: HTMLElement | null = null;
  let label = "";
  for (const h of items) {
    const l = dayLabel(h.ts_ms);
    if (l !== label || !card) {
      label = l;
      nodes.push(el("h2", "hday", l));
      card = el("div", "card");
      nodes.push(card);
    }
    card.append(historyRow(h));
  }
  host.replaceChildren(...nodes);
}

async function loadHistory(): Promise<void> {
  try {
    historyItems = await invoke<HistoryItem[]>("get_history");
    renderHistory();
  } catch (e) {
    status("hist-status", errorText(e), true);
  }
}

$("hist-search").addEventListener("input", renderHistory);

// ---------- secrets ----------

function renderSecrets(): void {
  $<HTMLDivElement>("secrets").replaceChildren(
    ...SECRETS().map(({ key, label, use }) => {
      const present = secrets[key] === true;
      const row = el("div", "set");
      const text = el("label", "set-text");
      text.htmlFor = `secret-${key}`;
      text.append(el("b", "", label), el("small", "", use));
      const badge = el("span", present ? "badge ok" : "badge", t(present ? "secretStored" : "secretNone"));
      const input = el("input");
      input.id = `secret-${key}`;
      input.type = "password";
      input.autocomplete = "off";
      input.placeholder = t(present ? "secretChange" : "secretPaste");
      // A redraw (language switch) keeps a key typed but not saved yet.
      input.value = document.querySelector<HTMLInputElement>(`#secret-${key}`)?.value ?? "";
      const save = button(t("save"));
      const store = async () => {
        if (!input.value.trim()) return;
        try {
          await invoke("set_secret", { key, value: input.value });
          input.value = "";
          await reloadSecrets(key);
        } catch (e) {
          status("save-status", errorText(e), true);
        }
      };
      save.addEventListener("click", () => void store());
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter") void store();
      });
      const del = button(t("delete"), "ghost");
      del.disabled = !present;
      del.addEventListener("click", async () => {
        try {
          await invoke("delete_secret", { key });
          await reloadSecrets(key);
        } catch (e) {
          status("save-status", errorText(e), true);
        }
      });
      const ctl = el("div", "ctl");
      ctl.append(badge, input, save, del);
      row.append(text, ctl);
      return row;
    }),
  );
}

async function reloadSecrets(key: SecretKey): Promise<void> {
  secrets = (await invoke<Snapshot>("get_snapshot")).secrets;
  renderSecrets();
  renderKeyBadges();
  if (BALANCE_KEYS.includes(key)) void loadOverview();
}

// ---------- hotkey capture ----------

const SIDED: Record<string, string> = {
  ControlLeft: "LeftCtrl",
  ControlRight: "RightCtrl",
  ShiftLeft: "LeftShift",
  ShiftRight: "RightShift",
  AltLeft: "LeftAlt",
  AltRight: "RightAlt",
  MetaLeft: "LeftWin",
  MetaRight: "RightWin",
};

const NAMED: Record<string, string> = {
  Space: "Space",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  CapsLock: "CapsLock",
  Pause: "Pause",
  ScrollLock: "ScrollLock",
  Insert: "Insert",
  Delete: "Delete",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowLeft: "Left",
  ArrowUp: "Up",
  ArrowRight: "Right",
  ArrowDown: "Down",
  ContextMenu: "Menu",
  Backquote: "Backquote",
};

function keyName(code: string): string | null {
  if (NAMED[code]) return NAMED[code];
  let m = /^Key([A-Z])$/.exec(code);
  if (m) return m[1];
  m = /^Digit(\d)$/.exec(code);
  if (m) return m[1];
  m = /^(F\d{1,2})$/.exec(code);
  if (m) return m[1];
  m = /^Numpad(\d)$/.exec(code);
  if (m) return `Numpad${m[1]}`;
  return null;
}

function captureHotkey(): void {
  const btn = $<HTMLButtonElement>("hotkey-capture");
  const field = $("hotkey");
  const previous = field.value;
  let soloModifier: string | null = null;
  let timer = 0;

  const finish = (value: string | null) => {
    window.removeEventListener("keydown", onDown, true);
    window.removeEventListener("keyup", onUp, true);
    window.clearTimeout(timer);
    void invoke("set_hotkey_capture", { active: false });
    field.value = value ?? previous;
    btn.textContent = t("change");
    btn.disabled = false;
    updateDirty();
  };
  const onDown = (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.repeat) return;
    if (e.code === "Escape") return finish(null);
    if (SIDED[e.code]) {
      // A lone modifier becomes the shortcut if it is released without another key.
      soloModifier = soloModifier === null && !anyModifier(e, e.code) ? SIDED[e.code] : "";
      return;
    }
    soloModifier = "";
    const key = keyName(e.code);
    if (!key) {
      field.value = t("unsupportedKey", { code: e.code });
      return;
    }
    const parts = [e.ctrlKey && "Ctrl", e.shiftKey && "Shift", e.altKey && "Alt", e.metaKey && "Win", key];
    finish(parts.filter(Boolean).join("+"));
  };
  const onUp = (e: KeyboardEvent) => {
    e.preventDefault();
    if (SIDED[e.code] && soloModifier === SIDED[e.code]) finish(soloModifier);
  };

  btn.disabled = true;
  btn.textContent = t("pressShortcut");
  field.value = "";
  void invoke("set_hotkey_capture", { active: true });
  window.addEventListener("keydown", onDown, true);
  window.addEventListener("keyup", onUp, true);
  timer = window.setTimeout(() => finish(null), 10_000);
}

/** Whether a modifier other than `code` is held. */
function anyModifier(e: KeyboardEvent, code: string): boolean {
  const own = SIDED[code];
  return (
    (e.ctrlKey && !own.endsWith("Ctrl")) ||
    (e.shiftKey && !own.endsWith("Shift")) ||
    (e.altKey && !own.endsWith("Alt")) ||
    (e.metaKey && !own.endsWith("Win"))
  );
}

$<HTMLButtonElement>("hotkey-capture").addEventListener("click", captureHotkey);

// ---------- devices, updates, timings ----------

async function loadDevices(selected: string): Promise<void> {
  const sel = $<HTMLSelectElement>("audio_device");
  const devices = await invoke<string[]>("list_input_devices").catch(() => [] as string[]);
  const names = selected && !devices.includes(selected) ? [selected, ...devices] : devices;
  sel.replaceChildren(new Option(t("systemDefault"), ""), ...names.map((d) => new Option(d, d)));
  sel.value = selected;
}

function showUpdate(u: UpdateStatus): void {
  status("update-status", u.message || u.state, u.state === "błąd");
}

$<HTMLButtonElement>("update-check").addEventListener("click", async () => {
  showUpdate(await invoke<UpdateStatus>("check_updates"));
  window.setTimeout(async () => showUpdate((await invoke<Snapshot>("get_snapshot")).update), 3000);
});

$<HTMLButtonElement>("open-logs").addEventListener("click", () => void invoke("open_logs_dir"));
document.querySelectorAll<HTMLButtonElement>("button[data-link]").forEach((b) =>
  b.addEventListener("click", () => void invoke("open_link", { link: b.dataset.link })),
);

const ms = (v: number | null) => (v === null ? "–" : `${v} ms`);

/** What one dictation cost: the known parts summed ("≈" when the speech part
 *  is estimated from the price list, "sub." for post-processing on a
 *  subscription, "?" for a part without a price), and the breakdown. */
function costOf(d: Timing): { text: string; detail: string } {
  if (d.stt_cost_usd === undefined) return { text: "–", detail: t("costNotRecorded") };
  const stt = d.stt_cost_usd;
  const llm = d.llm_cost_usd ?? null;
  const subscription = !!d.llm && llm === null && !d.llm.startsWith("openrouter:");
  const detail = [
    stt === null
      ? t("costNoPrice", { name: d.stt })
      : t(d.stt_cost_estimated ? "costEstimated" : "costFromProvider", { name: d.stt, cost: fmt.usdCost.format(stt) }),
  ];
  if (d.llm) {
    detail.push(
      llm !== null
        ? t("costFromProvider", { name: d.llm, cost: fmt.usdCost.format(llm) })
        : `${d.llm}: ${t(subscription ? "costSubscription" : "costNoAmount")}`,
    );
  }
  const parts = [];
  if (stt !== null || llm !== null) {
    parts.push((stt !== null && d.stt_cost_estimated ? "≈" : "") + fmt.usdCost.format((stt ?? 0) + (llm ?? 0)));
  }
  if (subscription) parts.push(t("costSubShort"));
  if (stt === null || (!!d.llm && llm === null && !subscription)) parts.push("?");
  // A narrow column wraps before "+", never between "+" and its part.
  return { text: parts.join(" + "), detail: detail.join("\n") };
}

async function loadTimings(): Promise<void> {
  const rows = await invoke<Timing[]>("recent_timings");
  const body = $<HTMLTableSectionElement>("timings");
  if (rows.length === 0) {
    const tr = el("tr");
    const td = el("td", "muted", t("noDictations"));
    td.colSpan = 10;
    tr.append(td);
    body.replaceChildren(tr);
    return;
  }
  body.replaceChildren(
    ...rows.map((d) => {
      const tr = el("tr");
      for (const v of [
        d.outcome,
        modeName(d.mode),
        d.stt,
        t(d.streamed ? "yes" : "no"),
        ms(d.audio_ms),
        ms(d.release_to_stt_ms),
        d.llm ? ms(d.llm_ms) : "–",
        ms(d.release_to_paste_ms),
        String(d.chars),
      ]) {
        tr.append(el("td", "", v));
      }
      if (d.llm) tr.children[6].setAttribute("title", d.llm);
      const cost = costOf(d);
      const td = el("td", "wrap", cost.text);
      td.title = cost.detail;
      tr.append(td);
      return tr;
    }),
  );
}

// ---------- overview: balances and usage ----------

const DAY_MS = 86_400_000;

function daysUntil(unix: number): number {
  return Math.max(0, Math.ceil((unix * 1000 - Date.now()) / DAY_MS));
}

type Row = [string, string | Node];

function balanceCard(title: string, tag: string, amount: string, unit: string, rows: Row[], fill?: number): HTMLElement {
  const card = el("article", "card balance");
  const head = el("header");
  head.append(el("strong", "", title), el("span", "tag", tag));
  const big = el("div", "amount");
  big.append(el("b", "", amount), el("span", "", unit));
  card.append(head, big);
  if (fill !== undefined) {
    const meter = el("div", fill < 0.15 ? "meter low" : "meter");
    const bar = el("i");
    bar.style.width = `${Math.round(Math.min(1, Math.max(0, fill)) * 100)}%`;
    meter.append(bar);
    card.append(meter);
  }
  const dl = el("dl");
  for (const [k, v] of rows) {
    const dd = el("dd");
    dd.append(v);
    dl.append(el("dt", "", k), dd);
  }
  card.append(dl);
  return card;
}

function missingCard(title: string, tag: string, fetched: { state: "no_key" } | { state: "error"; message: string }): HTMLElement {
  if (fetched.state === "error") {
    return balanceCard(title, tag, t("error"), "", [[t("response"), el("span", "error", fetched.message)]]);
  }
  const add = button(t("addKey"), "link");
  add.addEventListener("click", () => showPage("keys"));
  return balanceCard(title, tag, t("keyMissing"), "", [[t("balance"), add]]);
}

function lastDays(rows: UsageRow[], days: number): UsageRow[] {
  const since = Date.now() / 1000 - days * 86_400;
  return rows.filter((r) => r.ts >= since);
}

const minutes = (rows: UsageRow[]) => rows.reduce((sum, r) => sum + r.audio_ms, 0) / 60_000;

/** `week` is this computer's usage; it is not split between the two accounts, so only the main card shows it. */
function elevenLabsCard(b: ElevenLabsBalance, title: string, week?: UsageRow[]): HTMLElement {
  const rows: Row[] = [];
  if (b.reset_unix !== null) {
    const left = daysUntil(b.reset_unix);
    rows.push([t("renewal"), t("renewalValue", { date: fmt.day.format(b.reset_unix * 1000), n: left, days: plural(left, "days") })]);
  }
  rows.push([
    t("lastsFor"),
    t("elevenLabsLasts", {
      live: fmt.nf0.format(b.remaining / b.credits_per_min_realtime),
      batch: fmt.nf0.format(b.remaining / b.credits_per_min_batch),
    }),
  ]);
  if (b.per_day !== null) {
    const verdict =
      b.runs_out_unix === null
        ? el("span", "good", t("enoughUntilRenewal"))
        : el("span", "warn", t("runsOut", { date: fmt.day.format(b.runs_out_unix * 1000) }));
    const pace = el("span", "", t("pace", { n: fmt.nf0.format(b.per_day), credits: plural(b.per_day, "credits") }));
    const both = el("span");
    both.append(pace, verdict);
    rows.push([t("accountPace"), both]);
  }
  if (week) {
    const mine = week.filter((r) => r.stt === "ElevenLabs");
    const credits = mine.reduce(
      (sum, r) => sum + (r.audio_ms / 60_000) * (r.streamed ? b.credits_per_min_realtime : b.credits_per_min_batch),
      0,
    );
    rows.push([t("thisComputer7"), `≈ ${fmt.nf0.format(credits)} ${plural(credits, "credits")} (${fmt.nf1.format(minutes(mine))} min)`]);
  }
  const tier = b.tier ? t("tierPlan", { tier: b.tier }) : t("speechToText");
  return balanceCard(title, tier, fmt.nf0.format(b.remaining), t("ofCredits", { limit: fmt.nf0.format(b.limit) }), rows, b.remaining / b.limit);
}

function openRouterCard(b: OpenRouterBalance): HTMLElement {
  const rows: Row[] = [];
  const of = (amount: number, total: number) => t("amountOf", { amount: fmt.usd.format(amount), total: fmt.usd.format(total) });
  rows.push([
    t("account"),
    b.account_remaining !== null && b.account_total !== null ? of(b.account_remaining, b.account_total) : t("keyNoAccount"),
  ]);
  rows.push([t("keyLimit"), b.key_limit !== null && b.key_remaining !== null ? of(b.key_remaining, b.key_limit) : t("noLimit")]);
  rows.push([t("last7"), t("last7Value", { week: fmt.usd.format(b.usage_weekly), today: fmt.usd.format(b.usage_daily) })]);
  rows.push([
    t("lastsFor"),
    b.days_left !== null
      ? t("openRouterLasts", { n: fmt.nf0.format(b.days_left), days: plural(b.days_left, "days") })
      : t("noSpending"),
  ]);
  // The meter follows whichever amount runs out first.
  const byKey = b.key_remaining !== null && b.key_limit && b.available === Math.max(0, b.key_remaining);
  const fill = byKey
    ? b.key_remaining! / b.key_limit!
    : b.account_remaining !== null && b.account_total
      ? b.account_remaining / b.account_total
      : undefined;
  const amount = b.available === null ? "—" : fmt.usd.format(b.available);
  return balanceCard("OpenRouter", t("paidPerToken"), amount, t("toSpend"), rows, fill);
}

function cliProxyCard(week: UsageRow[]): HTMLElement {
  const calls = week.filter((r) => r.llm.startsWith("endpoint:")).length;
  return balanceCard("CLIProxy", t("subscription"), t("noBalance"), t("subscriptionNotTokens"), [
    [t("model"), settings.llm.model || "—"],
    [t("thisComputer7"), t("postProcessingRequests", { n: calls, requests: plural(calls, "requests") })],
  ]);
}

/** Whether any mode sends the transcript to the post-processing backend. */
const postProcesses = (s: Settings) => s.modes.some((m) => m.post_process);

/** Only providers the saved settings use get a card (`Fetched` "unused" otherwise). */
function renderBalances(o: Overview): void {
  const week = lastDays(o.usage, 7);
  const cards: HTMLElement[] = [];
  const notes: string[] = [];
  if (o.elevenlabs.state !== "unused") {
    cards.push(
      o.elevenlabs.state === "ok"
        ? elevenLabsCard(o.elevenlabs.data, "ElevenLabs", week)
        : missingCard("ElevenLabs", t("speechToText"), o.elevenlabs),
    );
    // The backup account gets a card only once its key is stored.
    const backup = o.elevenlabs_backup;
    const title = t("backupTitle");
    if (backup.state === "ok") cards.push(elevenLabsCard(backup.data, title));
    if (backup.state === "error") cards.push(missingCard(title, t("speechToText"), backup));
    notes.push(t("elevenLabsNote"));
  }
  if (o.openrouter.state !== "unused") {
    cards.push(
      o.openrouter.state === "ok" ? openRouterCard(o.openrouter.data) : missingCard("OpenRouter", t("paidPerToken"), o.openrouter),
    );
    notes.push(t("openRouterNote"));
  }
  if (settings.llm.backend === "open_ai_compatible" && postProcesses(settings)) cards.push(cliProxyCard(week));
  const stt = settings.stt.provider;
  if (stt === "xai" || stt === "openai") notes.push(t("noBalanceApi", { provider: STT_LABELS[stt] }));
  if (cards.length === 0) notes.unshift(t("noBalances"));
  $<HTMLDivElement>("balances").replaceChildren(...cards);
  $<HTMLElement>("balance-note").textContent = notes.join(" ");
}

function stat(label: string, value: string): HTMLElement {
  const d = el("div");
  d.append(el("span", "", label), el("b", "", value));
  return d;
}

function median(values: number[]): number | null {
  if (values.length === 0) return null;
  const s = [...values].sort((a, b) => a - b);
  const mid = s.length >> 1;
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}

function renderUsage(rows: UsageRow[]): void {
  const week = lastDays(rows, 7);
  const paste = median(week.filter((r) => r.outcome === "ok" && r.release_to_paste_ms !== null).map((r) => r.release_to_paste_ms!));
  const failed = week.filter((r) => r.outcome !== "ok").length;
  $<HTMLDivElement>("usage-stats").replaceChildren(
    stat(t("days7"), `${week.length} ${plural(week.length, "dictations")}, ${fmt.nf1.format(minutes(week))} min`),
    stat(t("days30"), `${rows.length} ${plural(rows.length, "dictations")}, ${fmt.nf1.format(minutes(rows))} min`),
    stat(t("medianPaste"), paste === null ? "—" : `${fmt.nf1.format(paste / 1000)} s`),
    stat(t("failed7"), String(failed)),
  );

  // One bar per local calendar day, oldest first.
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const buckets = Array.from({ length: 30 }, (_, i) => ({ start: today.getTime() - (29 - i) * DAY_MS, min: 0, n: 0 }));
  for (const r of rows) {
    const d = new Date(r.ts * 1000);
    d.setHours(0, 0, 0, 0);
    const b = buckets.find((x) => x.start === d.getTime());
    if (b) {
      b.min += r.audio_ms / 60_000;
      b.n += 1;
    }
  }
  const max = Math.max(...buckets.map((b) => b.min), 0.01);
  const chart = $<HTMLDivElement>("usage-chart");
  chart.replaceChildren(
    ...buckets.map((b) => {
      const bar = el("i", b.n ? "" : "empty");
      bar.style.height = `${b.n ? Math.max(4, (b.min / max) * 100) : 2}%`;
      bar.title = `${fmt.day.format(b.start)}: ${fmt.nf1.format(b.min)} min, ${b.n} ${plural(b.n, "dictations")}`;
      return bar;
    }),
  );
  chart.setAttribute("aria-label", t("chartAria", { min: fmt.nf1.format(minutes(rows)) }));
  $<HTMLElement>("chart-from").textContent = fmt.day.format(buckets[0].start);
}

const HOTKEY_MODES: Record<Settings["hotkey_mode"], Key> = {
  auto: "hkAuto",
  hold: "hkHold",
  toggle: "hkToggle",
};

function renderTiles(): void {
  const s = settings;
  const text = (id: string, value: string) => ($<HTMLElement>(id).textContent = value);
  text("ov-hotkey", s.hotkey);
  text("side-hotkey", s.hotkey);
  text("ov-hotkey-sub", t(HOTKEY_MODES[s.hotkey_mode]));
  const mode = s.modes.find((m) => m.id === s.active_mode);
  text("ov-mode", modeName(s.active_mode));
  const kind = mode ? t(MODE_KINDS.find(([k]) => k === modeKind(mode))![1]) : "";
  const modeSub = mode?.post_process && mode.llm_model ? `${kind}, ${mode.llm_model}` : kind;
  text("ov-mode-sub", modeSub === modeName(s.active_mode) ? "" : modeSub);
  const stt = sttInUse(s.stt);
  text("ov-stt", STT_LABELS[s.stt.provider]);
  text("ov-stt-sub", `${stt.model}, ${t(stt.live ? "live" : "batch")}`);
  const model = { open_ai_compatible: s.llm.model, open_router: s.llm.openrouter_model, claude_cli: s.llm.cli_model, codex_cli: s.llm.cli_model }[
    s.llm.backend
  ];
  text("ov-llm", LLM_LABELS[s.llm.backend]);
  text("ov-llm-sub", model || t("defaultModelShort"));
}

document.querySelectorAll<HTMLButtonElement>(".tile[data-goto]").forEach((t) => t.addEventListener("click", () => showPage(t.dataset.goto!)));

let overviewLoading = false;

async function loadOverview(): Promise<void> {
  if (overviewLoading) return;
  overviewLoading = true;
  const btn = $<HTMLButtonElement>("ov-refresh");
  btn.disabled = true;
  status("ov-status", t("loadingBalance"));
  void loadNews();
  try {
    const o = await invoke<Overview>("get_overview");
    renderBalances(o);
    renderUsage(o.usage);
    status("ov-status", t("stateAt", { time: fmt.clock.format(o.fetched_at * 1000) }));
  } catch (e) {
    status("ov-status", errorText(e), true);
  } finally {
    overviewLoading = false;
    btn.disabled = false;
  }
}

$<HTMLButtonElement>("ov-refresh").addEventListener("click", () => {
  rankingRequest = null;
  void loadOverview();
});

// ---------- overview: new models ----------

function newsCard(
  title: string,
  tag: string,
  models: NewModel[],
  error: string | null,
  empty: string,
  page: string,
  ranked = new Map<string, RankedModel>(),
): HTMLElement {
  const card = el("article", "card balance");
  const head = el("header");
  head.append(el("strong", "", title), el("span", "tag", tag));
  card.append(head);
  if (error) card.append(el("p", "error", t("listFailed", { error })));
  else if (!models.length) card.append(el("p", "muted", empty));
  else {
    const dl = el("dl");
    for (const m of models) {
      const dd = el("dd");
      const name = el("span", "", m.name);
      name.title = m.id;
      dd.append(name);
      const price = priceText(m.price);
      if (price) dd.append(" ", el("span", "muted", price));
      if (m.newer_than_yours) dd.append(" ", el("span", "badge new", t("newerThanYours")));
      const r = ranked.get(m.id);
      if (r) {
        // A test without a score says why: "przerwany: …" or "niedostępny: …" (ranking.json, Polish only).
        const unscored = r.note.split(":")[0] || t("unscored");
        const badge = el("span", "badge", r.score === null ? unscored : `${fmt.score.format(r.score)} · ${secondsText(r.median_ms)}`);
        badge.title =
          r.score === null ? r.note : t("rankingBadgeTitle", { score: fmt.score.format(r.score), time: secondsText(r.median_ms) });
        dd.append(" ", badge);
      }
      dl.append(el("dt", "", m.created ? fmt.day.format(m.created * 1000) : "—"), dd);
    }
    card.append(dl);
  }
  const go = button(t("changeModel"), "link");
  go.addEventListener("click", () => showPage(page));
  card.append(go);
  return card;
}

let newsLoading = false;

async function loadNews(): Promise<void> {
  if (newsLoading) return;
  newsLoading = true;
  try {
    const [n, ranking] = await Promise.all([invoke<ModelNews>("get_model_news"), fetchRanking().catch(() => null)]);
    const ranked = new Map(n.llm_source === "OpenRouter" ? (ranking?.models ?? []).map((m) => [m.model, m]) : []);
    const since = t("sinceDays", { n: n.days, days: plural(n.days, "days") });
    const none = t("noNewModels", { since });
    const cards = [newsCard(t("pageStt"), "OpenRouter", n.stt, n.stt_error, none, "stt")];
    cards.push(
      n.llm_source
        ? newsCard(t("pageLlm"), n.llm_source, n.llm, n.llm_error, none, "llm", ranked)
        : newsCard(t("pageLlm"), "CLI", [], null, t("cliNoList"), "llm"),
    );
    $("news").replaceChildren(...cards);
    status("news-note", t("newsNote", { since }));
  } catch (e) {
    status("news-note", errorText(e), true);
  } finally {
    newsLoading = false;
  }
}

// ---------- post-processing: model ranking ----------

const RANKING_ROWS = 10;
let rankingRequest: Promise<RankingView> | null = null;
let rankingAll = false;

/** The ranking once per window; again after a failure or on `force`. */
function fetchRanking(force = false): Promise<RankingView> {
  if (force || !rankingRequest) {
    const request = invoke<RankingView>("get_model_ranking");
    rankingRequest = request;
    request.catch(() => {
      if (rankingRequest === request) rankingRequest = null;
    });
  }
  return rankingRequest;
}

function secondsText(ms: number | null): string {
  if (ms === null) return "–";
  const s = ms / 1000;
  return `${s < 1 ? fmt.nf2.format(s) : fmt.nf1.format(s)} s`;
}

/** The OpenRouter model the form has now (unsaved choices included), without its routing suffix. */
function formOpenRouterModel(): string {
  if ($<HTMLSelectElement>("llm_backend").value !== "open_router") return "";
  return $("llm_openrouter_model").value.trim().split(":")[0];
}

function useRanked(m: RankedModel): void {
  const backend = $<HTMLSelectElement>("llm_backend");
  if (backend.value !== "open_router") {
    backend.value = "open_router";
    backend.dispatchEvent(new Event("change"));
  }
  $("llm_openrouter_model").value = m.use_model;
  $("llm_reasoning_effort").value = m.effort;
  $("llm_model_custom_row").hidden = true;
  renderModelPick();
  refreshModeModels();
  updateDirty();
  void loadRanking();
  status("ranking-status", t("rankingPicked", { name: m.name, model: m.use_model, effort: m.effort }));
}

function rankingRow(m: RankedModel, place: number, yours: string): HTMLTableRowElement {
  const tr = el("tr");
  const mine = m.model === yours;
  if (mine) tr.className = "yours";
  if (!m.available || m.score === null) tr.classList.add("dim");

  const name = el("td", "wrap");
  const label = el("span", "", m.name);
  label.title = `${m.use_model}, reasoning effort ${m.effort}`;
  name.append(label);
  if (mine) name.append(" ", el("span", "badge ok", t("yours")));
  if (!m.available) name.append(" ", el("span", "badge", t("withdrawn")));
  if (m.other_prompt) name.append(" ", el("span", "badge missing", t("otherPrompt")));
  if (m.note) name.append(" ", el("span", "muted", m.note));

  const score = el("td", "", m.score === null ? "–" : fmt.score.format(m.score));
  score.title = [
    t("scoreTitle", { good: m.good, cases: m.cases, errors: m.errors }),
    ...m.issues.map((i) => `• ${i}`),
  ].join("\n");

  const time = el("td", "", secondsText(m.median_ms));
  time.title = [
    t("timeTitle", { date: fmt.day.format(m.timed_at * 1000) }),
    m.p90_ms !== null ? t("p90", { time: secondsText(m.p90_ms) }) : "",
    ...m.earlier.map((e) => t("earlier", { date: fmt.day.format(e.at * 1000), time: secondsText(e.median_ms) })),
  ]
    .filter(Boolean)
    .join("\n");

  const cost = el("td", "", m.cost_usd === null ? "–" : fmt.usd2.format(m.cost_usd * 1000));
  cost.title = t("costTitle");

  const use = button(t(mine ? "inUse" : "use"));
  use.disabled = mine || !m.available || m.score === null;
  use.addEventListener("click", () => useRanked(m));
  const action = el("td");
  action.append(use);

  tr.append(el("td", "muted", String(place)), name, score, time, cost, el("td", "", fmt.day.format(m.tested_at * 1000)), action);
  return tr;
}

async function loadRanking(force = false): Promise<void> {
  const body = $<HTMLTableSectionElement>("ranking");
  const message = (text: string, cls = "muted") => {
    const td = el("td", cls, text);
    td.colSpan = 7;
    const tr = el("tr");
    tr.append(td);
    body.replaceChildren(tr);
  };
  if (!body.children.length) message(t("loadingRanking"));
  let r: RankingView;
  try {
    r = await fetchRanking(force);
  } catch (e) {
    message(t("rankingFailed", { error: errorText(e) }), "error");
    $("ranking-more").hidden = true;
    status("ranking-note", "");
    return;
  }
  const yours = formOpenRouterModel();
  const shown = rankingAll ? r.models : r.models.slice(0, RANKING_ROWS);
  // The model in use stays visible even below the cut.
  const own = r.models.find((m) => m.model === yours);
  if (own && !shown.includes(own)) shown.push(own);
  if (!shown.length) message(t("rankingEmpty"));
  else body.replaceChildren(...shown.map((m) => rankingRow(m, r.models.indexOf(m) + 1, yours)));

  const more = $<HTMLButtonElement>("ranking-more");
  more.hidden = r.models.length <= RANKING_ROWS;
  more.textContent = rankingAll ? t("showTop") : t("showAll", { n: r.models.length });

  const notes = [
    t("rankingNote", {
      date: fmt.day.format(r.generated_at * 1000),
      time: fmt.clock.format(r.generated_at * 1000),
      cases: r.cases,
      dictations: plural(r.cases, "dictations"),
      judge: r.judge,
      price: fmt.usdPrice.format(5),
    }),
  ];
  if (r.age_days >= 2) notes.unshift(t("rankingStale", { n: r.age_days, days: plural(r.age_days, "days") }));
  if (r.prompt_sha !== r.app_prompt_sha) notes.push(t("rankingOtherPrompt", { theirs: r.prompt_sha, ours: r.app_prompt_sha }));
  status("ranking-note", notes.join(" "), r.age_days >= 2);
}

$<HTMLButtonElement>("ranking-more").addEventListener("click", () => {
  rankingAll = !rankingAll;
  void loadRanking();
});

// ---------- navigation ----------

let currentPage = "";

/** Data a page needs fresh each time it is shown. */
function pageShown(name: string): void {
  if (!settings) return; // `load` calls this again once the snapshot is in
  if (name === "llm") {
    void loadModels();
    void loadRanking();
  }
  if (name === "stt") void loadSttModels();
  if (name === "modes") {
    refreshModeModels();
    void loadModels();
  }
  if (name === "history") void loadHistory();
}

function showPage(name: string): void {
  if (!document.querySelector(`.page[data-page="${CSS.escape(name)}"]`)) name = "overview";
  document.querySelectorAll<HTMLElement>(".page").forEach((p) => (p.hidden = p.dataset.page !== name));
  document.querySelectorAll<HTMLButtonElement>(".nav").forEach((b) => {
    if (b.dataset.page === name) b.setAttribute("aria-current", "page");
    else b.removeAttribute("aria-current");
  });
  document.querySelector("main")?.scrollTo(0, 0);
  currentPage = name;
  pageShown(name);
}

document.querySelectorAll<HTMLButtonElement>(".nav").forEach((b) => b.addEventListener("click", () => showPage(b.dataset.page ?? "overview")));
// The tray opens a page in an already open window.
void listen<string>("navigate", (e) => showPage(e.payload));

// ---------- save / load ----------

/** Whether the footer says "Unsaved changes" (in whichever language). */
let showsUnsaved = false;

function updateDirty(): void {
  if (!settings) return;
  const dirty = JSON.stringify(readForm()) !== baseline;
  $<HTMLButtonElement>("save").disabled = !dirty;
  document.querySelector("footer")?.classList.toggle("dirty", dirty);
  if (dirty) status("save-status", t("unsaved"));
  else if (showsUnsaved) status("save-status", "");
  showsUnsaved = dirty;
}

function markSaved(): void {
  baseline = JSON.stringify(readForm());
  updateDirty();
}

document.addEventListener("input", updateDirty);
document.addEventListener("change", updateDirty);

/** What the balance and news cards depend on. */
const overviewInputs = (s: Settings) => JSON.stringify([s.stt, s.llm, s.modes.map((m) => [m.post_process, m.llm_model])]);

$<HTMLButtonElement>("save").addEventListener("click", async () => {
  const btn = $<HTMLButtonElement>("save");
  btn.disabled = true;
  try {
    const before = overviewInputs(settings);
    const languageBefore = settings.ui_language;
    settings = await invoke<Settings>("save_settings", { settings: readForm() });
    fillForm();
    renderTiles();
    markSaved();
    status("save-status", t("saved"));
    // The app words the update status when it is read: read it again in the new language.
    if (settings.ui_language !== languageBefore) showUpdate((await invoke<Snapshot>("get_snapshot")).update);
    if (currentPage === "history") renderHistory();
    if (overviewInputs(settings) !== before) void loadOverview();
  } catch (e) {
    status("save-status", errorText(e), true);
    btn.disabled = false;
  }
});

async function load(): Promise<void> {
  const snap = await invoke<Snapshot>("get_snapshot");
  settings = snap.settings;
  builtinPrompts = snap.builtin_prompts;
  secrets = snap.secrets;
  snapshot = snap;
  applyLanguage(settings.ui_language);
  $<HTMLElement>("logs-dir").textContent = snap.logs_dir;
  fillForm();
  renderTiles();
  renderSecrets();
  showUpdate(snap.update);
  await Promise.all([loadDevices(settings.audio.device), loadTimings()]);
  markSaved();
  pageShown(currentPage);
}

/**
 * A setting changed from the tray and is already saved: apply it to the form
 * and to the saved baseline, keeping the user's other unsaved edits.
 */
function savedElsewhere(apply: (s: Settings) => void): void {
  if (!baseline) return; // still loading: the snapshot already has the change
  apply(settings);
  const saved = JSON.parse(baseline) as Settings;
  apply(saved);
  baseline = JSON.stringify(saved);
  renderTiles();
  updateDirty();
}

void listen<string>("active-mode-changed", (e) =>
  savedElsewhere((s) => {
    s.active_mode = e.payload;
    markActiveMode();
  }),
);
void listen<boolean>("autostart-changed", (e) =>
  savedElsewhere((s) => {
    s.autostart = e.payload;
    $("autostart").checked = e.payload;
  }),
);
$<HTMLSelectElement>("ui_language").addEventListener("change", (e) => {
  applyLanguage((e.target as HTMLSelectElement).value as Lang);
  rerender();
});
applyLanguage(rememberedLang());
showPage(decodeURIComponent(location.hash.slice(1)) || "overview");
window.setInterval(() => void loadTimings(), 5000);
load()
  .then(loadOverview)
  .catch((e) => status("save-status", errorText(e), true));
