const invoke = window.__TAURI__.core.invoke;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const presets = {
  codex: ['Open Codex', 'https://chatgpt.com/codex'],
  usage: ['Usage', 'https://chatgpt.com/codex/settings/analytics'],
  repo: ['GitHub repo', 'https://github.com/inerthel-agi/codex-rpc'],
};

const fields = {
  mode: document.querySelector('#mode'),
  labels: [document.querySelector('#label0'), document.querySelector('#label1')],
  urls: [document.querySelector('#url0'), document.querySelector('#url1')],
  usage5hToggle: document.querySelector('#usage-5h-toggle'),
  usageWeekToggle: document.querySelector('#usage-week-toggle'),
  usageSpark5hToggle: document.querySelector('#usage-spark-5h-toggle'),
  usageSparkWeekToggle: document.querySelector('#usage-spark-week-toggle'),
  effortToggle: document.querySelector('#effort-toggle'),
  fastModeToggle: document.querySelector('#fast-mode-toggle'),
  creditsToggle: document.querySelector('#credits-toggle'),
  costToggle: document.querySelector('#cost-toggle'),
  costTotalToggle: document.querySelector('#cost-total-toggle'),
  projectTokensToggle: document.querySelector('#project-tokens-toggle'),
  allTokensToggle: document.querySelector('#all-tokens-toggle'),
  alwaysOnToggle: document.querySelector('#always-on-toggle'),
  status: document.querySelector('#status'),
  message: document.querySelector('#message'),
  previewActivity: document.querySelector('#preview-activity'),
  previewDetails: document.querySelector('#preview-details'),
  previewState: document.querySelector('#preview-state'),
  previewButtons: document.querySelector('#preview-buttons'),
  themeButtons: [...document.querySelectorAll('[data-theme-option]')],
};

let loading = true;
let saveTimer = null;
let lastStatusLine = 'Codex: Off';

function readForm() {
  const buttons = [];
  for (let i = 0; i < 2; i += 1) {
    const label = fields.labels[i].value.trim();
    const url = fields.urls[i].value.trim();
    if (label && url) buttons.push({ label, url });
  }
  return {
    mode: fields.mode.value,
    buttons,
    show_primary_usage: fields.usage5hToggle.dataset.enabled === 'true',
    show_weekly_usage: fields.usageWeekToggle.dataset.enabled === 'true',
    show_spark_primary_usage: fields.usageSpark5hToggle.dataset.enabled === 'true',
    show_spark_weekly_usage: fields.usageSparkWeekToggle.dataset.enabled === 'true',
    show_effort: fields.effortToggle.dataset.enabled === 'true',
    show_fast_mode: fields.fastModeToggle.dataset.enabled === 'true',
    show_credits: fields.creditsToggle.dataset.enabled === 'true',
    show_cost: fields.costToggle.dataset.enabled === 'true',
    show_cost_total: fields.costTotalToggle.dataset.enabled === 'true',
    show_project_tokens: fields.projectTokensToggle.dataset.enabled === 'true',
    show_all_tokens: fields.allTokensToggle.dataset.enabled === 'true',
    always_on: fields.alwaysOnToggle.dataset.enabled === 'true',
  };
}

function writeForm(settings) {
  fields.mode.value = settings.mode || 'playing';
  const legacyHidden = settings.show_usage === false;
  syncUsageToggle(fields.usage5hToggle, !legacyHidden && settings.show_primary_usage !== false, '5h');
  syncUsageToggle(fields.usageWeekToggle, !legacyHidden && settings.show_weekly_usage !== false, 'Week');
  syncUsageToggle(
    fields.usageSpark5hToggle,
    !legacyHidden && settings.show_spark_primary_usage !== false,
    'Spark 5h',
  );
  syncUsageToggle(
    fields.usageSparkWeekToggle,
    !legacyHidden && settings.show_spark_weekly_usage !== false,
    'Spark week',
  );
  syncUsageToggle(fields.effortToggle, settings.show_effort !== false, 'Effort');
  syncUsageToggle(fields.fastModeToggle, settings.show_fast_mode !== false, 'Fast mode');
  syncUsageToggle(fields.creditsToggle, !legacyHidden && settings.show_credits !== false, 'Credits');
  // Enforce the XOR pairing on load (project wins if a legacy config has both on).
  const showCost = settings.show_cost === true;
  const showProjectTokens = settings.show_project_tokens === true;
  syncUsageToggle(fields.costToggle, showCost, 'Project cost');
  syncUsageToggle(fields.costTotalToggle, settings.show_cost_total === true && !showCost, 'All cost');
  syncUsageToggle(fields.projectTokensToggle, showProjectTokens, 'Project tokens');
  syncUsageToggle(fields.allTokensToggle, settings.show_all_tokens === true && !showProjectTokens, 'All tokens');
  syncAlwaysOnToggle(fields.alwaysOnToggle, settings.always_on === true);
  for (let i = 0; i < 2; i += 1) {
    fields.labels[i].value = settings.buttons?.[i]?.label || '';
    fields.urls[i].value = settings.buttons?.[i]?.url || '';
  }
  syncButtons();
  updatePreview();
}

