const invoke = window.__TAURI__.core.invoke;
let signature = '';
let previousHeight = 0;
function applyTheme() {
  const theme = localStorage.getItem('codex-rpc-theme') || 'dark';
  document.body.dataset.theme = theme === 'system' ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : theme;
}
async function refresh() {
  try {
    applyTheme();
    const snapshot = await invoke('tray_snapshot');
    document.querySelector('#status-dot').classList.toggle('active', /CLI|Desktop/.test(snapshot.state));
    document.querySelector('#status-dot').title = snapshot.state;
    document.querySelector('#model-line').textContent = snapshot.model || snapshot.state;
    document.querySelector('#plan').textContent = UsageView.planLabel(snapshot.plan);
    const next = JSON.stringify([snapshot.usage, snapshot.plan]);
    if (next !== signature) { UsageView.render(document.querySelector('#usage'), snapshot.usage, snapshot.plan); signature = next; }
    document.querySelector('#startup-label').textContent = snapshot.startup_label;
    document.querySelector('#startup-switch').classList.toggle('on', snapshot.startup_enabled);
    document.querySelector('#item-startup').setAttribute('aria-pressed', String(snapshot.startup_enabled));
    document.querySelector('#discord-line').textContent = snapshot.discord || 'Discord: connecting…';
    const height = document.querySelector('#card').offsetHeight + 20;
    if (height !== previousHeight) { await invoke('fit_tray', { height }); previousHeight = height; }
  } catch { document.querySelector('#discord-line').textContent = 'Status temporarily unavailable'; }
}
document.querySelector('#item-settings').addEventListener('click', () => invoke('open_settings_from_tray'));
document.querySelector('#item-startup').addEventListener('click', async () => { await invoke('toggle_startup'); await refresh(); });
document.querySelector('#item-quit').addEventListener('click', () => invoke('quit_app'));
window.addEventListener('keydown', event => { if (event.key === 'Escape') invoke('hide_tray'); });
window.addEventListener('contextmenu', event => event.preventDefault());
refresh();
setInterval(refresh, 2000);
