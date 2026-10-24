// Layout metadata only. Restoring a group must never restore page access or
// infer ownership from its name/members. Chrome clears session storage on
// extension reload/update and browser restart, when group IDs may change.
export class TaskTabGroups {
  constructor(api) {
    this.api = api;
    this.records = new Map();
    this.ready = this.restore();
  }

  async restore() {
    const stored = await this.api.storage?.session?.get('taskGroups').catch(() => ({}));
    for (const [key, record] of Object.entries(stored?.taskGroups ?? {}).slice(-128)) {
      if (!validKey(key) || !Number.isInteger(record?.groupId) || record.groupId < 0 || !Array.isArray(record.tabIds)) continue;
      const tabIds = record.tabIds.filter(id => Number.isInteger(id) && id > 0).slice(-256);
      if (tabIds.length) this.records.set(key, { groupId: record.groupId, tabIds });
    }
  }

  async save() {
    // Quota/storage failure does not invalidate a successfully created tab.
    await this.api.storage?.session?.set({ taskGroups: Object.fromEntries(this.records) }).catch(() => {});
  }

  async existing(key) {
    const record = this.records.get(key);
    if (!record) return;
    try {
      const group = await this.api.tabGroups.get(record.groupId);
      const members = await this.api.tabs.query({ groupId: group.id });
      record.tabIds = record.tabIds.filter(id => members.some(tab => tab.id === id));
      // A group containing only user-added tabs is no longer our workspace.
      if (record.tabIds.length) return group;
    } catch { /* Removed groups/windows are recreated on the next open. */ }
    this.records.delete(key);
  }

  async open(key, url) {
    if (!validKey(key)) throw new Error('Invalid Chrome task key.');
    if (!this.api.tabGroups) throw new Error('Reload the Open Harness Chrome extension to enable task tab groups.');
    await this.ready;
    const existing = await this.existing(key);
    let tab;
    if (existing) tab = await this.api.tabs.create({ url, active: false, windowId: existing.windowId });
    else {
      // A background browser process can have no normal windows (for example
      // after Windows --no-startup-window). Do not require the user to focus or
      // open one, and never reuse an incognito or popup window.
      const windows = (await this.api.windows.getAll({ windowTypes: ['normal'] })).filter(window => !window.incognito);
      const target = windows.find(window => window.focused) ?? windows[0];
      if (target) tab = await this.api.tabs.create({ url, active: false, windowId: target.id });
      else {
        const window = await this.api.windows.create({ url, type: 'normal', focused: false, incognito: false });
        tab = window?.tabs?.[0];
        if (!Number.isInteger(tab?.id) || tab.incognito || tab.windowId !== window.id) throw new Error(`Created Chrome window ${window?.id ?? 'unknown'}, but its tab could not be identified. Inspect Chrome before retrying; do not repeat chrome_open.`);
        // Its single tab is active within an unfocused window. Preserve that
        // state: close_owned must continue to protect active tabs from closing.
      }
    }
    try {
      const groupId = await this.api.tabs.group({ tabIds: [tab.id], ...(existing ? { groupId: existing.id } : {}) });
      if (!existing) await this.api.tabGroups.update(groupId, { title: `Open Harness · ${key.slice(0, 6)}`, color: 'green' });
      const record = existing ? this.records.get(key) : { groupId, tabIds: [] };
      record.tabIds = [...record.tabIds, tab.id].slice(-256);
      this.records.delete(key); this.records.set(key, record);
      while (this.records.size > 128) this.records.delete(this.records.keys().next().value);
      await this.save();
      return { ...tab, groupId };
    } catch (error) {
      // Roll back only the tab just created, never a group or its other members.
      // If the user already took focus/navigated it, leave it for review.
      let retained = false;
      try {
        const current = await this.api.tabs.get(tab.id);
        if (!current.active && (current.pendingUrl ?? current.url) === url) await this.api.tabs.remove(tab.id);
        else retained = true;
      } catch { retained = true; }
      throw new Error(`Could not organize the new Chrome tab: ${error.message}.${retained ? ` Tab ${tab.id} may remain open; inspect before retrying.` : ' The newly created tab was closed.'}`);
    }
  }

  async forget(tabId) {
    await this.ready;
    for (const [key, record] of this.records) {
      record.tabIds = record.tabIds.filter(id => id !== tabId);
      if (!record.tabIds.length) this.records.delete(key);
    }
    await this.save();
  }
}

function validKey(key) { return typeof key === 'string' && /^(?:[a-f\d]{64}|[a-f\d-]{36})$/.test(key); }
