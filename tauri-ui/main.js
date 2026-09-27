const invoke = window.__TAURI__.core.invoke;
const appWindow = window.__TAURI__.window.getCurrentWindow();
const $ = selector => document.querySelector(selector);
const $$ = selector => [...document.querySelectorAll(selector)];
const presets = {
  codex: ['Open Codex', 'https://chatgpt.com/codex'],
  usage: ['Usage', 'https://chatgpt.com/codex/settings/analytics'],
  repo: ['GitHub repo', 'https://github.com/inerthel-agi/codex-rpc'],
};
const activityNames = { playing: 'Playing', watching: 'Watching', listening: 'Listening to', competing: 'Competing in' };
const labels = [$('#label0'), $('#label1')];
const urls = [$('#url0'), $('#url1')];
const message = $('#message');
let settings = null;
let status = { state: 'Codex: Off', model_parts: [], credits: null, usage: [], plan: '', discord: '', presence: 'off', started_at_ms: null };
let loading = true;
let timer;

// ---------- Settings form ----------
function readButtons() {
  return labels.map((label, i) => ({ label: label.value.trim(), url: urls[i].value.trim() })).filter(button => button.label && button.url);
}
function writeForm() {
  $$('#modes [data-mode]').forEach(button => button.setAttribute('aria-checked', String(button.dataset.mode === settings.mode)));
  // Older options default to on; "Plan" is opt-in.
  $$('#chips .chip').forEach(chip => chip.setAttribute('aria-pressed', String(chip.dataset.key === 'show_plan' ? settings.show_plan === true : settings[chip.dataset.key] !== false)));
  $$('.switch[data-key]').forEach(toggle => toggle.setAttribute('aria-checked', String(Boolean(settings[toggle.dataset.key]))));
  const idle = String(settings.idle_clear_minutes || 0);
  if (![...$('#idle').options].some(option => option.value === idle)) $('#idle').append(new Option(`After ${idle} min`, idle));
  $('#idle').value = idle;
  $('#template').value = settings.custom_state || '';
  labels.forEach((label, i) => { label.value = settings.buttons?.[i]?.label || ''; urls[i].value = settings.buttons?.[i]?.url || ''; });
  $('#lock').hidden = settings.mode === 'watching';
  updatePreview();
}
async function save() {
  clearTimeout(timer);
  if (loading || !settings) return;
  try {
    // paused_until_ms is owned by the tray; Rust keeps the value on disk.
    await invoke('save_settings', { settings: { ...settings, paused_until_ms: 0, buttons: readButtons() } });
    $('#saved-icon').classList.remove('pending');
    message.textContent = 'Saved · ' + new Date().toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  } catch (error) {
    message.textContent = 'Could not save: ' + String(error);
  }
}
function change(key, value) {
  if (key) settings[key] = value;
  updatePreview();
  if (loading) return;
  clearTimeout(timer);
  $('#saved-icon').classList.add('pending');
  message.textContent = 'Saving…';
  timer = setTimeout(save, 300);
}

