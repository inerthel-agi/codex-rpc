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

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
window.el = el;

// Native progress values work with the app's strict Content Security Policy.
window.UsageView = {
  isActive(state) {
    return Boolean(state) && state !== 'Codex: Off';
  },
  sourceLabel(state) {
    return { 'Codex: CLI/Desktop': 'CLI + Desktop', 'Codex: CLI': 'CLI', 'Codex: Desktop': 'Desktop' }[state] || 'Offline';
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
    // Pro tiers are named after the 100/200/500 switch on the pricing page.
    const known = {
      prolite: 'Pro 100',
      pro: 'Pro 200',
      promax: 'Pro 500',
      plus: 'Plus',
      go: 'Go',
      free: 'Free',
      team: 'Business',
      business: 'Business',
      enterprise: 'Enterprise',
      edu: 'Edu',
    };
    if (known[value]) return known[value];
    if (value.startsWith('pro')) return 'Pro';
    // No plan means an API-key sign-in, or usage not loaded yet.
    return value ? value.charAt(0).toUpperCase() + value.slice(1) : 'API key / unknown';
  },
  isPro(plan) {
    return (plan || '').toLowerCase().startsWith('pro');
  },
  /** Limits the plan exposes, with placeholders for the ones not reported yet. */
  limits(entries = [], plan = '') {
    const pro = this.isPro(plan);
    const plus = (plan || '').toLowerCase() === 'plus';
    const known = entries.filter(entry => ['5h', 'week'].includes(entry.label.toLowerCase()));
    const names = pro ? ['week'] : plus ? ['5h', 'week'] : known.map(entry => entry.label.toLowerCase());
    return [...new Set(names)].map(name => {
      const entry = known.find(item => item.label.toLowerCase() === name);
      const available = Boolean(entry) && Number.isFinite(entry.percent);
      const percent = available ? Math.max(0, Math.min(100, entry.percent)) : 0;
      return {
        name,
        title: name === '5h' ? '5-hour limit' : 'Weekly limit',
        short: name === '5h' ? '5h' : 'Week',
        available,
        percent,
        resetsAt: entry?.resets_at_ms || null,
      };
    });
  },
  level(limit) {
    if (!limit.available) return 'unavailable';
    return limit.percent <= 10 ? 'critical' : limit.percent <= 25 ? 'low' : '';
  },
  duration(ms) {
    const minutes = Math.max(1, Math.floor(ms / 60000));
    const days = Math.floor(minutes / 1440), hours = Math.floor(minutes / 60) % 24, mins = minutes % 60;
    return days ? `${days}d ${hours}h` : hours ? `${hours}h ${mins}m` : `${mins}m`;
  },
  resetIn(limit) {
    return limit.resetsAt && limit.resetsAt > Date.now() ? 'Resets in ' + this.duration(limit.resetsAt - Date.now()) : '';
  },
  resetDate(limit) {
    if (!limit.resetsAt) return '';
    return new Date(limit.resetsAt).toLocaleString(undefined, { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
  },
  /** Big percentage, progress bar and reset line for one limit. */
  hero(limit, plan) {
    const root = el('div', 'hero');
    const top = el('div', 'hero-top');
    const figure = el('div', 'hero-figure');
    figure.append(el('b', '', limit.available ? Math.round(limit.percent) + '%' : '—'), el('span', '', limit.available ? `of ${limit.title.toLowerCase()} left` : `${limit.title} unavailable`));
    top.append(figure);
    if (plan !== undefined) top.append(el('span', 'plan-chip', this.planLabel(plan)));
    const progress = el('progress', this.level(limit));
    progress.max = 100;
    progress.value = limit.percent;
    progress.setAttribute('aria-label', limit.title + ' remaining');
    if (!limit.available) progress.setAttribute('aria-valuetext', 'Unavailable');
    const meta = el('div', 'hero-meta');
    meta.append(el('span', '', this.resetIn(limit)), el('span', '', this.resetDate(limit)));
    root.append(top, progress);
    if (limit.resetsAt) root.append(meta);
    return root;
  },
  empty() {
    return el('p', 'usage-empty', 'Waiting for Codex usage…');
  },
  /** Model, effort and speed from the status line, e.g. ["GPT-6-Astra", "Extra High", "Fast"]. */
  splitModel(parts = []) {
    const effortRe = /^(minimal|low|medium|high|extra high|max|ultra)$/i;
    const speedRe = /^(fast|standard)$/i;
    return {
      model: parts.find((part, i) => i === 0 && !effortRe.test(part) && !speedRe.test(part)) || '',
      effort: parts.find(part => effortRe.test(part)) || '',
      speed: parts.find(part => speedRe.test(part)) || '',
    };
  },
};