function syncUsageToggle(button, enabled, label) {
  button.dataset.enabled = String(enabled);
  button.textContent = `${label} ${enabled ? 'on' : 'off'}`;
  button.classList.toggle('active', enabled);
  button.setAttribute('aria-pressed', String(enabled));
}

function syncAlwaysOnToggle(button, enabled) {
  button.dataset.enabled = String(enabled);
  button.textContent = enabled ? 'On' : 'Off';
  button.classList.toggle('active', enabled);
  button.setAttribute('aria-pressed', String(enabled));
}

// Cost/Tokens come in mutually-exclusive pairs (project XOR total): enabling one
// disables its partner so the Discord line shows a single value.
function toggleCostOption(button, label, partner, partnerLabel) {
  const enabled = button.dataset.enabled !== 'true';
  syncUsageToggle(button, enabled, label);
  if (enabled) syncUsageToggle(partner, false, partnerLabel);
  scheduleSave();
}

function syncButtons() {
  const enabled = fields.mode.value === 'watching';
  for (const input of [...fields.labels, ...fields.urls]) input.disabled = !enabled;
  document.querySelectorAll('.presets button, #clear').forEach((button) => {
    button.disabled = !enabled;
  });
}

async function load() {
  try {
    applyTheme(localStorage.getItem('codex-rpc-theme') || 'dark');
    initCollapsible();
    await invoke('start_daemon');
    writeForm(await invoke('load_settings'));
    const status = await invoke('load_status');
    setStatus(status.status_line);
    loading = false;
  } catch (error) {
    fields.message.textContent = String(error);
    loading = false;
  }
}

function initCollapsible() {
  document.querySelectorAll('.panel').forEach((panel) => {
    const legend = panel.querySelector('.legend');
    if (!legend) return;
    if (!panel.querySelector('.chevron')) return;

    const panelId = legend.textContent.trim().toLowerCase();

    legend.addEventListener('click', () => {
      panel.classList.toggle('collapsed');
      localStorage.setItem(`panel-collapsed-${panelId}`, panel.classList.contains('collapsed'));
    });

    const isCollapsed = localStorage.getItem(`panel-collapsed-${panelId}`) === 'true';
    if (isCollapsed) {
      panel.classList.add('collapsed');
    }
  });
}

function formatStatus(value) {
  return (value || 'Codex: Off').split('|').filter(Boolean).join(' | ');
}

function parseStatus(value) {
  const [codex = 'Codex: Off', model = '', usage = '', discord = '', cost = ''] = (value || 'Codex: Off')
    .split('|')
    .map((part) => part.trim());
  return { codex, model, usage, discord, cost };
}

function updatePreview() {
  const status = parseStatus(lastStatusLine);
  const settings = readForm();
  const mode = settings.mode || 'playing';
  const activity = {
    watching: 'Watching Codex',
    listening: 'Listening Codex',
    competing: 'Competing Codex',
    playing: 'Playing Codex',
  }[mode] || 'Playing Codex';
  const modelPart = previewModel(status.model, settings);
  const usageParts = previewUsageParts(status.usage, settings);
  const stateParts = [modelPart, ...usageParts].filter(Boolean);
  const costEnabled =
    settings.show_cost ||
    settings.show_cost_total ||
    settings.show_project_tokens ||
    settings.show_all_tokens;
  if (costEnabled && status.cost) {
    const costText = status.cost.replace(/^Cost:\s*/i, '').trim();
    if (costText) stateParts.push(costText);
  }

  fields.previewActivity.textContent = activity;
  fields.previewDetails.textContent = previewDetails(status.codex, mode);
  fields.previewState.textContent = truncateText(stateParts.join(' - ') || status.codex, 64);
  renderPreviewButtons(mode, settings.buttons);
}

