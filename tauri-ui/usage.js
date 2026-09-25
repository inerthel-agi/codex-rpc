// Shared by the settings window and the tray menu.
window.Theme = {
  key: 'codex-rpc-theme',
  stored() {
    const value = localStorage.getItem(this.key);
    return ['dark', 'system', 'light'].includes(value) ? value : 'dark';
  },
  apply(choice = this.stored()) {
    document.body.dataset.theme = choice === 'system'
      ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark')
      : choice;
    return choice;
  },
};

// Native progress values work with the app's strict Content Security Policy.
window.UsageView = {
  isActive(state) {
    return Boolean(state) && state !== 'Codex: Off';
  },
  statusLabel(state) {
    return {
      'Codex: CLI/Desktop': 'Connected to Codex CLI and Desktop',
      'Codex: CLI': 'Connected to Codex CLI',
      'Codex: Desktop': 'Connected to Codex Desktop',
    }[state] || 'Codex is not running';
  },
  planLabel(plan) {
    const value = (plan || '').toLowerCase();
    if (value.startsWith('pro')) return 'Pro';
    if (value === 'plus') return 'Plus';
    return value ? value.charAt(0).toUpperCase() + value.slice(1) : 'Subscription';
  },
  render(root, entries = [], plan = '') {
    const pro = (plan || '').toLowerCase().startsWith('pro');
    const plus = (plan || '').toLowerCase() === 'plus';
    const limits = entries.filter(entry => ['5h', 'week'].includes(entry.label.toLowerCase()));
    const names = pro ? ['week'] : plus ? ['5h', 'week'] : limits.map(entry => entry.label.toLowerCase());
    root.replaceChildren();
    if (!names.length) {
      const empty = document.createElement('p');
      empty.className = 'usage-empty';
      empty.textContent = 'Waiting for Codex usage…';
      root.append(empty);
      return;
    }
    for (const name of [...new Set(names)]) {
      const entry = limits.find(item => item.label.toLowerCase() === name);
      const available = entry && Number.isFinite(entry.percent);
      const percent = available ? Math.max(0, Math.min(100, entry.percent)) : 0;
      const row = document.createElement('div');
      row.className = 'usage-row';
      const label = document.createElement('span');
      label.className = 'usage-label';
      label.textContent = name === '5h' ? '5-hour limit' : 'Weekly limit';
      const value = document.createElement('span');
      value.className = 'usage-value';
      value.textContent = available ? Math.round(percent) + '% remaining' : 'Unavailable';
      const progress = document.createElement('progress');
      progress.max = 100;
      progress.value = percent;
      progress.setAttribute('aria-label', label.textContent + ' remaining');
      if (!available) progress.setAttribute('aria-valuetext', 'Unavailable');
      progress.className = !available ? 'unavailable' : percent <= 10 ? 'critical' : percent <= 25 ? 'low' : '';
      row.append(label, value, progress);
      root.append(row);
    }
  },
};