$$('#modes [data-mode]').forEach(button => button.addEventListener('click', () => {
  $$('#modes [data-mode]').forEach(other => other.setAttribute('aria-checked', String(other === button)));
  $('#lock').hidden = button.dataset.mode === 'watching';
  change('mode', button.dataset.mode);
}));
$$('#chips .chip').forEach(chip => chip.addEventListener('click', () => {
  const on = chip.getAttribute('aria-pressed') !== 'true';
  chip.setAttribute('aria-pressed', String(on));
  change(chip.dataset.key, on);
}));
$$('.switch[data-key]').forEach(toggle => toggle.addEventListener('click', () => {
  const on = toggle.getAttribute('aria-checked') !== 'true';
  toggle.setAttribute('aria-checked', String(on));
  change(toggle.dataset.key, on);
}));
$('#idle').addEventListener('change', event => change('idle_clear_minutes', Number(event.target.value)));
$('#template').addEventListener('input', event => change('custom_state', event.target.value));
$$('#tokens button').forEach(button => button.addEventListener('click', () => {
  const input = $('#template');
  const at = input.selectionStart ?? input.value.length;
  const before = input.value.slice(0, at);
  const token = (before && !before.endsWith(' ') ? ' ' : '') + button.textContent;
  input.value = before + token + input.value.slice(input.selectionEnd ?? at);
  input.focus();
  input.setSelectionRange(at + token.length, at + token.length);
  change('custom_state', input.value);
}));
for (const input of [...labels, ...urls]) input.addEventListener('input', () => change());
$$('[data-remove]').forEach(button => button.addEventListener('click', () => {
  const i = Number(button.dataset.remove);
  labels[i].value = '';
  urls[i].value = '';
  change();
}));
$$('[data-preset]').forEach(button => button.addEventListener('click', () => {
  const slot = labels[0].value.trim() ? 1 : 0;
  [labels[slot].value, urls[slot].value] = presets[button.dataset.preset];
  change();
}));
$('#to-watching').addEventListener('click', () => $('#modes [data-mode="watching"]').click());

// ---------- Discord preview (mirrors daemon.rs build_details / build_state_line) ----------
function dropDanglingSeparators(text) {
  const separator = word => /^[·\-|/•—]+$/.test(word);
  const words = [];
  for (const word of text.split(/\s+/).filter(Boolean)) {
    if (separator(word) && (!words.length || separator(words[words.length - 1]))) continue;
    words.push(word);
  }
  while (words.length && separator(words[words.length - 1])) words.pop();
  return words.join(' ');
}
function previewState() {
  const pro = UsageView.isPro(status.plan);
  const { model, effort, speed } = UsageView.splitModel(status.model_parts);
  const shown = settings.hide_model
    ? { model: 'Coding', effort: '', speed: '' }
    : { model, effort: settings.show_effort ? effort : '', speed: settings.show_fast_mode ? speed : '' };
  const find = name => status.usage.find(entry => entry.label.toLowerCase() === name);
  const percent = entry => entry ? Math.round(entry.percent) + '%' : '';
  const h5 = settings.show_primary_usage && !pro ? find('5h') : null;
  const week = settings.show_weekly_usage ? find('week') : null;
  const credits = settings.show_credits ? (/(\d[\d.,]*)/.exec(status.credits || '')?.[1] || '') : '';
  const plan = settings.show_plan && status.plan ? UsageView.planLabel(status.plan) : '';
  const template = (settings.custom_state || '').trim();
  if (template) {
    // Like {credits}, {plan} stays empty unless its chip is ticked (daemon.rs filter_usage).
    const values = { model: shown.model, effort: shown.effort, speed: shown.speed, '5h': percent(h5), week: percent(week), credits, plan, project: settings.hide_model ? '' : 'project' };
    const text = dropDanglingSeparators(template.replace(/\{(model|effort|speed|5h|week|credits|plan|project)\}/g, (_, key) => values[key] || ''));
    if (text.length >= 2) return text.slice(0, 128);
  }
  const fallback = { 'Codex: CLI': 'Terminal session active', 'Codex: CLI/Desktop': 'CLI + Desktop' }[status.state] || 'Desktop session';
  const base = [shown.model, shown.effort, shown.speed].filter(Boolean).join(' - ') || fallback;
  const creditsPart = Math.round(Number(credits.replace(',', '.'))) > 0 ? `${Math.round(Number(credits.replace(',', '.')))} credits` : '';
  return [base, h5 && '5h ' + percent(h5), week && 'week ' + percent(week), creditsPart, plan && 'ChatGPT ' + plan].filter(Boolean).join(' - ');
}
function updatePreview() {
  if (!settings) return;
  const active = UsageView.isActive(status.state);
  const watching = settings.mode === 'watching';
  const cli = status.state.includes('CLI');
  const both = status.state === 'Codex: CLI/Desktop';
  $('#dc-activity').textContent = activityNames[settings.mode] || 'Playing';
  $('#dc-details').textContent = !active ? 'Codex is not running'
    : both ? (watching ? 'Watching Codex (CLI + Desktop)' : 'Coding with Codex (CLI + Desktop)')
    : cli ? (watching ? 'Watching Codex CLI' : 'Coding with Codex CLI')
    : (watching ? 'Watching Codex' : 'Using Codex');
  $('#dc-state').textContent = active ? previewState() : 'Nothing is shown on Discord right now';
  const buttons = $('#dc-buttons');
  buttons.replaceChildren(...(watching ? readButtons().map(button => el('span', '', button.label)) : []));
  buttons.hidden = !buttons.children.length;
  const note = !active ? '' : status.presence === 'paused' ? 'Paused from the tray menu: Discord shows nothing until you resume.'
    : status.presence === 'idle' ? 'Codex is idle, so the presence is cleared for now.' : '';
  $('#dc-note').textContent = note;
  $('#dc-note').hidden = !note;
  tickElapsed();
}
function tickElapsed() {
  const time = $('#dc-time');
  const show = settings?.show_elapsed && status.started_at_ms && UsageView.isActive(status.state);
  time.hidden = !show;
  if (!show) return;
  const seconds = Math.max(0, Math.floor((Date.now() - status.started_at_ms) / 1000));
  const pad = value => String(value).padStart(2, '0');
  time.textContent = `${pad(Math.floor(seconds / 3600))}:${pad(Math.floor(seconds / 60) % 60)}:${pad(seconds % 60)} elapsed`;
}

