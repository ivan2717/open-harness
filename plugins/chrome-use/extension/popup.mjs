const status = document.querySelector('#status');
async function request(action, extra = {}) {
  try {
    const response = await chrome.runtime.sendMessage({ action, ...extra });
    if (response.error) throw new Error(response.error);
    const state = response.result;
    status.textContent = state.connected ? '已连接 · 可在后台操作' : state.connecting ? '正在连接…' : '未连接，请先在 Open Harness 中准备 Chrome 连接。';
    if (state.error && !state.connected) status.textContent += ` ${state.error}`;
    document.querySelector('#allow-new').checked = state.allowNewTabs;
    const list = document.querySelector('#shared'); list.replaceChildren();
    for (const item of state.shared) {
      const row = document.createElement('li'); const label = document.createElement('span'); label.textContent = `${item.origin} · #${item.tab_id}`;
      const remove = document.createElement('button'); remove.textContent = '取消共享'; remove.addEventListener('click', () => { void request('revoke', { tab_id: item.tab_id }); });
      row.append(label, remove); list.append(row);
    }
  } catch (error) { status.textContent = error.message; }
}
document.querySelector('#connect').addEventListener('click', () => { void request('connect'); });
document.querySelector('#share').addEventListener('click', () => { void request('shareCurrent'); });
document.querySelector('#disconnect').addEventListener('click', () => { void request('disconnect'); });
document.querySelector('#allow-new').addEventListener('change', event => { void request('allowNewTabs', { value: event.target.checked }); });
void request('status');
const poll = setInterval(() => { void request('status'); }, 1000);
addEventListener('pagehide', () => clearInterval(poll), { once: true });
