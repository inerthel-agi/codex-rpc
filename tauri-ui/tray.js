const invoke = window.__TAURI__.core.invoke;
const $ = selector => document.querySelector(selector);
let signature = '';
let previousHeight = 0;
let snapshot = null;

function discordUser(discord) {
  return /Connected \((.+)\)$/.exec(discord || '')?.[1] || null;
}
function isPaused(s) {
  return s.paused_until_ms > Date.now();
}
function resumesIn(s) {
  return s.paused_until_ms >= Number.MAX_SAFE_INTEGER ? '' : 'resumes in ' + UsageView.duration(s.paused_until_ms - Date.now());
}

function renderUsage(limits, s) {
  const root = $('#usage');
  const mini = $('#mini');
  root.replaceChildren();
  mini.replaceChildren();
  $('#banner').hidden = true;
  if (!limits.length) {
    root.append(UsageView.empty());
    mini.hidden = true;
    return;
  }
  // The most constrained limit leads; the others move to the compact line.
  const hero = limits.reduce((best, limit) => limit.available && (!best.available || limit.percent < best.percent) ? limit : best);
  root.append(UsageView.hero(hero, s.plan));
  const level = UsageView.level(hero);
  if (level === 'low' || level === 'critical') {
    const reset = UsageView.resetIn(hero);
    $('#banner').className = 'banner ' + level;
    $('#banner-text').textContent = (level === 'critical'
      ? `Only ${Math.round(hero.percent)}% of your ${hero.title.toLowerCase()} left.`
      : `${hero.title} is getting low.`) + (reset ? ' ' + reset + '.' : '');
    $('#banner').hidden = false;
  }
  const facts = limits.filter(limit => limit !== hero).map(limit => [limit.short, limit.available ? Math.round(limit.percent) + '%' : '—']);
  const credits = /(\d[\d.,]*)/.exec(s.credits || '')?.[1];
  if (credits) facts.push(['Credits', credits]);
  const { speed } = UsageView.splitModel(s.model_parts);
  if (speed) facts.push(['Mode', speed]);
  for (const [label, value] of facts) {
    const item = el('span', '', label + ' ');
    item.append(el('b', '', value));
    mini.append(item);
  }
  mini.hidden = !facts.length;
}

function renderDiscord(s, active, paused) {
  const text = $('#discord-text');
  const action = $('#discord-action');
  const user = discordUser(s.discord);
  const bold = value => el('b', '', value);
  text.replaceChildren();
  action.hidden = true;
  if (!user) {
    text.append('Discord not connected');
    action.textContent = 'Reconnect';
    action.dataset.action = 'reconnect';
    action.hidden = false;
  } else if (!active) {
    text.append('Connected as ', bold(user), ' · waiting for Codex');
  } else if (paused) {
    const until = resumesIn(s);
    text.append('Hidden from ', bold(user), until ? ' · ' + until : '');
    action.textContent = 'Resume';
    action.dataset.action = 'resume';
    action.hidden = false;
  } else if (s.presence === 'idle') {
    text.append('Cleared from ', bold(user), ' · Codex is idle');
  } else {
    const since = s.started_at_ms ? ' · ' + UsageView.duration(Date.now() - s.started_at_ms) : '';
    text.append('Showing on ', bold(user), since);
  }
}

function render(s) {
  snapshot = s;
  const active = UsageView.isActive(s.state);
  const paused = isPaused(s);
  const status = UsageView.statusLabel(s.state);
  $('#model-line').textContent = s.model_parts.join(' · ') || status;

  const presence = !active ? 'off' : paused ? 'paused' : s.presence;
  const pill = $('#state-pill');
  pill.className = 'pill ' + presence;
  pill.textContent = { off: 'Offline', paused: 'Paused', idle: 'Idle' }[presence] || UsageView.sourceLabel(s.state);
  pill.title = status;

  const limits = UsageView.limits(s.usage, s.plan);
  // Minute granularity keeps the reset countdown fresh without rebuilding every tick.
  const next = JSON.stringify([limits, s.plan, s.credits, s.model_parts, Math.floor(Date.now() / 60000)]);
  if (next !== signature) { renderUsage(limits, s); signature = next; }

  $('#item-pause').setAttribute('aria-checked', String(paused));
  $('#item-pause .item-label').textContent = paused ? 'Presence paused' + (resumesIn(s) ? ' · ' + resumesIn(s) : '') : 'Pause Discord presence';
  $('#snooze').hidden = paused;
  $('#item-private').setAttribute('aria-checked', String(s.hide_model));
  $('#item-update').hidden = !s.update;
  $('#update-label').textContent = 'Update available · v' + s.update;
  renderDiscord(s, active, paused);
}

async function refresh() {
  try {
    Theme.apply();
    render(await invoke('tray_snapshot'));
    const height = $('#card').offsetHeight + 20;
    if (height !== previousHeight) { await invoke('fit_tray', { height }); previousHeight = height; }
  } catch {
    $('#discord-text').textContent = 'Status temporarily unavailable';
  }
}

async function run(command, args) {
  try { await invoke(command, args); } finally { await refresh(); }
}

$('#item-pause').addEventListener('click', () => run(snapshot && isPaused(snapshot) ? 'resume_presence' : 'pause_presence', { minutes: 0 }));
document.querySelectorAll('#snooze button').forEach(button => button.addEventListener('click', () => run('pause_presence', { minutes: Number(button.dataset.minutes) })));
$('#item-private').addEventListener('click', () => run('set_hide_model', { enabled: !snapshot?.hide_model }));
$('#item-codex').addEventListener('click', () => { invoke('hide_tray'); invoke('open_codex'); });
$('#item-cli').addEventListener('click', () => { invoke('hide_tray'); invoke('open_codex_cli'); });
$('#item-settings').addEventListener('click', () => invoke('open_settings_from_tray'));
$('#item-update').addEventListener('click', () => { invoke('hide_tray'); invoke('open_release_page'); });
$('#item-quit').addEventListener('click', () => invoke('quit_app'));
$('#discord-action').addEventListener('click', event => run(event.currentTarget.dataset.action === 'resume' ? 'resume_presence' : 'reconnect_discord'));
window.addEventListener('keydown', event => {
  if (event.key === 'Escape') invoke('hide_tray');
  if (event.key === ',' && (event.ctrlKey || event.metaKey)) invoke('open_settings_from_tray');
});
window.addEventListener('contextmenu', event => event.preventDefault());
refresh();
setInterval(refresh, 2000);
