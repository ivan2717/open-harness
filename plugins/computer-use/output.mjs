// Budget the escaped MCP envelope, not just the inner AX JSON. Keep this below
// the host's 32 KiB limit so snapshot IDs and element refs remain machine-readable.
export const MAX_METADATA_BYTES = 28 * 1024;
const labels = ['AXTitle', 'AXDescription', 'AXIdentifier', 'AXValue', 'name', 'automation_id', 'value', 'help_text', 'class_name'];
const interactive = /Button|TextField|TextArea|CheckBox|Radio|Slider|Link|Hyperlink|ComboBox|MenuItem|PopUp|Edit|ListItem|TreeItem|DataItem/;
const actionable = element => interactive.test(element.role) || element.actions?.length > 0 || element.settable_value;
const focused = element => element.AXFocused || element.focused;
const shorten = (text, limit) => Array.from(text.toWellFormed()).slice(0, limit).join('');
function encode(value) {
  const text = JSON.stringify(value);
  return { text, bytes: Buffer.byteLength(JSON.stringify({ content: [{ type: 'text', text }] })) };
}

export function metadataText(metadata) {
  let encoded = encode(metadata);
  if (encoded.bytes <= MAX_METADATA_BYTES) return encoded.text;
  const result = structuredClone(metadata);
  result.truncated = true;
  result.truncation_hint = 'Observation text was shortened. Use the visible refs or a fresh screenshot; omitted content is not evidence of absence.';
  for (const limit of [128, 64, 32]) {
    for (const element of result.elements ?? []) {
      for (const key of labels) {
        if (typeof element[key] === 'string') element[key] = shorten(element[key], limit);
      }
    }
    for (const window of result.windows ?? []) {
      if (typeof window.title === 'string') window.title = shorten(window.title, limit);
    }
    for (const app of result.applications ?? []) {
      if (typeof app.name === 'string') app.name = shorten(app.name, limit);
    }
    if (typeof result.error === 'string') result.error = shorten(result.error, 1024);
    encoded = encode(result);
    if (encoded.bytes <= MAX_METADATA_BYTES) return encoded.text;
  }
  // Preserve focused and actionable controls before spending space on containers
  // and static text. Retained refs are unchanged and still index the native state.
  while (result.elements?.length && encoded.bytes > MAX_METADATA_BYTES) {
    let index = result.elements.findLastIndex(element => !focused(element) && !actionable(element));
    if (index === -1) index = result.elements.findLastIndex(element => !focused(element));
    if (index === -1) index = result.elements.length - 1;
    result.elements.splice(index, 1);
    encoded = encode(result);
  }
  while (result.applications?.length && encoded.bytes > MAX_METADATA_BYTES) {
    result.applications.pop();
    encoded = encode(result);
  }
  while (result.windows?.length && encoded.bytes > MAX_METADATA_BYTES) {
    const index = result.windows.findLastIndex(window => window.id !== result.selected_window_id);
    if (index < 0) break;
    result.windows.splice(index, 1);
    encoded = encode(result);
  }
  if (encoded.bytes > MAX_METADATA_BYTES) {
    throw new Error('Computer metadata exceeded the result budget. The previous action may already have occurred; observe before continuing.');
  }
  return encoded.text;
}

// Preserve native delivery certainty; transport failures cannot prove an action
// was not delivered. Refresh state rather than automatically replaying input.
export function errorMetadata(error, requested = false) {
  const native = error?.computer;
  const dispatched = native && (typeof native.dispatched === 'boolean' || native.dispatched === null)
    ? native.dispatched : requested ? null : false;
  return {
    error: error instanceof Error ? error.message : 'Computer operation failed.',
    code: typeof native?.code === 'string' ? native.code : requested ? 'TRANSPORT_ERROR' : 'INVALID_REQUEST',
    recoverable: typeof native?.recoverable === 'boolean' ? native.recoverable : true,
    dispatched,
    next: typeof native?.next === 'string' ? native.next : requested
      ? 'Select the app again and observe its current state before deciding on another action. No action was replayed.'
      : 'Correct the request, then obtain a fresh observation before input.',
  };
}