function previewModel(model, settings) {
  if (!model) return '';
  const parts = model.split(' - ').filter(Boolean);
  if (parts.length <= 1) return model;
  return parts
    .filter((part, index) => {
      if (index === 0) return true;
      const lower = part.toLowerCase();
      if (['minimal', 'low', 'medium', 'high', 'extra high'].includes(lower)) {
        return settings.show_effort;
      }
      if (lower === 'fast' || lower === 'standard') {
        return settings.show_fast_mode;
      }
      return true;
    })
    .join(' - ');
}

function renderPreviewButtons(mode, buttons) {
  fields.previewButtons.replaceChildren();
  fields.previewButtons.hidden = mode !== 'watching' || buttons.length === 0;
  if (fields.previewButtons.hidden) return;
  for (const button of buttons.slice(0, 2)) {
    const item = document.createElement('span');
    item.textContent = button.label;
    fields.previewButtons.appendChild(item);
  }
}

function previewDetails(codex, mode) {
  const isWatching = mode === 'watching';
  if (codex.includes('CLI/Desktop')) {
    return isWatching ? 'Watching Codex (CLI + Desktop)' : 'Coding with Codex (CLI + Desktop)';
  }
  if (codex.includes('Desktop')) {
    return isWatching ? 'Watching Codex' : 'Using Codex';
  }
  if (codex.includes('CLI')) {
    return isWatching ? 'Watching Codex CLI' : 'Coding with Codex CLI';
  }
  if (codex.includes('Monitoring')) {
    return isWatching ? 'Watching Codex usage' : 'Monitoring Codex usage';
  }
  return 'No Codex activity';
}

function previewUsageParts(usage, settings) {
  return usage
    .replace(/^Usage:\s*/i, '')
    .split('/')
    .map((part) => part.trim())
    .filter(Boolean)
    .filter((part) => {
      const lower = part.toLowerCase();
      if (lower.startsWith('spark 5h')) return settings.show_spark_primary_usage;
      if (lower.startsWith('spark week') || lower.startsWith('spark wk')) {
        return settings.show_spark_weekly_usage;
      }
      if (lower.startsWith('5h')) return settings.show_primary_usage;
      if (lower.startsWith('week')) return settings.show_weekly_usage;
      if (lower.startsWith('credits')) return settings.show_credits;
      return true;
    })
    .map((part) => part.replace(/\s+left$/i, ''));
}

function truncateText(value, maxLength) {
  return value.length > maxLength ? `${value.slice(0, maxLength - 3)}...` : value;
}

function setStatus(value) {
  lastStatusLine = value || 'Codex: Off';
  fields.status.textContent = formatStatus(lastStatusLine);
  const isActive = lastStatusLine && !lastStatusLine.includes('Codex: Off');
  fields.status.classList.toggle('active', isActive);
  updatePreview();
}

async function save(kind = 'manual') {
  try {
    await invoke('save_settings', { settings: readForm() });
    fields.message.textContent = kind === 'auto' ? 'saved' : 'applied';
    updatePreview();
  } catch (error) {
    fields.message.textContent = String(error);
  }
}

function scheduleSave() {
  if (loading) return;
  clearTimeout(saveTimer);
  fields.message.textContent = 'saving...';
  updatePreview();
  saveTimer = setTimeout(() => save('auto'), 300);
}

