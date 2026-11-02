// Fixed page operation, serialized by Playwright. Validate the field type and
// write in the same document task so a password/file type change cannot race a
// separate host-side check. No page-provided code or selectors are evaluated.
export function fillTextField(element, { text, origin }) {
  if (location.origin !== origin) return { blocked: "origin_changed" };
  if (element.tagName === "INPUT" && ["password", "file", "hidden"].includes(element.type))
    return { blocked: "sensitive_input" };
  if (!["INPUT", "TEXTAREA"].includes(element.tagName) && !element.isContentEditable)
    return { blocked: "not_text_field" };
  if (!element.isConnected || element.disabled || element.readOnly || element.getAttribute("aria-disabled") === "true")
    return { blocked: "unavailable_field" };
  element.focus({ preventScroll: true });
  if (element.isContentEditable) element.textContent = text;
  else {
    const prototype = element.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, "value").set.call(element, text);
  }
  element.dispatchEvent(new Event("input", { bubbles: true }));
  element.dispatchEvent(new Event("change", { bubbles: true }));
  return { dispatched: true };
}
