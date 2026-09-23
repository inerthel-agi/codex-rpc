const invoke = window.__TAURI__.core.invoke;
const appWindow = window.__TAURI__.window.getCurrentWindow();
const presets = {
  codex: ['Open Codex', 'https://chatgpt.com/codex'],
  usage: ['Usage', 'https://chatgpt.com/codex/settings/analytics'],
  repo: ['GitHub repo', 'https://github.com/inerthel-agi/codex-rpc'],
};
const mode = document.querySelector('#mode');
const labels = [document.querySelector('#label0'), document.querySelector('#label1')];
const urls = [document.querySelector('#url0'), document.querySelector('#url1')];
const toggles = {
  show_primary_usage: document.querySelector('#usage-5h-toggle'),
  show_weekly_usage: document.querySelector('#usage-week-toggle'),
  show_effort: document.querySelector('#effort-toggle'),
  show_fast_mode: document.querySelector('#fast-mode-toggle'),
  show_credits: document.querySelector('#credits-toggle'),
};
const message = document.querySelector('#message');
let loading = true;
let timer;
let snapshot = { status_line: 'Codex: Off', usage: [], plan: '' };

function readForm() {
  return {
    mode: mode.value,
    buttons: labels.map((label, i) => ({ label: label.value.trim(), url: urls[i].value.trim() }))
      .filter(button => button.label && button.url),
    ...Object.fromEntries(Object.entries(toggles).map(([key, input]) => [key, input.checked])),
  };
}
function writeForm(settings) {
  mode.value = settings.mode || 'playing';
  for (const [key, input] of Object.entries(toggles)) {
    input.checked = settings[key] !== false && !(settings.show_usage === false && /usage|credits/.test(key));
  }
  labels.forEach((label, i) => { label.value = settings.buttons?.[i]?.label || ''; urls[i].value = settings.buttons?.[i]?.url || ''; });
  syncButtons();
}
function syncButtons() {
  const enabled = mode.value === 'watching';
  for (const input of [...labels, ...urls, ...document.querySelectorAll('[data-preset], #clear')]) input.disabled = !enabled;
}
function renderStatus(status) {
  snapshot = status;
  const [state = 'Codex: Off'] = status.status_line.split('|');
  document.querySelector('#status').textContent = state === 'Codex: Off' ? 'Codex is not running' : state;
  document.querySelector('#status').classList.toggle('active', state !== 'Codex: Off');
  document.querySelector('#plan').textContent = UsageView.planLabel(status.plan);
  const pro = (status.plan || '').toLowerCase().startsWith('pro');
  document.querySelector('#usage-5h-option').hidden = pro;
  document.querySelector('#plan-hint').textContent = pro ? 'Pro · Weekly allowance' : status.plan === 'plus' ? 'Plus · 5-hour and weekly allowances' : 'Limits update automatically from your Codex account.';
  UsageView.render(document.querySelector('#usage'), status.usage, status.plan);
  updatePreview();
}
function updatePreview() {
  const settings = readForm();
  const [state, rawModel = '', rawUsage = ''] = snapshot.status_line.split('|');
  const active = state !== 'Codex: Off';
  const model = rawModel.split(' - ').filter((part, i) => {
    if (!i) return true;
    if (/^(minimal|low|medium|high|extra high|max|ultra)$/i.test(part)) return settings.show_effort;
    if (/^(fast|standard)$/i.test(part)) return settings.show_fast_mode;
    return true;
  }).join(' · ');
  const usage = (snapshot.usage || []).filter(entry => {
    const label = entry.label.toLowerCase();
    return label === 'week' ? settings.show_weekly_usage : label === '5h' && settings.show_primary_usage && !(snapshot.plan || '').startsWith('pro');
  }).map(entry => entry.label + ' ' + Math.round(entry.percent) + '%');
  const credits = rawUsage.split('/').find(part => part.trim().startsWith('credits'));
  if (settings.show_credits && credits) usage.push(credits.trim());
  const activity = { playing: 'Playing', watching: 'Watching', listening: 'Listening', competing: 'Competing' }[mode.value];
  document.querySelector('#preview-activity').textContent = active ? activity + ' Codex' : 'Presence paused';
  document.querySelector('#preview-details').textContent = active ? (state.includes('CLI') ? 'Coding with Codex CLI' : 'Using Codex') : 'Resumes when Codex is running';
  document.querySelector('#preview-state').textContent = active ? [model, ...usage].filter(Boolean).join(' · ') : '—';
  const buttons = document.querySelector('#preview-buttons');
  buttons.replaceChildren();
  if (active && mode.value === 'watching') for (const button of settings.buttons) {
    const item = document.createElement('span'); item.textContent = button.label; buttons.append(item);
  }
}
async function save() {
  clearTimeout(timer);
  if (loading) return;
  try { await invoke('save_settings', { settings: readForm() }); message.textContent = 'Saved'; }
  catch (error) { message.textContent = 'Could not save: ' + String(error); }
}
function scheduleSave() {
  updatePreview();
  if (loading) return;
  clearTimeout(timer); message.textContent = 'Saving…'; timer = setTimeout(save, 300);
}
mode.addEventListener('change', () => { syncButtons(); scheduleSave(); });
for (const input of Object.values(toggles)) input.addEventListener('change', scheduleSave);
for (const input of [...labels, ...urls]) input.addEventListener('input', scheduleSave);
document.querySelector('#apply').addEventListener('click', save);
document.querySelector('#clear').addEventListener('click', () => { for (const input of [...labels, ...urls]) input.value = ''; scheduleSave(); });
document.querySelectorAll('[data-preset]').forEach(button => button.addEventListener('click', () => {
  const slot = labels[0].value.trim() ? 1 : 0;
  [labels[slot].value, urls[slot].value] = presets[button.dataset.preset]; scheduleSave();
}));
async function closeSettings() { await save(); await invoke('close_settings'); }
document.querySelector('#close').addEventListener('click', closeSettings);
document.querySelector('#titlebar-close').addEventListener('click', closeSettings);
document.querySelector('#titlebar-minimize').addEventListener('click', () => appWindow.minimize());
document.querySelector('#titlebar-maximize').addEventListener('click', () => appWindow.toggleMaximize());
function applyTheme(theme) {
  const safe = ['dark', 'system', 'light'].includes(theme) ? theme : 'dark';
  document.body.dataset.theme = safe === 'system' ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : safe;
  localStorage.setItem('codex-rpc-theme', safe);
  document.querySelectorAll('[data-theme-option]').forEach(button => {
    button.classList.toggle('active', button.dataset.themeOption === safe);
    button.setAttribute('aria-pressed', String(button.dataset.themeOption === safe));
  });
}
document.querySelectorAll('[data-theme-option]').forEach(button => button.addEventListener('click', () => applyTheme(button.dataset.themeOption)));
matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => applyTheme(localStorage.getItem('codex-rpc-theme')));
async function refresh() { try { renderStatus(await invoke('load_status')); } catch { message.textContent = 'Status temporarily unavailable'; } }
(async () => {
  applyTheme(localStorage.getItem('codex-rpc-theme'));
  try { await invoke('start_daemon'); writeForm(await invoke('load_settings')); await refresh(); }
  catch (error) { message.textContent = String(error); }
  finally { loading = false; }
  setInterval(refresh, 2000);
})();