// ---------- Status and usage ----------
function renderStatus(next) {
  status = next;
  const active = UsageView.isActive(status.state);
  $('#side-status').textContent = active ? UsageView.sourceLabel(status.state) : 'Offline';
  $('#side-status').classList.toggle('active', active);
  $('#side-status').title = UsageView.statusLabel(status.state);
  $('#chip-5h').hidden = UsageView.isPro(status.plan);
  $('#chip-plan').disabled = !status.plan;
  $('#chip-plan').title = status.plan ? 'ChatGPT ' + UsageView.planLabel(status.plan) : '';
  $('#plan-hint').hidden = Boolean(status.plan);
  $('#discord-status').textContent = status.discord || 'Not connected';
  const root = $('#usage');
  const limits = UsageView.limits(status.usage, status.plan);
  root.replaceChildren(...(limits.length ? limits.map((limit, i) => UsageView.hero(limit, i ? undefined : status.plan)) : [UsageView.empty()]));
  $('#stat-credits').textContent = /(\d[\d.,]*)/.exec(status.credits || '')?.[1] || '—';
  updatePreview();
}
async function refreshStatus() {
  try { renderStatus(await invoke('load_status')); }
  catch { message.textContent = 'Status temporarily unavailable'; }
}
async function refreshHistory() {
  let hours = [];
  try { hours = await invoke('usage_history'); } catch { return; }
  const days = [...Array(7)].map((_, i) => {
    const date = new Date();
    date.setHours(0, 0, 0, 0);
    date.setDate(date.getDate() - (6 - i));
    return { key: date.toDateString(), name: date.toLocaleDateString(undefined, { weekday: 'short' }), used: 0 };
  });
  for (const [hour, used] of hours) {
    const day = days.find(item => item.key === new Date(hour * 3600000).toDateString());
    if (day) day.used += used;
  }
  const max = Math.max(1, ...days.map(day => day.used));
  const chart = $('#chart');
  chart.replaceChildren(...days.map((day, i) => {
    const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
    const height = Math.max(2, (day.used / max) * 68);
    rect.setAttribute('x', String(i * 40 + 5));
    rect.setAttribute('y', String(72 - height));
    rect.setAttribute('width', '30');
    rect.setAttribute('height', String(height));
    rect.setAttribute('rx', '3');
    if (i === 6) rect.setAttribute('class', 'today');
    const title = document.createElementNS('http://www.w3.org/2000/svg', 'title');
    title.textContent = `${day.name}: ${Math.round(day.used)}% of the weekly limit`;
    rect.append(title);
    return rect;
  }));
  $('#chart-days').replaceChildren(...days.map(day => el('span', '', day.name)));
  $('#stat-today').textContent = hours.length ? Math.round(days[6].used) + '%' : '—';
  $('#stat-week').textContent = hours.length ? Math.round(days.reduce((sum, day) => sum + day.used, 0)) + '%' : '—';
  $('#chart-note').hidden = hours.length > 0;
}
async function refreshGeneral() {
  try {
    const snapshot = await invoke('tray_snapshot');
    $('#startup-label').textContent = snapshot.startup_label;
    $('#startup').setAttribute('aria-checked', String(snapshot.startup_enabled));
    const info = await invoke('app_info');
    $('#version').textContent = 'Codex RPC v' + info.version;
    $('#about-version').textContent = `Version ${info.version} · Discord Rich Presence for OpenAI Codex · MIT license`;
    $('#update-link').hidden = !info.update;
    $('#update-link').textContent = info.update ? `v${info.update} available` : '';
  } catch { /* shown again on the next poll */ }
}

