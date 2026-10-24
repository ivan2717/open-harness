// Executed only in an extension-created isolated world. No page-provided code,
// selectors or JavaScript expressions are accepted from model/tool arguments.
export function pageOperation(operation, args) {
  // Navigation can replace the document after tabs.get / Page.getFrameTree.
  // Check again inside the exact isolated world before reading or dispatching.
  if (!location.href || location.href === 'about:blank') throw new Error('page_loading: wait for navigation, then retry chrome_snapshot only.');
  if (location.origin !== args.expected_origin) throw new Error('origin_changed: document access requires authorization again.');
  const key = '__openHarnessPageState';
  let state = globalThis[key];
  if (!state) {
    state = globalThis[key] = { version: 0, refs: new Map(), next: 0, observer: null };
    state.observer = new MutationObserver(() => { state.version++; });
    state.observer.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    for (const event of ['input', 'change', 'scroll']) document.addEventListener(event, () => { state.version++; }, true);
  }
  const flush = () => { if (state.observer.takeRecords().length) state.version++; };
  flush();
  const short = (value, limit) => String(value ?? '').toWellFormed().slice(0, limit).replace(/[\uD800-\uDBFF]$/u, '');
  const visible = element => { const box = element.getBoundingClientRect(); const style = getComputedStyle(element); return box.width > 0 && box.height > 0 && style.visibility !== 'hidden' && style.display !== 'none'; };
  if (operation === 'snapshot') {
    state.refs = new Map(); state.next = 0;
    const elements = [];
    const selector = 'a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=tab],[role=combobox],[role=textbox],[contenteditable=true]';
    for (const element of document.querySelectorAll(selector)) {
      if (elements.length >= 100) break;
      if (!visible(element)) continue;
      const rect = element.getBoundingClientRect();
      if (rect.bottom < 0 || rect.top > innerHeight || rect.right < 0 || rect.left > innerWidth) continue;
      const ref = `e${++state.next}`; state.refs.set(ref, element);
      const type = element.getAttribute('type');
      elements.push({ ref, role: element.getAttribute('role') || element.tagName.toLowerCase(), name: short(element.getAttribute('aria-label') || element.labels?.[0]?.innerText || element.getAttribute('placeholder') || element.innerText || element.getAttribute('title'), 180), ...(type ? { type } : {}), ...(element.tagName === 'A' ? { href: short(element.href, 2048) } : {}), ...(element.tagName === 'SELECT' ? { options: Array.from(element.options).slice(0, 30).map(option => ({ value: short(option.value, 100), label: short(option.label, 100) })) } : {}) });
    }
    const allText = document.body?.innerText ?? ''; const offset = args.text_offset ?? 0;
    return { version: state.version, title: short(document.title, 500), url: location.href, text: short(allText.slice(offset), 16000), ...(allText.length > offset + 16000 ? { next_text_offset: offset + 16000 } : {}), elements, viewport: { width: innerWidth, height: innerHeight } };
  }
  if (state.version !== args.version) throw new Error('stale_snapshot: page changed; take a fresh snapshot.');
  if (operation === 'scroll') { scrollBy({ top: args.delta_y, behavior: 'instant' }); return { dispatched: true }; }
  const element = state.refs.get(args.ref);
  if (!element || !element.isConnected || !visible(element)) throw new Error('stale_snapshot: element is no longer available.');
  if (element.disabled || element.getAttribute('aria-disabled') === 'true') throw new Error('Control is disabled.');
  if (operation === 'click') {
    const rect = element.getBoundingClientRect();
    const x = Math.min(innerWidth - 1, Math.max(0, rect.x + rect.width / 2)); const y = Math.min(innerHeight - 1, Math.max(0, rect.y + rect.height / 2));
    const hit = document.elementFromPoint(x, y);
    if (hit !== element && !element.contains(hit)) throw new Error('Control is obscured; observe or scroll first.');
    return { x, y };
  }
  if (operation === 'focus') { element.focus({ preventScroll: true }); if (document.activeElement !== element) throw new Error('Control could not be focused.'); return { focused: true }; }
  if (operation === 'fill') {
    if (element.tagName === 'INPUT' && ['file', 'password', 'hidden'].includes(element.type)) throw new Error('File and password inputs must be completed by the user.');
    if (!['INPUT', 'TEXTAREA'].includes(element.tagName) && !element.isContentEditable) throw new Error('Control is not a text field.');
    if (element.readOnly) throw new Error('Control is read-only.');
    if (element.isContentEditable) element.textContent = args.text;
    else {
      const prototype = element.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(element, args.text);
    }
    element.dispatchEvent(new Event('input', { bubbles: true })); element.dispatchEvent(new Event('change', { bubbles: true }));
    state.version++; return { dispatched: true };
  }
  if (operation === 'select') {
    if (element.tagName !== 'SELECT' || !Array.from(element.options).some(option => option.value === args.text)) throw new Error('Invalid select option.');
    element.value = args.text; element.dispatchEvent(new Event('input', { bubbles: true })); element.dispatchEvent(new Event('change', { bubbles: true })); state.version++;
    return { dispatched: true };
  }
  throw new Error('Unsupported page operation.');
}