document.querySelector('#apply').addEventListener('click', () => save());
document.querySelector('#close').addEventListener('click', () => invoke('close_settings'));
document.querySelector('#titlebar-minimize').addEventListener('click', () => appWindow.minimize());
document.querySelector('#titlebar-maximize').addEventListener('click', () => appWindow.toggleMaximize());
document.querySelector('#titlebar-close').addEventListener('click', () => invoke('close_settings'));
fields.themeButtons.forEach((button) => {
  button.addEventListener('click', () => applyTheme(button.dataset.themeOption));
});
document.querySelector('#clear').addEventListener('click', () => {
  for (const input of [...fields.labels, ...fields.urls]) input.value = '';
  scheduleSave();
});
fields.mode.addEventListener('change', () => {
  syncButtons();
  scheduleSave();
});
fields.usage5hToggle.addEventListener('click', () => {
  syncUsageToggle(fields.usage5hToggle, fields.usage5hToggle.dataset.enabled !== 'true', '5h');
  scheduleSave();
});
fields.usageWeekToggle.addEventListener('click', () => {
  syncUsageToggle(fields.usageWeekToggle, fields.usageWeekToggle.dataset.enabled !== 'true', 'Week');
  scheduleSave();
});
fields.usageSpark5hToggle.addEventListener('click', () => {
  syncUsageToggle(
    fields.usageSpark5hToggle,
    fields.usageSpark5hToggle.dataset.enabled !== 'true',
    'Spark 5h',
  );
  scheduleSave();
});
fields.usageSparkWeekToggle.addEventListener('click', () => {
  syncUsageToggle(
    fields.usageSparkWeekToggle,
    fields.usageSparkWeekToggle.dataset.enabled !== 'true',
    'Spark week',
  );
  scheduleSave();
});
fields.effortToggle.addEventListener('click', () => {
  syncUsageToggle(fields.effortToggle, fields.effortToggle.dataset.enabled !== 'true', 'Effort');
  scheduleSave();
});
fields.fastModeToggle.addEventListener('click', () => {
  syncUsageToggle(fields.fastModeToggle, fields.fastModeToggle.dataset.enabled !== 'true', 'Fast mode');
  scheduleSave();
});
fields.creditsToggle.addEventListener('click', () => {
  syncUsageToggle(fields.creditsToggle, fields.creditsToggle.dataset.enabled !== 'true', 'Credits');
  scheduleSave();
});
fields.costToggle.addEventListener('click', () =>
  toggleCostOption(fields.costToggle, 'Project cost', fields.costTotalToggle, 'All cost'));
fields.costTotalToggle.addEventListener('click', () =>
  toggleCostOption(fields.costTotalToggle, 'All cost', fields.costToggle, 'Project cost'));
fields.projectTokensToggle.addEventListener('click', () =>
  toggleCostOption(fields.projectTokensToggle, 'Project tokens', fields.allTokensToggle, 'All tokens'));
fields.allTokensToggle.addEventListener('click', () =>
  toggleCostOption(fields.allTokensToggle, 'All tokens', fields.projectTokensToggle, 'Project tokens'));
fields.alwaysOnToggle.addEventListener('click', () => {
  syncAlwaysOnToggle(fields.alwaysOnToggle, fields.alwaysOnToggle.dataset.enabled !== 'true');
  scheduleSave();
});
for (const input of [...fields.labels, ...fields.urls]) input.addEventListener('input', scheduleSave);

document.querySelectorAll('[data-preset]').forEach((button) => {
  button.addEventListener('click', () => {
    const [label, url] = presets[button.dataset.preset];
    const slot = fields.labels[0].value.trim() ? 1 : 0;
    fields.labels[slot].value = label;
    fields.urls[slot].value = url;
    scheduleSave();
  });
});

window.addEventListener('DOMContentLoaded', load);
setInterval(async () => {
  try {
    setStatus((await invoke('load_status')).status_line);
  } catch {
    setStatus('Codex: Off');
  }
}, 250);

function applyTheme(theme) {
  const safeTheme = ['dark', 'system', 'light'].includes(theme) ? theme : 'dark';
  const resolved =
    safeTheme === 'system'
      ? window.matchMedia('(prefers-color-scheme: light)').matches
        ? 'light'
        : 'dark'
      : safeTheme;
  document.body.dataset.theme = resolved;
  fields.themeButtons.forEach((button) => {
    const active = button.dataset.themeOption === safeTheme;
    button.classList.toggle('active', active);
    button.setAttribute('aria-pressed', String(active));
  });
  localStorage.setItem('codex-rpc-theme', safeTheme);
}
