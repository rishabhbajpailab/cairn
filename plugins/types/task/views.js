// Minimal renderers for `task`. Replace with richer ones as the UI grows (ADR 0006).
// Missing renderers (e.g. editor) fall back to the generic JSON renderer in web/.

function render(root, item) {
  root.replaceChildren();
  const title = document.createElement('strong');
  title.textContent = item.payload.name ?? item.title;
  const meta = document.createElement('small');
  meta.className = 'cairn-meta';
  meta.textContent = `${item.payload.uses} · ${item.payload.enabled === false ? "disabled" : Object.keys(item.payload.trigger ?? {})[0] ?? ""}`;
  root.append(title, meta);
}

const mount = (root, item) => {
  render(root, item);
  return { update: (next) => render(root, next), destroy: () => root.replaceChildren() };
};

export default { card: mount, node: mount, detail: mount };