// ---------- Navigation and general actions ----------
$$('.nav').forEach(nav => nav.addEventListener('click', () => {
  $$('.nav').forEach(other => other === nav ? other.setAttribute('aria-current', 'page') : other.removeAttribute('aria-current'));
  $$('[data-panel]').forEach(panel => { panel.hidden = panel.dataset.panel !== nav.dataset.tab; });
  $('.content').scrollTop = 0;
  if (nav.dataset.tab === 'usage') refreshHistory();
  if (nav.dataset.tab === 'general') refreshGeneral();
}));
$('#startup').addEventListener('click', async () => {
  try { await invoke('toggle_startup'); } catch (error) { message.textContent = String(error); }
  refreshGeneral();
});
$('#reconnect').addEventListener('click', async () => { await invoke('reconnect_discord'); message.textContent = 'Reconnecting to Discord…'; });
$('#open-data').addEventListener('click', () => invoke('open_data_folder').catch(error => { message.textContent = String(error); }));
$('#update-link').addEventListener('click', () => invoke('open_release_page'));
$$('[data-link]').forEach(button => button.addEventListener('click', () => invoke('open_link', { kind: button.dataset.link }).catch(error => { message.textContent = String(error); })));

function applyTheme(theme) {
  const safe = Theme.apply(['dark', 'system', 'light'].includes(theme) ? theme : Theme.stored());
  localStorage.setItem(Theme.key, safe);
  $$('[data-theme-option]').forEach(button => button.setAttribute('aria-pressed', String(button.dataset.themeOption === safe)));
}
$$('[data-theme-option]').forEach(button => button.addEventListener('click', () => applyTheme(button.dataset.themeOption)));
matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => applyTheme());

async function closeSettings() { await save(); await invoke('close_settings'); }
$('#close').addEventListener('click', closeSettings);
$('#titlebar-close').addEventListener('click', closeSettings);
$('#titlebar-minimize').addEventListener('click', () => appWindow.minimize());
window.addEventListener('keydown', event => { if (event.key === 'Escape') closeSettings(); });
// The tray can change the privacy toggle while this window stays open in the background.
window.addEventListener('focus', async () => {
  if (loading || !settings) return;
  try {
    const fresh = await invoke('load_settings');
    settings.hide_model = fresh.hide_model;
    $('.switch[data-key="hide_model"]').setAttribute('aria-checked', String(fresh.hide_model));
    updatePreview();
  } catch { /* keep the local copy */ }
  refreshGeneral();
});

(async () => {
  applyTheme();
  try {
    await invoke('start_daemon');
    settings = await invoke('load_settings');
    writeForm();
    await refreshStatus();
  } catch (error) {
    message.textContent = String(error);
  } finally {
    loading = false;
  }
  refreshGeneral();
  refreshHistory();
  setInterval(refreshStatus, 2000);
  setInterval(refreshHistory, 60000);
  setInterval(tickElapsed, 1000);
})();
